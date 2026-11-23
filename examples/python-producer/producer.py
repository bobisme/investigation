#!/usr/bin/env python3
"""Example producer that records a run through the `investigation` JSON CLI.

Python 3 standard library only. The producer never imports Investigation code
and never touches its files: it spawns the `investigation` binary with an
argument array (no shell) and one JSON request on stdin, and reads one JSON
response on stdout. That protocol, `inv.capture.v1`, is the whole contract.

Two ways to run it (see README.md):

  producer.py --bin BIN --workspace DIR --fixtures DIR
      Self-check against a built binary: lifecycle, lost responses, crashes
      and resume, and the producer-kinds fixtures. Exit 0 on success.

  producer.py describe | run ... | resume ...
      The `inv.producer-adapter.v1` interface that the conformance runner
      drives (docs/recording.md, "Producer adapter protocol").
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys

sys.dont_write_bytecode = True  # an example leaves nothing beside itself

ADAPTER_SCHEMA = "inv.producer-adapter.v1"
SCENARIOS = ["synthetic-run", "partial-evidence", "interrupted-run", "late-item"]
FAULT_KINDS = ["drop-response", "crash-after"]
OPERATIONS = ["begin", "append", "attach", "finalize"]
CRASH_STATUS = 75
RESULT_BYTES = b'{"metric":"latency.p99","value":18.7,"unit":"ms"}\n'
RETRIES = 3


class Crash(Exception):
    """An injected `crash-after` fault fired."""


class Rejected(Exception):
    """The recorder refused a call the scenario expected to succeed."""

    def __init__(self, code):
        super().__init__(code)
        self.code = code


def _no_constant(name):
    raise ValueError("non-standard JSON constant " + name)


def strict_loads(text):
    """Parse strict JSON: no NaN or Infinity, no duplicate object keys."""

    def pairs(items):
        keys = [key for key, _ in items]
        if len(keys) != len(set(keys)):
            raise ValueError("duplicate object key")
        return dict(items)

    return json.loads(text, parse_constant=_no_constant, object_pairs_hook=pairs)


def strict_dumps(value):
    """Serialize strict JSON as UTF-8 (allow_nan=False refuses NaN/Infinity)."""
    return json.dumps(value, allow_nan=False, ensure_ascii=False, separators=(",", ":")).encode()


class Recorder:
    """Thin client: one `investigation capture ...` process per call."""

    def __init__(self, binary, workspace, faults=(), log=None):
        self.binary = binary
        self.workspace = workspace
        self.faults = set(faults)
        self.counts = {op: 0 for op in OPERATIONS}
        self.calls = log if log is not None else []
        self.run_id = None
        self.handle = None

    def _spawn(self, argv, payload=None):
        # An argument array and a pipe: nothing is interpolated into a shell.
        env = {k: v for k, v in os.environ.items() if k not in ("AGENT", "FORMAT")}
        done = subprocess.run(
            [self.binary, "--workspace", self.workspace, *argv, "--format", "json"],
            input=None if payload is None else strict_dumps(payload),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            check=False,
        )
        if done.stderr:
            sys.stderr.write(done.stderr.decode("utf-8", "replace"))
        return done.returncode, done.stdout

    def _exchange(self, argv, payload):
        """Send one request. Returns the parsed response, or None if it is lost."""
        _, out = self._spawn(argv, payload)
        try:
            response = strict_loads(out.decode("utf-8"))
        except ValueError:
            return None
        if not isinstance(response, dict):
            return None
        # A failure whose commit status is unknown is retried like a lost response.
        if response.get("ok") is False and response.get("commit_status") == "unknown":
            return None
        return response

    @staticmethod
    def _outcome(operation, response):
        """(result, error_code) for one response."""
        if response.get("ok") is False:
            return "rejected", (response.get("error") or {}).get("code")
        if operation == "attach":
            return response["receipt"]["result"], None
        if operation == "append":
            return response["result"], None
        return response["commit"]["result"], None

    def call(self, operation, payload):
        """One mutating call with the retry rule: resend the identical request."""
        self.counts[operation] += 1
        number = self.counts[operation]
        argv = ["capture", operation, "--input", "-"]
        key, position = payload.get("key"), payload.get("position")
        dropped = "drop-response:%s:%d" % (operation, number) in self.faults
        response = None
        for _ in range(1 + RETRIES):
            response = self._exchange(argv, payload)
            if response is not None:
                self._note(operation, key, position, response)
                if not dropped:
                    break
                response = None  # the acknowledgement is discarded: resend
                dropped = False
        if response is None:
            raise Rejected("NO_RESPONSE")
        result, code = self._outcome(operation, response)
        if operation == "begin" and result != "rejected":
            self.run_id, self.handle = response["run_id"], response["handle"]
        if "crash-after:%s:%d" % (operation, number) in self.faults:
            raise Crash()
        if result == "rejected":
            raise Rejected(code)
        return response

    def _note(self, operation, key, position, response):
        result, code = self._outcome(operation, response)
        self.calls.append(
            {
                "operation": "capture." + operation,
                "key": key,
                "position": position,
                "result": result,
                "error_code": code,
            }
        )

    def recover(self, handle):
        """Read-only: what the recorder holds for one handle."""
        status, out = self._spawn(["capture", "recover", "--handle", handle])
        response = strict_loads(out.decode("utf-8"))
        if status != 0 or response.get("ok") is False:
            raise Rejected((response.get("error") or {}).get("code", "RECOVER_FAILED"))
        return response["handles"][0]

    def read(self, argv):
        """Any read-only CLI call; returns the parsed response."""
        _, out = self._spawn(argv)
        return strict_loads(out.decode("utf-8"))


# ---------------------------------------------------------------------------
# Scenarios. Each is data: a begin request, items at fixed positions, a
# finalize request, and optionally a late item sent after sealing.


def _assertion():
    return {"kind": "reported_metric", "name": "latency.p99", "value": 18.7, "unit": "ms"}


def scenario(name, external_run_id, producer, out_dir, protocol=None):
    """Return (begin, items, finalize, late) for a named scenario.

    Requests carry no clock or random value, so a rerun or a resume sends
    identical requests. `items` are (operation, position, payload) triples.
    """
    result_path = os.path.join(os.path.abspath(out_dir), "result.json")
    begin = {
        "key": external_run_id + "/begin",
        "actor": "agent:" + producer,
        "producer": {"name": producer, "version": "0.1.0"},
        "external_run_id": external_run_id,
        "intent": {"label": "synthetic run"},
        "case": {"kind": "new_record_case", "title": "Synthetic runs"},
    }
    if protocol is not None:
        begin["protocol"] = protocol
    started = ("append", 1, {"key": "started", "position": 1, "fact": {"kind": "started"}})
    result = (
        "attach",
        2,
        {
            "key": "result",
            "position": 2,
            "role": "original-output",
            "source": {"kind": "file", "path": result_path},
            "media_type": "application/json",
        },
    )
    finalize = {"key": external_run_id + "/finalize"}
    late = None
    if name in ("synthetic-run", "late-item"):
        items = [
            started,
            result,
            ("append", 3, {"key": "phase-summarize", "position": 3,
                           "fact": {"kind": "phase_changed", "phase": "summarize"}}),
        ]
        finalize.update(
            expected_last_position=3,
            completion="complete",
            execution={"state": "completed", "exit_code": 0},
            assertions=[_assertion()],
        )
        if name == "late-item":
            late = ("append", 4, {"key": "late-note", "position": 4,
                                  "fact": {"kind": "phase_changed", "phase": "late"}})
    elif name == "partial-evidence":
        host = {"example-org.host": {"slot": 3}}
        seed = {"example-org.fixture": {"seed": 7}}
        begin["metadata"] = seed
        items = [
            ("append", 1, {"key": "started", "position": 1, "fact": {"kind": "started"}, "metadata": host}),
            ("attach", 2, dict(result[2], channel="results",
                               expected_digest="sha256:" + hashlib.sha256(RESULT_BYTES).hexdigest(),
                               expected_size_bytes=len(RESULT_BYTES), metadata=host)),
            ("attach", 3, {"key": "trace", "position": 3, "role": "log", "channel": "trace",
                           "source": {"kind": "missing", "reason": "trace collector exited before flush"}}),
            ("append", 4, {"key": "trace-gap", "position": 4,
                           "fact": {"kind": "channel", "channel": "trace",
                                    "state": {"kind": "gap", "detail": "collector restarted"}}}),
        ]
        finalize.update(
            expected_last_position=4,
            completion="complete",
            execution={"state": "completed", "exit_code": 0},
            channels=[{"channel": "results", "completeness": "complete"},
                      {"channel": "trace", "completeness": "complete"}],
            declared_artifacts=["result", "trace", "summary"],
            assertions=[_assertion()],
            controls=[{"name": "steady-load", "producer_result": "reported_pass"},
                      {"name": "warm-cache", "producer_result": "not_run"}],
            receipts=[{"kind": "signed-statement", "reference": "urn:example:statement:1"}],
            limitations=["Single synthetic workload"],
            metadata=seed,
        )
    elif name == "interrupted-run":
        items = [started, result]
        finalize.update(
            expected_last_position=4,
            completion="interrupted",
            execution={"state": "unknown"},
            declared_artifacts=["result"],
            limitations=["Producer cannot vouch for positions after 2"],
        )
    else:
        raise ValueError("unknown scenario " + name)
    return begin, items, finalize, late


def write_result(out_dir):
    """The synthetic external action: fabricate the result bytes, once."""
    path = os.path.join(out_dir, "result.json")
    try:
        with open(path, "rb") as existing:
            if existing.read() == RESULT_BYTES:
                return  # done by an earlier attempt; never repeat the action
    except FileNotFoundError:
        pass
    temporary = path + ".tmp"
    with open(temporary, "wb") as written:
        written.write(RESULT_BYTES)
        written.flush()
        os.fsync(written.fileno())
    os.replace(temporary, path)  # closed and flushed before it is attached


def run_scenario(recorder, name, producer, external_run_id, out_dir, resume, protocol=None):
    """Drive one scenario. Returns the finalize receipt."""
    begin, items, finalize, late = scenario(name, external_run_id, producer, out_dir, protocol)
    handle = recorder.call("begin", begin)["handle"]  # on resume this resends step 1
    last, receipt = 0, None
    if resume:
        state = recorder.recover(handle)
        if state["seal"] != "open":
            # Sealed or finalized: only the identical finalize completes it.
            receipt = recorder.call("finalize", dict(finalize, handle=handle))
        else:
            last = state["last_position"] or 0
            held = state["items"] + [artifact["receipt"] for artifact in state["artifacts"]]
            ours = {position: payload["key"] for _, position, payload in items}
            if any(ours.get(entry["position"]) != entry["key"] for entry in held):
                # A recorded step is not one of ours: this stream cannot be vouched for.
                receipt = recorder.call(
                    "finalize",
                    {
                        "handle": handle,
                        "key": finalize["key"],
                        "expected_last_position": state["last_position"],
                        "completion": "interrupted",
                        "execution": {"state": "unknown"},
                    },
                )
    if receipt is None:
        for operation, position, payload in items:
            if position <= last:
                continue  # recorded already: do not repeat the step or its action
            if payload.get("source", {}).get("kind") == "file":
                write_result(out_dir)
            recorder.call(operation, dict(payload, handle=handle))
        receipt = recorder.call("finalize", dict(finalize, handle=handle))
    if late is not None:
        # A sealed handle must refuse a late item; recording it would falsify the manifest.
        operation, _, payload = late
        try:
            recorder.call(operation, dict(payload, handle=handle))
        except Rejected as refusal:
            if refusal.code != "REVISION_CONFLICT":
                raise
        else:
            raise Rejected("LATE_ITEM_ACCEPTED")
    return receipt


# ---------------------------------------------------------------------------
# Replaying a producer-run envelope through the capture lifecycle


def sha256_prefixed(digest):
    return digest if digest.startswith("sha256:") else "sha256:" + digest


def source_time(value, quality):
    return {
        "timestamp": {"state": "known", "value": value},
        "offset": {"state": "unknown"},
        "clock_quality": quality or "unverified",
    }


def record_envelope(recorder, envelope, base_dir, case_title="Producer kinds"):
    """Record one `inv.producer-run.v1` document with capture begin..finalize.

    Reads only the envelope's generic fields; nothing branches on its kind.
    Artifact paths are resolved beneath `base_dir`, which must be an import
    root. Returns the finalize receipt.
    """
    run_id = envelope["external_run_id"]
    begun = recorder.call(
        "begin",
        {
            "key": run_id + "/begin",
            "actor": "agent:" + envelope["producer"]["name"],
            "producer": envelope["producer"],
            "external_run_id": run_id,
            "intent": {"label": envelope["intent"]["label"]},
            "case": {"kind": "new_record_case", "title": case_title},
        },
    )
    handle, position = begun["handle"], 1
    recorder.call(
        "append",
        {"handle": handle, "key": "started", "position": position, "fact": {"kind": "started"}},
    )
    declared, absent = [], 0
    for artifact in envelope.get("artifacts", []):
        relative = artifact["path"]
        while relative.startswith("./"):
            relative = relative[2:]
        request = {
            "handle": handle,
            "key": relative,
            "position": position + 1,
            "role": artifact["role"],
            "source": {"kind": "file", "path": os.path.join(base_dir, relative)},
        }
        if "media_type" in artifact:
            request["media_type"] = artifact["media_type"]
        if "expected_sha256" in artifact:
            request["expected_digest"] = sha256_prefixed(artifact["expected_sha256"])
        declared.append(relative)
        try:
            receipt = recorder.call("attach", request)
            position += 1
            if receipt["availability"] != "staged":
                absent += 1  # recorded as missing, not retained
        except Rejected as refusal:
            # A refused attachment consumes no position. Its key stays declared,
            # so the manifest names it as missing instead of dropping it.
            if refusal.code != "INVALID_INPUT":
                raise
            absent += 1
    source = envelope["execution"]
    execution = {"state": source["state"]}
    for field in ("exit_code", "error"):
        if source.get(field) is not None:
            execution[field] = source[field]
    for field in ("started_at", "ended_at"):
        if source.get(field) is not None:
            execution[field] = source_time(source[field], source.get("clock_quality"))
    finalize = {
        "handle": handle,
        "key": run_id + "/finalize",
        "expected_last_position": position,
        "completion": "partial" if absent else "complete",
        "execution": execution,
        "declared_artifacts": declared,
    }
    for field in ("assertions", "controls", "limitations"):
        if field in envelope:
            finalize[field] = envelope[field]
    return recorder.call("finalize", finalize)


# ---------------------------------------------------------------------------
# Canonical form: what two producers must agree on


def canonical_run(recorder, run_id):
    """The run's recorded facts with ids, times and manifest digests removed.

    Source mappings (artifact key, position, role, digest, size) are kept: a
    producer in another language that records the same bytes under the same
    keys must produce the same mappings.
    """
    shown = recorder.read(["show", run_id, "--depth", "full"])
    body = shown["record"]["body"]
    section = dict(body["capture_finalization"])
    for volatile in ("command_id", "manifest", "snapshot"):
        section.pop(volatile, None)
    artifacts = []
    for entry in section.pop("artifacts"):
        flat = {k: v for k, v in entry.items() if k != "retention"}
        for name, value in entry["retention"].items():
            if name not in ("source_ref", "artifact_ref"):  # record ids vary per workspace
                flat["retention" if name == "state" else name] = value
        artifacts.append(flat)
    return {
        "producer": body["capture"]["producer"],
        "external_run_id": body["capture"]["external_run_id"],
        "execution_status": body["execution_status"],
        "evidence_completeness": body["evidence_completeness"],
        "evidence_validity": body["evidence_validity"],
        "capture_finalization": dict(section, artifacts=artifacts),
    }


def compare_canonical(left, right):
    """Differences between two canonical documents, as readable strings."""
    problems = []
    for name in sorted(set(left) | set(right)):
        if name not in left or name not in right:
            problems.append("%s: only in the %s file" % (name, "second" if name in right else "first"))
        elif left[name] != right[name]:
            problems.append("%s: canonical envelopes differ" % name)
    return problems


# ---------------------------------------------------------------------------
# Adapter and command line


def parse_faults(values):
    for value in values:
        parts = value.split(":")
        if (
            len(parts) != 3
            or parts[0] not in FAULT_KINDS
            or parts[1] not in OPERATIONS
            or not parts[2].isdigit()
            or int(parts[2]) < 1
        ):
            raise ValueError("unknown fault " + value)
    return list(values)


def adapter_main(args):
    binary = args.bin or os.environ.get("INVESTIGATION_BIN")
    if args.scenario not in SCENARIOS:
        sys.stderr.write("unknown scenario %s\n" % args.scenario)
        return 2
    try:
        faults = parse_faults(getattr(args, "fault", []))
    except ValueError as error:
        sys.stderr.write("%s\n" % error)
        return 2
    if not binary:
        sys.stderr.write("set INVESTIGATION_BIN or pass --bin\n")
        return 2
    calls = []
    recorder = Recorder(binary, args.workspace, faults, calls)
    report = {
        "schema_version": ADAPTER_SCHEMA,
        "scenario": args.scenario,
        "external_run_id": args.external_run_id,
        "outcome": "rejected",
        "run_id": None,
        "handle": None,
        "calls": calls,
        "finalize": None,
    }
    status = 1
    try:
        final = run_scenario(
            recorder,
            args.scenario,
            args.producer,
            args.external_run_id,
            args.output_dir,
            args.command == "resume",
            args.protocol,
        )
        report.update(outcome="finalized", finalize=final)
        status = 0
    except Crash:
        report["outcome"] = "crashed"
        status = CRASH_STATUS
    except Rejected as refusal:
        sys.stderr.write("rejected: %s\n" % refusal.code)
    except (OSError, ValueError, KeyError) as error:
        # Never leave stdout empty: the runner reads one JSON object.
        sys.stderr.write("%s: %s\n" % (type(error).__name__, error))
    report.update(run_id=recorder.run_id, handle=recorder.handle)
    sys.stdout.write(json.dumps(report, allow_nan=False, sort_keys=True) + "\n")
    return status


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--bin", help="investigation executable (default: $INVESTIGATION_BIN)")
    parser.add_argument("--workspace", help="workspace directory")
    parser.add_argument("--fixtures", help="the repository's fixtures directory")
    parser.add_argument("--canonical-out", help="write the canonical envelopes to this file")
    sub = parser.add_subparsers(dest="command")
    sub.add_parser("describe")
    for name in ("run", "resume"):
        step = sub.add_parser(name)
        step.add_argument("--workspace", dest="workspace", required=True)
        step.add_argument("--output-dir", required=True)
        step.add_argument("--scenario", required=True)
        step.add_argument("--producer", required=True)
        step.add_argument("--external-run-id", required=True)
        step.add_argument("--bin", dest="bin")
        step.add_argument("--protocol", default=None)
        if name == "run":
            step.add_argument("--fault", action="append", default=[])
    compare = sub.add_parser("compare", help="compare two canonical files")
    compare.add_argument("first")
    compare.add_argument("second")
    return parser


def main(argv=None):
    parser = build_parser()
    try:
        args = parser.parse_args(argv)
    except SystemExit as exit_request:
        return 2 if exit_request.code else 0
    if args.command == "describe":
        print(
            json.dumps(
                {
                    "schema_version": ADAPTER_SCHEMA,
                    "protocols": ["inv.capture.v1"],
                    "scenarios": SCENARIOS,
                    "faults": FAULT_KINDS,
                }
            )
        )
        return 0
    if args.command in ("run", "resume"):
        return adapter_main(args)
    if args.command == "compare":
        with open(args.first, encoding="utf-8") as one, open(args.second, encoding="utf-8") as two:
            problems = compare_canonical(strict_loads(one.read()), strict_loads(two.read()))
        for problem in problems:
            print(problem)
        return 1 if problems else 0
    if not (args.bin and args.workspace and args.fixtures):
        parser.print_usage(sys.stderr)
        return 2
    import selfcheck

    return selfcheck.main(args)


if __name__ == "__main__":
    sys.exit(main())
