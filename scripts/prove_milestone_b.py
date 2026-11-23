#!/usr/bin/env python3
"""Run the Milestone B proof on one executable and report it as one JSON object.

    cargo build --locked -p investigation-cli
    python3 scripts/prove_milestone_b.py target/debug/investigation --output milestone-b.json

The driver exports the committed tree (`git archive HEAD`) to a new temporary directory
and runs every check from that copy, so uncommitted files cannot change the result. It
compiles the test executables in the copy, then installs BIN at the path they were
compiled against, so the Rust tests, the scenario scripts and the vault renders run the
same bytes. It runs the tests and scripts with a cleared environment: PATH holds only
python3, no producer, viewer application or extra method pack is installed, and, when
bubblewrap is available, there is no network.

The report covers the four scenarios with their W10 steps, the conformance suite with
the reference, Python and Rust producers, the four producer kinds, vault renders and
the vault fixtures, drills 1-6, neutrality, the scripted first-use path and the whole
test suite. Drills 1-2 (W10.24) run when their test target exists; until then their
slot is reported as pending and the proof is not complete. The exit status is 0 only
when every step that ran passed.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
import traceback


ROOT = Path(__file__).resolve().parents[1]
# Test targets that drills 1-2 (W10.24, bn-28lh) may land as.
DRILLS_1_2 = ("adversarial_drills", "drills_1_2")
DRILL_TARGETS = {
    "3-4": ("investigation-cli", "drills_3_4", None),
    "5": ("investigation-cli", "drill_lesson_transfer", None),
    "6": ("investigation-cli", "drill_closure", None),
    "6/crash-model": ("investigation-store", "investigation_store", "closure_drill_tests"),
}
# Named external tools and security concepts that the default surfaces must not contain,
# beyond the reviewed deny-list in scripts/neutrality-denylist.txt.
NAMED_TOOLS = (
    "obsidian", "jira", "github", "gitlab", "bitbucket", "asana", "trello",
    "docker", "kubernetes", "pytest", "valgrind", "wireshark", "prometheus", "grafana", "datadog",
    "sentry", "jaeger", "jmeter", "locust", "burp", "nmap", "metasploit", "semgrep", "codeql",
    "sonarqube", "openai", "chatgpt", "claude", "ollama", "copilot",
)
SECURITY_TERMS = (
    "cve", "cwe", "cvss", "owasp", "exploit", "vulnerability", "vulnerabilities", "attack",
    "attacker", "threat model", "malware", "pentest", "penetration test", "privilege escalation",
    "xss", "csrf", "sql injection", "red team", "security", "credential",
)
# A named tool the default surfaces may mention, with the reason. Reported, never hidden.
ALLOWED_NAMED = {}
FIRST_USE_FAMILY = "kiln-thermocouple-offset"


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def tail(text, limit=4000):
    return text if len(text) <= limit else "…" + text[-limit:]


class Proof:
    def __init__(self, binary, scratch, output, jobs, target):
        self.binary_source = binary
        self.scratch, self.output, self.jobs = scratch, output, jobs
        self.src = scratch / "src"
        self.target = target
        self.out = output
        self.steps = []
        self.report = {"schema": "milestone-b-proof.v1"}
        self.tests = {}
        self.executables = {}
        self.bins = {}
        pathbin = scratch / "pathbin"
        pathbin.mkdir()
        (pathbin / "python3").symlink_to(sys.executable)
        for name in ("home", "tmp"):
            (scratch / name).mkdir()
        self.env = {"PATH": str(pathbin), "HOME": str(scratch / "home"), "TMPDIR": str(scratch / "tmp"),
                    "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "TZ": "UTC", "PYTHONDONTWRITEBYTECODE": "1"}
        self.isolate = []
        bwrap = shutil.which("bwrap")
        if bwrap:
            probe = [bwrap, "--dev-bind", "/", "/", "--unshare-net", "--die-with-parent", "--", "true"]
            if subprocess.run(probe, capture_output=True).returncode == 0:
                self.isolate = probe[:-1]

    # ---- running things -------------------------------------------------------------
    def run(self, command, cwd=None, env=None, stdin=None, isolated=True, timeout=1800):
        started = time.monotonic()
        child = subprocess.run(
            [*(self.isolate if isolated else []), *map(str, command)], cwd=cwd or self.src,
            env=self.env if env is None else env, input=stdin, capture_output=True, text=True, timeout=timeout,
        )
        return child, round(time.monotonic() - started, 2)

    def step(self, name, function):
        started = time.monotonic()
        entry = {"name": name}
        try:
            detail = function()
            entry["ok"] = True
            if detail is not None:
                entry["detail"] = detail
        except Exception as error:  # a failed step is a finding, never a crash of the proof
            entry["ok"] = False
            entry["error"] = tail(f"{type(error).__name__}: {error}\n{traceback.format_exc(limit=3)}")
        entry["seconds"] = round(time.monotonic() - started, 2)
        self.steps.append(entry)
        print(f"{'ok  ' if entry['ok'] else 'FAIL'} {name} ({entry['seconds']} s)", file=sys.stderr, flush=True)
        return entry

    def cli(self, *args, cwd, stdin=None, check=True):
        child, _ = self.run([self.binary, *args], cwd=cwd, stdin=stdin)
        if check and child.returncode != 0:
            raise AssertionError(f"{args}: exit {child.returncode}\n{tail(child.stdout)}\n{tail(child.stderr)}")
        return child

    def json(self, *args, cwd, stdin=None):
        return json.loads(self.cli(*args, "--format", "json", cwd=cwd, stdin=stdin).stdout)

    # ---- preparation ----------------------------------------------------------------
    def versions(self):
        git = lambda *args: subprocess.check_output(["git", "-C", ROOT, *args], text=True).strip()
        dirty = git("status", "--porcelain")
        tool = lambda *command: subprocess.run(command, capture_output=True, text=True, cwd=ROOT).stdout.strip()
        self.report["versions"] = {
            "git": {"commit": git("rev-parse", "HEAD"), "subject": git("log", "-1", "--format=%s"),
                    "uncommitted_changes_excluded": bool(dirty)},
            "binary": {"path": str(self.binary_source), "sha256": sha256(self.binary_source),
                       "version": tool(str(self.binary_source), "--version")},
            "toolchain": {"rustc": tool("rustc", "-V"), "cargo": tool("cargo", "-V"),
                          "python": platform.python_version()},
            "host": {"kernel": platform.release(), "machine": platform.machine()},
        }

    def export(self):
        self.src.mkdir()
        archive = subprocess.run(["git", "-C", ROOT, "archive", "--format=tar", "HEAD"], capture_output=True, check=True)
        subprocess.run(["tar", "-x", "-C", self.src], input=archive.stdout, check=True)
        return {"tree": "git archive HEAD", "files": sum(1 for path in self.src.rglob("*") if path.is_file())}

    def build(self):
        env = dict(os.environ, CARGO_TARGET_DIR=str(self.target))
        child = subprocess.run(
            ["cargo", "test", "--locked", "--workspace", "--no-run", "--message-format=json-render-diagnostics"],
            cwd=self.src, env=env, capture_output=True, text=True)
        if child.returncode:
            raise AssertionError(tail(child.stderr))
        for line in child.stdout.splitlines():
            message = json.loads(line)
            if message.get("reason") != "compiler-artifact" or not message.get("executable"):
                continue
            target = message["target"]
            name = self.package_name(message)
            if message["profile"]["test"]:
                key = (name, target["name"], target["kind"][0])
                self.executables[key] = {"path": message["executable"], "cwd": str(Path(message["manifest_path"]).parent)}
            elif "bin" in target["kind"]:
                self.bins[target["name"]] = message["executable"]
        compiled = self.target / "debug" / "investigation"
        assert Path(self.bins["investigation"]) == compiled, self.bins
        return {"test_executables": len(self.executables), "bins": sorted(self.bins)}

    @staticmethod
    def package_name(message):
        manifest = Path(message["manifest_path"])
        text = manifest.read_text(encoding="utf-8")
        found = re.search(r'(?m)^name\s*=\s*"([^"]+)"', text)
        return found.group(1)

    def guards(self):
        env = dict(os.environ, CARGO_TARGET_DIR=str(self.target))
        child = subprocess.run([sys.executable, "scripts/check-boundaries.py"], cwd=self.src, env=env,
                               capture_output=True, text=True)
        if child.returncode:
            raise AssertionError(tail(child.stderr + child.stdout))
        return {"command": "python3 scripts/check-boundaries.py", "checks": [
            "crate dependency direction", "no frontend crate under a producer library",
            "deny-listed terms absent from help, schemas, the general pack, embedded docs and a fresh workspace"]}

    def install(self):
        # Every CLI test executable was compiled against this path (CARGO_BIN_EXE_investigation).
        self.binary = self.target / "debug" / "investigation"
        self.binary.unlink()
        shutil.copy2(self.binary_source, self.binary)
        assert sha256(self.binary) == self.report["versions"]["binary"]["sha256"]
        return {"installed_at": "CARGO_BIN_EXE_investigation of every test executable",
                "sha256": sha256(self.binary)}

    # ---- the whole test suite ---------------------------------------------------------
    def suite(self):
        keep = self.out / "workspaces" / "scenario-d"
        env = dict(self.env, INVESTIGATION_KEEP_SCENARIO_D=str(keep))

        def one(item):
            key, executable = item
            child, seconds = self.run([executable["path"], "--color", "never"], cwd=executable["cwd"], env=env)
            results = {}
            for name, outcome in re.findall(r"(?m)^test (\S+) \.\.\. (ok|FAILED|ignored)", child.stdout):
                results[name] = outcome
            return key, {"exit": child.returncode, "seconds": seconds, "tests": results,
                         "output": tail(child.stdout + child.stderr) if child.returncode else None}

        with ThreadPoolExecutor(self.jobs) as pool:
            for key, result in pool.map(one, sorted(self.executables.items())):
                self.tests[key] = result
        counts = {"ok": 0, "FAILED": 0, "ignored": 0}
        failures = []
        for (package, target, kind), result in self.tests.items():
            for name, outcome in result["tests"].items():
                counts[outcome] += 1
                if outcome == "FAILED":
                    failures.append(f"{package}/{target}::{name}")
            if result["exit"] and not any(outcome == "FAILED" for outcome in result["tests"].values()):
                failures.append(f"{package}/{target}: exit {result['exit']}")
        detail = {"executables": len(self.tests), "passed": counts["ok"], "failed": counts["FAILED"],
                  "ignored": counts["ignored"], "failures": failures,
                  "doctests": "not run: they need the toolchain at run time",
                  "environment": {"PATH": "python3 only", "network": "none" if self.isolate else "not isolated",
                                  "producers_installed": 0, "viewer_on_path": False}}
        self.report["test_suite"] = detail
        assert not failures, failures
        return {key: value for key, value in detail.items() if key != "failures"}

    def target_results(self, package, target, contains=None):
        matches = [result for (name, test, _), result in self.tests.items() if name == package and test == target]
        if not matches:
            raise AssertionError(f"no test executable {package}/{target}")
        tests = {name: outcome for result in matches for name, outcome in result["tests"].items()
                 if contains is None or contains in name}
        assert tests, f"no tests in {package}/{target} matching {contains}"
        failed = sorted(name for name, outcome in tests.items() if outcome != "ok")
        assert not failed, failed
        return {"target": f"{package}/{target}" + (f" [{contains}]" if contains else ""), "tests": sorted(tests)}

    # ---- scenarios ------------------------------------------------------------------
    def script(self, *args):
        child, seconds = self.run([sys.executable, *args])
        if child.returncode:
            raise AssertionError(tail(child.stdout + child.stderr))
        return json.loads(child.stdout), seconds

    def scenario_a(self):
        result, seconds = self.script("scripts/scenario_a_proof.py", self.binary, self.out / "handoffs" / "scenario-a.json",
                                      "--workspace", self.out / "workspaces" / "scenario-a")
        return {"command": "scripts/scenario_a_proof.py", "seconds": seconds, "case": result["case"],
                "validated": result["validated"], "w10": {
                    "blindspots": "comparability.environment_differs on the refused candidate",
                    "blindspot_review": result["blindspot_review"],
                    "retrospective": "from retrospective --draft, recorded with case.close"},
                "successor": {key: result["successor"][key] for key in ("outcome", "evidence")}}

    def scenario_b(self):
        result, seconds = self.script("scripts/scenario_b_proof.py", "--bin", self.binary,
                                      "--workspace", self.out / "workspaces" / "scenario-b")
        return {"command": "scripts/scenario_b_proof.py", "seconds": seconds, "case": result["case"],
                "properties": len(result["properties"]), "decimals_checked": result["decimals_checked"],
                "case_outcome": result["case_outcome"], "w10": {
                    "blindspots": "before and after the blindspot review",
                    "blindspot_review": result["blindspot_review"],
                    "retrospective": result["retrospective"]}}

    def scenario_c(self):
        result, seconds = self.script("scripts/scenario_c_proof.py", "--bin", self.binary,
                                      "--handoff", self.out / "handoffs" / "scenario-c.json",
                                      "--workspace", self.out / "workspaces" / "scenario-c")
        return {"command": "scripts/scenario_c_proof.py", "seconds": seconds, "case": result["case_id"],
                "decision": result["decision_id"], "case_outcome": result["case_outcome"],
                "w10": {"blindspots": "criterion.hard_unmeasured before and after the threshold change",
                        "blindspot_review": result["blindspot_review"], "stale_reviews": result["stale_reviews"],
                        "retrospective": "from retrospective --draft, recorded with case.close"}}

    def scenario_d(self):
        detail = self.target_results("investigation-cli", "scenario_d")
        assert (self.out / "workspaces" / "scenario-d" / "investigation.toml").is_file()
        detail["w10"] = {"blindspots": "duplicate-origin trap and empty-search absence",
                         "blindspot_review": "every gap dispositioned; one reviewer gap recorded as a Question",
                         "retrospective": "from retrospective --draft, recorded with case.close"}
        return detail

    def final_audits(self):
        audits = {}
        for workspace in sorted((self.out / "workspaces").iterdir()):
            for case in sorted(path.name for path in (workspace / "state" / "cases").iterdir()):
                audit = self.json("--workspace", workspace, "--case", case, "blindspots", cwd=workspace)
                assert audit["schema_version"] == "inv.audit.v1"
                statement = audit["statement"].lower()
                assert "all blindspots resolved" not in statement and "verified correct" not in statement
                shown = self.json("--workspace", workspace, "--case", case, "show", case, "--depth", "full", cwd=workspace)
                body = shown["record"]["body"]
                audits[f"{workspace.name}/{case}"] = {
                    "outcome": audit["outcome"], "findings": len(audit["checks"]),
                    "lifecycle": body.get("lifecycle"), "closure_outcome": body.get("closure_outcome"),
                    "retrospective": (body.get("closure") or {}).get("retrospective_ref"),
                    "retrospective_waived": bool((body.get("closure") or {}).get("retrospective_waived"))}
        assert len({key.split("/")[0] for key in audits}) == 4, audits
        assert all(entry["lifecycle"] == "closed" for entry in audits.values()), audits
        return audits

    # ---- producers ------------------------------------------------------------------
    def conformance(self):
        producers = ["python3 scripts/conformance/reference_producer.py",
                     "python3 -B examples/python-producer/producer.py", self.bins["example-rust-producer"]]
        command = [sys.executable, "scripts/run-conformance.py", "--bin", self.binary, "--json",
                   "--jobs", str(self.jobs)]
        for producer in producers:
            command += ["--producer", producer]
        child, seconds = self.run(command)
        report = json.loads(child.stdout)
        assert child.returncode == 0 and report["passed"], tail(child.stdout + child.stderr)
        equivalence = report["equivalence"]
        assert equivalence and all(entry["equal"] for entry in equivalence)
        rows = {}
        for entry, name in zip(report["producers"], ("reference", "python", "rust")):
            cases = entry["cases"]
            assert all(case["passed"] for case in cases)
            rows[name] = {"cases_passed": sum(case["passed"] for case in cases), "cases": len(cases)}
        detail = {"command": "scripts/run-conformance.py with the reference, Python and Rust producers",
                  "seconds": seconds, "producers": rows,
                  "equivalent_to_reference": f"{sum(entry['equal'] for entry in equivalence)}/{len(equivalence)}"}
        detail["rust_test"] = self.target_results("investigation-cli", "conformance")
        return detail

    def producer_kinds(self):
        return self.target_results("investigation-cli", "producer_kinds")

    # ---- vault ----------------------------------------------------------------------
    def vault(self):
        renders = {}
        for workspace in sorted((self.out / "workspaces").iterdir()):
            copy = self.out / "vaults" / workspace.name
            shutil.copytree(workspace, copy)
            config = copy / "investigation.toml"
            text = config.read_text(encoding="utf-8")
            config.write_text(text.replace("enabled = false", "enabled = true", 1), encoding="utf-8")
            for human in ("Notes", "Boards", "Inbox"):
                (copy / "vault" / human).mkdir(parents=True, exist_ok=True)
            # No viewer program, a fixed locale and zone, as the golden check does.
            child, seconds = self.run([self.binary, "--workspace", copy, "vault", "render", "--format", "json"],
                                      cwd=copy, env={"PATH": "", "TZ": "UTC", "LC_ALL": "C"})
            report = json.loads(child.stdout)
            assert child.returncode == 0 and report.get("state") == "complete", tail(child.stdout + child.stderr)
            notes = [path for path in (copy / "vault").rglob("*.md")
                     if path.relative_to(copy / "vault").parts[0] not in ("Notes", "Boards", "Inbox")]
            renders[workspace.name] = {"state": report["state"], "notes": len(notes), "seconds": seconds}
        assert set(renders) == {"scenario-a", "scenario-b", "scenario-c", "scenario-d"}, renders
        child, seconds = self.run([sys.executable, "scripts/vault-goldens.py", self.binary])
        assert child.returncode == 0, tail(child.stdout + child.stderr)
        return {"renders": renders, "goldens": child.stdout.strip(),
                "fixtures": self.target_results("investigation-cli", "vault_fixtures"),
                "scenario_vaults": self.target_results("investigation-cli", "vault_scenarios"),
                "commands": self.target_results("investigation-cli", "vault_commands")}

    # ---- drills ---------------------------------------------------------------------
    def drills(self):
        detail = {}
        landed = [name for name in DRILLS_1_2 if any(test == name for (_, test, _) in self.executables)]
        if landed:
            detail["1-2"] = self.target_results("investigation-cli", landed[0])
        else:
            detail["1-2"] = {"status": "pending", "reason": "W10.24 (bn-28lh) is not merged; no test target "
                             + " or ".join(DRILLS_1_2) + " exists in this tree"}
        for drill, (package, target, contains) in DRILL_TARGETS.items():
            detail[drill] = self.target_results(package, target, contains)
        self.report["complete"] = bool(landed)
        return detail

    # ---- neutrality -----------------------------------------------------------------
    def neutrality(self):
        denylist = [line.strip() for line in (self.src / "scripts/neutrality-denylist.txt").read_text().splitlines()
                    if line.strip() and not line.startswith("#")]
        categories = {"deny-list": denylist, "named tool": NAMED_TOOLS, "security concept": SECURITY_TERMS}
        patterns = [(category, term, re.compile(rf"\b{re.escape(term)}\b", re.IGNORECASE))
                    for category, terms in categories.items() for term in terms]
        work = self.scratch / "neutrality"
        work.mkdir()
        surfaces = {}
        help_text = self.cli("--help", cwd=work).stdout
        surfaces["default help"] = {"investigation --help": help_text}
        commands = re.findall(r"(?m)^  ([a-z][a-z-]*)\s{2,}", help_text.split("Commands:", 1)[1].split("Options:", 1)[0])
        surfaces["subcommand help"] = {f"investigation {name} --help": self.cli(name, "--help", cwd=work).stdout
                                       for name in commands if name != "help"}
        listed = self.json("schema", "--list", cwd=work)
        operations = [name for stage in listed["stages"] for name in stage["operations"]]
        published = {"investigation schema --list": json.dumps(listed)}
        for name in operations:
            published[f"investigation schema {name}"] = self.cli("schema", name, "--format", "json", cwd=work).stdout
        for path in sorted((self.src / "schemas").rglob("*")):
            if path.is_file():
                published[str(path.relative_to(self.src))] = path.read_text(encoding="utf-8")
        surfaces["published schemas"] = published
        self.cli("init", "fresh", cwd=work)
        fresh = work / "fresh"
        files = {f"fresh workspace/{path.relative_to(fresh)}": path.read_bytes().decode("utf-8", "replace")
                 for path in sorted(fresh.rglob("*")) if path.is_file()}
        files["investigation onboard"] = self.cli("--workspace", fresh, "onboard", "--format", "json", cwd=work).stdout
        surfaces["fresh workspace"] = files
        doctor = self.json("--workspace", fresh, "doctor", cwd=work)
        packs = self.json("--workspace", fresh, "method", "list", cwd=work)
        hits = {}
        for surface, documents in surfaces.items():
            for document, content in documents.items():
                for number, line in enumerate(content.splitlines(), start=1):
                    for category, term, pattern in patterns:
                        if pattern.search(line):
                            hits.setdefault(surface, []).append(
                                {"category": category, "term": term, "where": f"{document}:{number}",
                                 "line": line.strip()[:160]})
        required = ("default help", "published schemas", "fresh workspace")
        blocking = [hit for surface in required for hit in hits.get(surface, [])
                    if not (hit["category"] == "named tool" and hit["term"] in ALLOWED_NAMED)]
        built_in = [pack["pack_id"] for pack in doctor["capabilities"]["method_packs"]["built_in"]]
        self.report["versions"]["doctor"] = doctor["versions"]
        detail = {
            "surfaces_scanned": {surface: len(documents) for surface, documents in surfaces.items()},
            "terms": {category: len(terms) for category, terms in categories.items()},
            "hits": hits, "allowed": {term: ALLOWED_NAMED[term] for term in ALLOWED_NAMED
                                      if any(hit["term"] == term for found in hits.values() for hit in found)},
            "blocking": blocking, "built_in_packs": built_in, "method_list": packs,
            "viewer_on_ambient_path": bool(shutil.which("obsidian")),
        }
        assert not blocking, blocking
        assert built_in == ["general"], built_in
        return detail

    # ---- first use ------------------------------------------------------------------
    def first_use(self):
        """The scripted first-use path and a fresh successor read (W05b.17/W05b.18)."""
        family = self.src / "fixtures/development/first-use" / FIRST_USE_FAMILY / "default"
        key = json.loads((self.src / "training/first-use/keys" / f"{FIRST_USE_FAMILY}.json").read_text())
        warranted = {item["id"]: item for item in key["variants"]["default"]["warranted"]}
        decisive = set(warranted["decisive_citation"]["citation"]["any_lines"])
        limitation_pattern = re.compile(warranted["material_limitation"]["all"][0], re.IGNORECASE)
        work = self.out / "first-use"
        work.mkdir(parents=True)
        shutil.copyfile(family / "source.txt", work / "source.txt")
        source = (work / "source.txt").read_bytes()
        digest = hashlib.sha256(source).hexdigest()
        quote, limitation = "the controller read about 18 C low or more", "Only firing 112 had witness cones. One firing."
        calls = []

        def call(*args, stdin=None):
            child, seconds = self.run([self.binary, *args], cwd=work, stdin=stdin)
            calls.append({"argv": ["investigation", *map(str, args)], "exit": child.returncode, "seconds": seconds})
            assert child.returncode == 0, tail(child.stdout + child.stderr)
            return child

        def persisted():
            blob = work / "casework/state/blobs/sha256" / digest[:2] / digest
            return blob.is_file() and sha256(blob) == digest and any((work / "casework/state/cases").glob("*/genesis.json"))

        started = time.monotonic()
        call("--help")
        call("init", "./casework", "--no-vault", "--import-root", ".", "--format", "json")
        child = call("--workspace", "./casework", "--actor", "agent:investigator", "start",
                     "Does the kiln controller read low?", "--attach", "./source.txt", "--format", "json")
        receipt_call = len(calls) if persisted() else None
        start = json.loads(child.stdout)
        command_id = re.search(r"command_id: (\S+)", child.stderr).group(1)
        case = next(item["id"] for item in start["created"] if item["client_ref"] == "case")
        item = start["items"][0]
        assert item["digest"] == "sha256:" + digest and item["outcome"] == "imported", item
        again = json.loads(call("--workspace", "./casework", "receipt", command_id, "--actor", "agent:investigator",
                                "--format", "json").stdout)
        assert [entry["id"] for entry in again["created"]] == [entry["id"] for entry in start["created"]], again
        template = start["next_write"]
        assert set(template["unresolved_fields"]) == {"STATEMENT", "EXACT TEXT", "LIMITATION"}, template
        fill = {"STATEMENT": "The cones show the controller read about 18 C low or more in firing 112.",
                "EXACT TEXT": quote, "LIMITATION": limitation}
        argv = []
        for argument in template["argv"][1:]:
            for name, value in fill.items():
                argument = argument.replace(name, value)
            argv.append(argument)
        observed = json.loads(call(*argv).stdout)
        assert observed["observation_checks"][0]["quote_state"] == "verified", observed
        investigator_seconds = round(time.monotonic() - started, 2)

        # The successor: a fresh process with the directory only.
        successor_started = time.monotonic()
        brief = json.loads(call("--workspace", "./casework", "brief", "--format", "json").stdout)
        (evidence,) = [entry for entry in brief["evidence"] if limitation in entry.get("recorded_limitations", [])]
        shown = json.loads(call("--workspace", "./casework", "--case", case, "show", evidence["id"],
                                "--depth", "full", "--format", "json").stdout)
        basis = shown["record"]["body"]["basis"]
        selector = {name: basis["selector"][name] for name in ("schema_version", "artifact_digest", "selector")}
        read = json.loads(call("--workspace", "./casework", "--case", case, "evidence", "read", basis["artifact_ref"]["id"],
                               "--selector-file", "-", "--format", "json", stdin=json.dumps(selector)).stdout)
        excerpt = read["excerpt"].get("text") or ""
        recorded_quote = basis["text_quote"]
        assert recorded_quote["state"] == "verified" and recorded_quote["exact"] == quote, recorded_quote
        # The successor finds the verified quote in the retained bytes, not in the original file.
        cited = sorted(number for number, line in enumerate(excerpt.splitlines(), start=1)
                       if recorded_quote["exact"] in line)
        assert excerpt.encode("utf-8") == source or quote in excerpt, "retained bytes differ from the source"
        recovered = {
            "observation": evidence["id"], "artifact": basis["artifact_ref"]["id"], "selector": selector["selector"],
            "text_quote": recorded_quote, "retained_text_equals_source": excerpt.encode("utf-8") == source,
            "content_trust": read["content_trust"], "cited_lines": cited,
            "decisive_line": bool(set(cited) & decisive), "limitation": evidence["recorded_limitations"],
            "limitation_matches_key": bool(limitation_pattern.search(" ".join(evidence["recorded_limitations"]))),
        }
        assert quote in excerpt and recovered["decisive_line"] and recovered["limitation_matches_key"], recovered
        assert read["content_trust"] == "untrusted_evidence"
        successor_seconds = round(time.monotonic() - successor_started, 2)
        first_contact = 1
        return {
            "family": FIRST_USE_FAMILY, "calls": calls,
            "receipt": {"case": case, "source": item["source_id"], "artifact": item["artifact_id"],
                        "digest": item["digest"], "command_id": command_id, "receipt_call": receipt_call,
                        "within_three_calls_of_first_contact": receipt_call is not None
                        and receipt_call - first_contact <= 3, "receipt_lookup_matches": True},
            "successor_recovery": recovered,
            "seconds": {"investigator": investigator_seconds, "successor": successor_seconds,
                        "total": round(investigator_seconds + successor_seconds, 2)},
            "first_use_test": self.target_results("investigation-cli", "first_use"),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                     formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, help="Write the JSON report here (default: stdout).")
    parser.add_argument("--keep", type=Path, help="Keep the workspaces, vaults and handoffs in this new directory.")
    parser.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1))
    parser.add_argument("--target-dir", type=Path, default=ROOT / "target" / "milestone-b-proof",
                        help="Cargo target directory for the clean copy's build (default: %(default)s). "
                             "A RAM-backed /tmp is often too small for it.")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    began, wall = datetime.now(timezone.utc), time.monotonic()
    with tempfile.TemporaryDirectory(prefix="milestone-b-") as directory:
        scratch = Path(directory)
        output = args.keep.resolve() if args.keep else scratch / "out"
        output.mkdir(parents=True)
        (output / "workspaces").mkdir()
        (output / "handoffs").mkdir()
        proof = Proof(binary, scratch, output, args.jobs, args.target_dir.resolve())
        proof.versions()
        report = proof.report
        report["started_at"] = began.isoformat(timespec="seconds")
        report["environment"] = {"network": "none (bubblewrap --unshare-net)" if proof.isolate else "not isolated",
                                 "path": "python3 only", "env": "cleared"}
        sections = {}
        prepared = all(proof.step(name, function)["ok"] for name, function in (
            ("export clean copy", proof.export), ("build test executables", proof.build),
            ("neutrality guards (W00.10)", proof.guards), ("install BIN", proof.install)))
        if prepared:
            plan = (
                ("test suite", "test_suite_summary", proof.suite),
                ("scenario A", "scenario_a", proof.scenario_a),
                ("scenario B", "scenario_b", proof.scenario_b),
                ("scenario C", "scenario_c", proof.scenario_c),
                ("scenario D", "scenario_d", proof.scenario_d),
                ("final audits", "final_audits", proof.final_audits),
                ("conformance", "conformance", proof.conformance),
                ("four producer kinds", "producer_kinds", proof.producer_kinds),
                ("vault", "vault", proof.vault),
                ("drills", "drills", proof.drills),
                ("neutrality", "neutrality", proof.neutrality),
                ("first use", "first_use", proof.first_use),
            )
            for name, section, function in plan:
                entry = proof.step(name, function)
                sections[section] = entry.get("detail", {"error": entry.get("error")})
        report.update(sections)
        report["steps"] = [{key: value for key, value in step.items() if key != "detail"} for step in proof.steps]
        report["ok"] = all(step["ok"] for step in proof.steps)
        report.setdefault("complete", False)
        report["wall_seconds"] = round(time.monotonic() - wall, 1)
        text = json.dumps(report, indent=2, ensure_ascii=False, default=str) + "\n"
        if args.output:
            args.output.write_text(text, encoding="utf-8")
        else:
            sys.stdout.write(text)
    print(f"Milestone B proof: {'passed' if report['ok'] else 'FAILED'}"
          f"{'' if report['complete'] else ' (incomplete: pending slots)'} in {report['wall_seconds']} s",
          file=sys.stderr)
    sys.exit(0 if report["ok"] else 1)


if __name__ == "__main__":
    main()
