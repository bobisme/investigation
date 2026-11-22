#!/usr/bin/env python3
"""Exercise the timeout investigation and the incompatible-build trap through the CLI."""

import argparse
from contextlib import nullcontext
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


ACTOR = "agent:timeout-fixture"
REVIEWER = "agent:timeout-fixture-review"
FIXTURES = Path(__file__).resolve().parents[1] / "fixtures/development/scenario-a-timeout"
VARIABLE = "release_before_await"
ENVIRONMENT = "Synthetic build family build-7c2a, burst-3-40-concurrent, dataset-sim-2, /items/lookup"


def ref(record_id, revision=1):
    return {"id": record_id, "revision": revision}


def op(name, payload, alias=None):
    operation = {"op": name, "payload": payload}
    if alias is not None:
        operation["client_ref"] = alias
    return operation


class Cli:
    def __init__(self, binary, root):
        self.binary, self.root = binary, root
        self.sequence = 0

    def call(self, *args, request=None, exit_code=0):
        child = subprocess.run(
            [self.binary, *args, "--format", "json"],
            cwd=self.root, input=None if request is None else json.dumps(request),
            text=True, capture_output=True, check=False,
        )
        if child.returncode != exit_code:
            raise AssertionError(f"{args}: {child.returncode} != {exit_code}\n{child.stdout}\n{child.stderr}")
        return json.loads(child.stdout)

    def record(self, case, *operations, preconditions=(), actor=ACTOR, exit_code=0):
        self.sequence += 1
        return self.call(
            "record", "--input", "-", exit_code=exit_code, request={
                "schema_version": "inv.command.v0-alpha", "command_id": f"timeout-{self.sequence}",
                "case_id": case, "actor": actor, "preconditions": list(preconditions),
                "operations": operations,
            },
        )

    def create(self, case, name, payload, **kwargs):
        return self.record(case, op(name, payload), **kwargs)["created"][0]["id"]

    def show(self, case, record_id, depth="full"):
        return self.call("--case", case, "show", record_id, "--depth", depth)

    def capture(self, case, directory, filename):
        self.sequence += 1
        source = FIXTURES / directory / "sources" / filename
        target = self.root / filename
        shutil.copyfile(source, target)
        manifest = {
            "schema_version": "inv.ingest.v0-alpha", "command_id": f"capture-{self.sequence}",
            "actor": ACTOR, "case_id": case, "source": {"title": filename, "format": "application/json"},
            "paths": [filename], "extract_text": True,
        }
        request = self.root / "manifest.json"
        request.write_text(json.dumps(manifest), encoding="utf-8")
        item = self.call("ingest", "--input", str(request))["items"][0]
        data = source.read_bytes()
        assert item["digest"] == "sha256:" + hashlib.sha256(data).hexdigest()
        return item, json.loads(data)


def selector(capture, **selection):
    return {"schema_version": "inv.selector.v1", "artifact_digest": capture["digest"],
            "selector": selection}


def run_record(cli, case, capture, intent, environment, variables=(), validity="valid"):
    return cli.create(case, "run.record", {
        "intent": {"kind": "exploration", "intent": intent},
        "producer": "agent:synthetic-fixture", "environment": environment,
        "execution_status": "completed", "evidence_completeness": "complete",
        "evidence_validity": validity, "artifact_refs": [ref(capture["artifact_id"])],
        "evidence_kind": "interventional" if variables else "observational",
        "variables_changed": list(variables),
    })


def measurement(cli, case, capture, run, duration, variables=(), unit="ms", method=None):
    return cli.create(case, "observation.add", {
        "run_ref": ref(run), "value": {"kind": "measurement", "value": str(duration), "unit": unit},
        "basis": {"kind": "source", "artifact_ref": ref(capture["artifact_id"]),
                  "selector": selector(capture, type="whole")},
        "extraction_method": method or "Read the connection-hold interval in the synthetic capture in milliseconds.",
        "validity": "valid", "quality": [], "limitations": ["Synthetic workload only."],
        "evidence_kind": "interventional" if variables else "observational",
        "variables_changed": list(variables),
    })


