#!/usr/bin/env python3
"""Exercise the Scenario B latency-regression family through the public binary.

Run after building the binary:
    python3 scripts/scenario_b_proof.py --bin target/debug/investigation

Every run of every variant is ingested as its inv.producer-run.v1 envelope,
recorded as an Observation carrying the typed measurement block, and judged
with measurement.assess. Each named property in each variant's expected.json is
then evaluated against what the binary recorded and reports. The script itself
only recomputes nearest-rank percentiles, quarter means and fractions from the
retained producer bytes, in integer tenths of a millisecond, to check that the
recorded values are the supplied ones; it never passes a computed statistic to
the core. Nothing here enables a performance-specific flag or mode.
"""

import argparse
import contextlib
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
FAMILY = ROOT / "fixtures/development/scenario-b-latency-regression"
VARIANTS = ["baseline", "negative-short-window", "sequential-order", "closed-loop-omission",
            "missing-outcome", "practical-equivalence"]
ACTOR = "agent:scenario-b"
REVIEWER = "agent:scenario-b-review"
FORBIDDEN_KEYS = ("p_value", "power", "confidence_interval")
# Fixture-only conditions; the core has no flag for them. The investigator
# records each as a named limit of an `invalid` measurement judgment.
MONITOR_FAILED = "fixture.monitor_self_check_failed"
ENVIRONMENT = {key: value for key, value in os.environ.items() if key not in ("AGENT", "FORMAT")}
WINDOW_SHORT = "fixture.observation_window_below_intended"


def ref(identifier, revision=1):
    return {"id": identifier, "revision": revision}


def op(name, payload, alias=None):
    operation = {"op": name, "payload": payload}
    if alias is not None:
        operation["client_ref"] = alias
    return operation


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def t10(text):
    """A decimal string with at most one fractional digit, in integer tenths."""
    negative = text.startswith("-")
    whole, _, fraction = text.lstrip("-").partition(".")
    assert len(fraction) <= 1, text
    value = int(whole) * 10 + int(fraction or 0)
    return -value if negative else value


def fmt(tenths):
    return f"{tenths // 10}.{tenths % 10}"


