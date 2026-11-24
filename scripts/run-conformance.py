#!/usr/bin/env python3
"""Run the language-neutral recording conformance suite against producer adapters.

Each producer is an executable that implements the producer adapter protocol
(`inv.producer-adapter.v1`, docs/recording.md). For every case in
fixtures/conformance/v1/cases the runner creates a fresh workspace, drives the
producer through the steps, and judges the result from canonical state that it
reads through the `investigation` binary: `receipt`, `show`, `capture recover`,
`evidence read` and `check`. The adapter's own output explains a failure; it
never proves a pass.

With more than one producer it also compares their normalized canonical states
case by case, so two languages must record equivalent runs.

    python3 scripts/run-conformance.py --bin target/debug/investigation \
        --producer 'python3 scripts/conformance/reference_producer.py'

Exit status 0 means every producer passed every selected case.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SUITE = ROOT / "fixtures/conformance/v1"
ADAPTER = "inv.producer-adapter.v1"
SUITE_VERSION = "inv.conformance-suite.v1"
CASE_VERSION = "inv.conformance-case.v1"
TIMEOUT = 300
ID = re.compile(r"\b(case|run|art|src|tx|handle|ann|obs)_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
# Values that differ between two recordings of identical requests: wall-clock
# acceptance times, journal hashes over them, and digests over requests that name
# a workspace-specific handle.
VOLATILE = {"accepted_at", "recorded_at", "created_at", "head_hash", "request_digest"}
ANY = "<any>"


class Failure(Exception):
    pass


def sha256(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def substitute(value, names):
    if isinstance(value, str):
        for name, replacement in names.items():
            value = value.replace("${" + name + "}", str(replacement))
        return value
    if isinstance(value, list):
        return [substitute(item, names) for item in value]
    if isinstance(value, dict):
        return {key: substitute(item, names) for key, item in value.items()}
    return value


def match(expected, actual, path="$"):
    """Yield mismatches of `actual` against the pattern `expected`.

    Objects match when every expected key matches (extra keys are allowed, so
    additive v1 fields never break the suite). Lists match element by element
    and must have the same length. `"<any>"` matches any present value.
    """
    if expected == ANY:
        if actual is None:
            yield f"{path}: expected a value, found null"
        return
    if isinstance(expected, dict):
        if not isinstance(actual, dict):
            yield f"{path}: expected an object, found {json.dumps(actual)}"
            return
        for key, value in expected.items():
            if key not in actual:
                yield f"{path}.{key}: missing"
            else:
                yield from match(value, actual[key], f"{path}.{key}")
        return
    if isinstance(expected, list):
        if not isinstance(actual, list) or len(actual) != len(expected):
            yield f"{path}: expected {json.dumps(expected)}, found {json.dumps(actual)}"
            return
        for index, (want, have) in enumerate(zip(expected, actual)):
            yield from match(want, have, f"{path}[{index}]")
        return
    if expected != actual or type(expected) is not type(actual):
        yield f"{path}: expected {json.dumps(expected)}, found {json.dumps(actual)}"


def differences(left, right, path="$"):
    if isinstance(left, dict) and isinstance(right, dict):
        for key in sorted(left.keys() | right.keys()):
            if key not in left or key not in right:
                yield f"{path}.{key}: present in only one"
            else:
                yield from differences(left[key], right[key], f"{path}.{key}")
    elif isinstance(left, list) and isinstance(right, list) and len(left) == len(right):
        for index, (one, two) in enumerate(zip(left, right)):
            yield from differences(one, two, f"{path}[{index}]")
    elif left != right:
        yield f"{path}: {json.dumps(left)[:200]} != {json.dumps(right)[:200]}"


class Workspace:
    """One case's workspace, read and written only through the binary."""

    def __init__(self, binary, root, attempts):
        self.binary = binary
        self.root = root
        self.path = root / "workspace"
        self.attempts = attempts
        for name in attempts:
            self.output_dir(name).mkdir(parents=True)
        roots = [arg for name in attempts for arg in ("--import-root", str(self.output_dir(name)))]
        self.cli(["init", str(self.path), "--no-vault", *roots], workspace=False)

    def output_dir(self, attempt):
        return self.root / f"out-{attempt}"

    def cli(self, argv, stdin=None, workspace=True, actor=None):
        command = [str(self.binary)]
        if workspace:
            command += ["--workspace", str(self.path)]
        if actor:
            command += ["--actor", actor]
        command += [*argv, "--format", "json"]
        completed = subprocess.run(
            command, input=stdin, capture_output=True, text=True, check=False, timeout=TIMEOUT
        )
        try:
            return json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise Failure(f"`{' '.join(argv)}` printed no JSON: {completed.stderr.strip()[:300]}") from error

    def names(self, attempt):
        identity = self.attempts[attempt]
        return {
            "workspace": str(self.path),
            "output_dir": str(self.output_dir(attempt)),
            "producer": identity["producer"],
            "external_run_id": identity["external_run_id"],
        }

    def read_bytes(self, artifact_id, digest, case_id):
        selector = {"schema_version": "inv.selector.v1", "artifact_digest": digest, "selector": {"type": "whole"}}
        read = self.cli(
            ["evidence", "read", artifact_id, "--case", case_id, "--selector-file", "-"], json.dumps(selector)
        )
        if read.get("ok") is not True or read["excerpt"].get("availability") != "text":
            raise Failure(f"evidence read {artifact_id} failed: {json.dumps(read)[:300]}")
        return read["excerpt"]["text"].encode("utf-8")

    def show(self, record_id, case_id):
        shown = self.cli(["show", record_id, "--case", case_id, "--depth", "full"])
        if shown.get("ok") is not True:
            raise Failure(f"show {record_id}: {json.dumps(shown)[:300]}")
        return shown["record"]

    def workspace_state(self):
        check = self.cli(["check"])
        return {
            "ok": check.get("ok"),
            "cases": len(check.get("heads", [])),
            "sequences": sorted(head["snapshot"]["sequence"] for head in check.get("heads", [])),
            "errors": sorted(issue["code"] for issue in check.get("issues", []) if issue.get("severity") == "error"),
            "warnings": sorted(issue["code"] for issue in check.get("issues", []) if issue.get("severity") != "error"),
            "staging": check.get("staging"),
        }

    def attempt_state(self, attempt):
        """Canonical state of one attempt, found by its begin receipt, never by adapter output."""
        identity = self.attempts[attempt]
        actor = "agent:" + identity["producer"]
        begin_key = identity["external_run_id"] + "/begin"
        receipt = self.cli(["receipt", begin_key, "--actor", actor])
        if receipt.get("result") != "committed":
            return None
        created = {item["client_ref"]: item["id"] for item in receipt.get("created", [])}
        run_id = created.get("run")
        if run_id is None:
            raise Failure(f"begin receipt {begin_key} created no Run")
        case_id = created.get("case") or receipt.get("case_id")
        if case_id is None:
            raise Failure(f"begin receipt {begin_key} names no case")
        run = self.show(run_id, case_id)
        body = run["body"]
        handle = body["capture"]["handle"]
        case = self.show(case_id, case_id)["body"]
        recover = self.cli(["capture", "recover", "--handle", handle])
        if len(recover.get("handles", [])) != 1:
            raise Failure(f"capture recover --handle {handle}: {json.dumps(recover)[:300]}")
        state = {
            "names": {"handle": handle, "run_id": run_id, "case_id": case_id},
            "case": {"mode": case.get("mode"), "title": case.get("title"), "lifecycle": case.get("lifecycle")},
            "run": body,
            "recover": recover["handles"][0],
            "manifest": None,
            "artifact_bytes": {},
        }
        finalization = body.get("capture_finalization")
        if finalization is not None:
            manifest = finalization["manifest"]
            data = self.read_bytes(manifest["artifact_ref"]["id"], manifest["digest"], case_id)
            state["manifest_bytes_digest"] = sha256(data)
            state["manifest"] = json.loads(data)
            for artifact in finalization["artifacts"]:
                retention = artifact["retention"]
                if retention["state"] == "retained":
                    data = self.read_bytes(retention["artifact_ref"]["id"], retention["digest"], case_id)
                    state["artifact_bytes"][artifact["key"]] = sha256(data)
            finalize_key = identity["external_run_id"] + "/finalize"
            state["finalize_receipt"] = self.cli(["receipt", finalize_key, "--actor", actor])
        return state

    def normalize(self, state, attempt):
        """Replace workspace-specific and volatile values, so two recordings compare."""
        replacements = [
            (str(self.path), "${workspace}"),
            (str(self.output_dir(attempt)), "${output_dir}"),
            (state["names"]["handle"], "${handle}"),
            (state["names"]["run_id"], "${run_id}"),
            (state["names"]["case_id"], "${case_id}"),
            (self.attempts[attempt]["producer"], "${producer}"),
            (self.attempts[attempt]["external_run_id"], "${external_run_id}"),
        ]
        manifest_digest = state["run"].get("manifest_digest")

        def walk(value):
            if isinstance(value, str):
                if manifest_digest and value == manifest_digest:
                    return "<manifest-digest>"
                for old, new in replacements:
                    value = value.replace(old, new)
                return ID.sub(lambda found: found.group(1) + "_*", value)
            if isinstance(value, list):
                return [walk(item) for item in value]
            if isinstance(value, dict):
                result = {key: walk(item) for key, item in value.items() if key not in VOLATILE}
                if result.get("digest") == "<manifest-digest>" and "size_bytes" in result:
                    result["size_bytes"] = "<manifest-size>"
                return result
            return value

        keys = ("case", "run", "recover", "manifest", "artifact_bytes")
        return walk({key: state[key] for key in keys})


