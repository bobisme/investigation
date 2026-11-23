#!/usr/bin/env python3
"""Exercise the architecture-choice slice through the public binary.

Run after building the binary: python3 scripts/scenario_c_proof.py --bin target/debug/investigation
The optional --handoff path saves the sole input to the successor check.
"""
import argparse
import contextlib
import json
import pathlib
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "fixtures/development/scenario-c-architecture-choice/baseline/sources/recovery-tests.json"
BENCHMARK = SOURCE.with_name("benchmark-samples.json")
ACTOR = "agent:scenario-c"


def ref(identifier, revision=1):
    return {"id": identifier, "revision": revision}


class Driver:
    def __init__(self, binary, workspace):
        self.binary, self.workspace = binary, workspace
        self.counter = 0
        self.created_ids = []

    def call(self, *args, stdin=None, success=True):
        result = subprocess.run(
            [str(self.binary), "--workspace", str(self.workspace), "--format", "json", *map(str, args)],
            input=stdin, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=self.workspace,
        )
        try:
            value = json.loads(result.stdout)
        except ValueError as error:
            raise AssertionError(f"invalid JSON from {args}: {result.stdout!r}, stderr={result.stderr!r}") from error
        if success != (result.returncode == 0 and value.get("ok") is True):
            raise AssertionError(f"unexpected result from {args}: {value}; stderr={result.stderr!r}")
        if value.get("ok"):
            self.created_ids.extend(item["id"] for item in value.get("created", []))
        return value

    def record(self, case, op, payload, success=True):
        self.counter += 1
        envelope = {"schema_version": "inv.command.v1", "command_id": f"scenario-c-{self.counter}",
                    "case_id": case, "actor": ACTOR, "preconditions": [],
                    "operations": [{"op": op, "payload": payload}]}
        return self.call("record", "--input", "-", stdin=json.dumps(envelope).encode(), success=success)

    def batch(self, case, operations, preconditions):
        self.counter += 1
        envelope = {"schema_version": "inv.command.v1", "command_id": f"scenario-c-{self.counter}",
                    "case_id": case, "actor": ACTOR, "preconditions": preconditions, "operations": operations}
        return self.call("record", "--input", "-", stdin=json.dumps(envelope).encode())

    def show(self, case, identifier):
        return self.call("--case", case, "show", identifier, "--depth", "full")

    def brief(self, case):
        return self.call("--case", case, "brief", "--level", "standard", "--max-bytes", "65536")


def created(receipt):
    assert len(receipt["created"]) == 1, receipt
    return receipt["created"][0]["id"]


def add_option(d, case, question, description):
    identifier = created(d.record(case, "option.create", {"question_ref": ref(question),
        "description": description, "dependencies": [], "tradeoff_notes": []}))
    receipt = d.record(case, "option.disposition", {"option_ref": ref(identifier),
        "lifecycle": "eligible", "reason": "Plausible candidate for the stated application"})
    assert receipt["updated"][0]["revision"] == 2
    return ref(identifier, 2)


def criterion(question, name, kind, unit, threshold=None):
    return {"name": name, "kind": kind, "scope": {"kind": "all_options", "question_ref": ref(question)},
            "scale": {"unit": unit, "description": name, "anchors": []}, "preferred_direction": "lower",
            "threshold": threshold, "weighting_rationale": None}