def nearest_rank(values, percent):
    ordered = sorted(values)
    return ordered[(percent * len(ordered) + 99) // 100 - 1]


class Cli:
    def __init__(self, binary, root):
        self.binary, self.root = binary, root
        self.sequence = 0
        self.outputs = []
        self.inputs = []

    def call(self, *args, request=None, exit_code=0):
        if request is not None:
            self.inputs.append(json.dumps(request))
        child = subprocess.run(
            [str(self.binary), "--workspace", str(self.root), *map(str, args), "--format", "json"],
            cwd=self.root, input=None if request is None else json.dumps(request),
            text=True, capture_output=True, check=False, env=ENVIRONMENT,
        )
        if exit_code is not None and child.returncode != exit_code:
            raise AssertionError(f"{args}: exit {child.returncode} != {exit_code}\n{child.stdout}\n{child.stderr}")
        self.outputs.append(child.stdout)
        return json.loads(child.stdout)

    def record(self, case, *operations, preconditions=(), actor=ACTOR, exit_code=0):
        self.sequence += 1
        return self.call("record", "--input", "-", exit_code=exit_code, request={
            "schema_version": "inv.command.v1", "command_id": f"scenario-b-{self.sequence}",
            "case_id": case, "actor": actor, "preconditions": list(preconditions),
            "operations": list(operations),
        })

    def create(self, case, name, payload, **kwargs):
        receipt = self.record(case, op(name, payload), **kwargs)
        assert len(receipt["created"]) == 1, receipt
        return receipt["created"][0]["id"]

    def show(self, case, record_id, depth="full"):
        return self.call("--case", case, "show", record_id, "--depth", depth)

    def current(self, case, record_id):
        return ref(record_id, self.show(case, record_id)["record"]["header"]["revision"])


class Run:
    """One fixture run as recorded: its Run, retained output and Observation."""

    def __init__(self, variant, spec):
        self.variant, self.name = variant, spec["id"]
        self.spec = spec
        base = FAMILY / variant
        self.envelope_path = base / spec["envelope"]
        self.envelope = load(self.envelope_path)
        self.measurement = load(base / spec["measurement"])
        self.output_path = base / spec["output"]
        self.output = load(self.output_path)
        self.run = self.artifact = self.digest = self.observation = None
        self.receipt = None
        self.retained = None


def ingest(cli, case, run):
    """Ingest the producer envelope; its declared artifact resolves under the workspace."""
    shutil.copyfile(run.output_path, cli.root / "output.json")
    receipt = cli.call("--case", case, "--actor", ACTOR, "ingest", "--input", run.envelope_path)
    assert receipt["ok"] is True and receipt["case_id"] == case, receipt
    assert receipt["run_outcome"] == "imported", receipt
    # Process exit is not measurement validity: exit 0 and `completed` leave validity unknown.
    assert run.envelope["execution"]["exit_code"] == 0
    assert receipt["execution"] == "completed" and receipt["measurement_validity"] == "unknown", receipt
    assert receipt["evidence_completeness"] == "complete", receipt
    (item,) = receipt["artifacts"]
    assert item["outcome"] == "imported" and item["role"] == "original-output", item
    assert item["digest"] == "sha256:" + run.spec["sha256"]["output"], item
    run.receipt = receipt
    run.run = receipt["run"]
    run.artifact, run.digest = item["artifact_id"], item["digest"]
    return run


def selector(run, pointer=None):
    chosen = {"type": "whole"} if pointer is None else {"type": "json_pointer", "pointer": pointer, "parser": None}
    return {"schema_version": "inv.selector.v1", "artifact_digest": run.digest, "selector": chosen}


def retained_output(cli, case, run):
    """The producer output as retained by the store, read back in bounded excerpts."""
    if run.retained is None:
        size, chunk, text = run.receipt["artifacts"][0]["size_bytes"], 32 * 1024, ""
        path = cli.root / "selector.json"
        for start in range(0, size, chunk):
            path.write_text(json.dumps({"schema_version": "inv.selector.v1", "artifact_digest": run.digest,
                "selector": {"type": "byte_range", "start": start, "end": min(size, start + chunk)}}), encoding="utf-8")
            excerpt = cli.call("--case", case, "evidence", "read", run.artifact, "--selector-file", path)
            assert excerpt["content_trust"] == "untrusted_evidence", excerpt
            assert excerpt["excerpt"]["availability"] == "text", excerpt
            text += excerpt["excerpt"]["text"]
        assert "sha256:" + hashlib.sha256(text.encode("utf-8")).hexdigest() == run.digest
        run.retained = json.loads(text)
    return run.retained


def read_evidence(cli, case, run, pointer):
    """One value of the retained producer output, by JSON pointer."""
    value = retained_output(cli, case, run)
    for part in pointer.strip("/").split("/"):
        value = value[int(part)] if isinstance(value, list) else value[part]
    return value


def observe(cli, case, run, kind, variables):
    """Record the producer's reported p99 with the typed measurement block."""
    (assertion,) = run.envelope["assertions"]
    payload = {
        "run_ref": run.run,
        "value": {"kind": "measurement", "value": assertion["value"], "unit": assertion["unit"]},
        "basis": {"kind": "source", "artifact_ref": ref(run.artifact), "selector": selector(run), "text_quote": None},
        "extraction_method": "Producer-reported nearest-rank p99 of the retained capture, checked against the retained samples",
        "validity": "unknown", "quality": [], "limitations": list(run.envelope["limitations"]),
        "evidence_kind": kind, "variables_changed": list(variables), "measurement": run.measurement,
    }
    receipt = cli.record(case, op("observation.add", payload))
    run.observation = receipt["created"][0]["id"]
    return receipt


def ids(references):
    return [reference["id"] for reference in references]


def flag_ids(view):
    return sorted(flag["flag_id"] for flag in view["measurement_flags"]["flags"])


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin", required=True, type=pathlib.Path)
    parser.add_argument("--workspace", type=pathlib.Path, help="Keep the proof in this new directory.")
    args = parser.parse_args()
    binary = args.bin.resolve(strict=True)
    temporary = (tempfile.TemporaryDirectory(prefix="investigation-scenario-b-") if args.workspace is None
                 else contextlib.nullcontext(None))
    with temporary as tmp:
        workspace = pathlib.Path(tmp) / "workspace" if tmp is not None else args.workspace.resolve()
        result = proof(binary, workspace)
        result["workspace"] = str(workspace) if args.workspace else None
        print(json.dumps(result))


def judge(cli, case, run, validity, rationale, limits, verification=None):
    """Record one attributed measurement.assess judgment and return the new revision."""
    if verification is None:
        verification = [{"kind": "instrument_check", "evidence_ref": ref(run.receipt["envelope"]["artifact_id"]),
                         "rationale": "The retained producer envelope names the harness and monitor and reports its own controls."}]
    current = ref(run.observation)  # each Observation is judged once, at its first revision
    receipt = cli.record(case, op("measurement.assess", {
        "observation_ref": current, "channel": None, "validity": validity,
        "verification": verification, "rationale": rationale, "limits": limits,
    }))
    (updated,) = receipt["updated"]
    assert updated["id"] == run.observation and updated["revision"] == current["revision"] + 1, receipt
    return ref(run.observation, updated["revision"])


def fixture_conditions(run, recorded_measurement):
    """The fixture-only conditions, derived from the producer's recorded controls
    and the manifest's intended window against the recorded window."""
    conditions = []
    if any(control["producer_result"] == "reported_fail" for control in run.envelope["controls"]):
        conditions.append(MONITOR_FAILED)
    window = recorded_measurement.get("observation_window", {})
    intended = run.spec.get("intended_observation_window_s")
    if intended is not None and window.get("state") == "known":
        assert window["value"]["unit"] == "s"
        if int(window["value"]["value"]) < intended:
            conditions.append(WINDOW_SHORT)
    return conditions


def current_limits(view):
    """Named conditions in the current judgments' limits (`ID: text`)."""
    validity = view["record"]["body"].get("measurement_assessment", {"judgments": []})
    return sorted({limit.split(":", 1)[0] for judgment in validity["judgments"] for limit in judgment["limits"]
                   if limit.startswith("fixture.")})


def audit(cli, case):
    result = cli.call("--case", case, "blindspots")
    assert result["schema_version"] == "inv.audit.v1", result
    return result


def findings(result, check_id, subject=None):
    return [entry for entry in result["checks"] if entry["check_id"] == check_id
            and (subject is None or subject in {item["id"] for item in entry["subject_refs"]})]


def gate_codes(receipt):
    actual = receipt["error"].get("actual") or {}
    return sorted({item["code"] for reason in actual.get("reasons", []) for item in reason.get("missing", [])})


def request_review(cli, case, conclusion, exit_code=0):
    basis = cli.show(case, conclusion)["review_basis"]
    return cli.record(case, op("conclusion.request_review", {"conclusion_ref": basis["subject_ref"]}),
                      preconditions=[basis["subject_ref"], *basis["read_set"]], exit_code=exit_code)


def review_context(activity):
    return {"saw_original_conclusion": {"state": "known", "value": True},
            "gathered_new_evidence": {"state": "known", "value": False},
            "method_or_source_changes": [], "activity": activity, "dependencies": "shared",
            "dependence_basis": "The same retained synthetic captures the investigator read."}


DOWNGRADE = {"validity": "ok", "consistency": "concern", "directness": "concern",
             "precision": "ok", "completeness": "not_assessed"}


DECIMAL = re.compile(r"(?<![\w.])-?\d+\.\d+(?![\w.])")
# The miss probability of the order-statistic rule, stated in its flag text. It
# is a fixed rule parameter, not a value computed from the case.
RULE_CONSTANTS = {"0.05"}


def shown(ctx, identifier):
    """The current full view of a record; the case is closed and no longer changes."""
    if identifier not in ctx["views"]:
        ctx["views"][identifier] = ctx["cli"].show(ctx["case"], identifier)
    return ctx["views"][identifier]


def obs_views(ctx, variant, name):
    """Current views of the Observations recorded for one run."""
    run = ctx["runs"][variant][name]
    identifiers = list(run.bounds.values()) if variant == "practical-equivalence" else [run.observation]
    return [shown(ctx, identifier) for identifier in identifiers]


def obs_view(ctx, variant, name):
    (view,) = obs_views(ctx, variant, name)
    return view


def run_view(ctx, variant, name):
    return shown(ctx, ctx["runs"][variant][name].run["id"])["record"]["body"]


def stored(view):
    return view["record"]["body"]["measurement"]


def stored_samples(measurement):
    samples = measurement["samples"]
    if samples["shape"] == "flat":
        return [t10(value) for value in samples["values"]]
    ordered = sorted(samples["samples"], key=lambda sample: sample["order"]["value"])
    return [t10(sample["value"]) for sample in ordered]


def output_latencies(ctx, run, intended=False):
    """Latencies from the retained producer bytes, read back through the binary."""
    cli, case = ctx["cli"], ctx["case"]
    if "latency_ms" in run.output:
        return [t10(value) for value in read_evidence(cli, case, run, "/latency_ms")]
    requests = read_evidence(cli, case, run, "/requests")
    if requests and "done_ms" in requests[0]:
        start = "intended_send_ms" if intended else "actual_send_ms"
        return [t10(item["done_ms"]) - t10(item[start]) for item in requests]
    return [t10(item["latency_ms"]) for item in requests if item["latency_ms"] is not None]


def derived_flags(view):
    """Core advisory flag IDs plus the fixture conditions named in the current judgments."""
    return sorted(set(flag_ids(view)) | set(current_limits(view)))


def pair_flags(view):
    return sorted(flag["flag_id"] for flag in view["measurement_flags"]["flags"] if len(flag["related_refs"]) == 2)


def assessment_view(ctx, variant, names):
    key = ctx["assessments"][(variant, *names)]
    return ctx["assessed"][key]


def evaluate(ctx, variant, prop):
    kind, where = prop["assert"], f"{variant}/{prop['id']}"
    runs = ctx["runs"][variant]
    if kind == "nearest_rank_p99":
        for name, value in prop["per_run"].items():
            view, run = obs_view(ctx, variant, name), runs[name]
            samples = stored_samples(stored(view))
            assert output_latencies(ctx, run) == samples, f"{where}: {name}: retained output and recorded samples differ"
            assert fmt(nearest_rank(samples, 99)) == value, where
            recorded = view["record"]["body"]["value"]
            assert recorded == {"kind": "measurement", "value": value, "unit": prop["unit"]}, (where, recorded)
            (assertion,) = run_view(ctx, variant, name)["producer_run"]["assertions"]
            assert assertion["value"] == value and assertion["validity_claim"] == "producer_reported", (where, assertion)
    elif kind == "comparable_pair":
        first, second = (stored(obs_view(ctx, variant, name)) for name in prop["runs"])
        environments = [run_view(ctx, variant, name)["producer_run"]["environment"] for name in prop["runs"]]
        for field in prop["same"]:
            if field == "workload_mix":
                assert environments[0][field] == environments[1][field], (where, field)
            else:
                assert first[field] == second[field], (where, field)
        for component in prop["differs"]:
            values = [measurement["environment_identity"]["value"][component] for measurement in (first, second)]
            assert values[0] != values[1], (where, component)
        # Only the varied revision differs between the two Runs' environments.
        assert {key for key in environments[0] if environments[0][key] != environments[1][key]} == {"revision"}, where
        view = assessment_view(ctx, variant, prop["runs"])
        assert pair_flags(view) == [], (where, view["measurement_flags"])
        # The varied keys are named in variables_changed, so the audit does not
        # report the matched pair as recorded under different conditions.
        subject = ctx[ctx["assessments"][(variant, *prop["runs"])]]
        assert not findings(ctx["audit"], "comparability.environment_differs", subject), where
    elif kind == "p99_greater":
        greater, lesser = (obs_view(ctx, variant, name)["record"]["body"]["value"]["value"] for name in (prop["greater"], prop["lesser"]))
        assert t10(greater) - t10(lesser) >= t10(prop["min_difference_ms"]), where
        bounded = shown(ctx, ctx["bounded"])["record"]["body"]
        assert bounded["status"] == "review_ready" and "ledgerd-mixed-requests" in bounded["scope"], bounded
        assert shown(ctx, ctx["causal"])["record"]["body"]["status"] == "candidate"
    elif kind == "run_confounded":
        view = obs_view(ctx, variant, prop["run"])
        assert prop["reason"] in flag_ids(view) and view["measurement_validity"]["state"] == "invalid", where
        same = stored(obs_view(ctx, variant, prop["same_build_as"]))
        assert stored(view)["environment_identity"] == same["environment_identity"], where
        higher = obs_view(ctx, variant, prop["p99_higher_than"])["record"]["body"]["value"]["value"]
        assert t10(view["record"]["body"]["value"]["value"]) > t10(higher), where
        pooled = ctx["pooled"]
        assert pooled["commit_status"] == "not_committed" and pooled["error"]["path"].endswith("/evidence_refs/1"), pooled
        assert prop["must_not_pool_with"] == [prop["same_build_as"]]
    elif kind == "recorded_field_differs":
        values = [int(stored(obs_view(ctx, variant, name))[prop["field"]]["value"]["value"]) for name in prop["runs"]]
        assert values == prop["values_s"], (where, values)
        assert f"measurement.compared_{prop['field']}_differs" in pair_flags(assessment_view(ctx, variant, prop["runs"])), where
    elif kind == "environment_differs":
        values = [run_view(ctx, variant, name)["producer_run"]["environment"][prop["field"]] for name in prop["runs"]]
        assert values == prop["values"] and values[0] != values[1], (where, values)
        (finding,) = findings(ctx["audit"], "comparability.environment_differs", ctx["naive"])
        assert "run_environment_differs" in {fact["fact"] for fact in finding["facts"]}, finding
    elif kind == "comparability_limits_conclusion":
        assert prop["conclusion_ceiling"] == "bounded_to_workload_and_unresolved_for_effect", where
        current = shown(ctx, ctx["naive"])
        assert current["record"]["body"]["status"] == "superseded", current
        successor = shown(ctx, current["record"]["body"]["superseded_by"]["id"])["record"]["body"]
        assert successor["relation"] == "inconclusive" and successor["status"] == "current", successor
        cited = {obs["id"] for obs in successor["evidence_refs"]}
        assert cited == {ctx["runs"][variant][name].observation for name in prop["runs"]}, where
        assert {"warmup", "workload_mix"} == set(prop["reasons"])
    elif kind == "exit_does_not_imply_validity":
        for name in prop["runs_exit_zero"]:
            body = run_view(ctx, variant, name)
            assert body["producer_run"]["execution"]["exit_code"] == 0 and body["execution_status"] == "completed", where
            assert body["evidence_validity"] == "unknown", (where, name)
        assert obs_view(ctx, variant, prop["run_not_valid"])["measurement_validity"]["state"] == "invalid", where
    elif kind == "derived_flags_equal":
        assert set(prop["per_run"]) == set(runs), where
        for name, flags in prop["per_run"].items():
            for view in obs_views(ctx, variant, name):
                assert derived_flags(view) == sorted(flags), (where, name, derived_flags(view), sorted(flags))
    elif kind == "derived_pair_flags_equal":
        for pair in prop["pairs"]:
            assert pair_flags(assessment_view(ctx, variant, pair["runs"])) == sorted(pair["flags"]), (where, pair)
    elif kind == "statistic_recorded":
        for name in prop["runs"]:
            statistic = stored(obs_view(ctx, variant, name))["statistic"]["value"]
            assert (statistic["kind"], statistic["quantile"], statistic["population"]["value"]) == (
                prop["kind"], prop["quantile"], prop["population"]), (where, statistic)
            assert not [flag for flag in flag_ids(obs_view(ctx, variant, name)) if "quantile" in flag], where
    elif kind == "equality_claim_forbidden":
        for name in prop["runs"]:
            view = obs_view(ctx, variant, name)
            assert set(prop["reasons"]) <= set(derived_flags(view)), (where, name)
            assert view["measurement_validity"]["state"] == "invalid", where
        refusal = ctx["absence_refusal"]
        assert refusal["error"]["actual"]["ceiling"] == prop["conclusion_ceiling"], refusal
        assert {"absence.claim_without_basis", "absence.instrument_invalid",
                "absence.observation_window_below_minimum"} <= set(ctx["absence_codes"]), ctx["absence_codes"]
        assert shown(ctx, ctx["no_effect"])["record"]["body"]["status"] == "candidate"
    elif kind == "inputs_contain_no_key":
        for run in runs.values():
            text = json.dumps([run.envelope, run.measurement, run.output])
            assert not any(f'"{key}"' in text for key in prop["keys"]), where
        for output in ctx["cli"].outputs:
            assert not any(f'"{key}"' in output for key in prop["keys"]), (where, output[:200])
    elif kind == "negative_result_retained":
        for name in prop["runs"]:
            view = obs_view(ctx, variant, name)
            assert view["record"]["body"].get("status") != "retracted", where
            assert view["record"]["body"]["value"]["value"] == runs[name].envelope["assertions"][0]["value"], where
        negative = shown(ctx, ctx["negative"])["record"]["body"]
        assert negative["status"] == "current" and negative["relation"] == "inconclusive", negative
        assert {ref_["id"] for ref_ in negative["evidence_refs"]} == {runs[name].observation for name in prop["runs"]}
        assert f"assessment.inconclusive/{ctx['negative']}" in ctx["digest_items"]
    elif kind == "flag_present":
        for name in prop["runs"]:
            assert prop["flag"] in flag_ids(obs_view(ctx, variant, name)), (where, name)
    elif kind == "order_effect_visible":
        run = runs[prop["run"]]
        measurement = stored(obs_view(ctx, variant, prop["run"]))
        orders = [sample["order"]["value"] for sample in measurement["samples"]["samples"]]
        assert orders == sorted(orders) and len(set(orders)) == len(orders), where
        values = stored_samples(measurement)
        assert values == output_latencies(ctx, run), where
        quarter = len(values) // 4
        first, last = sum(values[:quarter]) // quarter, sum(values[-quarter:]) // quarter
        assert (first, last) == (prop["quarters"]["first_quarter_mean_t10"], prop["quarters"]["last_quarter_mean_t10"]), where
        assert first - last >= prop["first_quarter_mean_exceeds_last_by_at_least_t10"], where
    elif kind == "conclusion_ceiling":
        ceiling = prop["ceiling"]
        if ceiling == "order_confounded_not_paired_evidence":
            view = assessment_view(ctx, variant, prop["runs"])
            flagged = [flag for flag in view["measurement_flags"]["flags"] if flag["flag_id"] == "measurement.sequential_trial_order"]
            assert {flag["subject_ref"]["id"] for flag in flagged} == {runs[name].observation for name in prop["runs"]}, where
            codes = {(concern["reason"], concern["code"]) for concern in view["confidence"]["computed_concerns"]}
            assert ("validity", "measurement_bias_flag") in codes, (where, codes)
        elif ceiling == "understates_tail_latency":
            (name,) = prop["runs"]
            assert obs_view(ctx, variant, name)["measurement_validity"]["state"] == "partial", where
            assert ctx["tail_refused"]["commit_status"] == "not_committed", ctx["tail_refused"]
            assert shown(ctx, ctx["tail"])["record"]["body"]["status"] == "candidate"
        elif ceiling == "criterion_unmet_or_unresolved_never_satisfied":
            refused = ctx["decision_refused"]
            assert refused["error"]["path"].endswith("/hard_conditions") and ctx["criterion"] in refused["error"]["message"], refused
            cell = shown(ctx, ctx["comparison"])["record"]["body"]
            assert cell["outcome"] != "meets", cell
            (finding,) = findings(ctx["after_audit"], "criterion.hard_unmeasured", ctx["criterion"])
            assert "hard_cell:unknown" in {fact["fact"] for fact in finding["facts"]}, finding
            accepted = shown(ctx, ctx["decision"])["record"]["body"]["lifecycle"]
            assert accepted == "proposed", accepted
        else:
            raise AssertionError(f"{where}: unknown ceiling {ceiling}")
    elif kind == "p99_from_intended_send":
        run = runs[prop["run"]]
        intended = output_latencies(ctx, run, intended=True)
        assert fmt(nearest_rank(intended, 99)) == prop["value_ms"], where
        reported = obs_view(ctx, variant, prop["run"])["record"]["body"]["value"]["value"]
        assert nearest_rank(intended, 99) >= prop["at_least_times_reported"] * t10(reported), where
    elif kind == "attempt_counts":
        attempts = stored(obs_view(ctx, variant, prop["run"]))["attempts"]
        for key in ("started", "observed", "timed_out", "excluded"):
            assert attempts[key]["count"] == {"state": "known", "value": prop[key]}, (where, key)
            assert attempts[key]["definition"]["state"] == "known", (where, key)
        requests = read_evidence(ctx["cli"], ctx["case"], runs[prop["run"]], "/requests")
        assert len(requests) == prop["started"] == prop["observed"] + prop["timed_out"] + prop["excluded"], where
    elif kind == "fraction_within_threshold":
        run = runs[prop["run"]]
        requests = read_evidence(ctx["cli"], ctx["case"], run, "/requests")
        within = sum(item["latency_ms"] is not None and t10(item["latency_ms"]) <= t10(prop["threshold_ms"]) for item in requests)
        started = stored(obs_view(ctx, variant, prop["run"]))["attempts"][prop["denominator"]]["count"]["value"]
        assert (within, started, len(requests)) == (prop["within"], prop["of"], prop["of"]), where
        whole, _, fraction = prop["required_fraction"].partition(".")
        assert whole == "0" and len(fraction) == 2
        assert (within * 100 >= int(fraction) * started) is prop["met"], where
    elif kind == "all_attempt_p99_at_least":
        run = runs[prop["run"]]
        requests = read_evidence(ctx["cli"], ctx["case"], run, "/requests")
        assert t10(read_evidence(ctx["cli"], ctx["case"], run, "/timeout_ms")) == t10(prop["treat_unfinished_as_ms"])
        filled = [t10(item["latency_ms"]) if item["latency_ms"] is not None else t10(prop["treat_unfinished_as_ms"]) for item in requests]
        assert fmt(nearest_rank(filled, 99)) == prop["value_ms"], where
    elif kind == "inquiry_frozen_before_results":
        inquiry = shown(ctx, ctx["band"])["record"]["body"]
        assert inquiry["lifecycle"] == "frozen", inquiry
        plan = inquiry["equivalence"]
        assert all(plan.get(field) for field in prop["fields"]), (where, plan)
        for assessment, _ in ctx["intervals"].values():
            evaluation = shown(ctx, assessment)["record"]["body"]["equivalence_evaluation"]
            assert evaluation["margin_timing"] == "prespecified", evaluation
            assert evaluation["margin"]["stated_at_sequence"] < evaluation["interval"]["first_recorded_at_sequence"], evaluation
    elif kind == "interval_within_margin":
        assessment, warnings = ctx["intervals"][prop["run"]]
        evaluation = shown(ctx, assessment)["record"]["body"]["equivalence_evaluation"]
        interval, margin = evaluation["interval"], evaluation["margin"]
        assert (interval["lower"], interval["upper"], interval["unit"]) == (str(prop["lower_ms"]), str(prop["upper_ms"]), "ms"), evaluation
        assert (margin["lower"], margin["upper"]) == (str(-prop["margin_ms"]), str(prop["margin_ms"])), evaluation
        assert evaluation["outcome"] == prop["outcome"], (where, evaluation["outcome"])
        claim, review = ctx["equivalence_claims"][prop["run"]]
        status = shown(ctx, claim)["record"]["body"]["status"]
        if prop["outcome"] == "bounded_equivalence_supported":
            assert review["ok"] is True and status == "review_ready" and warnings == [], (where, review)
        else:
            assert review["ok"] is False and status == "candidate", (where, review)
            assert "equivalence.interval_crosses_margin" in gate_codes(review), review
            assert any(warning.startswith("equivalence.not_supported:") for warning in warnings), warnings
    else:
        raise AssertionError(f"{where}: unknown assert {kind}")


def trials_preserved(ctx):
    """Every capture stays recorded and unretracted with its value and typed fields;
    the positive pair, the negative short-window pair and the confounded run keep
    their first revision readable beside the judged one."""
    cli, case = ctx["cli"], ctx["case"]
    history = {("baseline", "control"), ("baseline", "treatment"), ("baseline", "treatment-debug-logging"),
               ("negative-short-window", "control-short"), ("negative-short-window", "optimization-a-short")}
    for variant in VARIANTS:
        for name, run in ctx["runs"][variant].items():
            for view in obs_views(ctx, variant, name):
                body = view["record"]["body"]
                assert body.get("status") != "retracted" and body["measurement"] == run.measurement, (variant, name)
                assert view["record"]["header"]["revision"] == 2, "one judgment, appended in a new revision"
                assert view["measurement_validity"]["judgments_kept"] == 1, view["measurement_validity"]
                if (variant, name) in history:
                    identifier = view["record"]["header"]["id"]
                    first = cli.call("--case", case, "show", identifier, "--at", "1", "--depth", "full")["record"]["body"]
                    assert "measurement_assessment" not in first and first["validity"] == "unknown", first
                    assert body["value"] == first["value"] and first["measurement"] == run.measurement, (variant, name)
    pair = shown(ctx, ctx["pair"])["record"]["body"]
    assert pair["status"] == "current" and pair["relation"] == "supports", pair


def decimals(text):
    return set(DECIMAL.findall(text))


def no_invented_numbers(cli):
    """Every decimal in every response was supplied: by the fixture, or by a request."""
    supplied = set(RULE_CONSTANTS)
    for path in FAMILY.rglob("*.json"):
        supplied |= decimals(path.read_text(encoding="utf-8"))
    for request in cli.inputs:
        supplied |= decimals(request)
    found = set()
    for output in cli.outputs:
        parsed = json.loads(output)
        if parsed.get("result") == "evidence_read":
            # Retained bytes, checked against the fixture digest when read;
            # an excerpt boundary may split a supplied number.
            parsed["excerpt"].pop("text", None)
            output = json.dumps(parsed)
        found |= decimals(output)

        def walk(value):
            if isinstance(value, float):
                raise AssertionError(f"a computed floating-point number appeared: {value}")
            if isinstance(value, dict):
                for key, item in value.items():
                    assert key not in FORBIDDEN_KEYS, key
                    walk(item)
            elif isinstance(value, list):
                for item in value:
                    walk(item)
        walk(json.loads(output))
    invented = found - supplied
    assert not invented, f"decimals not supplied by any input: {sorted(invented)}"
    return len(found)


def proof(binary, workspace):
    init = subprocess.run([str(binary), "init", str(workspace), "--no-vault", "--format", "json"],
                          capture_output=True, text=True, check=True, env=ENVIRONMENT)
    assert json.loads(init.stdout)["ok"] is True
    config = workspace / "investigation.toml"
    text = config.read_text(encoding="utf-8")
    assert "import_roots = []" in text
    config.write_text(text.replace("import_roots = []", 'import_roots = ["."]'), encoding="utf-8")
    cli = Cli(binary, workspace)
    manifests = {variant: load(FAMILY / variant / "manifest.json") for variant in VARIANTS}
    expected = {variant: load(FAMILY / variant / "expected.json") for variant in VARIANTS}
    runs = {variant: {spec["id"]: Run(variant, spec) for spec in manifests[variant]["runs"]} for variant in VARIANTS}
    question_text = manifests["baseline"]["question"]
    assert all(manifest["question"] == question_text for manifest in manifests.values())
    b, neg, seq = runs["baseline"], runs["negative-short-window"], runs["sequential-order"]
    closed = runs["closed-loop-omission"]["closed-loop-capture"]
    timeout = runs["missing-outcome"]["timeout-capture"]
    equivalence = runs["practical-equivalence"]

    # ---- Frame: one question, overlapping hypotheses, and the margin frozen before results.
    case = cli.create(None, "case.create", {
        "title": "Ledgerd p99 after the Marlin 1.9 upgrade", "mode": "explain",
        "objective": "Decide whether the allocator change regressed p99 and whether optimization A improves it",
        "decision_relevance": "Whether to keep slab-v2 and ship arena reuse on this workload",
    })
    question = cli.create(case, "question.create", {"wording": question_text, "importance": "high"})

    def hypothesis(statement, rivals=()):
        return cli.create(case, "hypothesis.create", {
            "question_ref": ref(question), "statement": statement, "applicability": "ledgerd-mixed-requests on fixture-host-1",
            "possible_falsifiers": ["The p99 gap does not appear in comparable interleaved captures."],
            "rival_refs": [ref(rival) for rival in rivals], "assumption_refs": []})

    allocator = hypothesis("The slab-v2 allocator change raises Ledgerd p99 on this workload.")
    runtime = hypothesis("A Marlin 1.9 change other than the allocator raises p99 on this workload.", [allocator])
    optimization = hypothesis("Optimization A (arena reuse) lowers p99 by a decision-relevant amount on the 1.9 build.")

    frozen = manifests["practical-equivalence"]["frozen_inquiry"]
    assert frozen["frozen_before_results"] is True
    plan = {"population": frozen["population"], "unit": frozen["unit"], "aggregation": frozen["aggregation"],
            "replication_unit": frozen["replication_unit"], "timeout_exclusion_policy": frozen["timeout_exclusion_policy"],
            "margin": {"lower": str(frozen["margin"]["lower_ms"]), "upper": str(frozen["margin"]["upper_ms"])}}
    band = cli.create(case, "inquiry.create", {
        "purpose": "Bound the p99 difference between optimization A and the treatment build.",
        "method": "Compare an externally supplied interval with the prespecified margin.",
        "controls": [], "limits": ["Only intervals supplied by the producer are used."],
        "desired_observations": ["An interval for the p99 difference, optimization A minus treatment."],
        "inquiry": {"mode": "exploratory"}, "equivalence": plan})
    band_frozen = cli.record(case, op("inquiry.freeze", {"inquiry_ref": ref(band)}), preconditions=[ref(band)])
    band_ref = ref(band, band_frozen["updated"][0]["revision"])

    # ---- Collect: every run is a producer-run envelope; nothing is computed by the core.
    # variables_changed names the environment identity components and the
    # Run environment keys ("revision") that the 1.9 build varies.
    kinds = {  # evidence_kind and variables_changed the investigator declares per run
        ("baseline", "control"): ("observational", []),
        ("baseline", "treatment"): ("interventional", ["runtime", "allocator", "revision"]),
        ("baseline", "treatment-debug-logging"): ("interventional", ["runtime", "allocator", "revision", "debug_logging"]),
        ("baseline", "optimization-a"): ("interventional", ["allocator"]),
        ("negative-short-window", "control-short"): ("observational", []),
        ("negative-short-window", "optimization-a-short"): ("interventional", ["allocator"]),
        ("sequential-order", "a-first-control"): ("observational", []),
        ("sequential-order", "b-second-treatment"): ("interventional", ["runtime", "allocator", "revision"]),
        ("closed-loop-omission", "closed-loop-capture"): ("observational", []),
        ("missing-outcome", "timeout-capture"): ("observational", []),
    }
    warnings = {}
    for variant in VARIANTS:
        for run in runs[variant].values():
            ingest(cli, case, run)
            if variant == "practical-equivalence":
                (assertion,) = run.envelope["assertions"]
                run.bounds = {}
                for bound in ("lower", "upper"):
                    receipt = cli.record(case, op("observation.add", {
                        "run_ref": run.run,
                        "value": {"kind": "measurement", "value": run.output[bound], "unit": run.output["unit"]},
                        "basis": {"kind": "source", "artifact_ref": ref(run.artifact), "selector": selector(run, f"/{bound}"),
                                  "text_quote": None},
                        "extraction_method": f"Read the {bound} bound of the producer's interval for the p99 difference, optimization A minus treatment",
                        "validity": "unknown", "quality": [], "limitations": list(run.envelope["limitations"]),
                        "evidence_kind": "observational", "variables_changed": [], "measurement": run.measurement}))
                    run.bounds[bound] = receipt["created"][0]["id"]
                    assert receipt["warnings"] == [], receipt
                run.observation = run.bounds["lower"]
                assert assertion["name"] == "latency.p99.difference" and assertion["kind"] == "reported_interval"
            else:
                receipt = observe(cli, case, run, *kinds[(variant, run.name)])
            warnings[(variant, run.name)] = receipt["warnings"]
            # Advisory flags never block recording; the receipt warns with the same IDs.
            assert all("does not block recording" in warning for warning in receipt["warnings"]), receipt

    # Validity is recorded before any interpretation. A valid control under the
    # wrong build does not qualify the treatment, and the refusal commits nothing.
    before = cli.call("--case", case, "brief")["snapshot"]
    refused = cli.record(case, op("measurement.assess", {
        "observation_ref": ref(b["treatment"].observation), "channel": None, "validity": "valid",
        "verification": [{"kind": "control", "observation_ref": ref(b["control"].observation), "expected_environment": None,
                          "rationale": "Same harness and host."}],
        "rationale": "Treat the control as the treatment's control.", "limits": []}), exit_code=5)
    assert refused["error"]["code"] == "GATE_FAILED" and refused["commit_status"] == "not_committed", refused
    reasons = set(refused["error"]["actual"][0])
    assert {"CONTROL_ENVIRONMENT_MISMATCH", "CONTROL_ENVIRONMENT_COMPONENT_MISMATCH"} <= reasons, refused
    assert cli.call("--case", case, "brief")["snapshot"] == before

    valid_reason = "The harness and monitor are named and the producer's steady-load control passed; the instrument behaved as specified."
    judged = {}
    for name in ("control", "treatment", "optimization-a"):
        judged[b[name].observation] = judge(cli, case, b[name], "valid", valid_reason,
                                            ["Validity of the instrument only; comparability is judged separately."])
    debug = b["treatment-debug-logging"]
    judged[debug.observation] = judge(cli, case, debug, "invalid",
        "Debug logging was switched on for this capture only, so the instrumentation differs from every run it would be compared with.",
        ["measurement.instrumentation_changed: debug logging enabled during capture"],
        [{"kind": "equivalent", "justification": "The producer's limitations and the recorded instrumentation_changed field both state the debug logging.",
          "evidence_refs": [ref(debug.receipt["envelope"]["artifact_id"])]}])
    for run in neg.values():
        recorded = cli.show(case, run.observation)["record"]["body"]["measurement"]
        conditions = fixture_conditions(run, recorded)
        texts = {MONITOR_FAILED: "the producer's monitor-self-check control reported a failure",
                 WINDOW_SHORT: f"the recorded window {recorded['observation_window']['value']['value']} s is below the intended {run.spec['intended_observation_window_s']} s"}
        judged[run.observation] = judge(cli, case, run, "invalid",
            "The monitor failed its self-check and the capture stopped early, so the instrument did not measure the planned window.",
            [f"{condition}: {texts[condition]}" for condition in conditions])
    for run in seq.values():
        judged[run.observation] = judge(cli, case, run, "valid", valid_reason,
                                        ["Run order is a comparability matter and is not judged here."])
    judged[closed.observation] = judge(cli, case, closed, "partial",
        "The capture is a correct record of service time from actual send, but a closed-loop generator delayed later sends, so it is not user-facing latency.",
        ["Latency is measured from actual, not intended, send time."])
    judged[timeout.observation] = judge(cli, case, timeout, "valid", valid_reason,
        ["The producer's p99 covers completed requests only; unfinished requests have no latency value."])
    bounds = {}
    for name, run in equivalence.items():
        for bound, identifier in run.bounds.items():
            run.observation = identifier
            judged[identifier] = bounds[(name, bound)] = judge(cli, case, run, "valid",
                "The bound is read exactly as the producer supplied it; how the producer computed the interval is not judged here.",
                ["The producer states neither method nor confidence level for the interval."],
                [{"kind": "equivalent", "justification": "The observation records the producer's own statement and nothing derived from it.",
                  "evidence_refs": [ref(run.receipt["envelope"]["artifact_id"])]}])
        run.observation = run.bounds["lower"]

    obs = {key: ref(run.observation, judged[run.observation]["revision"])
           for key, run in ((f"{variant}/{name}", run) for variant in VARIANTS for name, run in runs[variant].items())}

    def assess(subject, evidence, relation, rationale, exit_code=0, **extra):
        payload = {"subject_ref": ref(subject) if isinstance(subject, str) else subject,
                   "evidence_refs": evidence, "relation": relation, "rationale": rationale}
        payload.update(extra)
        return cli.record(case, op("assessment.add", payload), exit_code=exit_code)

    equally = [{"rival_ref": ref(runtime), "expectation": "equally_expected",
                "rationale": "The runtime and the allocator changed together between these builds."}]
    # The comparable pair: same workload, mix, warm-up, window, load model, order and tool.
    pair = assess(allocator, [obs["baseline/control"], obs["baseline/treatment"]], "supports",
                  "Interleaved captures with matched conditions show the 1.9 build's p99 above the 1.8 build's.",
                  rival_expectations=equally)["created"][0]["id"]
    # Pooling the confounded debug-logging run is refused: its measurement is invalid.
    pooled = assess(allocator, [obs["baseline/treatment"], obs["baseline/treatment-debug-logging"]], "supports",
                    "Pool both 1.9 captures.", exit_code=5, rival_expectations=equally)
    assert pooled["error"]["code"] in ("GATE_FAILED", "BLOCKED"), pooled
    assert pooled["error"]["path"].endswith("/evidence_refs/1") and pooled["commit_status"] == "not_committed", pooled
    # The offered mistake: optimization A against the treatment as if comparable.
    naive = assess(optimization, [obs["baseline/treatment"], obs["baseline/optimization-a"]], "supports",
                   "Optimization A's p99 is lower than the treatment's.")["created"][0]["id"]
    ordered = assess(allocator, [obs["sequential-order/a-first-control"], obs["sequential-order/b-second-treatment"]],
                     "supports", "Run B's p99 is above run A's.", rival_expectations=equally)["created"][0]["id"]

    # ---- The negative trial: "no improvement detected" under a failed monitor and a short window.
    intended = {run.spec["intended_observation_window_s"] for run in neg.values()}
    assert len(intended) == 1
    absence = {"expected_signal": "A lower p99 under optimization A than under the control capture.",
               "opportunity": {"description": "One control and one optimization A capture at steady load.",
                               "refs": [obs["negative-short-window/control-short"], obs["negative-short-window/optimization-a-short"]],
                               "minimum_observation_window": {"value": str(intended.pop()), "unit": "s"}},
               "detection_limit": "Not established: the window was cut short and the monitor failed."}
    negative = assess(optimization, [obs["negative-short-window/control-short"], obs["negative-short-window/optimization-a-short"]],
                      "inconclusive", "No improvement was detected, under a failed monitor and a cut-short window.",
                      absence=absence)
    assert any(warning.startswith("absence.basis_incomplete:") for warning in negative["warnings"]), negative
    negative = negative["created"][0]["id"]

    # ---- Practical equivalence against the margin frozen before the intervals arrived.
    intervals = {}
    for name, run in equivalence.items():
        receipt = assess(optimization, [bounds[(name, "lower")], bounds[(name, "upper")]], "supports",
                         "Read the supplied interval against the frozen margin.",
                         equivalence={"inquiry_ref": band_ref, "interval": {"run_ref": run.run, "assertion": "latency.p99.difference"}})
        intervals[name] = (receipt["created"][0]["id"], receipt["warnings"])

    # ---- The missing-outcome denominator: an all-attempt hard criterion.
    keep = cli.create(case, "option.create", {"question_ref": ref(question), "description": "Keep the Marlin 1.9 build with slab-v2",
                                              "dependencies": [], "tradeoff_notes": []})
    keep_ref = ref(keep, cli.record(case, op("option.disposition", {"option_ref": ref(keep), "lifecycle": "eligible",
        "reason": "The current production candidate"}))["updated"][0]["revision"])
    criterion = cli.create(case, "criterion.create", {
        "name": "99% of started requests complete within 25 ms", "kind": "hard",
        "scope": {"kind": "all_options", "question_ref": ref(question)},
        "scale": {"unit": "ms", "description": "p99 over every started request; a request unfinished at the timeout counts as slower than 25 ms", "anchors": []},
        "preferred_direction": "lower",
        "threshold": {"kind": "measured", "operator": "at_most", "value": {"decimal": "25", "unit": "ms"}},
        "weighting_rationale": None})
    # The Comparison restates only the value and unit. Whether the value can
    # settle the cell is read from the cited Observation's measurement fields.
    mistaken = cli.record(case, op("comparison.add", {
        "option_ref": keep_ref, "criterion_ref": ref(criterion), "outcome": "meets",
        "method": "Producer-reported p99 against the 25 ms requirement",
        "value": {"decimal": timeout.envelope["assertions"][0]["value"], "unit": "ms"},
        "evidence_refs": [obs["missing-outcome/timeout-capture"]], "assumption_refs": [],
        "uncertainty": "One capture", "rationale": "The reported p99 is below 25 ms."}))
    comparison = mistaken["created"][0]["id"]
    assert any("measurement.outcome_from_completion_only_statistic" in warning for warning in mistaken["warnings"]), mistaken

    # A decision cannot rest on the completion-only p99: acceptance reads the
    # cited Observation's statistic population and attempt counts, so the
    # percentile cell is unresolved and the hard criterion must be
    # acknowledged, never satisfied.
    decision_payload = {
        "question_ref": ref(question), "choice": {"kind": "select", "option_ref": keep_ref},
        "compared_alternatives": [keep_ref], "comparison_refs": [ref(comparison)], "hard_conditions": [],
        "evidence_refs": [obs["missing-outcome/timeout-capture"]], "assumption_refs": [],
        "tradeoffs": ["None recorded"], "consequences": ["Keep the 1.9 build"],
        "reopen_trigger": "A capture counting every started request", "expected_outcome": "Requests meet the 25 ms requirement",
        "check_when": "After the next capture", "reversal_conditions": []}
    decision = cli.create(case, "decision.propose", decision_payload)
    basis = cli.show(case, decision)["review_basis"]
    decision_review = cli.create(case, "review.submit", {
        "kind": "validation", "subject_ref": basis["subject_ref"], "basis_digest": basis["basis_digest"],
        "read_set": basis["read_set"], "method": "Read the comparison and the capture it cites.",
        "materials_examined": [obs["missing-outcome/timeout-capture"]], "shared_dependencies": [], "objection_refs": [],
        "verdict": "supports", "caveats": ["One capture"], "context_isolation": "same_session_self_review",
        "context": review_context("Check the requirement against the recorded capture.")}, actor=REVIEWER)
    before = cli.call("--case", case, "brief")["snapshot"]
    refused = cli.record(case, op("decision.accept", {"decision_ref": basis["subject_ref"], "review_ref": ref(decision_review)}),
                         exit_code=5)
    assert refused["error"]["code"] == "GATE_FAILED" and refused["error"]["path"].endswith("/hard_conditions"), refused
    assert criterion in refused["error"]["message"], refused
    assert cli.call("--case", case, "brief")["snapshot"] == before
    # The recorded counts bound the requirement (95 of 100 started requests),
    # but the all-attempt p99 is censored at the timeout, so the cell stays unresolved.
    attempts = timeout.measurement["attempts"]
    revised = cli.record(case, op("comparison.revise", {
        "comparison_ref": ref(comparison), "reason": "The requirement counts every started request, and 5 never finished.",
        "replacement": {"option_ref": keep_ref, "criterion_ref": ref(criterion), "outcome": "unknown",
                        "method": "Count of started requests that finished within 25 ms",
                        "basis": {"kind": "unknown", "reason": "The all-attempt p99 is censored at the timeout"},
                        "evidence_refs": [obs["missing-outcome/timeout-capture"]], "assumption_refs": [],
                        "uncertainty": "One capture of 100 started requests",
                        "rationale": f"{attempts['observed']['count']['value']} of {attempts['started']['count']['value']} started requests finished; {attempts['timed_out']['count']['value']} timed out at the 1000 ms timeout."}}))
    comparison_ref = ref(comparison, revised["updated"][0]["revision"])

    # ---- Conclusions bounded to the workload; confidence names its downgrade reasons.
    scope = "ledgerd-mixed-requests, read90-write10, open-loop steady load on fixture-host-1, one interleaved capture per build"
    caveats = ["The runtime and the allocator changed together between these builds.",
               "One capture per build: repeatability is not established."]
    bounded = cli.create(case, "conclusion.create", {
        "statement": "On this workload the marlin-1.9/slab-v2 build's p99 is higher than the marlin-1.8/slab-v1 build's.",
        "scope": scope, "significance": "A regression exists for this workload; its cause is not isolated.",
        "support_refs": [ref(pair)], "contradiction_refs": [], "caveats": caveats,
        "confidence": {"level": "moderate", "rationale": "Matched interleaved captures with a qualified instrument.", "limitations": caveats}})
    unnamed = request_review(cli, case, bounded, exit_code=5)
    assert unnamed["error"]["code"] == "GATE_FAILED" and unnamed["error"]["path"].endswith("/conclusion_ref"), unnamed
    assert set(unnamed["error"]["expected"]["confidence"]["downgrade_reasons"]) == set(DOWNGRADE), unnamed
    replacement = {"statement": "On this workload the marlin-1.9/slab-v2 build's p99 is higher than the marlin-1.8/slab-v1 build's.",
                   "scope": scope, "significance": "A regression exists for this workload; its cause is not isolated.",
                   "support_refs": [ref(pair)], "contradiction_refs": [], "caveats": caveats,
                   "confidence": {"level": "moderate", "rationale": "Matched interleaved captures with a qualified instrument.",
                                  "limitations": caveats, "downgrade_reasons": DOWNGRADE}}
    cli.record(case, op("conclusion.amend", {"conclusion_ref": ref(bounded), "reason": "Name the downgrade reasons considered.",
                                             "replacement": replacement}))
    ready = request_review(cli, case, bounded)
    assert cli.show(case, bounded)["record"]["body"]["status"] == "review_ready", ready

    # The causal claim names the allocator; with the runtime changed too and no
    # baseline/treatment/reversal toggle it stays a candidate.
    causal = cli.create(case, "conclusion.create", {
        "statement": "The slab-v2 allocator change causes the p99 regression.", "scope": scope,
        "significance": "Would justify reverting the allocator.", "support_refs": [ref(pair)], "contradiction_refs": [],
        "caveats": caveats, "causal_factors": [{"hypothesis_ref": ref(allocator), "role": "trigger"}]})
    blocked = request_review(cli, case, causal, exit_code=5)
    assert blocked["error"]["code"] == "GATE_FAILED" and "/causal_test" in blocked["error"]["path"], blocked

    # A tail bound from the closed-loop capture is refused: the measurement is
    # only partially valid, so it cannot provide direct support.
    tail = cli.create(case, "conclusion.create", {
        "statement": "The closed-loop capture bounds user-facing p99 latency on the 1.9 build.", "scope": scope,
        "significance": "Would settle the tail requirement.", "support_refs": [obs["closed-loop-omission/closed-loop-capture"]],
        "contradiction_refs": [], "caveats": ["Timed from actual send."]})
    tail_refused = request_review(cli, case, tail, exit_code=5)
    assert tail_refused["error"]["code"] == "GATE_FAILED" and tail_refused["error"]["path"].endswith("/support_refs"), tail_refused

    # The negative trial cannot become evidence of equality.
    no_effect = cli.create(case, "conclusion.create", {
        "statement": "Optimization A does not change p99.", "scope": scope,
        "significance": "Would close the optimization question.", "support_refs": [ref(negative)], "contradiction_refs": [],
        "caveats": ["Captures stopped early under a failed monitor."], "claim_form": "absence"})
    refused_absence = request_review(cli, case, no_effect, exit_code=5)
    assert refused_absence["error"]["code"] == "GATE_FAILED", refused_absence
    assert refused_absence["error"]["actual"]["ceiling"] == "inconclusive", refused_absence
    absence_codes = gate_codes(refused_absence)

    # Equivalence follows the supplied interval against the frozen margin.
    equivalence_claims = {}
    for name, (assessment, _) in intervals.items():
        claim = cli.create(case, "conclusion.create", {
            "statement": "Optimization A and the treatment build are practically equivalent in p99 within the frozen margin.",
            "scope": frozen["population"], "significance": "Decides whether optimization A is worth shipping.",
            "support_refs": [ref(assessment)], "contradiction_refs": [],
            "caveats": ["The interval was computed outside Investigation; its method is not stated."], "claim_form": "equivalence"})
        equivalence_claims[name] = (claim, request_review(cli, case, claim, exit_code=None))

    # ---- Freeze a confirmatory repeat plan. A first draft predicts the same
    # outcome for both rivals; the audit flags it before any run.
    untested = {"rival_ref": ref(optimization),
                "predicted_outcome": "Not tested: optimization A is not run in this plan, so no outcome bears on it."}
    shared = "The 1.9 build's p99 exceeds the 1.8 build's by at least 5.0 ms in each of three interleaved repeat pairs."
    confirmatory = {
        "purpose": "Decide whether the regression is repeatable and whether it follows the allocator.",
        "method": "Three interleaved repeat pairs, then the allocator alone reverted on marlin-1.9.",
        "controls": ["Same workload and read90-write10 mix", "30 s warm-up and 120 s window", "Open-loop load timed from intended send",
                     "Debug logging off and monitor self-check passing", "At least 299 samples per capture", "Interleaved order"],
        "limits": ["Stop after three pairs and one revert pair."],
        "desired_observations": ["p99 per capture with its sample count and attempt counts"],
        "inquiry": {"mode": "confirmatory", "confirmatory": {
            "predictions": [{"rival_ref": ref(allocator), "predicted_outcome": shared},
                            {"rival_ref": ref(runtime), "predicted_outcome": shared}, untested],
            "evidence_holdout_boundary": "Only captures produced after this plan is frozen.",
            "prior_exposure": "seen",
            "stop_analysis_rule": "Analyse after all four pairs; no early stop."}}}
    repeat = cli.create(case, "inquiry.create", confirmatory)
    found = findings(audit(cli, case), "inquiry.non_discriminating_plan", repeat)
    assert len(found) == 1, found
    confirmatory["inquiry"]["confirmatory"]["predictions"] = [
        {"rival_ref": ref(allocator), "predicted_outcome": "The p99 gap closes when only the allocator is reverted on marlin-1.9."},
        {"rival_ref": ref(runtime), "predicted_outcome": "The p99 gap persists when only the allocator is reverted on marlin-1.9."},
        untested]
    amended = cli.record(case, op("inquiry.amend", {"inquiry_ref": ref(repeat), "rationale": "Make the predictions discriminate the rivals.",
                                                    "replacement": confirmatory}))
    repeat_ref = ref(repeat, amended["updated"][0]["revision"])
    repeat_ref = ref(repeat, cli.record(case, op("inquiry.freeze", {"inquiry_ref": repeat_ref, "question_ref": ref(question)}),
                                        preconditions=[repeat_ref])["updated"][0]["revision"])
    # Re-reading the captures already seen cannot count as the repeat.
    reuse = assess(allocator, [obs["baseline/control"], obs["baseline/treatment"]], "supports",
                   "Re-read the existing pair under the frozen plan.", rival_expectations=[
                       {"rival_ref": ref(runtime), "expectation": "equally_expected", "rationale": "Nothing was reverted."}],
                   frozen_basis={"plan_ref": repeat_ref, "stop_rule_applied": "Read the existing pair only.",
                                 "actual_inputs": [{"input_ref": obs["baseline/control"]}, {"input_ref": obs["baseline/treatment"]}]})
    reuse_id = reuse["created"][0]["id"]

    # ---- Blindspots: the audit reads typed fields only and claims no completeness.
    before_review = audit(cli, case)
    assert not findings(before_review, "inquiry.non_discriminating_plan", repeat), "the amended plan discriminates"
    assert "not proof of completeness" in before_review["limits"], before_review["limits"]
    assert "all blindspots resolved" not in before_review["statement"] and "verified correct" not in before_review["statement"]
    assert before_review["statement"].startswith(f"{len(before_review['checks'])} structural gaps identified under checks")
    by_rule = {}
    for entry in findings(before_review, "measurement.advisory_flags"):
        by_rule.setdefault(entry["rule_id"], set()).update(ref_["id"] for ref_ in entry["subject_refs"])
    monitored = {
        "measurement.instrumentation_changed": {debug.observation},
        "measurement.sequential_trial_order": {run.observation for run in seq.values()},
        "measurement.closed_loop_without_intended_send_timing": {closed.observation},
        "measurement.completion_only_statistic": {timeout.observation},
        "measurement.quantile_below_order_statistic_bound": {timeout.observation, *(run.observation for run in neg.values())},
        "measurement.compared_warmup_differs": {naive},
    }
    for rule, subjects in monitored.items():
        assert subjects <= by_rule.get(rule, set()), (rule, by_rule)
    # Missing comparability: the optimization A comparison relies on two Runs
    # whose recorded environments differ, and on a differing warm-up.
    comparability = {entry["subject_refs"][0]["id"]: entry for entry in findings(before_review, "comparability.environment_differs")}
    assert {fact["fact"] for fact in comparability[naive]["facts"]} == {"run_environment_differs"}, comparability
    # The matched pair is not flagged: its Run environments differ only in the
    # revision key and its identities only in components, all named in variables_changed.
    assert pair not in comparability, comparability[pair]
    rival = findings(before_review, "conclusion.support_not_assessed_against_rival", causal)
    assert rival and {fact["fact"] for fact in rival[0]["facts"]} == {"support_equally_expected_under_rival"}, rival

    def gap(check_id, subject, rule=None):
        (entry,) = [item for item in findings(before_review, check_id, subject) if rule is None or item.get("rule_id") == rule]
        return entry

    assessed = {name: cli.show(case, identifier) for name, identifier in (("pair", pair), ("naive", naive), ("ordered", ordered))}
    naive_gap = gap("comparability.environment_differs", naive)
    warmup_gap = gap("measurement.advisory_flags", naive, "measurement.compared_warmup_differs")
    debug_gap = gap("measurement.advisory_flags", debug.observation, "measurement.instrumentation_changed")
    rival_gap = gap("conclusion.support_not_assessed_against_rival", causal)
    case_ref = cli.current(case, case)
    # Exact current revisions of what the reviewer read: each was written once, or judged once.
    current = {identifier: ref(identifier) for identifier in (naive, pair, causal, allocator, runtime)}
    current.update({run.observation: obs[f"{run.variant}/{run.name}"] for run in (debug, *neg.values())})
    review_payload = {
        "kind": "blindspot", "subject_ref": case_ref, "audited_snapshot": before_review["snapshot"],
        "read_set": list(current.values()),
        "method": "Read the structural audit, then each capture's typed fields, controls and limitations.",
        "scope_examined": "Comparability, monitoring and denominators of every capture; not the producer's internals.",
        "checks_considered": ["comparability of compared captures", "monitor and window adequacy", "run order", "denominators"],
        "gaps": [], "evidence_refs": [], "assumption_refs": [],
        "limitations": ["No new capture was run."], "verdict": "gaps_identified",
        "context_isolation": "same_session_self_review", "context": review_context("Challenge the comparisons before any is relied on."),
        "audit": {"check_version": before_review["check_version"], "check_set_digest": before_review["check_set_digest"]},
        "gap_dispositions": [
            {"gap_id": naive_gap["gap_id"], "disposition": "accepted",
             "reason": "Optimization A ran with a 5 s warm-up and a read70-write30 mix against the treatment's 30 s and read90-write10.",
             "related_refs": [current[naive]]},
            {"gap_id": warmup_gap["gap_id"], "disposition": "accepted",
             "reason": "The warm-up differs, so the comparison is not of optimization A alone.", "related_refs": [current[naive]]},
            {"gap_id": debug_gap["gap_id"], "disposition": "addressed",
             "reason": "The debug-logging capture is judged invalid and is never pooled.", "related_refs": [current[debug.observation]],
             "record_refs": [current[debug.observation]]},
            {"gap_id": rival_gap["gap_id"], "disposition": "accepted",
             "reason": "Runtime and allocator changed together; the frozen repeat plan reverts the allocator alone.",
             "related_refs": [current[causal], current[runtime]]}],
        "novel_gaps": [
            {"statement": "A failed monitor self-check and a cut-short window are not typed measurement fields, so no check flags them.",
             "uncertainty": "Other producers may report monitor health differently.",
             "related_refs": [current[run.observation] for run in neg.values()], "record_refs": [{"client_ref": "monitoring"}]}],
    }
    reviewed = cli.record(case, op("question.create", {
        "wording": "Which monitor health and window conditions must hold before a capture is interpreted?",
        "importance": "high"}, "monitoring"), op("review.submit", review_payload))
    assert reviewed["updated"] == [], reviewed
    blindspot_review = reviewed["created"][1]["id"]
    stored = cli.show(case, blindspot_review)["record"]["body"]
    assert stored["audit"]["undispositioned_gap_ids"] != [] and stored["novel_gaps"][0]["origin"] == "reviewer", stored
    # The investigator acts on the review: the optimization A comparison is inconclusive.
    cli.record(case, op("assessment.supersede", {"assessment_ref": current[naive],
        "rationale": "Warm-up and workload mix differ from the treatment capture.",
        "replacement": {"subject_ref": ref(optimization), "evidence_refs": [obs["baseline/treatment"], obs["baseline/optimization-a"]],
                        "relation": "inconclusive",
                        "rationale": "Warm-up (30 s against 5 s) and workload mix differ, so the p99 difference is not attributable to optimization A."}}))
    after_review = audit(cli, case)
    assert not findings(after_review, "comparability.environment_differs", naive), "a superseded comparison is no longer audited"

    # ---- Retrospective from the digest, then closure with a precise reopen condition.
    digest = cli.call("--case", case, "retrospective", "--draft")
    assert "recorded work only" in digest["coverage"], digest
    point = digest["snapshot"]
    wasted = {
        "Debug logging was switched on for one capture, so it measured different instrumentation.": [obs["baseline/treatment-debug-logging"]],
        "Optimization A ran with a 5 s warm-up and a different workload mix, so it could not be compared.": [ref(naive)],
        "The short-window captures ran 8 s of 300 s under a failed monitor and settle nothing.": [
            obs["negative-short-window/control-short"], obs["negative-short-window/optimization-a-short"]],
        "The A-then-B captures carry a cold-start decay in the first run.": [
            obs["sequential-order/a-first-control"], obs["sequential-order/b-second-treatment"]],
        "The closed-loop capture timed from actual send hid a 600 ms stall.": [obs["closed-loop-omission/closed-loop-capture"]],
        "The completion-only p99 left out 5 of 100 started requests.": [obs["missing-outcome/timeout-capture"]],
    }
    controls_next = [
        ("Keep instrumentation identical across compared captures; no debug logging.", [obs["baseline/treatment-debug-logging"]]),
        ("Match warm-up, window and workload mix before comparing captures.", [ref(naive)]),
        ("Do not interpret a capture whose monitor self-check failed or whose window was cut short.", [obs["negative-short-window/control-short"]]),
        ("Interleave or randomise run order.", [obs["sequential-order/a-first-control"]]),
        ("Time latency from the intended send with an open-loop generator.", [obs["closed-loop-omission/closed-loop-capture"]]),
        ("Count every started request, timeouts included, before applying an all-attempt requirement.", [obs["missing-outcome/timeout-capture"]]),
    ]
    retrospective = dict(digest["record_template"],
        methods_attempted=[{"name": "Interleaved control/treatment captures with typed measurement fields"},
                           {"name": "Externally supplied interval against a frozen margin"}],
        effective_steps=[{"description": "The matched interleaved pair showed the regression on this workload.",
                          "support_refs": ids([obs["baseline/control"], obs["baseline/treatment"]])},
                         {"description": "Freezing the margin before the intervals let one interval resolve and the other stay unresolved.",
                          "support_refs": [band]}],
        invalid_or_misleading_steps=[{"description": text, "support_refs": ids(refs)} for text, refs in wasted.items()],
        costs_or_constraints="Synthetic offline captures; one capture per build and condition.",
        residual_unknowns=["Whether the regression repeats and follows the allocator alone.",
                           "Whether optimization A improves p99 under matched conditions."],
        lessons=[{"guidance": guidance, "applies_when": "Latency captures compared across builds or conditions.",
                  "support_refs": ids(refs), "transfer_limits": "Learned from one synthetic workload and host."}
                 for guidance, refs in controls_next])
    closing = {"outcome": "inconclusive",
               "reason": "A regression is shown for this workload; its cause and optimization A's effect are unresolved.",
               "unresolved_questions": [ref(question)],
               "reopen_condition": "Run the frozen confirmatory repeat plan with the allocator reverted alone."}
    closed_receipt = cli.record(case, op("retrospective.record", retrospective, "account"),
                                op("case.close", dict(closing, retrospective_ref={"client_ref": "account"})),
                                preconditions=[cli.current(case, case)])
    retro = closed_receipt["created"][0]["id"]
    closed_case = cli.show(case, case)["record"]["body"]
    assert closed_case["lifecycle"] == "closed" and closed_case["closure"]["retrospective_ref"]["id"] == retro, closed_case
    recorded_retro = cli.show(case, retro)["record"]["body"]
    assert [step["description"] for step in recorded_retro["invalid_or_misleading_steps"]] == list(wasted), recorded_retro
    assert [lesson["guidance"] for lesson in recorded_retro["lessons"]] == [text for text, _ in controls_next]

    digest_items = {item["id"] for item in digest["items"]}
    assert f"assessment.inconclusive/{negative}" in digest_items, digest_items
    assert f"review.stale/{blindspot_review}" in digest_items, "the blindspot review read the superseded comparison"
    # Measurement judgments reach the digest: invalid captures and the
    # partially valid closed-loop capture are listed, valid ones are not.
    judged_items = {f"observation.invalid/{run.observation}" for run in (debug, *neg.values())}
    judged_items.add(f"observation.partial/{closed.observation}")
    assert judged_items <= digest_items, judged_items - digest_items
    assert not any(item.endswith(f"/{timeout.observation}") for item in digest_items), digest_items

    context = {
        "cli": cli, "case": case, "runs": runs, "manifests": manifests, "obs": obs, "bounds": bounds,
        "assessed": assessed, "assessments": {("baseline", "control", "treatment"): "pair",
                                              ("baseline", "treatment", "optimization-a"): "naive",
                                              ("sequential-order", "a-first-control", "b-second-treatment"): "ordered"},
        "naive": naive, "negative": negative, "bounded": bounded, "pair": pair, "causal": causal, "tail": tail,
        "tail_refused": tail_refused, "no_effect": no_effect, "absence_refusal": refused_absence, "absence_codes": absence_codes,
        "pooled": pooled, "decision": decision, "decision_refused": refused, "comparison": comparison, "criterion": criterion,
        "band": band, "intervals": intervals, "equivalence_claims": equivalence_claims, "audit": before_review,
        "after_audit": after_review, "digest_items": digest_items, "views": {},
    }
    evaluated = []
    for variant in VARIANTS:
        for prop in expected[variant]["properties"]:
            evaluate(context, variant, prop)
            evaluated.append(f"{variant}/{prop['id']}")
    assert len(evaluated) == sum(len(expected[variant]["properties"]) for variant in VARIANTS)
    trials_preserved(context)
    numbers = no_invented_numbers(cli)
    assert cli.call("check")["structurally_valid"]
    return {"ok": True, "scenario": "B", "case": case, "properties": evaluated, "decimals_checked": numbers,
            "case_outcome": closed_case["closure_outcome"], "retrospective": retro, "blindspot_review": blindspot_review}


if __name__ == "__main__":
    main()