def invariants(state, identity):
    """Properties every finalized recording must have, whatever the scenario."""
    body = state["run"]
    finalization = body["capture_finalization"]
    manifest = state["manifest"]
    recover = state["recover"]
    problems = []
    if body["capture"]["producer"] != identity["producer"]:
        problems.append(f"Run capture.producer is {body['capture']['producer']}")
    if body["capture"]["external_run_id"] != identity["external_run_id"]:
        problems.append(f"Run capture.external_run_id is {body['capture']['external_run_id']}")
    digests = {state["manifest_bytes_digest"], body.get("manifest_digest"), finalization["manifest"]["digest"]}
    if len(digests) != 1:
        problems.append(f"manifest bytes, Run and finalization digests disagree: {sorted(map(str, digests))}")
    if body.get("evidence_validity") != "unknown":
        problems.append(f"evidence_validity is {body.get('evidence_validity')}; a producer report sets none")
    positions = [item["position"] for item in manifest["items"]]
    if positions != list(range(1, len(positions) + 1)):
        problems.append(f"manifest positions are not contiguous from 1: {positions}")
    keys = [item["key"] for item in manifest["items"]]
    if len(set(keys)) != len(keys):
        problems.append(f"manifest item keys repeat: {keys}")
    if recover.get("seal") != "finalized":
        problems.append(f"recover seal is {recover.get('seal')}")
    if recover.get("last_position") != finalization["sealed_position"]:
        problems.append(f"recover last_position {recover.get('last_position')} != sealed {finalization['sealed_position']}")
    recovered = sorted(
        (entry["position"], entry["key"], entry["request_digest"])
        for entry in [*recover.get("items", []), *(a["receipt"] for a in recover.get("artifacts", []))]
    )
    manifested = sorted((item["position"], item["key"], item["request_digest"]) for item in manifest["items"])
    if recovered != manifested:
        problems.append("recover receipts do not match the manifest items")
    for artifact in finalization["artifacts"]:
        retention = artifact["retention"]
        if retention["state"] == "retained" and state["artifact_bytes"].get(artifact["key"]) != retention["digest"]:
            problems.append(f"retained bytes of {artifact['key']} do not match {retention['digest']}")
    receipt = state["finalize_receipt"]
    if receipt.get("result") != "committed":
        problems.append(f"finalize receipt lookup: {receipt.get('result')}")
    elif not any(item["id"] == state["names"]["run_id"] for item in receipt.get("updated", [])):
        problems.append("finalize receipt does not update the Run")
    return problems