def quote(cli, case, capture, passage, statement):
    content = (cli.root / "trace-slow.json").read_bytes()
    quoted = passage.encode("utf-8")
    start = content.index(quoted)
    return cli.create(case, "observation.add", {
        "value": {"kind": "text", "statement": statement},
        "basis": {"kind": "source", "artifact_ref": ref(capture["derivative_id"]),
                  "selector": selector(capture, type="byte_range", start=start, end=start + len(quoted)),
                  "text_quote": {"exact": passage}},
        "validity": "valid", "quality": [], "limitations": ["One retained request trace."],
        "evidence_kind": "observational", "variables_changed": [],
    })


def candidate(factor, rival, support, a, b, c):
    return {
        "statement": "Holding the database connection across the asynchronous wait explains the overshoot in these recorded attempts.",
        "scope": "Synthetic build family build-7c2a, sampled burst workload and route only.",
        "significance": "Release the connection before awaiting the callback in this bounded workload.",
        "support_refs": [ref(record) for record in support], "contradiction_refs": [ref(rival)],
        "caveats": ["The scheduler may contribute; the incompatible-build trial cannot isolate its effect.",
                    "These synthetic captures do not establish release behavior in production."],
        "causal_factors": [{"hypothesis_ref": ref(factor), "role": "trigger"}],
        "causal_test": {"factor_ref": ref(factor), "variable": VARIABLE,
                        "baseline": ref(a), "treatment": ref(b), "reversal": ref(c),
                        "failure_boundary": {"operator": "at_least", "value": "25000", "unit": "ms"}},
    }


def expect_review(cli, case, record_id, exit_code=0):
    current = cli.show(case, record_id)["review_basis"]
    preconditions = [current["subject_ref"], *current["read_set"]]
    return cli.record(case, op("conclusion.request_review", {"conclusion_ref": ref(record_id)}),
                      preconditions=preconditions, exit_code=exit_code)


