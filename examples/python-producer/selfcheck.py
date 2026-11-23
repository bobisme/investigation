"""Self-check for producer.py: drives it against a built `investigation`.

Every case runs in its own fresh workspace beneath --workspace. Standard
library only. Exit status 0 means every check held.
"""

import json
import os
import subprocess
import sys

import producer

KINDS = ["benchmark", "file-analysis", "simulation", "trace-capture"]
VARIANTS = ["envelope", "envelope-incomplete"]
PRODUCER = "py-example"
HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "producer.py")


class Failure(Exception):
    pass


def check(condition, message):
    if not condition:
        raise Failure(message)


def init_workspace(binary, path, import_roots):
    """A fresh workspace that may read `import_roots` and nothing else."""
    check(not os.path.exists(path), "%s already exists; pass an empty --workspace" % path)
    argv = [binary, "init", path, "--no-vault", "--format", "json"]
    for root in import_roots:
        argv += ["--import-root", root]
    done = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    check(done.returncode == 0, "init failed: " + done.stdout.decode() + done.stderr.decode())


def adapter(binary, workspace, command, out_dir, run_id, faults=(), scenario="synthetic-run", extra=()):
    """Run the adapter exactly as the conformance runner would. (status, report)."""
    argv = [sys.executable, SCRIPT, command, "--workspace", workspace, "--output-dir", out_dir]
    argv += ["--scenario", scenario, "--producer", PRODUCER, "--external-run-id", run_id]
    for fault in faults:
        argv += ["--fault", fault]
    argv += list(extra)
    env = dict(os.environ, INVESTIGATION_BIN=binary)
    done = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, check=False)
    lines = done.stdout.decode().splitlines()
    check(len(lines) == 1, "adapter must print exactly one JSON object: %r %s" % (done.stdout, done.stderr.decode()))
    return done.returncode, producer.strict_loads(lines[0])


def canonical_of(binary, workspace, run_id):
    return producer.canonical_run(producer.Recorder(binary, workspace), run_id)


def synthetic(binary, base, name, faults, resume_after_crash):
    """One synthetic run in its own workspace; returns its canonical form."""
    workspace, out = os.path.join(base, name), os.path.join(base, "out-" + name)
    os.makedirs(out)
    init_workspace(binary, workspace, [out])
    status, report = adapter(binary, workspace, "run", out, "run-0001", faults)
    if resume_after_crash:
        check(status == 75 and report["outcome"] == "crashed", "%s: expected a crash, got %s" % (name, status))
        status, report = adapter(binary, workspace, "resume", out, "run-0001")
    check(status == 0 and report["outcome"] == "finalized", "%s: %s" % (name, report))
    check(report["finalize"]["finalization"] == "finalized", name + ": not finalized")
    check(report["finalize"]["sealed_position"] == 3, name + ": sealed position")
    check(report["finalize"]["execution"] == "completed", name + ": execution state")
    return report, canonical_of(binary, workspace, report["run_id"])


def check_describe():
    done = subprocess.run([sys.executable, SCRIPT, "describe"], stdout=subprocess.PIPE, check=True)
    described = producer.strict_loads(done.stdout.decode())
    check(described["schema_version"] == producer.ADAPTER_SCHEMA, "describe schema_version")
    check(described["scenarios"] == producer.SCENARIOS and len(producer.SCENARIOS) == 4, "describe scenarios")


def check_capture_discovery(binary):
    done = subprocess.run(
        [binary, "schema", "capture", "--format", "json"], stdout=subprocess.PIPE, check=True
    )
    text = done.stdout.decode()
    check("inv.capture.v1" in text, "the binary does not speak inv.capture.v1")


def check_fixture_requests(binary, fixtures, base):
    """The checked-in request examples run unchanged, except for the handle."""
    root = os.path.join(fixtures, "capture", "v1")
    workspace = os.path.join(base, "fixture-requests")
    init_workspace(binary, workspace, [root])
    recorder = producer.Recorder(binary, workspace)

    def load(name):
        with open(os.path.join(root, name), encoding="utf-8") as handle:
            return producer.strict_loads(handle.read())

    begun = recorder.call("begin", load("begin.json"))
    handle = begun["handle"]
    for operation, name in (("append", "append.json"), ("attach", "attach.json")):
        recorder.call(operation, dict(load(name), handle=handle))
    final = recorder.call("finalize", dict(load("finalize.json"), handle=handle))
    check(final["finalization"] == "finalized", "fixture lifecycle did not finalize")
    state = recorder.recover(handle)
    check(state["seal"] == "finalized", "recover should report finalized")
    # The documented recover request shape reads the same handle.
    status, out = recorder._spawn(["capture", "recover", "--input", "-"], dict(load("recover.json"), handle=handle))
    check(status == 0 and producer.strict_loads(out.decode())["handles"], "recover --input")


