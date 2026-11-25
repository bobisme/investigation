#!/usr/bin/env python3
"""Optional real-viewer gate (W12.07, plan §12.3, §13.4, §20.4): check rendered vaults in a
running Obsidian desktop app through its official command line interface.

Not part of `just check`, and never needed by the core suite: rendering works with no viewer
installed. The fake-CLI tests of this adapter (scripts/test-obsidian-ui-check.py) run in
`just check`; this script against a real app runs only when someone starts it.

  python3 scripts/obsidian-ui-check.py --bin BINARY prepare DIR   # build the test vault; no viewer
  python3 scripts/obsidian-ui-check.py --bin BINARY run DIR       # drive the running viewer
  python3 scripts/obsidian-ui-check.py --bin BINARY timings --vault VAULT --samples 5

`prepare` copies the Scenario A to D fixture workspaces (fixtures/vault-goldens/) into DIR and
renders each into one dedicated vault, DIR/investigation-ui-check, under its own managed prefix
(existing-vault mode). It adds one note of "the vault owner" with a deliberately unresolved link
and a probe folder of Bases that test the filter operators. It writes no viewer configuration:
open the vault once in the viewer ("Open folder as vault"), keep Restricted Mode on, then `run`.

`run` checks, in this order: the CLI answers `version`; the bundled compatibility profile
resolves that release (`investigation doctor --viewer-release`); the vault name the CLI targets
is exactly this folder; no community plugin is enabled; `investigation obsidian setup` (preview,
--apply) changes only its two app.json keys, and after a reload the viewer writes new links as
vault-root wikilinks; the viewer lists every prepared file; one note of each generated type
opens; aliases and excerpt anchors resolve; each Base's rows equal its static table; the probe
Bases confirm `!=`, `> 0` and that the frontier filter's and-ordering does not matter; after a
window reload and once two consecutive answers agree, no unresolved link starts in an owned file
(and the owner's planted one is reported); each Canvas opens and its file nodes resolve.

`timings` measures the viewer side of the vault load test (docs/vault-load.md) against a vault
kept by `just load-test DIR --cases 130`: CLI round trip, window reload until every file is
listed, one Base query, Canvas open, search, and a cold open (app restart) only with
--allow-restart, because `restart` restarts the whole app.

Every call is an argv list (never a shell string, never a shell eval), and every call after
`version` starts with `vault=<name>` and names a file with `path=<exact vault path>`. Checks
that need the CLI's `eval` command (active view, block cache, link generation) run fixed
JavaScript from this file with only JSON-encoded paths of the prepared vault inserted;
--no-eval skips them.

Exit status: 0 every check passed, or the viewer or its CLI is not available (status
"skipped", with the reason); 1 a check failed; 2 a usage error, or the run could not start
(DIR not prepared, vault not opened in the viewer, binary missing): status "blocked".
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
GOLDENS = ROOT / "fixtures/vault-goldens"
SCENARIOS = ("scenario-a", "scenario-b", "scenario-c", "scenario-d")
VAULT_NAME = "investigation-ui-check"
PREFIX_ROOT = "Investigation"
HUMAN = ("Notes", "Boards", "Inbox")
PROBE_FOLDER = "UI Check Probe"
OWNER_NOTE = "Notes/Owner note.md"
OWNER_LINK = "A note that does not exist"
EXPECTED = "expected.json"
EXIT_PASS, EXIT_FAIL, EXIT_BLOCKED = 0, 1, 2
# Files the running viewer rewrites by itself (open tabs, pane layout); not compared when
# checking that setup changed nothing but its keys.
VIEWER_WRITES = ("workspace.json", "workspace-mobile.json")
SETUP_KEYS = {"useMarkdownLinks": False, "newLinkFormat": "absolute"}
CHECK_GROUPS = ("plugins", "setup", "files", "notes", "anchors", "bases", "probes", "unresolved",
                "canvas", "errors")

# The commands and parameters used, as the official CLI documentation lists them. Checked on
# the date given; the page carries no revision of its own.
CLI_DOCS = {
    "url": "https://obsidian.md/help/cli",
    "checked": "2026-11-24",
    "also": ["https://obsidian.md/help/bases/syntax", "https://obsidian.md/help/plugins/canvas",
             "https://raw.githubusercontent.com/obsidianmd/obsidian-api/master/obsidian.d.ts"],
    "notes": [
        "Requires the 1.12 installer (1.12.7 or later recommended) and the app running; "
        "'If Obsidian is not running, the first command you run launches Obsidian.'",
        "vault=<name|id> must come first, before the command; without it the CLI uses the vault "
        "of the working directory or the active vault.",
        "path=<path> is the exact path from the vault root; file=<name> resolves like a "
        "wikilink, so it is never used here.",
        "Exit codes and error output are not documented: a non-zero exit or output starting "
        "with 'Error' is treated as a failed call, and raw output is kept in the report.",
        "Output shapes of unresolved (json), vaults verbose and eval are not documented; the "
        "parsers below accept the plausible shapes and fail a check on anything else.",
    ],
    "commands": {
        "version": "Show Obsidian version.",
        "vault": "info=name|path|files|folders|size",
        "plugins:enabled": "filter=core|community, versions, format=json|tsv|csv",
        "plugins:restrict": "on|off; no flag checks the current state (never toggled here)",
        "files": "folder=, ext=, total",
        "file": "file=, path=; prints path, name, extension, size, created, modified",
        "open": "file=, path=, newtab",
        "aliases": "file=, path=, total, verbose, active",
        "base:query": "file=, path=, view=, format=json|csv|tsv|md|paths (default json)",
        "unresolved": "total, counts, verbose (include source files), format=json|tsv|csv",
        "search": "query= (required), path=, limit=, format=text|json, total, case",
        "reload": "Reload the app window.",
        "restart": "Restart the app.",
        "dev:errors": "Show captured JavaScript errors; clear.",
        "eval": "code= (required): Execute JavaScript and return result.",
    },
}

# Fixed JavaScript for the CLI's eval command; the marker comment names the snippet, and
# only json.dumps() of a vault path is ever inserted.
JS_ACTIVE_VIEW = ("/*inv-ui:active-view*/ (() => { const leaf = app.workspace.getMostRecentLeaf();"
                  " const view = leaf && leaf.view; return JSON.stringify({type: view ?"
                  " view.getViewType() : null, path: view && view.file ? view.file.path : null});"
                  " })()")
JS_BLOCKS = ("/*inv-ui:blocks*/ (() => { const file = app.vault.getAbstractFileByPath(%s);"
             " const cache = file && app.metadataCache.getFileCache(file);"
             " return JSON.stringify(cache ? Object.keys(cache.blocks || {}) : null); })()")
JS_LINK = ("/*inv-ui:link*/ (() => { const file = app.vault.getAbstractFileByPath(%s);"
           " return JSON.stringify(file ? app.fileManager.generateMarkdownLink(file, %s) : null);"
           " })()")


class Blocked(Exception):
    """The run cannot start; the message says what to do."""


class Skip(Exception):
    """The viewer or its CLI is not available."""


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


# ---------------------------------------------------------------------------- vault reading


def frontmatter(text):
    """A flat frontmatter block as {key: value}; enough for the renderer's own notes."""
    if not text.startswith("---\n"):
        return {}
    end = text.find("\n---\n", 4)
    if end < 0:
        return {}
    values, key = {}, None
    for line in text[4:end].split("\n"):
        if line.startswith("  - ") and key is not None:
            values.setdefault(key, [])
            if isinstance(values[key], list):
                values[key].append(scalar(line[4:]))
            continue
        match = re.match(r"^([A-Za-z0-9_.]+):(?: (.*))?$", line)
        if not match:
            continue
        key, raw = match.group(1), match.group(2)
        values[key] = [] if raw is None else scalar(raw)
    return values