BUNDLE_ACTOR = "agent:bundle-round-trip"


def bundle_facts(index):
    """What re-importing a producer bundle must keep of each re-importable run,
    without the IDs, item names and origin a workspace assigns."""
    facts = {}
    for run in index["runs"]:
        if run["reingest"] is None:
            continue
        artifacts = []
        for artifact in run["artifacts"]:
            # Rejected bytes were never retained, so a re-import finds nothing.
            retention = "missing" if artifact["retention"] == "rejected" else artifact["retention"]
            retained = retention == "retained"
            artifacts.append({
                "role": artifact["role"] or "undeclared",
                "media_type": artifact["media_type"],
                "retention": retention,
                "digest": artifact["digest"] if retained else None,
                "size_bytes": artifact["size_bytes"] if retained else None,
            })
        key = f"{run['producer']['name']} {run['external_run_id']}"
        facts[key] = {
            "producer": run["producer"],
            "intent": run["intent"],
            "execution": run["execution"],
            "environment": run["environment"],
            "evidence_completeness": run["evidence_completeness"],
            "artifacts": sorted(artifacts, key=lambda item: json.dumps(item, sort_keys=True)),
            "assertions": run["assertions"],
            "controls": run["controls"],
            "receipts": [{"kind": r["kind"], "reference": r.get("reference")} for r in run["receipts"]],
            "limitations": run["limitations"],
            "metadata": run["metadata"],
            "attribution": run["attribution"],
        }
    return facts