def proof(binary, handoff, workspace=None):
    if workspace is not None:
        workspace.mkdir(parents=True)
    with (nullcontext(workspace) if workspace else tempfile.TemporaryDirectory(prefix="investigation-timeout-")) as directory:
        root = Path(directory)
        cli = Cli(binary, root)
        cli.call("init", ".", "--no-vault")
        config = root / "investigation.toml"
        config.write_text(config.read_text().replace("import_roots = []", 'import_roots = ["."]'))
        case = cli.create(None, "case.create", {"title": "Why does the synthetic lookup stall?", "mode": "explain"})
        question = cli.create(case, "question.create", {
            "wording": "Which factor explains the connection-hold overshoot in the retained workload?",
            "importance": "normal",
        })
        hypo = lambda statement: {"question_ref": ref(question), "statement": statement,
                                   "assumption_refs": [], "rival_refs": [],
                                   "applicability": "Sampled synthetic burst",
                                   "possible_falsifiers": ["The failure persists with the supposed factor isolated."]}
        factor = cli.create(case, "hypothesis.create", hypo("A checked-out connection is held during the asynchronous wait."))
        rival = cli.create(case, "hypothesis.create", hypo("Scheduler delay alone accounts for the connection-hold overshoot."))
        plan = cli.create(case, "inquiry.create", {
            "purpose": "Compare a slow/control pair and the synthetic toggle sequence.",
            "method": "Preserve exact captures and compare the connection hold around the named change.",
            "inquiry": {"mode": "exploratory"},
            "desired_observations": ["Connection hold time and compatibility of captured attempts."],
            "controls": ["Same workload, route, and dataset."], "limits": ["Offline synthetic captures only."],
        })
        cli.record(case, op("inquiry.freeze", {"inquiry_ref": ref(plan)}), preconditions=[ref(plan)])
        slow, trace = cli.capture(case, "baseline", "trace-slow.json")
        fast, control = cli.capture(case, "baseline", "trace-control.json")
        pool, _ = cli.capture(case, "baseline", "pool-telemetry.json")
        fixing, fixed = cli.capture(case, "baseline", "reproduction.json")
        wrong, wrong_data = cli.capture(case, "different-build-trap", "scheduler-only-test.json")
        reversing, reverted = cli.capture(case, "baseline", "revert-run.json")
        broken, _ = cli.capture(case, "broken-clock", "trace-unmapped-clock.json")
        assert all(trace[key] == control[key] for key in ("build_id", "workload_id", "route", "dataset_revision"))
        assert fixed["parent_build_id"] == trace["build_id"] and reverted["parent_build_id"] == fixed["build_id"]
        assert fixed["toggle"][VARIABLE] and not reverted["toggle"][VARIABLE]

        held = quote(cli, case, slow, "callback returned after 28 seconds", "The slow trace records a 28-second callback wait.")
        assert cli.show(case, held)["record"]["body"]["basis"]["text_quote"]["state"] == "verified"
        timing = {event["event"]: event["mono_ms"] for event in trace["events"]}
        interval = timing["db_connection_released"] - timing["db_connection_acquired"]
        base = measurement(cli, case, slow, run_record(cli, case, slow, "Measure the slow capture", ENVIRONMENT), interval)
        treated = measurement(cli, case, fixing, run_record(cli, case, fixing, "Enable release before await", ENVIRONMENT, [VARIABLE]),
                              fixed["max_connection_hold_ms"], [VARIABLE])
        trap_variables = ["bypass_scheduler"]
        trap = measurement(cli, case, wrong, run_record(cli, case, wrong, "Read the scheduler-only capture",
                           wrong_data["build_id"] + " and bypassed callback queue", trap_variables),
                           wrong_data["timeouts"], trap_variables, "count", "Read the reported timeout count.")
        reverse = measurement(cli, case, reversing, run_record(cli, case, reversing, "Revert the named toggle", ENVIRONMENT, [VARIABLE]),
                              reverted["max_connection_hold_ms"], [VARIABLE])
        unusable = run_record(cli, case, broken, "Inspect trace with an unmapped, jumping clock",
                              "Unaligned wall clock", validity="invalid")
        assert cli.show(case, unusable)["record"]["body"]["evidence_validity"] == "invalid"
        assert fast["digest"] != slow["digest"] and pool["digest"] != slow["digest"]

        wrong_claim = cli.create(case, "conclusion.create", candidate(factor, rival, [base], base, trap, reverse))
        before = cli.show(case, wrong_claim)["snapshot"]
        error = expect_review(cli, case, wrong_claim, 5)
        assert error["error"]["code"] == "GATE_FAILED" and error["commit_status"] == "not_committed"
        assert "compatible" in error["error"]["message"]
        assert cli.show(case, wrong_claim)["snapshot"] == before
        assert cli.show(case, wrong_claim)["effective_status"]["state"] == "candidate"
        claim = cli.create(case, "conclusion.create", candidate(factor, rival, [held, base, treated, reverse], base, treated, reverse))
        ready = expect_review(cli, case, claim)["updated"][0]["revision"]
        full = cli.show(case, claim)
        basis = full["review_basis"]
        review = cli.create(case, "review.submit", {
            "kind": "validation", "subject_ref": basis["subject_ref"], "basis_digest": basis["basis_digest"],
            "read_set": basis["read_set"], "materials_examined": [ref(held), ref(base), ref(treated), ref(reverse)],
            "method": "Read the retained captures for the control, toggle, and reversal.",
            "verdict": "supports", "context_isolation": "fresh_context_unblinded",
            "caveats": ["Only the scoped fixture workload was tested."], "shared_dependencies": ["The same offline fixture family."],
            "objection_refs": [], "context": {
                "activity": "Compare independent records of synthetic attempts.",
                "saw_original_conclusion": {"state": "known", "value": True},
                "gathered_new_evidence": {"state": "known", "value": False},
                "method_or_source_changes": [], "dependencies": "shared",
                "dependence_basis": "Shared retained trace and toggle captures.",
            },
        }, actor=REVIEWER)
        promoted = cli.record(case, op("conclusion.validate", {"conclusion_ref": ref(claim, ready), "review_ref": ref(review)}),
                              preconditions=[ref(claim, ready), ref(review), *basis["read_set"]])
        claim_ref = ref(claim, promoted["updated"][0]["revision"])
        point = cli.call("--case", case, "brief")["snapshot"]
        closing = {"outcome": "answered", "reason": "The synthetic fixture isolates the scoped mechanism.",
                   "unresolved_questions": [ref(question)], "reopen_condition": "A comparable scheduler-only toggle changes the result."}
        failure = cli.record(case, op("case.close", dict(closing, retrospective_waived="Attempted waiver")),
                             preconditions=[ref(case)], exit_code=5)
        assert failure["error"]["code"] == "RETROSPECTIVE_REQUIRED"
        retrospective = {
            "case_ref": ref(case), "snapshot": point,
            "methods_attempted": [{"name": "Compare a slow/control pair and a bounded toggle/reversal"}],
            "effective_steps": [{"description": "The compatible toggle and reversal distinguished the connection-hold mechanism.",
                                 "support_refs": [base, treated, reverse]}],
            "invalid_or_misleading_steps": [{"description": "The unmapped clock could not order events.",
                                             "support_refs": [unusable]}],
            "costs_or_constraints": "Offline synthetic captures only.",
            "residual_unknowns": ["The scheduler's independent contribution under a compatible build."],
            "lessons": [], "no_reusable_lesson_reason": "The result is scoped to a synthetic workload.",
        }
        receipt = cli.record(case, op("retrospective.record", retrospective, "methods"),
                             op("case.close", dict(closing, retrospective_ref={"client_ref": "methods"})),
                             preconditions=[ref(case)])
        closed = cli.show(case, case)
        assert closed["record"]["body"]["closure"]["retrospective_ref"]["id"] == receipt["created"][0]["id"]
        brief = cli.call("--case", case, "brief", "--level", "standard")
        handoff.write_text(json.dumps(brief, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        result = subprocess.run(["python3", Path(__file__).with_name("scenario_a_successor.py"), handoff],
                                text=True, capture_output=True, check=True)
        successor = json.loads(result.stdout)
        assert successor["outcome"] == claim_ref and successor["evidence"]["id"].startswith(("art_", "obs_"))

        selected = root / "selector.json"
        selected.write_text(json.dumps(selector(slow, type="json_pointer", pointer="/events/3/text")))
        evidence = cli.call("--case", case, "evidence", "read", slow["artifact_id"], "--selector-file", str(selected))
        assert evidence["excerpt"]["value"] == "callback returned after 28 seconds"
        assert evidence["content_trust"] == "untrusted_evidence"
        small = cli.create(None, "case.create", {"title": "Was a matched production sample available?"})
        cli.record(small, op("case.close", {
            "outcome": "inconclusive", "reason": "No sample was supplied.",
            "unresolved_questions": [], "reopen_condition": "A compatible sample is supplied.",
            "retrospective_waived": "The case ended before any measurement or interpretation.",
        }), preconditions=[ref(small)])
        waiver = cli.show(small, small)["record"]["body"]["closure"]
        assert waiver["retrospective_waived"] and waiver["retrospective_ref"] is None
        assert cli.call("check")["structurally_valid"]
        return {"case": case, "validated": claim_ref, "handoff": str(handoff), "successor": successor}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("handoff", type=Path)
    parser.add_argument("--workspace", type=Path, help="Keep the proof in this new directory.")
    args = parser.parse_args()
    result = proof(args.binary.resolve(strict=True), args.handoff.resolve(), args.workspace.resolve() if args.workspace else None)
    print(json.dumps(result))


if __name__ == "__main__":
    main()