def scalar(raw):
    raw = raw.strip()
    if raw.startswith('"'):
        try:
            return json.loads(raw)
        except ValueError:
            return raw.strip('"')
    if raw in ("true", "false"):
        return raw == "true"
    if raw == "[]":
        return []
    if re.fullmatch(r"-?\d+", raw):
        return int(raw)
    return raw


def first_column(note):
    """Link targets in the first cell of each body row of the first table in `note`."""
    rows, lines = [], note.split("\n")
    index = 0
    while index < len(lines) and not lines[index].startswith("| "):
        index += 1
    index += 2
    while index < len(lines) and lines[index].startswith("|"):
        cell = lines[index]
        if cell.startswith("| [["):
            cell = cell[4:]
            ends = [position for position in (cell.find("\\|"), cell.find("]]")) if position >= 0]
            rows.append(cell[:min(ends)])
        index += 1
    return rows


def static_rows(table):
    """The rows a view's Base must list: the static table the Base file names (plan §12.5)."""
    view = frontmatter(table).get("inv_view")
    section = {"frontier": "\n## Records named by an open gap\n",
               "timeline": "\n## Latest revision of each record\n"}.get(view)
    if section is not None:
        start = table.find(section)
        if start < 0:
            return None
        table = table[start:]
    return sorted(first_column(table))


def base_view_name(text):
    after = text.split("\nviews:\n", 1)
    if len(after) != 2:
        return None
    match = re.search(r'^\s+name:\s+(".*")\s*$', after[1], re.MULTILINE)
    return json.loads(match.group(1)) if match else None


def wikilinks(text):
    """(target, subpath) of every wikilink in a note, without alias."""
    found = []
    for match in re.finditer(r"\[\[([^\]]+?)\]\]", text):
        inner = match.group(1).replace("\\|", "|").split("|", 1)[0]
        target, _, subpath = inner.partition("#")
        found.append((target, subpath))
    return found


def note_type(relative):
    """A generated note's type from its path inside a managed prefix."""
    parts = relative.split("/")
    if parts == ["Start Here.md"]:
        return "start-here"
    if len(parts) == 3 and parts[0] == "Cases":
        return {"Overview.md": "overview", "Map Legend.md": "map-legend",
                "Map Left Out.md": "map-left-out"}.get(parts[2], "case-note:" + parts[2])
    if len(parts) == 4 and parts[0] == "Cases":
        if parts[2] == "Views":
            return "view:" + parts[3][:-3]
        return "record:" + parts[2]
    return "other:" + relative


def is_owned(path, prefixes):
    """Whether a vault path is a generated file of one of the managed prefixes."""
    for prefix in prefixes:
        if path.startswith(prefix + "/"):
            top = path[len(prefix) + 1:].split("/", 1)[0]
            return top not in HUMAN
    return False


def snapshot(folder):
    files = {}
    if folder.is_dir():
        for path in sorted(folder.rglob("*")):
            if path.is_file() and not path.is_symlink():
                files[path.relative_to(folder).as_posix()] = sha256(path.read_bytes())
    return files


# ---------------------------------------------------------------------------- prepare

PROBE_NOTES = {
    "p-view": ("probe", "view", 3),
    "p-zero": ("probe", "hypothesis", 0),
    "p-one": ("probe", "hypothesis", 1),
    "p-two": ("probe", "observation", 2),
    "p-other-case": ("probe-other", "hypothesis", 1),
    "p-missing-gaps": ("probe-missing", "hypothesis", None),
    "p-missing-kind": ("probe-missing", None, 1),
}
CASE, KIND, GAPS = ('inv_case_id == \\"{}\\"', 'inv_kind != \\"view\\"', "inv_open_gaps > 0")
# name: (filter lines in .base YAML, expected probe notes or None for observed only)
PROBE_BASES = {
    "frontier-as-emitted": ([f'    - "{CASE.format("probe")}"', "    - and:", f'        - "{KIND}"',
                             f'        - "{GAPS}"'], ["p-one", "p-two"]),
    "frontier-reordered": (["    - and:", f'        - "{GAPS}"', f'        - "{KIND}"',
                            f'    - "{CASE.format("probe")}"'], ["p-one", "p-two"]),
    "not-equal": ([f'    - "{CASE.format("probe")}"', f'    - "{KIND}"'],
                  ["p-one", "p-two", "p-zero"]),
    "greater-than-zero": ([f'    - "{CASE.format("probe")}"', f'    - "{GAPS}"'],
                          ["p-one", "p-two", "p-view"]),
    "missing-property": ([f'    - "{CASE.format("probe-missing")}"', "    - and:",
                          f'        - "{KIND}"', f'        - "{GAPS}"'], None),
}


def probe_files():
    files = {}
    for name, (case, kind, gaps) in PROBE_NOTES.items():
        lines = ["---", f'inv_case_id: "{case}"']
        if kind is not None:
            lines.append(f'inv_kind: "{kind}"')
        if gaps is not None:
            lines.append(f"inv_open_gaps: {gaps}")
        lines += ["---", f"# {name}", "",
                  "A probe note of the viewer integration gate; not part of any casebook.", ""]
        files[f"{PROBE_FOLDER}/{name}.md"] = "\n".join(lines)
    for name, (filters, _) in PROBE_BASES.items():
        files[f"{PROBE_FOLDER}/{name}.base"] = "\n".join(
            ["filters:", "  and:", *filters, "views:", "  - type: table", f'    name: "{name}"',
             "    order:", "      - file.name", "      - inv_kind", "      - inv_open_gaps", ""])
    return files


def run_binary(binary, args, timeout=120):
    result = subprocess.run([str(binary), *args, "--format", "json"], capture_output=True,
                            text=True, timeout=timeout, check=False)
    try:
        value = json.loads(result.stdout)
    except ValueError:
        value = None
    return result.returncode, value, result