def blindspots(d, case):
    """The read-only inv.audit.v1 response, which has no receipt `ok` field."""
    result = subprocess.run(
        [str(d.binary), "--workspace", str(d.workspace), "--format", "json", "--case", case, "blindspots"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=d.workspace, check=True)
    audit = json.loads(result.stdout)
    assert audit["schema_version"] == "inv.audit.v1", audit
    return audit


def hard_unmeasured(d, case):
    """criterion.hard_unmeasured findings of the current audit, by criterion ID."""
    audit = blindspots(d, case)
    return {entry["subject_refs"][0]["id"]: entry for entry in audit["checks"]
            if entry["check_id"] == "criterion.hard_unmeasured"}


def missing_options(entry):
    return {reference["id"] for fact in entry["facts"] if fact["fact"] == "hard_cell:missing"
            for reference in fact["refs"]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin", required=True, type=pathlib.Path)
    parser.add_argument("--handoff", type=pathlib.Path)
    parser.add_argument("--workspace", type=pathlib.Path, help="Keep a new empty workspace for inspection")
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    temporary = tempfile.TemporaryDirectory(prefix="investigation-scenario-c-") if args.workspace is None else contextlib.nullcontext(None)
    with temporary as tmp:
        workspace = pathlib.Path(tmp) / "workspace" if tmp is not None else args.workspace.resolve()
        init = subprocess.run([str(binary), "init", str(workspace), "--no-vault", "--format", "json"],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        assert json.loads(init.stdout)["ok"] is True
        config = workspace / "investigation.toml"
        configuration = config.read_text(encoding="utf-8")
        assert "import_roots = []" in configuration
        config.write_text(configuration.replace("import_roots = []", 'import_roots = ["."]'), encoding="utf-8")
        d = Driver(binary, workspace)
        case = created(d.record(None, "case.create", {"title": "Queue architecture choice", "mode": "evaluate",
            "objective": "Choose a queue for the synthetic application", "decision_relevance": "Select a maintainable queue with recoverable acknowledged jobs"}))
        question = created(d.record(case, "question.create", {"wording": "Which queue design should the application use?",
            "importance": "high", "context": "Known synthetic workload; no universal winner"}))
        options = [add_option(d, case, question, description) for description in [
            "Retain database-backed queue (do-nothing baseline)", "Adopt managed queue", "Run dedicated broker"]]
        hard_input = criterion(question, "Recovery time after acknowledged enqueue", "hard", "seconds",
            {"kind": "measured", "operator": "at_most", "value": {"decimal": "300", "unit": "seconds"}})
        hard = created(d.record(case, "criterion.create", hard_input))
        loss_input = criterion(question, "No acknowledged job is lost", "hard", None,
            {"kind": "qualitative", "requirement": "Zero acknowledged jobs lost under the specified failure model"})
        loss = created(d.record(case, "criterion.create", loss_input))
        soft = created(d.record(case, "criterion.create", criterion(question, "Operational cost estimate", "soft", "USD/month")))
        latency = created(d.record(case, "criterion.create", criterion(question, "Enqueue latency (small synthetic benchmark)", "soft", "milliseconds")))
        complexity = created(d.record(case, "criterion.create", criterion(question, "Team implementation effort", "soft", "engineer-weeks")))

        # Before the recovery measurement the audit names each hard criterion
        # with no evidence-backed disposition, keyed by the criterion itself.
        unmeasured = hard_unmeasured(d, case)
        assert set(unmeasured) == {hard, loss}, unmeasured
        assert all(entry["subject_refs"] == [ref(key)] and entry["suggested_schema"] == "comparison.add"
                   and missing_options(entry) == {option["id"] for option in options}
                   for key, entry in unmeasured.items()), unmeasured

        # Ingestion uses the selected workspace as an explicit import root; no
        # target code, parser command or network service is executed.
        shutil.copyfile(SOURCE, workspace / "recovery-tests.json")
        manifest = {"schema_version": "inv.ingest.v1", "command_id": "scenario-c-ingest",
            "actor": ACTOR, "case_id": case, "source": {"title": "Synthetic recovery trials",
            "producer": "offline simulator", "identity": "recovery-tests.json", "format": "application/json",
            "verification_limits": ["Synthetic simulator; 30 trials and the stated failure model only"]},
            "paths": ["recovery-tests.json"], "extract_text": True}
        manifest_path = workspace / "ingest.json"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        ingested = d.call("ingest", "--input", manifest_path)
        artifact = ingested["items"][0]["artifact_id"]
        digest = ingested["items"][0]["digest"]
        selector = {"schema_version": "inv.selector.v1", "artifact_digest": digest,
                    "selector": {"type": "json_pointer", "pointer": "/results/database_queue/recovery_p95/value", "parser": None}}
        selector_path = workspace / "selector.json"
        selector_path.write_text(json.dumps(selector), encoding="utf-8")
        excerpt = d.call("--case", case, "evidence", "read", artifact, "--selector-file", selector_path)
        assert excerpt["excerpt"]["availability"] == "json_value" and excerpt["excerpt"]["value"] == 110, excerpt
        assert "untrusted_evidence" in json.dumps(excerpt)
        observation = created(d.record(case, "observation.add", {"value": {"kind": "measurement", "value": "110", "unit": "seconds"},
            "basis": {"kind": "source", "artifact_ref": ref(artifact), "selector": selector, "text_quote": None},
            "extraction_method": "Read recovery_p95.value from retained synthetic trial JSON",
            "validity": "valid", "quality": ["30 synthetic trials"],
            "limitations": ["This value is a p95; the observation schema does not separately attest that statistic"],
            "evidence_kind": "observational", "variables_changed": [], "observed_at": None}))
        comparison = created(d.record(case, "comparison.add", {"option_ref": options[0], "criterion_ref": ref(hard),
            "outcome": "meets", "method": "synthetic recovery trial; p95 reported by source",
            "value": {"decimal": "110", "unit": "seconds"}, "evidence_refs": [observation], "assumption_refs": [],
            "uncertainty": "Only 30 synthetic trials under the stated failure model", "rationale": "110 seconds is below the 300-second threshold"}))
        unknown = created(d.record(case, "comparison.add", {"option_ref": options[0], "criterion_ref": ref(loss),
            "outcome": "unknown", "method": "inspection", "basis": {"kind": "unknown", "reason": "Independent verification of loss is missing"},
            "evidence_refs": [], "assumption_refs": [], "uncertainty": "Reported by one synthetic source", "rationale": "Do not infer a pass"}))
        # A soft unknown must stay unknown; no cost estimate is promoted into a measurement.
        soft_unknown = created(d.record(case, "comparison.add", {"option_ref": options[0], "criterion_ref": ref(soft),
            "outcome": "unknown", "method": "inspection", "basis": {"kind": "unknown", "reason": "Traffic and cost are estimates"},
            "evidence_refs": [], "assumption_refs": [], "uncertainty": "No measured production traffic", "rationale": "No measured cost"}))
        other_comparisons = []
        for option in options:
            other_comparisons.append(created(d.record(case, "comparison.add", {"option_ref": option, "criterion_ref": ref(complexity),
                "outcome": "unknown", "method": "review of synthetic planning interview",
                "basis": {"kind": "unknown", "reason": "Team-effort figures are unverified estimates"},
                "evidence_refs": [], "assumption_refs": [], "uncertainty": "No measured migration effort", "rationale": "Author preference is not a measured result"})))
        for option, label in zip(options[1:], ["managed queue", "dedicated broker"]):
            other_comparisons.append(created(d.record(case, "comparison.add", {"option_ref": option, "criterion_ref": ref(hard),
                "outcome": "unknown", "method": "review of recovery trial", "basis": {"kind": "unknown", "reason": f"{label} lacks the required recovery measurement"},
                "evidence_refs": [], "assumption_refs": [], "uncertainty": "Required recovery time was not measured", "rationale": "Unknown is not a pass"})))
            other_comparisons.append(created(d.record(case, "comparison.add", {"option_ref": option, "criterion_ref": ref(loss),
                "outcome": "unknown", "method": "review of recovery trial", "basis": {"kind": "unknown", "reason": "The full failure model is not independently established"},
                "evidence_refs": [], "assumption_refs": [], "uncertainty": "No independent verification", "rationale": "Unknown is not a pass"})))
            other_comparisons.append(created(d.record(case, "comparison.add", {"option_ref": option, "criterion_ref": ref(soft),
                "outcome": "unknown", "method": "inspection", "basis": {"kind": "unknown", "reason": "Production cost is not measured"},
                "evidence_refs": [], "assumption_refs": [], "uncertainty": "Traffic estimate is unverified", "rationale": "Estimated cost is not measured cost"})))
        shutil.copyfile(BENCHMARK, workspace / "benchmark-samples.json")
        benchmark_manifest = {"schema_version": "inv.ingest.v1", "command_id": "scenario-c-benchmark", "actor": ACTOR,
            "case_id": case, "source": {"title": "Synthetic enqueue benchmark", "producer": "offline simulator",
            "identity": "benchmark-samples.json", "format": "application/json", "verification_limits": ["Only twelve samples per option"]},
            "paths": ["benchmark-samples.json"], "extract_text": True}
        manifest_path.write_text(json.dumps(benchmark_manifest), encoding="utf-8")
        bench = d.call("ingest", "--input", manifest_path)["items"][0]
        latency_comparisons = []
        for option, key, value in zip(options, ["database_queue", "managed_queue", "dedicated_broker"], ["42", "35", "31"]):
            bench_selector = {"schema_version": "inv.selector.v1", "artifact_digest": bench["digest"],
                "selector": {"type": "json_pointer", "pointer": f"/reported_p99/{key}/value", "parser": None}}
            obs = created(d.record(case, "observation.add", {"value": {"kind": "measurement", "value": value, "unit": "milliseconds"},
                "basis": {"kind": "source", "artifact_ref": ref(bench["artifact_id"]), "selector": bench_selector, "text_quote": None},
                "extraction_method": "Read reported p99 from retained benchmark JSON", "validity": "valid",
                "quality": ["Twelve synthetic samples"], "limitations": ["A small sample, not production traffic"],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": None}))
            latency_comparisons.append(created(d.record(case, "comparison.add", {"option_ref": option, "criterion_ref": ref(latency),
                "outcome": "partially_meets", "method": "Synthetic benchmark reports nearest-rank p99",
                "value": {"decimal": value, "unit": "milliseconds", "statistic": "p99", "sample_count": 12},
                "evidence_refs": [obs], "assumption_refs": [], "uncertainty": "Twelve samples do not establish production p99",
                "rationale": "Recorded as a scoped comparison, not a universal ranking"})))
        assumption = created(d.record(case, "assumption.create", {"claim": "Traffic stays below 2500000 jobs per day",
            "consequence": "The estimated cost ordering may change above this level", "materiality": "material", "affected_refs": []}))
        decision_payload = {"question_ref": ref(question), "choice": {"kind": "select", "option_ref": options[0]},
            "compared_alternatives": options, "comparison_refs": [ref(x) for x in [comparison, unknown, soft_unknown, *other_comparisons, *latency_comparisons]],
            "hard_conditions": [
                {"criterion_ref": ref(hard), "reason": "Independently verify the reported p95 and failure model"},
                {"criterion_ref": ref(loss), "reason": "Independently verify no acknowledged job is lost"}],
            "evidence_refs": [ref(observation)], "assumption_refs": [ref(assumption)],
            "tradeoffs": ["The benchmark suggests higher enqueue latency; costs are estimates"],
            "consequences": ["Retain the existing database queue pending the hard-condition check"],
            "reopen_trigger": "Retest recovery if the threshold changes or traffic exceeds 2500000 jobs/day",
            "expected_outcome": "A maintainable queue for the stated workload", "check_when": "After an independent failure test",
            "reversal_conditions": [{"assumption_ref": ref(assumption), "criterion_ref": ref(soft),
                "comparison_ref": ref(soft_unknown), "option_ref": options[1],
                "condition": "A peak-traffic cost measurement favors the managed queue"}]}
        decision = created(d.record(case, "decision.propose", decision_payload))
        # Review requires an explicit digest and closure. Read-only discovery is
        # used here; the command must expose the real current basis to reviewers.
        decision_view = d.show(case, decision)
        review_basis = decision_view.get("review_basis") or decision_view.get("record", {}).get("review_basis")
        if not review_basis:
            raise AssertionError("Public decision view lacks review_basis with digest and required read_set")
        review_payload = {"kind": "validation", "subject_ref": ref(decision), "basis_digest": review_basis["basis_digest"],
            "read_set": review_basis["read_set"], "method": "Inspect the synthetic recovery evidence and unresolved constraints",
            "materials_examined": [ref(observation)], "shared_dependencies": [], "objection_refs": [],
            "verdict": "supports", "caveats": ["No independent proof of zero job loss", "Cost and peak traffic remain uncertain"],
            "context_isolation": "fresh_context_unblinded", "context": {"saw_original_conclusion": {"state": "unknown"},
            "gathered_new_evidence": {"state": "unknown"}, "method_or_source_changes": [], "activity": "Review synthetic choice",
            "dependencies": "unknown", "dependence_basis": "One synthetic evidence source"}}
        review = created(d.record(case, "review.submit", review_payload))
        unsafe_payload = dict(decision_payload)
        unsafe_payload["hard_conditions"] = []
        unsafe_decision = created(d.record(case, "decision.propose", unsafe_payload))
        unsafe_basis = d.show(case, unsafe_decision)["review_basis"]
        unsafe_review_payload = dict(review_payload)
        unsafe_review_payload.update({"subject_ref": ref(unsafe_decision), "basis_digest": unsafe_basis["basis_digest"],
                                      "read_set": unsafe_basis["read_set"]})
        unsafe_review = created(d.record(case, "review.submit", unsafe_review_payload))
        before = d.brief(case)["snapshot"]
        unsafe_accept = d.record(case, "decision.accept", {"decision_ref": ref(unsafe_decision),
            "review_ref": ref(unsafe_review)}, success=False)
        assert "hard_conditions" in unsafe_accept["error"].get("path", ""), unsafe_accept
        assert unsafe_accept["error"].get("repair"), unsafe_accept
        assert d.brief(case)["snapshot"] == before
        # A late criterion is a phantom in the reviewed set. Rejection must be
        # repairable and must leave the canonical snapshot unchanged.
        late = created(d.record(case, "criterion.create", criterion(question, "Temporary hard requirement", "hard", None,
            {"kind": "qualitative", "requirement": "Document incident response ownership"})))
        before = d.brief(case)["snapshot"]
        rejected = d.record(case, "decision.accept", {"decision_ref": ref(decision), "review_ref": ref(review)}, success=False)
        assert rejected["error"].get("repair"), rejected
        assert d.brief(case)["snapshot"] == before
        d.record(case, "criterion.retire", {"criterion_ref": ref(late), "reason": "Temporary phantom test is complete"})
        accepted = d.record(case, "decision.accept", {"decision_ref": ref(decision), "review_ref": ref(review)})
        assert accepted["updated"][0]["revision"] == 2
        # After acceptance the audit still shows what the conditional choice
        # rests on: unknown hard cells and the untested traffic assumption.
        audit = blindspots(d, case)
        assert set(hard_unmeasured(d, case)) == {hard, loss}, audit
        untested = [entry for entry in audit["checks"] if entry["check_id"] == "promoted.material_assumption_unsupported"]
        assert len(untested) == 1 and untested[0]["subject_refs"] == [ref(decision, 2)], audit
        assert {"fact": "assumption_status:unassessed", "refs": [ref(assumption)]} in untested[0]["facts"], untested
        frontier = d.show(case, question)["pareto"]
        assert {entry["option_ref"]["id"] for entry in frontier} == {option["id"] for option in options}, frontier
        assert all(entry["status"] == "uncertain_frontier" for entry in frontier), frontier
        handoff = d.brief(case)
        assert handoff.get("ok") is True
        # Start a fresh process with only the brief, not the workspace or chat.
        saved = workspace / "successor-brief.json"
        saved.write_text(json.dumps(handoff), encoding="utf-8")
        successor_check = subprocess.run(["python3", str(ROOT / "scripts/scenario_c_successor.py"), str(saved)],
                                         capture_output=True, check=True)
        successor_answer = json.loads(successor_check.stdout)
        assert "conditional" in successor_answer["conclusion"] and "loss" in successor_answer["gap"]
        assert ref(observation) in successor_answer["observation_refs"] and ref(artifact) in successor_answer["artifact_refs"], successor_answer
        if args.handoff:
            args.handoff.write_text(json.dumps(handoff, indent=2) + "\n", encoding="utf-8")
        strict = dict(hard_input)
        strict["threshold"] = {"kind": "measured", "operator": "at_most", "value": {"decimal": "60", "unit": "seconds"}}
        d.record(case, "criterion.amend", {"criterion_ref": ref(hard), "reason": "Stricter synthetic recovery requirement", "replacement": strict})
        after = d.show(case, decision)
        assert after["effective_status"]["state"] == "needs_review", after
        comparison_after = d.show(case, comparison)
        assert comparison_after["effective_status"]["state"] == "needs_review", comparison_after
        again = d.call("--case", case, "evidence", "read", artifact, "--selector-file", selector_path)
        assert again["excerpt"]["value"] == 110, again
        replacement = dict(decision_payload)
        replacement["comparison_refs"] = [ref(x) for x in [unknown, soft_unknown, *other_comparisons, *latency_comparisons]]
        replacement["hard_conditions"] = [
            {"criterion_ref": ref(hard, 2), "reason": "Reassess 110 seconds against the stricter 60-second limit"},
            {"criterion_ref": ref(loss), "reason": "Independently verify no acknowledged job is lost"}]
        superseded = d.record(case, "decision.supersede", {"decision_ref": ref(decision, 2),
            "reason": "Recovery requirement changed; preserve the original decision", "replacement": replacement})
        successor = created(superseded)
        assert any(item["id"] == decision for item in superseded["updated"]), superseded
        assert successor != decision
        closure_snapshot = d.brief(case)["snapshot"]
        case_revision = d.show(case, case)["record"]["header"]["revision"]
        retrospective = {"case_ref": ref(case, case_revision), "snapshot": closure_snapshot,
            "methods_attempted": [{"name": "Inspect synthetic recovery trials and bounded enqueue samples"}],
            "effective_steps": [{"description": "Retained and checked the recovery-time source", "support_refs": [observation]}],
            "invalid_or_misleading_steps": [], "none_observed_reason": "No invalid test result was promoted",
            "costs_or_constraints": "Synthetic data and twelve latency samples per option",
            "residual_unknowns": ["Independent zero-loss verification and peak-traffic costs remain unavailable",
                                  "The revised 60-second threshold requires a fresh comparison"],
            "lessons": [], "no_reusable_lesson_reason": "Synthetic evidence alone does not establish a transferable method"}
        closed = d.batch(case, [
            {"op": "retrospective.record", "client_ref": "account", "payload": retrospective},
            {"op": "case.close", "payload": {"outcome": "inconclusive",
                "reason": "The stricter requirement invalidated the earlier choice; the replacement remains proposed",
                "unresolved_questions": [ref(question)], "reopen_condition": "A reviewed comparison addresses the 60-second threshold",
                "retrospective_ref": {"client_ref": "account"}}}], [ref(case, case_revision)])
        assert len(closed["created"]) == 1 and closed["created"][0]["id"].startswith("retro_"), closed
        closed_case = d.show(case, case)["record"]["body"]
        assert closed_case["lifecycle"] == "closed" and closed_case["closure_outcome"] == "inconclusive", closed_case
        assert not any(identifier.startswith(("hyp_", "inq_", "concl_")) for identifier in d.created_ids), d.created_ids
        assert d.call("check")["ok"] is True
        print(json.dumps({"ok": True, "scenario": "C", "case_id": case, "decision_id": decision, "workspace": str(workspace) if args.workspace else None,
                          "handoff": "saved" if args.handoff else "checked", "after_threshold": "needs_review",
                          "case_outcome": "inconclusive", "retrospective": "recorded"}))


if __name__ == "__main__":
    main()