class CaseRun:
    def __init__(self, runner, producer, case, root):
        self.runner = runner
        self.producer = producer
        self.case = case
        attempts = case.get("attempts") or {"a": {"producer": "example-producer", "external_run_id": "run-0001"}}
        self.workspace = Workspace(runner.binary, root, attempts)
        self.snapshots = []

    def fail(self, step, message):
        raise Failure(f"step {step}: {message}")

    def run(self):
        for index, step in enumerate(self.case["steps"], 1):
            action = step["action"]
            getattr(self, "step_" + action.replace("-", "_"))(index, step)
        if self.runner.round_trip:
            self.round_trip()
        return self.snapshots

    def round_trip(self):
        """Export every case as a producer bundle and re-import it.

        Into a fresh workspace, every finalized captured run is re-imported and
        a bundle exported there gives the same facts; into this workspace,
        nothing is committed, since every run is an attempt already recorded.
        """
        workspace = self.workspace
        before = workspace.workspace_state()
        heads = workspace.cli(["check"]).get("heads", [])
        out_root = workspace.output_dir(next(iter(workspace.attempts)))
        fresh = workspace.root / "round-trip"
        init = workspace.cli(["init", str(fresh), "--no-vault", "--import-root", str(out_root)], workspace=False)
        if init.get("ok") is not True:
            self.fail("round-trip", f"init: {json.dumps(init)[:300]}")
        fresh_cli = Workspace.__new__(Workspace)
        fresh_cli.binary, fresh_cli.path = workspace.binary, fresh
        for number, head in enumerate(heads, 1):
            case_id = head["case_id"]
            bundle = out_root / f"producer-bundle-{number}"
            exported = workspace.cli(["--case", case_id, "export", "--kind", "producer-bundle",
                                      "--audience", "restricted", "--out", str(bundle)])
            if exported.get("ok") is not True:
                self.fail("round-trip", f"export {case_id}: {json.dumps(exported)[:300]}")
            verified = workspace.cli(["archive", "verify", str(bundle), "--scratch", str(workspace.root)],
                                     workspace=False)
            if verified.get("ok") is not True:
                self.fail("round-trip", f"archive verify: {json.dumps(verified)[:300]}")
            index = json.loads((bundle / "producer-bundle.json").read_text(encoding="utf-8"))
            for run in index["runs"]:
                finalized = run["origin"] == "capture" and run["finalization"] != "not_finalized"
                if finalized and run["reingest"] is None and run["not_reingestible"] != "incomplete_beyond_inventory":
                    self.fail("round-trip", f"finalized run {run['folder']} is not re-importable: {run['not_reingestible']}")
                if run["origin"] == "capture" and not finalized and run["reingest"] is not None:
                    self.fail("round-trip", f"unfinalized run {run['folder']} has an envelope")
            argv = ["--actor", BUNDLE_ACTOR, "ingest", "--input", str(bundle / "producer-bundle.json")]
            imported = fresh_cli.cli(argv)
            if imported.get("ok") is not True:
                self.fail("round-trip", f"ingest into a fresh workspace: {json.dumps(imported)[:400]}")
            if any(run["run_outcome"] != "imported" for run in imported["runs"]):
                self.fail("round-trip", f"fresh import outcomes: {json.dumps(imported['runs'])[:400]}")
            expected = bundle_facts(index)
            if imported["runs"]:
                again = out_root / f"producer-bundle-{number}-again"
                exported = fresh_cli.cli(["--case", imported["case_id"], "export", "--kind", "producer-bundle",
                                          "--audience", "restricted", "--out", str(again)])
                if exported.get("ok") is not True:
                    self.fail("round-trip", f"re-export: {json.dumps(exported)[:300]}")
                found = bundle_facts(json.loads((again / "producer-bundle.json").read_text(encoding="utf-8")))
                problems = list(differences(expected, found))
                if problems:
                    self.fail("round-trip", "re-imported facts differ: " + "; ".join(problems[:5]))
            back = workspace.cli(argv)
            if back.get("ok") is not True:
                self.fail("round-trip", f"ingest into the source workspace: {json.dumps(back)[:400]}")
            fresh_runs = [run for run in back["runs"] if not run["run_outcome"].startswith("already_")]
            if fresh_runs:
                self.fail("round-trip", f"a re-import into the source made new trials: {json.dumps(fresh_runs)[:400]}")
        after = workspace.workspace_state()
        if after["sequences"] != before["sequences"]:
            self.fail("round-trip", f"the source journal moved: {before['sequences']} -> {after['sequences']}")
        if heads:
            check = fresh_cli.cli(["check"])
            if check.get("ok") is not True:
                self.fail("round-trip", f"check in the fresh workspace: {json.dumps(check)[:300]}")

    def step_producer(self, index, step):
        attempt = step.get("attempt", "a")
        names = self.workspace.names(attempt)
        command = [
            *self.producer, step["command"],
            "--workspace", names["workspace"],
            "--output-dir", names["output_dir"],
            "--scenario", step["scenario"],
            "--producer", names["producer"],
            "--external-run-id", names["external_run_id"],
        ]
        if "protocol" in step:
            command += ["--protocol", step["protocol"]]
        for fault in step.get("faults", []):
            command += ["--fault", fault]
        environment = dict(os.environ, INVESTIGATION_BIN=str(self.runner.binary))
        completed = subprocess.run(
            command, capture_output=True, text=True, check=False, timeout=TIMEOUT, env=environment
        )
        try:
            output = json.loads(completed.stdout)
        except json.JSONDecodeError:
            self.fail(index, f"adapter printed no single JSON object (exit {completed.returncode}): "
                             f"{completed.stderr.strip()[-400:]}")
        if completed.returncode != step["exit"]:
            self.fail(index, f"adapter exit {completed.returncode}, expected {step['exit']}; "
                             f"calls {json.dumps(output.get('calls'))[:600]}; stderr {completed.stderr.strip()[-300:]}")
        for problem in match({"schema_version": ADAPTER, "outcome": step["outcome"],
                              "scenario": step["scenario"], "external_run_id": names["external_run_id"]}, output):
            self.fail(index, f"adapter output {problem}")
        calls = output.get("calls", [])
        for expected in step.get("calls", []):
            results = [call.get("result") for call in calls if call.get("key") == substitute(expected["key"], names)]
            if "results" in expected and results != expected["results"]:
                self.fail(index, f"calls for key {expected['key']}: results {results}, expected {expected['results']}")
            if "error_code" in expected:
                codes = [call.get("error_code") for call in calls if call.get("key") == substitute(expected["key"], names)]
                if codes[-1:] != [expected["error_code"]]:
                    self.fail(index, f"calls for key {expected['key']}: error codes {codes}, expected {expected['error_code']}")
        if step.get("only_already"):
            fresh = [call for call in calls if call.get("result") not in ("already_accepted", "already_committed")]
            if fresh:
                self.fail(index, f"a rerun made new recordings: {json.dumps(fresh)[:400]}")

    def step_check(self, index, step):
        if "workspace" in step:
            state = self.workspace.workspace_state()
            for problem in match(step["workspace"], state):
                self.fail(index, f"workspace {problem}")
        for attempt, expectation in step.get("attempts", {}).items():
            state = self.workspace.attempt_state(attempt)
            wanted = expectation["state"]
            if wanted == "absent":
                if state is not None:
                    self.fail(index, f"attempt {attempt} was recorded; expected nothing")
                continue
            if state is None:
                self.fail(index, f"attempt {attempt} has no begin receipt")
            seal = state["recover"].get("seal")
            if seal != wanted:
                self.fail(index, f"attempt {attempt} seal is {seal}, expected {wanted}")
            if wanted == "finalized":
                for problem in invariants(state, self.workspace.attempts[attempt]):
                    self.fail(index, f"attempt {attempt}: {problem}")
            normalized = self.workspace.normalize(state, attempt)
            if "expected" in expectation:
                pattern = self.runner.expected(expectation["expected"])
                for problem in match(pattern, normalized):
                    self.fail(index, f"attempt {attempt} canonical {problem}")
            if "recover" in expectation:
                for problem in match(expectation["recover"], normalized["recover"]):
                    self.fail(index, f"attempt {attempt} recover {problem}")
            self.snapshots.append({"step": index, "attempt": attempt, "state": normalized})

    def step_cli(self, index, step):
        attempt = step.get("attempt", "a")
        names = self.workspace.names(attempt)
        if step.get("resolve", True):
            state = self.workspace.attempt_state(attempt)
            if state is None:
                self.fail(index, f"attempt {attempt} has no begin receipt")
            names.update(state["names"])
        for name, content in step.get("write", {}).items():
            target = Path(names["output_dir"]) / name
            target.write_text(json.dumps(substitute(content, names)), encoding="utf-8")
        argv = substitute(step["argv"], names)
        stdin = None if "stdin" not in step else json.dumps(substitute(step["stdin"], names))
        response = self.workspace.cli(argv, stdin, actor=step.get("actor") and substitute(step["actor"], names))
        for problem in match(substitute(step["expect"], names), response):
            self.fail(index, f"`{' '.join(step['argv'])}` {problem}")

    def step_replay_items(self, index, step):
        """Resend every accepted item exactly as the manifest binds it."""
        attempt = step.get("attempt", "a")
        state = self.workspace.attempt_state(attempt)
        if state is None or state["manifest"] is None:
            self.fail(index, f"attempt {attempt} is not finalized")
        for item in state["manifest"]["items"]:
            operation = item["operation"].split(".")[1]
            response = self.workspace.cli(["capture", operation, "--input", "-"], json.dumps(item["request"]))
            receipt = response.get("receipt", response) if operation == "attach" else response
            got = (receipt.get("result"), receipt.get("position"), receipt.get("accepted_at"))
            want = ("already_accepted", item["position"], item["accepted_at"])
            if got != want:
                self.fail(index, f"replay of {item['key']}: {got}, expected {want}")