def prepare(args):
    directory = Path(args.dir).expanduser().absolute()
    if directory.exists() and any(directory.iterdir()):
        raise Blocked(f"{directory} is not empty; prepare needs an absent or empty directory.")
    binary = require_binary(args.bin)
    vault = directory / VAULT_NAME
    (vault / PREFIX_ROOT).mkdir(parents=True)
    prefixes, releases = {}, {}
    for scenario in SCENARIOS:
        workspace = directory / "workspaces" / scenario
        shutil.copytree(GOLDENS / scenario / "workspace", workspace)
        for path in workspace.rglob("*"):
            path.chmod(path.stat().st_mode | 0o200)
        for folder in ("state/cases", "state/events", "state/blobs/sha256", "state/checkpoints",
                       "state/staging", "state/locks", "state/tmp", "cache", "local", "exports",
                       "vault/Notes", "vault/Boards", "vault/Inbox"):
            (workspace / folder).mkdir(parents=True, exist_ok=True)
        prefix = f"{PREFIX_ROOT}/{scenario}"
        code, value, result = run_binary(binary, ["init", str(workspace), "--existing-vault",
                                                  str(vault), "--managed-prefix", prefix])
        if code != 0:
            raise Blocked(f"init {scenario} failed: {result.stdout}{result.stderr}")
        code, value, result = run_binary(binary, ["--workspace", str(workspace), "vault", "render"])
        if code != 0 or not value or value.get("complete") is not True:
            raise Blocked(f"render {scenario} failed: {result.stdout}{result.stderr}")
        prefixes[scenario] = prefix
        config = (workspace / "investigation.toml").read_text(encoding="utf-8")
        match = re.search(r'^viewer_release = "(.*)"$', config, re.MULTILINE)
        releases[scenario] = match.group(1) if match else None
    extra = probe_files()
    extra[OWNER_NOTE] = ("# Owner note\n\nA note of the vault owner, not generated. Its link is "
                         f"deliberately unresolved: [[{OWNER_LINK}]].\n")
    for relative, text in extra.items():
        target = vault / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")
    expected = {
        "schema": "inv.obsidian-ui-expected.v1",
        "prepared": now(),
        "vault": {"name": VAULT_NAME, "folder": VAULT_NAME},
        "prefixes": prefixes,
        "viewer_release_stated": releases,
        "files": snapshot(vault),
        "owner_files": [OWNER_NOTE],
    }
    (directory / EXPECTED).write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n",
                                      encoding="utf-8")
    print(f"Prepared {len(expected['files'])} files in {vault}.")
    print("Next: open that folder in the viewer once (Open folder as vault), keep Restricted Mode "
          "on (do not turn on community plugins), then run:")
    print(f"  just obsidian-ui run {directory}")
    return EXIT_PASS


def require_binary(path):
    if not path:
        raise Blocked("--bin BINARY is required (the investigation executable).")
    binary = Path(path).absolute()
    if not binary.is_file():
        raise Blocked(f"{binary} does not exist; build it with cargo build -p investigation-cli.")
    return binary


# ---------------------------------------------------------------------------- the CLI


class Cli:
    """The viewer's CLI: argv lists only, with the vault named first on every targeted call."""

    def __init__(self, argv_prefix, vault_name, timeout, log):
        self.prefix = list(argv_prefix)
        self.vault_name = vault_name
        self.timeout = timeout
        self.log = log

    def call(self, *arguments, targeted=True, timeout=None):
        argv = [*self.prefix, *([f"vault={self.vault_name}"] if targeted else []), *arguments]
        started = time.monotonic()
        try:
            result = subprocess.run(argv, capture_output=True, text=True, check=False,
                                    timeout=timeout or self.timeout, stdin=subprocess.DEVNULL)
            code, out, err = result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired as expired:
            code, out, err = None, expired.stdout or "", "timed out"
            out = out.decode() if isinstance(out, bytes) else out
        except OSError as error:
            code, out, err = None, "", str(error)
        elapsed = time.monotonic() - started
        ok = code == 0 and not out.lstrip().lower().startswith("error")
        self.log.append({"argv": argv[len(self.prefix):], "exit": code, "ok": ok,
                         "seconds": round(elapsed, 4), "stdout": out[:2000], "stderr": err[:500]})
        return ok, out, elapsed

    def eval_json(self, code):
        ok, out, _ = self.call("eval", f"code={code}")
        if not ok:
            return False, out
        text = out.strip()
        if text.startswith("=>"):
            text = text[2:].strip()
        for candidate in (text, text[text.find("{"):] if "{" in text else "",
                          text[text.find("["):] if "[" in text else ""):
            try:
                value = json.loads(candidate)
            except ValueError:
                continue
            if isinstance(value, str):
                try:
                    return True, json.loads(value)
                except ValueError:
                    return True, value
            return True, value
        return False, out


def find_cli(explicit, extra):
    if explicit:
        path = Path(explicit)
        if not path.is_file():
            raise Skip(f"--obsidian {explicit} does not exist.")
        return [str(path.absolute()), *extra]
    found = shutil.which("obsidian")
    if not found:
        raise Skip("No obsidian command on PATH: the viewer's CLI is not installed or not "
                   "registered (Settings, General, Command line interface).")
    return [found, *extra]


def parse_release(text):
    match = re.search(r"\b(\d+\.\d+\.\d+)\b", text)
    return match.group(1) if match else None


def lines(text):
    return [line.strip() for line in text.splitlines() if line.strip()]


def info_fields(text):
    fields = {}
    for line in lines(text):
        key, _, value = line.partition(" ")
        if "\t" in key:
            key, _, value = line.partition("\t")
        fields[key.rstrip(":")] = value.strip()
    return fields


def unresolved_pairs(text):
    """(link, [sources]) pairs from `unresolved verbose format=json`, whose shape the
    documentation does not give; None when the output is not understood."""
    try:
        value = json.loads(text)
    except ValueError:
        return None
    pairs = []

    def sources_of(item):
        for key in ("sources", "files", "from", "source", "paths"):
            if key in item:
                found = item[key]
                if isinstance(found, str):
                    # Obsidian 1.15.1 prints one string here (seen with one source); split a
                    # joined list so an owned source is never hidden behind another.
                    return [piece.strip() for piece in re.split(r",\s+|\n", found)
                            if piece.strip()]
                if isinstance(found, dict):
                    return list(found)
                if isinstance(found, list):
                    return [entry if isinstance(entry, str) else entry.get("path", "")
                            for entry in found]
        return []

    if isinstance(value, list):
        for item in value:
            if isinstance(item, str):
                pairs.append((item, []))
            elif isinstance(item, dict):
                link = next((item[key] for key in ("link", "target", "name", "unresolved")
                             if key in item), None)
                if link is None:
                    return None
                pairs.append((link, sources_of(item)))
            else:
                return None
    elif isinstance(value, dict):
        for link, item in value.items():
            if isinstance(item, list):
                pairs.append((link, [entry for entry in item if isinstance(entry, str)]))
            elif isinstance(item, dict):
                pairs.append((link, sources_of(item) or list(item)))
            else:
                pairs.append((link, []))
    else:
        return None
    return pairs


