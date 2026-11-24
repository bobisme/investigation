#!/usr/bin/env python3
"""Tests of the optional viewer gate (scripts/obsidian-ui-check.py) against a fake CLI.

    python3 scripts/test-obsidian-ui-check.py target/debug/investigation [--keep DIR]

No real viewer is involved, and none can be reached: the adapter runs with PATH set to one
folder that holds only the fake (scripts/fake-obsidian-cli.py installed as `obsidian`), or to
an empty folder, HOME set to a scratch folder, and the CLI given by absolute path. The
`investigation` binary prepares one test vault and answers doctor and setup.

Each case states an outcome. A fault the fake injects (a Base row the viewer does not list, an
unresolved link in a generated note, a Canvas the viewer does not know) must fail its check,
an absent CLI must skip with exit 0, and the fake's argv log must show the exact vault and
paths. Then each check is removed from a copy of the adapter (one source edit each), and the
case that relies on it must no longer get its outcome: the tests fail without the checks.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
ADAPTER = ROOT / "scripts/obsidian-ui-check.py"
FAKE = ROOT / "scripts/fake-obsidian-cli.py"
VAULT = "investigation-ui-check"


class Case:
    def __init__(self, name, env=None, checks=None, fake=True, extra=(), edit=None,
                 timings=False):
        self.name, self.env, self.checks, self.fake = name, env or {}, checks, fake
        self.extra, self.edit, self.timings = list(extra), edit, timings


def install_fake(folder):
    folder.mkdir(parents=True)
    target = folder / "obsidian"
    target.write_text(f"#!{sys.executable} -I\n" + FAKE.read_text(encoding="utf-8"),
                      encoding="utf-8")
    target.chmod(0o755)
    return target


def execute(case, prepared, scratch, binary, adapter=ADAPTER):
    """Run the adapter for one case in its own copy of the prepared folder."""
    work = scratch / case.name
    directory = work / "prepared"
    shutil.copytree(prepared, directory)
    vault = directory / VAULT
    if case.edit:
        case.edit(vault)
    path_folder = work / "bin"
    if case.fake:
        cli = install_fake(path_folder)
    else:
        path_folder.mkdir(parents=True)
        cli = None
    resolved = shutil.which("obsidian", path=str(path_folder))
    assert resolved in (None, str(cli)), f"PATH would reach {resolved}"
    env = {"PATH": str(path_folder), "HOME": str(work), "LC_ALL": "C", "TZ": "UTC",
           "FAKE_VAULT": str(vault), "FAKE_STATE": str(work / "fake-state"), **case.env}
    argv = [sys.executable, "-I", str(adapter), "--bin", str(binary)]
    if case.timings:
        argv += ["timings", "--vault", str(vault), "--samples", "2", "--open-timeout", "20"]
    else:
        argv += ["run", str(directory), "--settle", "0"]
    argv += ["--out", str(work / "report"), "--timeout", "20", *case.extra]
    if case.checks:
        argv += ["--checks", case.checks]
    result = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=300,
                            check=False)
    stem = "timings" if case.timings else "report"
    report_path = work / f"report/{stem}.json"
    report = json.loads(report_path.read_text()) if report_path.exists() else None
    log = work / "fake-state/argv.jsonl"
    calls = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
    return {"exit": result.returncode, "report": report, "calls": calls,
            "stdout": result.stdout, "stderr": result.stderr,
            "text": (work / f"report/{stem}.txt").read_text() if report else ""}


def check_status(outcome, check_id):
    for check in (outcome["report"] or {}).get("checks", []):
        if check["id"] == check_id:
            return check["status"]
    return None


def checks(outcome):
    return {c["id"]: c for c in (outcome["report"] or {}).get("checks", [])}


def drop_canvas(vault):
    for path in vault.rglob("Map.canvas"):
        path.unlink()
        return


def first(vault, pattern):
    return sorted(p.relative_to(vault).as_posix() for p in vault.rglob(pattern))[0]


def expectations(prepared):
    vault = prepared / VAULT
    frontier = first(vault, "Epistemic Frontier.base")
    canvas = first(vault, "Map.canvas")
    start = "Investigation/scenario-a/Start Here.md"

    def passed(o):
        statuses = {c["status"] for c in checks(o).values()}
        assert o["exit"] == 0 and o["report"]["status"] == "pass", (o["exit"], o["text"])
        assert statuses <= {"pass", "info"}, o["text"]
        for check_id in ("cli.version", "profile.resolution", "vault.target", "vault.unchanged",
                         "plugins.community_disabled", "setup.preview", "setup.apply",
                         "setup.kept_after_reload", "setup.honoured", "setup.idempotent",
                         "viewer.files", "notes.open", "notes.aliases", "links.anchors",
                         "bases.rows", "bases.operators", "links.unresolved", "canvas.open"):
            assert check_status(o, check_id) == "pass", (check_id, o["text"])
        report = o["report"]
        assert report["obsidian"]["release"] == "1.15.0"
        assert report["compatibility"]["level"] == "optional"
        assert report["vault"]["name"] == VAULT
        assert report["cli_documentation"]["url"] == "https://obsidian.md/help/cli"
        assert checks(o)["setup.honoured"]["discriminating"] is True
        bases = checks(o)["bases.rows"]["bases"]
        assert len(bases) >= 20 and all(b["static_rows"] == b["viewer_rows"] for b in bases)
        assert len(checks(o)["notes.open"]["types"]) >= 20, checks(o)["notes.open"]["types"]
        assert "Obsidian 1.15.0" in o["text"] and "PASS" in o["text"]

    def exact_argv(o):
        calls = o["calls"]
        assert calls and calls[0] == ["version"], calls[:2]
        for call in calls[1:]:
            assert call[0] == f"vault={VAULT}", call
            for argument in call[2:]:
                if argument.startswith("path="):
                    assert not argument.startswith("path=/") and "\\" not in argument, call
        assert not any(arg.startswith("file=") for call in calls for arg in call), "file= used"
        assert ["vault=" + VAULT, "base:query", f"path={frontier}", "view=Epistemic Frontier",
                "format=paths"] in calls
        assert [f"vault={VAULT}", "open", f"path={canvas}"] in calls
        assert [f"vault={VAULT}", "open", f"path={start}"] in calls
        assert [f"vault={VAULT}", "unresolved", "verbose", "format=json"] in calls

    def failed(check_id, detail=None):
        def verify(o):
            assert o["exit"] == 1 and o["report"]["status"] == "fail", (o["exit"], o["text"])
            assert check_status(o, check_id) == "fail", (check_id, o["text"])
            if detail:
                detail(checks(o)[check_id])
        return verify

    def row_missing(check):
        (failure,) = check["failures"]
        assert failure["path"] == frontier and failure["viewer_rows"] == failure["static_rows"] - 1
        assert len(failure["missing"]) == 1, failure

    def owned_unresolved(check):
        assert [entry["link"] for entry in check["owned"]] == ["Nowhere"], check

    def canvas_unknown(check):
        assert [f["path"] for f in check["failures"]] == [canvas], check

    def skipped(o):
        assert o["exit"] == 0, (o["exit"], o["stdout"], o["stderr"])
        assert o["report"]["status"] == "skipped", o["text"]
        assert "obsidian" in o["report"]["skip_reason"].lower(), o["report"]
        assert not o["calls"] and "SKIPPED" in o["text"]

    def blocked(o):
        assert o["exit"] == 2 and o["report"]["status"] == "blocked", (o["exit"], o["text"])
        assert "Open folder as vault" in o["report"]["blocked_reason"], o["report"]

    def timed(o):
        assert o["exit"] == 0 and o["report"]["status"] == "pass", (o["exit"], o["text"])
        summary = o["report"]["summary"]
        for key in ("cli_roundtrip", "base_query", "canvas_open", "search",
                    "reload_until_listed"):
            assert summary[key]["n"] == 2 and summary[key]["timed_out"] == 0, (key, summary)
        assert summary["cold_open_until_listed"] == "not measured: needs --allow-restart"
        assert ["restart"] not in [call[1:] for call in o["calls"]], "restarted without asking"
        assert o["report"]["vault"]["notes"] > 200 and o["report"]["compatibility"]
        assert "| base_query | 2 |" in o["text"], o["text"]

    all_checks = None
    cases = [
        (Case("pass", checks=all_checks), [passed, exact_argv]),
        (Case("pass-map-shape", env={"FAKE_UNRESOLVED_SHAPE": "map"}, checks="unresolved"),
         [lambda o: assert_pass(o, "links.unresolved")]),
        (Case("base-row-missing", env={"FAKE_DROP_ROW": frontier}, checks="bases"),
         [failed("bases.rows", row_missing)]),
        (Case("unresolved-in-owned", env={"FAKE_EXTRA_UNRESOLVED": f"{start}|Nowhere"},
              checks="unresolved"), [failed("links.unresolved", owned_unresolved)]),
        (Case("canvas-unknown", env={"FAKE_HIDE": canvas}, checks="canvas"),
         [failed("canvas.open", canvas_unknown)]),
        (Case("canvas-deleted", checks="canvas", edit=drop_canvas),
         [failed("vault.unchanged"), failed("canvas.open")]),
        (Case("setup-ignored", env={"FAKE_IGNORE_CONFIG": "1"}, checks="setup"),
         [failed("setup.honoured")]),
        (Case("community-plugin", env={"FAKE_COMMUNITY": "some-plugin"}, checks="plugins"),
         [failed("plugins.community_disabled")]),
        (Case("cli-absent", fake=False), [skipped]),
        (Case("timings", timings=True), [timed]),
        (Case("timings-cli-absent", fake=False, timings=True), [skipped]),
        (Case("cli-unreachable", env={"FAKE_VERSION_FAIL": "1"}),
         [lambda o: (o["exit"] == 0 and o["report"]["status"] == "skipped"
                     and o["calls"] == [["version"]]) or fail(o)]),
        (Case("vault-not-open", env={"FAKE_VAULT_NAME": "someone-elses-vault"}), [blocked]),
        (Case("release-outside-profile", env={"FAKE_VERSION": "1.9.3"}, checks="plugins"),
         [failed("profile.resolution")]),
    ]
    # One source edit per check; the case that relies on the check must then lose its outcome.
    mutants = [
        ("bases row comparison", "            return rows == wanted, rows, text",
         "            return True, rows, text", "base-row-missing"),
        ("owned unresolved links", 'report.check("links.unresolved", control and not owned,',
         'report.check("links.unresolved", control,', "unresolved-in-owned"),
        ("canvas failures", 'report.check("canvas.open", bool(canvases) and not failures,',
         'report.check("canvas.open", bool(canvases),', "canvas-unknown"),
        ("skip on an absent CLI", 'raise Skip("No obsidian command on PATH',
         'raise Blocked("No obsidian command on PATH', "cli-absent"),
        ("vault= on every call", '*([f"vault={self.vault_name}"] if targeted else [])',
         "*([] if targeted else [])", "pass"),
        ("exact path= on notes", 'target = f"path={path}"', 'target = f"file={Path(path).stem}"',
         "pass"),
    ]
    return cases, mutants


def assert_pass(outcome, check_id):
    assert check_status(outcome, check_id) == "pass", outcome["text"]


def fail(outcome):
    raise AssertionError(outcome["text"] or outcome["stderr"])


def verdict(verifiers, outcome):
    try:
        for verify in verifiers:
            verify(outcome)
        return None
    except AssertionError as error:
        return str(error) or "assertion failed"


def static_safety():
    """The adapter spawns argv lists only: no shell, no os.system, no Python eval."""
    source = ADAPTER.read_text(encoding="utf-8")
    for forbidden in ("shell=True", "os.system", "os.popen", "eval(", "exec(", "Popen("):
        assert forbidden not in source.replace("eval_json(", "").replace('"eval"', ""), forbidden


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--keep", type=Path, help="keep the scratch folder here")
    args = parser.parse_args()
    binary = args.binary.absolute()
    static_safety()
    scratch = Path(tempfile.mkdtemp(prefix="obsidian-ui-check-", dir=args.keep))
    prepared = scratch / "prepared"
    result = subprocess.run([sys.executable, "-I", str(ADAPTER), "--bin", str(binary), "prepare",
                             str(prepared)], env={"PATH": "", "HOME": str(scratch)},
                            capture_output=True, text=True, timeout=300, check=False)
    assert result.returncode == 0, result.stdout + result.stderr
    # The viewer creates its configuration folder when it first opens a vault.
    config = prepared / VAULT / ".obsidian"
    config.mkdir()
    (config / "app.json").write_text('{\n  "alwaysUpdateLinks": true\n}\n', encoding="utf-8")
    (config / "appearance.json").write_text("{}\n", encoding="utf-8")
    cases, mutants = expectations(prepared)
    by_name = {case.name: (case, verifiers) for case, verifiers in cases}
    mutant_dir = scratch / "mutants"
    mutant_dir.mkdir()
    jobs = []
    for case, verifiers in cases:
        jobs.append(("case", case.name, case, verifiers, ADAPTER, scratch / "cases"))
    source = ADAPTER.read_text(encoding="utf-8")
    for index, (label, old, new, case_name) in enumerate(mutants):
        assert source.count(old) == 1, (label, source.count(old))
        mutant = mutant_dir / f"mutant-{index}.py"
        mutant.write_text(source.replace(old, new), encoding="utf-8")
        case, verifiers = by_name[case_name]
        jobs.append(("mutant", label, case, verifiers, mutant, scratch / f"mutant-{index}"))

    def work(job):
        kind, label, case, verifiers, adapter, folder = job
        outcome = execute(case, prepared, folder, binary, adapter)
        return kind, label, case.name, verdict(verifiers, outcome)

    with ThreadPoolExecutor(max_workers=min(8, os.cpu_count() or 2)) as pool:
        results = list(pool.map(work, jobs))
    problems = []
    for kind, label, case_name, problem in results:
        if kind == "case":
            print(f"{'ok  ' if problem is None else 'FAIL'} case {label}")
            if problem:
                problems.append(f"case {label}: {problem}")
        else:
            killed = problem is not None
            print(f"{'ok  ' if killed else 'FAIL'} without {label}: case {case_name} "
                  f"{'fails' if killed else 'still passes'}")
            if not killed:
                problems.append(f"mutant '{label}' survived case {case_name}")
    if problems:
        print("\n".join(problems), file=sys.stderr)
        print(f"Scratch kept: {scratch}", file=sys.stderr)
        return 1
    if not args.keep:
        shutil.rmtree(scratch)
    print(f"{len(cases)} cases passed; {len(mutants)} check removals each fail a case.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