class Runner:
    def __init__(self, binary, suite, round_trip=False):
        self.binary = binary
        self.round_trip = round_trip
        self.suite_dir = suite
        self.suite = json.loads((suite / "suite.json").read_text(encoding="utf-8"))
        self.cases = {}
        for path in sorted((suite / "cases").glob("*.json")):
            case = json.loads(path.read_text(encoding="utf-8"))
            self.cases[case["name"]] = case
        self._expected = {}

    def expected(self, name):
        if name not in self._expected:
            self._expected[name] = json.loads((self.suite_dir / "expected" / f"{name}.json").read_text(encoding="utf-8"))
        return self._expected[name]

    def validate_suite(self):
        """The suite's own consistency: every plan item maps to cases that exist."""
        problems = []
        if self.suite.get("schema_version") != SUITE_VERSION:
            problems.append(f"suite.json schema_version is not {SUITE_VERSION}")
        scenarios = {path.stem for path in (self.suite_dir / "scenarios").glob("*.json")}
        if set(self.suite["scenarios"]) != scenarios:
            problems.append(f"suite.json scenarios {self.suite['scenarios']} != files {sorted(scenarios)}")
        covered = set()
        for item in self.suite["coverage"]:
            if not item["cases"]:
                problems.append(f"coverage item {item['id']} names no case")
            for name in item["cases"]:
                if name not in self.cases:
                    problems.append(f"coverage item {item['id']} names unknown case {name}")
                covered.add(name)
        items = {item["id"] for item in self.suite["coverage"]}
        for name, case in self.cases.items():
            if case.get("schema_version") != CASE_VERSION:
                problems.append(f"case {name}: schema_version is not {CASE_VERSION}")
            for item in case.get("covers", []):
                if item not in items:
                    problems.append(f"case {name} covers unknown item {item}")
            if name not in covered:
                problems.append(f"case {name} is not listed under any coverage item")
            for step in case["steps"]:
                if step["action"] == "producer" and step["scenario"] not in scenarios:
                    problems.append(f"case {name} uses unknown scenario {step['scenario']}")
                for expectation in step.get("attempts", {}).values():
                    if "expected" in expectation and not (self.suite_dir / "expected" / f"{expectation['expected']}.json").is_file():
                        problems.append(f"case {name} names missing expected file {expectation['expected']}")
        return problems

    def describe(self, producer):
        completed = subprocess.run([*producer, "describe"], capture_output=True, text=True, check=False, timeout=TIMEOUT)
        if completed.returncode != 0:
            return [f"describe exited {completed.returncode}: {completed.stderr.strip()[-300:]}"]
        try:
            description = json.loads(completed.stdout)
        except json.JSONDecodeError:
            return ["describe printed no JSON object"]
        problems = []
        if description.get("schema_version") != ADAPTER:
            problems.append(f"describe schema_version is {description.get('schema_version')}")
        missing = sorted(set(self.suite["scenarios"]) - set(description.get("scenarios", [])))
        if missing:
            problems.append(f"describe lacks required scenarios {missing}")
        faults = sorted({"drop-response", "crash-after"} - set(description.get("faults", [])))
        if faults:
            problems.append(f"describe lacks required faults {faults}")
        schema = subprocess.run([str(self.binary), "schema", "capture", "--format", "json"],
                                capture_output=True, text=True, check=True, timeout=TIMEOUT)
        accepted = json.loads(schema.stdout)["capabilities"]["protocols"]
        accepted = {entry if isinstance(entry, str) else entry.get("version") for entry in accepted}
        if self.suite["protocol"] not in description.get("protocols", []):
            problems.append(f"describe does not list {self.suite['protocol']}")
        unknown = sorted(set(description.get("protocols", [])) - accepted)
        if unknown:
            problems.append(f"describe lists protocols this build does not accept: {unknown}")
        return problems

    def run_case(self, producer, case, keep):
        root = Path(tempfile.mkdtemp(prefix=f"conformance-{case['name']}-")).resolve()
        try:
            return CaseRun(self, producer, case, root).run(), None
        except Failure as failure:
            return None, str(failure)
        except subprocess.TimeoutExpired as error:
            return None, f"timed out: {error.cmd}"
        finally:
            if keep is None:
                shutil.rmtree(root, ignore_errors=True)
            else:
                keep.mkdir(parents=True, exist_ok=True)
                shutil.move(str(root), keep / root.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--bin", required=True, type=Path, help="the investigation executable")
    parser.add_argument("--producer", action="append", default=[],
                        help="a producer adapter command line; repeat to compare producers")
    parser.add_argument("--suite", type=Path, default=SUITE)
    parser.add_argument("--case", action="append", help="run only this case (repeatable)")
    parser.add_argument("--keep", type=Path, help="keep each case's workspace under this directory")
    parser.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 1),
                        help="cases to run at once (default: up to 8)")
    parser.add_argument("--dump", type=Path, help="write each checked, normalized canonical state under this directory")
    parser.add_argument("--json", action="store_true", help="print one JSON report")
    parser.add_argument("--validate-suite", action="store_true", help="check the suite files only")
    parser.add_argument("--round-trip", action="store_true",
                        help="after each case, export every case as a producer bundle and re-import it into a "
                             "fresh workspace and into the source one")
    args = parser.parse_args()
    binary = args.bin.resolve()
    runner = Runner(binary, args.suite.resolve(), round_trip=args.round_trip)
    problems = runner.validate_suite()
    if problems or args.validate_suite:
        for problem in problems:
            print(f"suite: {problem}", file=sys.stderr)
        if not problems:
            print(f"suite ok: {len(runner.cases)} cases, {len(runner.suite['coverage'])} coverage items")
        return 1 if problems else 0
    if not args.producer:
        parser.error("name at least one --producer")
    selected = args.case or sorted(runner.cases)
    unknown = sorted(set(selected) - set(runner.cases))
    if unknown:
        parser.error(f"unknown case {unknown}")
    report = {"schema_version": "inv.conformance-report.v1", "suite": runner.suite["schema_version"],
              "protocol": runner.suite["protocol"], "producers": [], "equivalence": []}
    snapshots = {}
    producers = [(text, shlex.split(text)) for text in args.producer]
    # Cases are independent workspaces and spend their time in subprocesses, so
    # they run concurrently. Results are reported in a fixed order.
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = {
            (text, name): pool.submit(runner.run_case, producer, runner.cases[name], args.keep)
            for text, producer in producers
            for name in selected
        }
        descriptions = {text: pool.submit(runner.describe, producer) for text, producer in producers}
    for number, (text, _) in enumerate(producers, 1):
        entry = {"producer": text, "describe": descriptions[text].result(), "cases": []}
        for name in selected:
            states, failure = futures[(text, name)].result()
            entry["cases"].append({"case": name, "passed": failure is None, "failure": failure})
            if states is not None:
                snapshots.setdefault(name, []).append((text, states))
                if args.dump is not None:
                    target = args.dump / f"producer-{number}"
                    target.mkdir(parents=True, exist_ok=True)
                    for state in states:
                        path = target / f"{name}.step-{state['step']}.{state['attempt']}.json"
                        path.write_text(json.dumps(state["state"], indent=2, sort_keys=True) + "\n", encoding="utf-8")
            if not args.json:
                print(f"{'PASS' if failure is None else 'FAIL'}  {name}  [{text}]" + (f"\n      {failure}" if failure else ""))
        entry["passed"] = not entry["describe"] and all(case["passed"] for case in entry["cases"])
        if entry["describe"] and not args.json:
            for problem in entry["describe"]:
                print(f"FAIL  describe  [{text}]\n      {problem}")
        report["producers"].append(entry)
    for name, runs in snapshots.items():
        (first, base), *others = runs
        for other, states in others:
            found = [] if len(base) == len(states) else [f"{len(base)} checks != {len(states)} checks"]
            for one, two in zip(base, states):
                found += [f"step {one['step']} attempt {one['attempt']}: {d}" for d in differences(one["state"], two["state"])]
            report["equivalence"].append({"case": name, "producers": [first, other], "equal": not found,
                                          "differences": found[:20]})
            if not args.json:
                print(f"{'SAME' if not found else 'DIFF'}  {name}  [{first}] vs [{other}]" +
                      "".join(f"\n      {line}" for line in found[:20]))
    passed = all(entry["passed"] for entry in report["producers"]) and all(
        entry["equal"] for entry in report["equivalence"])
    report["passed"] = passed
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        total = sum(len(entry["cases"]) for entry in report["producers"])
        failed = sum(not case["passed"] for entry in report["producers"] for case in entry["cases"])
        print(f"conformance {'passed' if passed else 'FAILED'}: {total - failed}/{total} case runs passed")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