# ---------------------------------------------------------------------------- run


class Report:
    def __init__(self, mode):
        self.value = {"schema": "inv.obsidian-ui-report.v1", "mode": mode, "started": now(),
                      "cli_documentation": CLI_DOCS, "checks": [], "calls": []}

    def check(self, check_id, passed, summary, **detail):
        status = passed if isinstance(passed, str) else ("pass" if passed else "fail")
        self.value["checks"].append({"id": check_id, "status": status, "summary": summary,
                                     **detail})
        return status == "pass"

    def failed(self):
        return any(check["status"] == "fail" for check in self.value["checks"])


POLL_INTERVAL = 1.0


def settle(attempt, deadline, interval=None):
    """Repeat `attempt` until it returns a passing result or the deadline passes."""
    while True:
        result = attempt()
        if result[0] or time.monotonic() >= deadline:
            return result
        time.sleep(POLL_INTERVAL if interval is None else interval)


def run(args):
    directory = Path(args.dir).expanduser().absolute()
    report = Report("run")
    out = Path(args.out).absolute() if args.out else ROOT / "target/obsidian-ui"
    try:
        status = run_checks(args, directory, report)
    except Skip as skip:
        status = "skipped"
        report.value["skip_reason"] = str(skip)
    except Blocked as blocked:
        status = "blocked"
        report.value["blocked_reason"] = str(blocked)
    report.value["status"] = status
    report.value["finished"] = now()
    write_report(report.value, out, "report")
    return {"pass": EXIT_PASS, "skipped": EXIT_PASS, "fail": EXIT_FAIL}.get(status, EXIT_BLOCKED)


def doctor(binary, release):
    code, value, result = run_binary(binary, ["doctor", "--viewer-release", release])
    if code != 0 or not value or "viewer" not in value:
        raise Blocked(f"doctor --viewer-release failed: {result.stdout}{result.stderr}")
    return value["viewer"]


def run_checks(args, directory, report):
    expected_path = directory / EXPECTED
    if not expected_path.is_file():
        raise Blocked(f"{directory} is not prepared: run `just obsidian-ui prepare {directory}` "
                      "first.")
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    vault = directory / expected["vault"]["folder"]
    name = args.vault_name or expected["vault"]["name"]
    prefixes = sorted(expected["prefixes"].values())
    report.value["vault"] = {"name": name, "path": str(vault), "prefixes": prefixes,
                             "viewer_release_stated": expected["viewer_release_stated"]}
    groups = set(args.checks.split(",")) if args.checks else set(CHECK_GROUPS)
    unknown = groups - set(CHECK_GROUPS)
    if unknown:
        raise Blocked(f"unknown check group(s) {sorted(unknown)}; known: {list(CHECK_GROUPS)}")
    binary = require_binary(args.bin)
    global POLL_INTERVAL
    POLL_INTERVAL = args.poll_interval
    deadline = lambda: time.monotonic() + args.settle  # noqa: E731

    argv_prefix = find_cli(args.obsidian, args.obsidian_arg)
    cli = Cli(argv_prefix, name, args.timeout, report.value["calls"])
    report.value["obsidian"] = {"cli": argv_prefix}
    ok, version_text, _ = cli.call("version", targeted=False)
    if not ok:
        raise Skip("The obsidian command did not answer `version` (is the app installed, its CLI "
                   f"enabled and running?): {version_text.strip()[:200]}")
    release = parse_release(version_text)
    report.value["obsidian"].update({"version_output": version_text.strip(), "release": release})
    if release is None:
        report.check("cli.version", False, "`version` printed no release number",
                     output=version_text)
        return "fail"
    report.check("cli.version", True, f"the CLI reports release {release}")
    compat = doctor(binary, release)
    report.value["compatibility"] = compat
    report.check("profile.resolution", compat["level"] == "optional",
                 f"profile revision {compat['profile']['revision']} resolves {release} to "
                 f"{compat['level']} (range {compat['profile']['supported_range']['min']}.."
                 f"{compat['profile']['supported_range']['max']}, pinned "
                 f"{compat['profile']['pinned_release']} by {compat['profile']['verification']})")

    ok, path_text, _ = cli.call("vault", "info=path")
    targeted = lines(path_text)[-1] if ok and lines(path_text) else None
    if targeted is None or os.path.realpath(targeted) != os.path.realpath(vault):
        raise Blocked(f"vault={name} does not target {vault} (the CLI answered "
                      f"{path_text.strip()[:200]!r}). Open {vault} in the viewer once (Open folder "
                      "as vault), or pass --vault-name with the name or ID the viewer lists for "
                      "it, then run again.")
    report.check("vault.target", True, f"vault={name} targets {vault}")

    disk = snapshot(vault)
    changed = sorted(path for path, digest in expected["files"].items()
                     if disk.get(path) != digest)
    report.check("vault.unchanged", not changed,
                 "every prepared file is on disk with its prepared bytes" if not changed else
                 f"{len(changed)} prepared file(s) are missing or changed", paths=changed[:50])

    if "plugins" in groups:
        check_plugins(cli, report)
    if "setup" in groups:
        check_setup(cli, report, binary, vault, prefixes, expected, args.no_eval, args.settle)
    if "files" in groups:
        check_files(cli, report, expected, deadline())
    owned_notes = sorted(path for path in expected["files"]
                         if path.endswith(".md") and is_owned(path, prefixes))
    if "notes" in groups:
        check_notes(cli, report, vault, owned_notes, prefixes, args.no_eval, deadline())
    if "anchors" in groups:
        check_anchors(cli, report, vault, owned_notes, args.no_eval, deadline())
    if "bases" in groups:
        check_bases(cli, report, vault, expected, prefixes, deadline())
    if "probes" in groups:
        check_probes(cli, report, deadline())
    if "unresolved" in groups:
        check_unresolved(cli, report, prefixes, args.cache_settle, args.cache_reloads)
    if "canvas" in groups:
        check_canvas(cli, report, vault, expected, prefixes, args.no_eval)
    if "errors" in groups:
        ok, errors, _ = cli.call("dev:errors")
        report.check("viewer.errors", "info", "JavaScript errors the viewer captured, as printed "
                     "(not judged: they may come from anything in the app)",
                     output=errors.strip()[:4000])
    return "fail" if report.failed() else "pass"