def check_scenarios(binary, base):
    """The other adapter scenarios, and a protocol the binary does not speak."""
    found = {}
    for name in ("partial-evidence", "interrupted-run", "late-item"):
        for crash in (False, True):
            tag = name + ("-crash" if crash else "")
            workspace, out = os.path.join(base, tag), os.path.join(base, "out-" + tag)
            os.makedirs(out)
            init_workspace(binary, workspace, [out])
            faults = ["crash-after:finalize:1"] if crash else []
            status, report = adapter(binary, workspace, "run", out, "run-0001", faults, name)
            if crash:
                check(status == 75, tag + ": expected a crash")
                status, report = adapter(binary, workspace, "resume", out, "run-0001", (), name)
            check(status == 0 and report["outcome"] == "finalized", "%s: %s" % (tag, report))
            if name == "late-item":
                last = report["calls"][-1]
                check(last["result"] == "rejected" and last["error_code"] == "REVISION_CONFLICT", tag + ": late item")
            canon = canonical_of(binary, workspace, report["run_id"])["capture_finalization"]
            if name == "interrupted-run":
                check(canon["finalization"] == "finalized_partial" and canon["completion"] == "interrupted", tag)
                check(canon["missing_positions"] == [3, 4], tag + ": missing positions")
            if name == "partial-evidence":
                check(canon["finalization"] == "finalized", tag)
                check({a["key"] for a in canon["artifacts"]} >= {"result", "trace", "summary"}, tag + ": artifacts")
                check(canon["limitations"] == ["Single synthetic workload"], tag)
            found[tag] = canonical_of(binary, workspace, report["run_id"])
        check(found[name] == found[name + "-crash"], name + ": resume differs from a clean run")
    tag = "protocol"
    workspace, out = os.path.join(base, tag), os.path.join(base, "out-" + tag)
    os.makedirs(out)
    init_workspace(binary, workspace, [out])
    status, report = adapter(binary, workspace, "run", out, "run-0001", (), "synthetic-run", ["--protocol", "inv.capture.v2"])
    check(status == 1 and report["outcome"] == "rejected", "unsupported protocol must be rejected")
    check(report["calls"][0]["error_code"] == "INVALID_INPUT", "protocol refusal code")
    return found


def check_synthetic(binary, base):
    clean_report, clean = synthetic(binary, base, "clean", [], False)
    check([c["result"] for c in clean_report["calls"]] == ["committed", "accepted", "accepted", "accepted", "committed"],
          "clean run results: %s" % clean_report["calls"])
    check(clean["capture_finalization"]["assertions"][0]["value"] == 18.7, "assertion recorded as sent")
    check(clean["evidence_validity"] == "unknown", "a producer claim must not set validity")
    mappings = clean["capture_finalization"]["artifacts"]
    check(len(mappings) == 1 and mappings[0]["key"] == "result" and mappings[0]["position"] == 2, "source mapping")
    cases = {}
    for operation, count in (("begin", 1), ("append", 2), ("attach", 1), ("finalize", 1)):
        for number in range(1, count + 1):
            fault = "%s:%s:%d" % ("drop-response", operation, number)
            report, canon = synthetic(binary, base, "drop-%s-%d" % (operation, number), [fault], False)
            resent = [c for c in report["calls"] if c["operation"] == "capture." + operation]
            check(any(c["result"].startswith("already_") for c in resent), "%s: no replayed receipt" % fault)
            cases[fault] = canon
            fault = "crash-after:%s:%d" % (operation, number)
            _, canon = synthetic(binary, base, "crash-%s-%d" % (operation, number), [fault], True)
            cases[fault] = canon
    for fault, canon in cases.items():
        check(canon == clean, "%s: canonical envelope differs from a clean run" % fault)
    return {"synthetic-run": clean}


def check_kinds(binary, fixtures, base):
    """Replay each producer-kinds envelope through capture; return canonicals."""
    kinds = os.path.join(fixtures, "producer-kinds", "v1")
    found = {}
    for kind in KINDS:
        for variant in VARIANTS:
            directory = os.path.join(kinds, kind)
            with open(os.path.join(directory, variant + ".json"), encoding="utf-8") as handle:
                envelope = producer.strict_loads(handle.read())
            name = "%s-%s" % (kind, variant)
            workspace = os.path.join(base, name)
            init_workspace(binary, workspace, [directory])
            recorder = producer.Recorder(binary, workspace)
            final = producer.record_envelope(recorder, envelope, directory)
            complete = variant == "envelope"
            check(final["finalization"] == ("finalized" if complete else "finalized_partial"), name)
            check(final["execution"] == envelope["execution"]["state"], name + ": execution")
            canon = producer.canonical_run(recorder, recorder.run_id)
            recorded = canon["capture_finalization"]
            check(recorded["assertions"] == [dict(a, validity_claim="producer_reported")
                                              for a in envelope.get("assertions", [])], name + ": assertions")
            check(recorded["limitations"] == envelope.get("limitations", []), name + ": limitations")
            retained = [a for a in recorded["artifacts"] if a["retention"] == "retained"]
            check((len(retained) == len(envelope["artifacts"])) == complete, name + ": artifact retention")
            for artifact in retained:
                check(artifact["digest"].startswith("sha256:") and artifact["size_bytes"] > 0, name + ": mapping")
            check(canon["evidence_validity"] == "unknown", name + ": validity")
            check(recorder.read(["check"]).get("ok") is True, name + ": workspace check")
            found[name] = canon
    return found


def main(args):
    binary = os.path.abspath(args.bin)
    base = os.path.abspath(args.workspace)
    fixtures = os.path.abspath(args.fixtures)
    os.makedirs(base, exist_ok=True)
    canonical = {}
    try:
        check_describe()
        check_capture_discovery(binary)
        check_fixture_requests(binary, fixtures, base)
        canonical.update(check_synthetic(binary, base))
        check_scenarios(binary, base)
        canonical.update(check_kinds(binary, fixtures, base))
    except (Failure, producer.Rejected, KeyError, ValueError, subprocess.CalledProcessError) as error:
        sys.stderr.write("FAIL: %s: %s\n" % (type(error).__name__, error))
        return 1
    if args.canonical_out:
        with open(args.canonical_out, "w", encoding="utf-8") as out:
            json.dump(canonical, out, indent=2, sort_keys=True, allow_nan=False)
            out.write("\n")
    print("PRODUCER-OK %d canonical envelopes" % len(canonical))
    return 0