def check_plugins(cli, report):
    ok, text, _ = cli.call("plugins:enabled", "filter=community", "format=json")
    enabled = None
    if ok:
        try:
            value = json.loads(text) if text.strip() else []
            enabled = list(value) if isinstance(value, (list, dict)) else None
        except ValueError:
            enabled = None
    report.check("plugins.community_disabled", enabled == [],
                 "no community plugin is enabled in the test vault" if enabled == [] else
                 "community plugins are enabled, or the answer was not understood",
                 output=text.strip()[:2000])
    _, restrict, _ = cli.call("plugins:restrict")
    report.check("plugins.restricted_mode", "info", "Restricted Mode state as printed (read, never "
                 "toggled)", output=restrict.strip()[:500])


def config_folder(vault):
    return vault / ".obsidian"


def load_json(path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def check_setup(cli, report, binary, vault, prefixes, expected, no_eval, settle_seconds):
    """Comment 4 of W12.07: setup changes only its keys; the viewer then honours them."""
    config = config_folder(vault)
    if not config.is_dir():
        report.check("setup.config_folder", False, f"{config} does not exist: open the vault in "
                     "the viewer once so it creates its configuration folder")
        return
    compared = lambda files: {path: digest for path, digest in files.items()  # noqa: E731
                              if path not in VIEWER_WRITES}
    source = f"{prefixes[0]}/Start Here.md"
    target = next((path for path in sorted(expected["files"])
                   if path.startswith(prefixes[0] + "/Cases/") and path.count("/") >= 5
                   and path.endswith(".md")), None)
    before_link = None
    if not no_eval and target:
        _, before_link = cli.eval_json(JS_LINK % (json.dumps(target), json.dumps(source)))
    before = snapshot(config)
    before_app = load_json(config / "app.json") or {}
    code, preview, result = run_binary(binary, ["obsidian", "setup", "--vault", str(vault)])
    after_preview = snapshot(config)
    proposed = {row["key"]: row["proposed"] for row in (preview or {}).get("settings", [])}
    report.check("setup.preview",
                 code == 0 and proposed == SETUP_KEYS and not (preview or {}).get("written")
                 and compared(after_preview) == compared(before),
                 "the preview proposes exactly useMarkdownLinks=false and newLinkFormat=absolute "
                 "and writes nothing", proposed=proposed, exit=code,
                 output=None if code == 0 else result.stdout[-1500:])
    code, applied, result = run_binary(binary, ["obsidian", "setup", "--vault", str(vault),
                                                "--apply"])
    after = snapshot(config)
    other = sorted(path for path in set(before) | set(after)
                   if path not in VIEWER_WRITES and path != "app.json"
                   and before.get(path) != after.get(path))
    app = load_json(config / "app.json") or {}
    kept = {key: value for key, value in before_app.items() if key not in SETUP_KEYS}
    report.check("setup.apply",
                 code == 0 and not other and all(app.get(k) == v for k, v in SETUP_KEYS.items())
                 and {k: v for k, v in app.items() if k not in SETUP_KEYS} == kept,
                 "--apply wrote only the two keys of app.json; every other key and every other "
                 "file of the configuration folder is unchanged",
                 other_files_changed=other, written=(applied or {}).get("written"),
                 viewer_written_not_compared=list(VIEWER_WRITES))
    cli.call("reload")
    settle(lambda: cli.call("vault", "info=path"), time.monotonic() + settle_seconds)
    app_after_reload = load_json(config / "app.json") or {}
    report.check("setup.kept_after_reload",
                 all(app_after_reload.get(k) == v for k, v in SETUP_KEYS.items()),
                 "after a reload the viewer kept both keys in app.json",
                 values={k: app_after_reload.get(k) for k in SETUP_KEYS})
    if no_eval or not target:
        report.check("setup.honoured", "skipped", "needs eval (--no-eval given)" if no_eval else
                     "no target note")
    else:
        wanted = f"[[{target[:-3]}]]"
        ok, link = settle(lambda: (lambda r: (r[1] == wanted, r[1]))(
            cli.eval_json(JS_LINK % (json.dumps(target), json.dumps(source)))),
            time.monotonic() + settle_seconds)
        report.check("setup.honoured", ok,
                     "a new link from Start Here to a record note is a vault-root wikilink",
                     wanted=wanted, generated=link, before_setup=before_link,
                     discriminating=before_link != wanted)
    code, again, _ = run_binary(binary, ["obsidian", "setup", "--vault", str(vault), "--apply"])
    report.check("setup.idempotent", code == 0 and (again or {}).get("changed") is False,
                 "a second --apply changes nothing")


def check_files(cli, report, expected, deadline):
    wanted = set(expected["files"])

    def attempt():
        ok, text, _ = cli.call("files")
        listed = set(lines(text)) if ok else set()
        missing = sorted(wanted - listed)
        return not missing, missing

    ok, missing = settle(attempt, deadline)
    report.check("viewer.files", ok, f"the viewer lists all {len(wanted)} prepared files" if ok
                 else f"the viewer does not list {len(missing)} prepared file(s)",
                 missing=missing[:50])


def check_notes(cli, report, vault, owned_notes, prefixes, no_eval, deadline):
    by_type = {}
    for path in owned_notes:
        relative = next(path[len(prefix) + 1:] for prefix in prefixes
                        if path.startswith(prefix + "/"))
        by_type.setdefault(note_type(relative), path)
    failures, aliases_checked, alias_failures = [], 0, []
    for kind, path in sorted(by_type.items()):
        target = f"path={path}"
        opened, out, _ = cli.call("open", target)
        known, info, _ = cli.call("file", target)
        problem = None
        if not opened:
            problem = f"open failed: {out.strip()[:200]}"
        elif not known or info_fields(info).get("path") != path:
            problem = f"file did not report the path: {info.strip()[:200]}"
        elif not no_eval:
            _, view = cli.eval_json(JS_ACTIVE_VIEW)
            if not isinstance(view, dict) or view.get("path") != path \
                    or view.get("type") != "markdown":
                problem = f"the active view is not this note as Markdown: {view!r}"
        if problem:
            failures.append({"type": kind, "path": path, "problem": problem})
        titles = frontmatter((vault / path).read_text(encoding="utf-8")).get("aliases") or []
        if titles:
            aliases_checked += 1
            def attempt():
                ok, text, _ = cli.call("aliases", f"path={path}")
                return ok and all(title in text for title in titles), text

            ok, text = settle(attempt, deadline)
            if not ok:
                alias_failures.append({"path": path, "aliases": titles, "output": text[:300]})
    report.check("notes.open", not failures, f"opened one note of each of {len(by_type)} "
                 "generated note types" if not failures else
                 f"{len(failures)} of {len(by_type)} note types did not open",
                 types=sorted(by_type), failures=failures)
    report.check("notes.aliases", aliases_checked > 0 and not alias_failures,
                 f"the viewer reports the title alias of {aliases_checked} opened notes",
                 failures=alias_failures)


def check_anchors(cli, report, vault, owned_notes, no_eval, deadline):
    targets = {}
    for path in owned_notes:
        for target, subpath in wikilinks((vault / path).read_text(encoding="utf-8")):
            if subpath.startswith("^"):
                targets.setdefault(target + ".md", set()).add(subpath[1:])
    if no_eval:
        report.check("links.anchors", "skipped", "needs eval (--no-eval given)")
        return
    failures = []
    for target, wanted in sorted(targets.items()):
        def attempt():
            _, blocks = cli.eval_json(JS_BLOCKS % json.dumps(target))
            listed = isinstance(blocks, list)
            return listed and wanted <= set(blocks), blocks

        ok, blocks = settle(attempt, deadline)
        missing = sorted(wanted - set(blocks if isinstance(blocks, list) else []))
        if not ok:
            failures.append({"path": target, "missing": missing, "blocks": blocks})
    report.check("links.anchors", bool(targets) and not failures,
                 f"every linked excerpt anchor is in the viewer's block cache "
                 f"({sum(len(v) for v in targets.values())} anchors in {len(targets)} notes)",
                 failures=failures)


def query_rows(cli, path, view):
    ok, text, _ = cli.call("base:query", f"path={path}", f"view={view}", "format=paths")
    rows = lines(text) if ok else None
    if rows is not None and any(not row.endswith(".md") for row in rows):
        return None, text
    return (sorted(row[:-3] for row in rows) if rows is not None else None), text


def check_bases(cli, report, vault, expected, prefixes, deadline):
    bases = sorted(path for path in expected["files"]
                   if path.endswith(".base") and is_owned(path, prefixes))
    results, failures = [], []
    for path in bases:
        table = (vault / (path[:-5] + ".md")).read_text(encoding="utf-8")
        wanted = static_rows(table)
        view = base_view_name((vault / path).read_text(encoding="utf-8"))

        def attempt():
            rows, text = query_rows(cli, path, view)
            return rows == wanted, rows, text

        same, rows, text = settle(attempt, deadline)
        entry = {"path": path, "view": view, "static_rows": len(wanted or []),
                 "viewer_rows": None if rows is None else len(rows)}
        results.append(entry)
        if not same:
            entry.update(missing=sorted(set(wanted or []) - set(rows or [])),
                         extra=sorted(set(rows or []) - set(wanted or [])),
                         output=None if rows is not None else text[:500])
            failures.append(entry)
    report.check("bases.rows", bool(bases) and not failures,
                 f"each of {len(bases)} Bases lists exactly the rows of its static table"
                 if bases and not failures else
                 "no Base in the vault" if not bases else
                 f"{len(failures)} of {len(bases)} Bases differ from their static tables",
                 bases=results, failures=failures)


def check_probes(cli, report, deadline):
    """Comment 2 of W12.07: `!=`, `> 0` and the frontier's and-ordering in the viewer."""
    observed, failures = {}, []
    for name, (_, wanted) in PROBE_BASES.items():
        path = f"{PROBE_FOLDER}/{name}.base"
        wanted_paths = None if wanted is None else sorted(f"{PROBE_FOLDER}/{n}" for n in wanted)

        def attempt():
            rows, text = query_rows(cli, path, name)
            return wanted_paths is None or rows == wanted_paths, rows, text

        same, rows, _ = settle(attempt, deadline)
        observed[name] = rows
        if not same:
            failures.append({"base": name, "wanted": wanted_paths, "viewer": rows})
    report.check("bases.operators", not failures,
                 "!= and > 0 select as the offline evaluator does, and the frontier filter gives "
                 "the same rows in either and-order" if not failures else
                 "a probe Base differs from the offline evaluator", failures=failures,
                 observed=observed)
    report.check("bases.missing_property", "info", "rows the viewer gives when a compared property "
                 "is absent (generated Bases never rely on this: every compared property is "
                 "present on every note they reach)", rows=observed.get("missing-property"))


def unresolved_answer(cli):
    ok, text, _ = cli.call("unresolved", "verbose", "format=json")
    pairs = unresolved_pairs(text) if ok else None
    key = None if pairs is None else json.dumps(sorted([link, sorted(sources)]
                                                       for link, sources in pairs))
    return pairs, key, text


def settle_cache(cli, budget):
    """Reload the window, then poll `unresolved` until two consecutive answers agree.

    bn-17wk: on a freshly opened vault the viewer listed every file node of one Canvas as
    unresolved for a minute, through an earlier reload; a reload once the vault was indexed
    cleared it within 10 s. So the reload comes right before the check, and the answer
    counts only once it repeats.
    """
    started = time.monotonic()
    deadline = started + budget
    reloaded = cli.call("reload")[0]
    back = None
    while time.monotonic() < deadline:
        if cli.call("vault", "info=path")[0]:
            back = round(time.monotonic() - started, 3)
            break
        time.sleep(POLL_INTERVAL)
    answers, pairs, text = [], None, ""
    while True:
        pairs, key, text = unresolved_answer(cli)
        answers.append(key)
        settled = len(answers) >= 2 and key is not None and answers[-1] == answers[-2]
        if settled or time.monotonic() >= deadline:
            break
        time.sleep(POLL_INTERVAL)
    return {"reloaded": reloaded, "window_back_s": back, "polls": len(answers),
            "distinct_answers": len(set(answers)), "settled": settled,
            "settle_s": round(time.monotonic() - started, 3)}, pairs, text


def check_unresolved(cli, report, prefixes, budget, max_reloads):
    rounds = []
    owned, pairs = [], None
    for _ in range(max(1, max_reloads)):
        info, pairs, text = settle_cache(cli, budget)
        rounds.append(info)
        settle_seconds = round(sum(r["settle_s"] for r in rounds), 3)
        if not info["settled"]:
            report.check("links.unresolved", False,
                         f"the unresolved-link answer did not settle within {budget:g} s after a "
                         f"reload ({info['polls']} answers, {info['distinct_answers']} distinct, "
                         "the last two differ)", settle=rounds, settle_seconds=settle_seconds,
                         output=text[:1000])
            return
        if pairs is None:
            report.check("links.unresolved", False, "the unresolved-link answer was not "
                         "understood", settle=rounds, output=text[:1000])
            return
        owned = [{"link": link, "sources": [s for s in sources if is_owned(s, prefixes)]}
                 for link, sources in pairs
                 if any(is_owned(s, prefixes) for s in sources) or not sources]
        if not owned:
            break
        # A link whose target the viewer itself lists is a cache that has not caught up, not a
        # broken link: reload once more (bounded). A missing target is final.
        ok, listed_text, _ = cli.call("files")
        listed = set(lines(listed_text)) if ok else set()
        info["owned_with_listed_targets"] = sum(
            1 for entry in owned if entry["link"] in listed or entry["link"] + ".md" in listed)
        if not info["owned_with_listed_targets"]:
            break
    settle_seconds = round(sum(r["settle_s"] for r in rounds), 3)
    control = any(link == OWNER_LINK or link.startswith(OWNER_LINK) for link, _ in pairs)
    stale = rounds[-1].get("owned_with_listed_targets", 0)
    report.check("links.unresolved", control and not owned,
                 "no unresolved link starts in an owned file; the owner note's planted link is "
                 f"reported (settled in {settle_seconds:g} s after {len(rounds)} reload(s))"
                 if control and not owned else
                 "the planted owner link was not reported, so the answer cannot be trusted"
                 if not control else f"{len(owned)} unresolved link(s) in owned files "
                 "(or with no source named)" + (
                     f"; {stale} of them name a file the viewer lists, still after "
                     f"{len(rounds)} reload(s)" if stale else ""),
                 owned=owned[:50], total=len(pairs), settle=rounds,
                 settle_seconds=settle_seconds)


def check_canvas(cli, report, vault, expected, prefixes, no_eval):
    canvases = sorted(path for path in expected["files"]
                      if path.endswith(".canvas") and is_owned(path, prefixes))
    _, listed_text, _ = cli.call("files", "ext=canvas")
    listed = set(lines(listed_text))
    failures, nodes_checked = [], 0
    for path in canvases:
        problem = None
        if path not in listed:
            problem = "the viewer does not list it"
        opened, out, _ = cli.call("open", f"path={path}")
        known, info, _ = cli.call("file", f"path={path}")
        if problem is None and not opened:
            problem = f"open failed: {out.strip()[:200]}"
        elif problem is None and (not known or info_fields(info).get("path") != path):
            problem = f"file did not report the path: {info.strip()[:200]}"
        elif problem is None and not no_eval:
            _, view = cli.eval_json(JS_ACTIVE_VIEW)
            if not isinstance(view, dict) or view.get("path") != path \
                    or view.get("type") != "canvas":
                problem = f"the active view is not this Canvas: {view!r}"
        unresolved_nodes = []
        canvas = load_json(vault / path)
        for node in (canvas or {}).get("nodes", []):
            if node.get("type") == "file":
                nodes_checked += 1
                ok, node_info, _ = cli.call("file", f"path={node['file']}")
                if not ok or info_fields(node_info).get("path") != node["file"]:
                    unresolved_nodes.append(node["file"])
        if canvas is None and problem is None:
            problem = "the file is not on disk or is not JSON"
        if problem or unresolved_nodes:
            failures.append({"path": path, "problem": problem, "unresolved_nodes": unresolved_nodes})
    report.check("canvas.open", bool(canvases) and not failures,
                 f"each of {len(canvases)} Canvas files opens and its {nodes_checked} file nodes "
                 "resolve" if canvases and not failures else
                 "no Canvas in the vault" if not canvases else
                 f"{len(failures)} of {len(canvases)} Canvas files failed", failures=failures)


# ---------------------------------------------------------------------------- timings


def timings(args):
    report = Report("timings")
    out = Path(args.out).absolute() if args.out else ROOT / "target/obsidian-ui"
    vault = Path(args.vault).expanduser().absolute()
    name = args.vault_name or vault.name
    report.value["vault"] = {"name": name, "path": str(vault)}
    try:
        status = measure(args, vault, name, report)
    except Skip as skip:
        status = "skipped"
        report.value["skip_reason"] = str(skip)
    except Blocked as blocked:
        status = "blocked"
        report.value["blocked_reason"] = str(blocked)
    report.value["status"] = status
    report.value["finished"] = now()
    write_report(report.value, out, "timings")
    return {"pass": EXIT_PASS, "skipped": EXIT_PASS, "fail": EXIT_FAIL}.get(status, EXIT_BLOCKED)


def machine():
    memory = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                memory = f"{int(line.split()[1]) / 1048576:.0f} GiB"
    except OSError:
        pass
    return {"platform": platform.platform(), "machine": platform.machine(),
            "logical_cpus": os.cpu_count(), "memory": memory,
            "load_average": list(os.getloadavg()) if hasattr(os, "getloadavg") else None}


def measure(args, vault, name, report):
    if not vault.is_dir():
        raise Blocked(f"{vault} is not a folder; keep one with `just load-test DIR --cases 130` and "
                      "pass DIR/viewer_1_15_1/vault.")
    binary = require_binary(args.bin)
    files = [p for p in vault.rglob("*") if p.is_file()
             and not any(part.startswith(".") for part in p.relative_to(vault).parts)]
    notes = sum(1 for p in files if p.suffix == ".md")
    bases = sorted(p.relative_to(vault).as_posix() for p in files if p.name == "Timeline.base")
    canvases = sorted(p.relative_to(vault).as_posix() for p in files if p.suffix == ".canvas")
    report.value["vault"].update(files=len(files), notes=notes, bases=sum(
        1 for p in files if p.suffix == ".base"), canvases=len(canvases),
        bytes=sum(p.stat().st_size for p in files))
    report.value["machine_start"] = machine()
    argv_prefix = find_cli(args.obsidian, args.obsidian_arg)
    cli = Cli(argv_prefix, name, args.timeout, report.value["calls"])
    ok, version_text, _ = cli.call("version", targeted=False)
    if not ok:
        raise Skip(f"The obsidian command did not answer `version`: {version_text.strip()[:200]}")
    release = parse_release(version_text)
    report.value["obsidian"] = {"cli": argv_prefix, "version_output": version_text.strip(),
                                "release": release}
    if release:
        report.value["compatibility"] = doctor(binary, release)
    ok, path_text, _ = cli.call("vault", "info=path")
    if not ok or os.path.realpath(lines(path_text)[-1] if lines(path_text) else "") != \
            os.path.realpath(vault):
        raise Blocked(f"vault={name} does not target {vault}: open it in the viewer once, or pass "
                      "--vault-name with the name or ID the viewer lists for it.")
    samples = {key: [] for key in ("cli_roundtrip", "base_query", "canvas_open", "search",
                                   "reload_until_listed", "cold_open_until_listed")}
    case_id = bases[0].split("/")[-3] if bases else None
    counts = {}
    for _ in range(args.samples):
        samples["cli_roundtrip"].append(cli.call("version", targeted=False)[2])
        if bases:
            ok, text, seconds = cli.call("base:query", f"path={bases[0]}", "view=Timeline",
                                         "format=paths")
            samples["base_query"].append(seconds)
            counts["base_query_rows"] = len(lines(text)) if ok else None
        if canvases:
            samples["canvas_open"].append(cli.call("open", f"path={canvases[0]}")[2])
        if case_id:
            ok, text, seconds = cli.call("search", f"query={case_id}", "format=json")
            samples["search"].append(seconds)
            try:
                counts["search_results"] = len(json.loads(text)) if ok else None
            except ValueError:
                counts["search_results"] = None

    def until_listed(command):
        started = time.monotonic()
        cli.call(command, timeout=args.timeout)
        limit = started + args.open_timeout
        while time.monotonic() < limit:
            ok, text, _ = cli.call("files", "total")
            if ok and lines(text) and lines(text)[-1].isdigit() and \
                    int(lines(text)[-1]) >= len(files):
                return time.monotonic() - started
            time.sleep(0.25)
        return None

    for _ in range(args.samples):
        samples["reload_until_listed"].append(until_listed("reload"))
    if args.allow_restart:
        for _ in range(args.samples):
            samples["cold_open_until_listed"].append(until_listed("restart"))
    summary = {}
    for key, values in samples.items():
        measured = [value for value in values if value is not None]
        summary[key] = None if not measured else {
            "n": len(measured), "timed_out": len(values) - len(measured),
            "median_s": round(statistics.median(measured), 3), "min_s": round(min(measured), 3),
            "max_s": round(max(measured), 3)}
    report.value.update(
        samples={k: [None if v is None else round(v, 4) for v in vs] for k, vs in samples.items()},
        summary=summary, counts=counts, machine_end=machine(),
        targets={"base_query": bases[:1], "canvas_open": canvases[:1], "search": case_id},
        order="per sample: CLI round trip (version), Base query (first case's Timeline.base, "
              "format=paths), Canvas open (first Map.canvas), search (that case's ID); then "
              "window reloads, each timed until `files total` reaches the file count on disk; "
              "then app restarts the same way, only with --allow-restart",
        caveats=["Times are CLI round trips: a command may return before the viewer has drawn "
                 "the view, and every figure includes the CLI's own process start (see "
                 "cli_roundtrip).", "reload keeps the viewer's metadata cache; only a restart "
                 "approximates a cold open, and the page cache stays warm."])
    if not args.allow_restart:
        report.value["summary"]["cold_open_until_listed"] = "not measured: needs --allow-restart"
    return "pass"


# ---------------------------------------------------------------------------- report


def write_report(value, out, stem):
    out.mkdir(parents=True, exist_ok=True)
    (out / f"{stem}.json").write_text(json.dumps(value, indent=2, sort_keys=True) + "\n",
                                      encoding="utf-8")
    text = summary_text(value)
    (out / f"{stem}.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    print(f"Report: {out / (stem + '.json')}")


def summary_text(value):
    out = [f"obsidian-ui-check {value['mode']}  {value['status'].upper()}"]
    for key in ("skip_reason", "blocked_reason"):
        if key in value:
            out.append(f"  {value[key]}")
    viewer = value.get("obsidian", {})
    if viewer.get("version_output"):
        out.append(f"viewer  Obsidian {viewer.get('release')}  ({viewer['version_output']!r} from "
                   f"{' '.join(viewer['cli'])})")
    compat = value.get("compatibility")
    if compat:
        out.append(f"profile  rev {compat['profile']['revision']}: {compat['stated_release']} -> "
                   f"{compat['level']}, {len(compat['allowed'])} optional feature(s), range "
                   f"{compat['profile']['supported_range']['min']}.."
                   f"{compat['profile']['supported_range']['max']}, pinned "
                   f"{compat['profile']['pinned_release']} ({compat['profile']['verification']})")
    if value.get("vault"):
        out.append(f"vault  {value['vault']['name']}  {value['vault']['path']}")
    for check in value.get("checks", []):
        out.append(f"{check['status']:<8} {check['id']:<28} {check['summary']}")
    if value.get("summary"):
        vault = value["vault"]
        out.append("")
        out.append(f"Obsidian {viewer.get('release')}, {vault.get('notes')} notes, "
                   f"{vault.get('bases')} Base files, {vault.get('canvases')} Canvas files; "
                   f"{platform.machine()}, {os.cpu_count()} logical CPUs; order: {value['order']}")
        out.append("")
        out.append("| Measure | n | Median | Min | Max |")
        out.append("| --- | --- | --- | --- | --- |")
        for key, item in value["summary"].items():
            if isinstance(item, dict):
                out.append(f"| {key} | {item['n']} | {item['median_s']} s | {item['min_s']} s | "
                           f"{item['max_s']} s |")
            else:
                out.append(f"| {key} | 0 | {item or 'not measured'} | | |")
    out.append(f"CLI documentation: {CLI_DOCS['url']} (checked {CLI_DOCS['checked']})")
    return "\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", help="the investigation executable")
    sub = parser.add_subparsers(dest="command", required=True)
    prep = sub.add_parser("prepare", help="build the test vault in DIR (no viewer needed)")
    prep.add_argument("dir")

    def viewer_options(p):
        p.add_argument("--obsidian", help="the viewer's CLI executable (default: obsidian on PATH)")
        p.add_argument("--obsidian-arg", action="append", default=[],
                       help="an argument placed after the executable, before vault= (repeatable)")
        p.add_argument("--vault-name", help="the vault name or ID the viewer lists for the vault")
        p.add_argument("--out", help="report folder (default target/obsidian-ui)")
        p.add_argument("--timeout", type=float, default=60.0, help="seconds per CLI call")

    check = sub.add_parser("run", help="check the prepared vault in the running viewer")
    check.add_argument("dir")
    viewer_options(check)
    check.add_argument("--checks", help=f"comma-separated subset of {','.join(CHECK_GROUPS)}")
    check.add_argument("--settle", type=float, default=30.0,
                       help="seconds to keep retrying a check while the viewer indexes")
    check.add_argument("--cache-settle", type=float, default=60.0,
                       help="seconds, per reload, for the unresolved-link answer to repeat")
    check.add_argument("--cache-reloads", type=int, default=2,
                       help="reloads allowed while owned links name files the viewer lists")
    check.add_argument("--poll-interval", type=float, default=1.0,
                       help="seconds between repeated CLI answers")
    check.add_argument("--no-eval", action="store_true",
                       help="skip the checks that use the CLI's eval command")
    measure_parser = sub.add_parser("timings", help="viewer-side load timings for a large vault")
    measure_parser.add_argument("--vault", required=True)
    viewer_options(measure_parser)
    measure_parser.add_argument("--samples", type=int, default=5)
    measure_parser.add_argument("--open-timeout", type=float, default=600.0,
                                help="seconds to wait for a reload or restart to list every file")
    measure_parser.add_argument("--allow-restart", action="store_true",
                                help="also time cold opens; restarts the whole app each sample")
    args = parser.parse_args()
    try:
        return {"prepare": prepare, "run": run, "timings": timings}[args.command](args)
    except Blocked as blocked:
        print(f"obsidian-ui-check: {blocked}", file=sys.stderr)
        return EXIT_BLOCKED


if __name__ == "__main__":
    sys.exit(main())
