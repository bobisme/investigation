#!/usr/bin/env python3
"""Mechanically evaluate the Scenario B fixture: byte-identical regeneration, digests, p99, flags, traps."""

import hashlib
import sys
import importlib.util
import json
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
FAMILY = ROOT / "fixtures/development/scenario-b-latency-regression"
spec = importlib.util.spec_from_file_location("gen_b", ROOT / "scripts/generate-scenario-b-fixture.py")
gen = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gen)
VARIANTS = [name for name, _, _ in gen.VARIANTS]
FORBIDDEN_NAMES = ("p_value", "power", "confidence_interval")


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def t10(text):
    whole, _, frac = text.partition(".")
    return int(whole) * 10 + int(frac or 0)


def values_of(measurement):
    samples = measurement.get("samples")
    if not samples:
        return None
    if samples["shape"] == "flat":
        return [t10(v) for v in samples["values"]]
    return [t10(s["value"]) for s in samples["samples"]]


def derive_flags(run, measurement, envelope):
    """Mirror of the advisory single-record rules in measurement_flags.rs, plus two fixture-level conditions."""
    flags = set()
    samples = measurement.get("samples")
    if samples and samples["shape"] == "flat":
        flags.add("measurement.flat_samples")
    statistic = (measurement.get("statistic") or {}).get("value")
    attempts = measurement.get("attempts") or {}
    if statistic and statistic["kind"] == "quantile":
        count = measurement.get("sample_count", {})
        if count.get("state") == "known":
            level = float(statistic["quantile"])
            minimum, miss = 0, 1.0
            while miss > 0.05:
                miss *= level
                minimum += 1
            if count["value"] < minimum:
                flags.add("measurement.quantile_below_order_statistic_bound")
        else:
            flags.add("measurement.quantile_sample_count_unrecorded")
    if statistic:
        population = (statistic.get("population") or {}).get("value")
        started, observed = attempts.get("started"), attempts.get("observed")
        all_completed = bool(started and observed and started["count"] == observed["count"])
        if population == "completed_attempts" and not all_completed:
            flags.add("measurement.completion_only_statistic")
    order = measurement.get("trial_order", {})
    if order.get("state") == "known" and order["value"] == "sequential":
        flags.add("measurement.sequential_trial_order")
    load_model = (measurement.get("load_model") or {}).get("value")
    if load_model and load_model["loop"].get("value") == "closed" and load_model["timed_from_intended_send"] != {"state": "known", "value": True}:
        flags.add("measurement.closed_loop_without_intended_send_timing")
    if measurement.get("instrumentation_changed") == {"state": "known", "value": True}:
        flags.add("measurement.instrumentation_changed")
    if any("fail" in c["producer_result"] for c in envelope["controls"]):
        flags.add("fixture.monitor_self_check_failed")
    window = measurement.get("observation_window", {}).get("value")
    if window and "intended_observation_window_s" in run and int(window["value"]) < run["intended_observation_window_s"]:
        flags.add("fixture.observation_window_below_intended")
    return sorted(flags)


def derive_pair_flags(left, right):
    flags = []
    for field, flag in (("workload", "workload"), ("load_model", "load_model"), ("warmup", "warmup"), ("tool_identity", "tool_identity")):
        if left["measurement"].get(field) != right["measurement"].get(field):
            flags.append(f"measurement.compared_{flag}_differs")
    return sorted(flags)


def check_variant(name, files):
    base = FAMILY / name
    manifest, expected = load(base / "manifest.json"), load(base / "expected.json")
    assert manifest["variant"] == expected["variant"] == name and manifest["scenario"] == "B"
    assert manifest["synthetic"] and manifest["offline"] and not manifest["provider_specific_inputs"]
    assert manifest["mistake_offered"] and manifest["traps"]
    runs = {}
    for run in manifest["runs"]:
        parts = {key: load(ROOT / FAMILY.relative_to(ROOT) / name / run[key]) for key in ("envelope", "measurement", "output")}
        for key in ("envelope", "measurement", "output"):
            data = (base / run[key]).read_bytes()
            assert hashlib.sha256(data).hexdigest() == run["sha256"][key], f"{name}/{run['id']}: {key} digest"
        envelope = parts["envelope"]
        assert envelope["schema_version"] == "inv.producer-run.v1" and envelope["execution"]["exit_code"] == 0
        artifact = envelope["artifacts"][0]
        assert artifact["expected_sha256"] == run["sha256"]["output"]
        assert envelope["external_run_id"] == f"{name}-{run['id']}"
        assert all(a["validity_claim"] == "producer_reported" for a in envelope["assertions"])
        text = json.dumps(parts)
        assert not any(f'"{k}"' in text for k in FORBIDDEN_NAMES), f"{name}/{run['id']}: invented statistic"
        runs[run["id"]] = {"run": run, **parts}
    props = {p["id"]: p for p in expected["properties"]}
    assert len(props) == len(expected["properties"]), f"{name}: duplicate property id"
    for prop in props.values():
        evaluate(name, prop, runs, manifest)
    return len(runs), len(props)


def p99_of(runs, run_id):
    return gen.nearest_rank(values_of(runs[run_id]["measurement"]), 99)


def evaluate(name, prop, runs, manifest):
    kind = prop["assert"]
    where = f"{name}/{prop['id']}"
    if kind == "nearest_rank_p99":
        for run_id, value in prop["per_run"].items():
            measured = runs[run_id]["measurement"]
            samples = values_of(measured)
            output = runs[run_id]["output"]
            if "latency_ms" in output:
                assert [t10(v) for v in output["latency_ms"]] == samples, f"{where}: output vs measurement samples"
            assert gen.fmt(gen.nearest_rank(samples, 99)) == value, f"{where}: {run_id} p99"
            reported = runs[run_id]["envelope"]["assertions"]
            assert not reported or reported[0]["value"] == value, f"{where}: producer-reported value"
    elif kind == "comparable_pair":
        first, second = (runs[r] for r in prop["runs"])
        for field in prop["same"]:
            if field == "workload_mix":
                same = first["envelope"]["environment"][field] == second["envelope"]["environment"][field]
            else:
                same = first["measurement"][field] == second["measurement"][field]
            assert same, f"{where}: {field}"
        ids = [r["measurement"]["environment_identity"]["value"] for r in (first, second)]
        for key in prop["differs"]:
            assert ids[0][key] != ids[1][key], f"{where}: {key} should differ"
        assert ids[0]["host"] == ids[1]["host"], where
    elif kind == "p99_greater":
        diff = p99_of(runs, prop["greater"]) - p99_of(runs, prop["lesser"])
        assert diff >= t10(prop["min_difference_ms"]), f"{where}: difference {diff}"
    elif kind == "run_confounded":
        run = runs[prop["run"]]
        assert run["measurement"]["instrumentation_changed"] == {"state": "known", "value": True}
        other = runs[prop["same_build_as"]]
        assert run["measurement"]["environment_identity"] == other["measurement"]["environment_identity"]
        assert p99_of(runs, prop["run"]) > p99_of(runs, prop["p99_higher_than"])
        assert any("debug" in lim.lower() for lim in run["envelope"]["limitations"])
    elif kind == "recorded_field_differs":
        got = [int(runs[r]["measurement"][prop["field"]]["value"]["value"]) for r in prop["runs"]]
        assert got == prop["values_s"], f"{where}: {got}"
    elif kind == "environment_differs":
        got = [runs[r]["envelope"]["environment"][prop["field"]] for r in prop["runs"]]
        assert got == prop["values"] and got[0] != got[1], f"{where}: {got}"
    elif kind == "exit_does_not_imply_validity":
        for r in prop["runs_exit_zero"]:
            assert runs[r]["envelope"]["execution"]["exit_code"] == 0
        assert "measurement.instrumentation_changed" in derive_flags(runs[prop["run_not_valid"]]["run"], runs[prop["run_not_valid"]]["measurement"], runs[prop["run_not_valid"]]["envelope"])
    elif kind == "derived_flags_equal":
        assert set(prop["per_run"]) == set(runs), f"{where}: runs"
        for r, flags in prop["per_run"].items():
            got = derive_flags(runs[r]["run"], runs[r]["measurement"], runs[r]["envelope"])
            assert got == sorted(flags), f"{where}: {r}: derived {got} vs expected {sorted(flags)}"
    elif kind == "derived_pair_flags_equal":
        for pair in prop["pairs"]:
            left, right = (runs[r] for r in pair["runs"])
            assert derive_pair_flags(left, right) == sorted(pair["flags"]), f"{where}: {pair['runs']}"
    elif kind == "statistic_recorded":
        for r in prop["runs"]:
            value = runs[r]["measurement"]["statistic"]["value"]
            assert value["kind"] == prop["kind"] and value["quantile"] == prop["quantile"], where
            assert value["population"]["value"] == prop["population"], where
    elif kind == "equality_claim_forbidden":
        for r in prop["runs"]:
            got = set(derive_flags(runs[r]["run"], runs[r]["measurement"], runs[r]["envelope"]))
            assert set(prop["reasons"]) <= got, f"{where}: {r}"
    elif kind == "inputs_contain_no_key":
        for run in runs.values():
            text = json.dumps([run["envelope"], run["measurement"], run["output"]])
            assert not any(f'"{k}"' in text for k in prop["keys"])
    elif kind == "negative_result_retained":
        assert set(prop["runs"]) <= set(runs)
    elif kind == "flag_present":
        for r in prop["runs"]:
            assert prop["flag"] in derive_flags(runs[r]["run"], runs[r]["measurement"], runs[r]["envelope"]), f"{where}: {r}"
    elif kind == "order_effect_visible":
        values = values_of(runs[prop["run"]]["measurement"])
        q = len(values) // 4
        first, last = sum(values[:q]) // q, sum(values[-q:]) // q
        assert (first, last) == (prop["quarters"]["first_quarter_mean_t10"], prop["quarters"]["last_quarter_mean_t10"]), where
        assert first - last >= prop["first_quarter_mean_exceeds_last_by_at_least_t10"], where
        orders = [s["order"]["value"] for s in runs[prop["run"]]["measurement"]["samples"]["samples"]]
        assert orders == sorted(orders) and len(set(orders)) == len(orders)
    elif kind == "conclusion_ceiling":
        assert set(prop["runs"]) <= set(runs) and prop["ceiling"]
    elif kind == "p99_from_intended_send":
        records = runs[prop["run"]]["output"]["requests"]
        intended = [t10(r["done_ms"]) - t10(r["intended_send_ms"]) for r in records]
        actual = [t10(r["done_ms"]) - t10(r["actual_send_ms"]) for r in records]
        assert gen.fmt(gen.nearest_rank(intended, 99)) == prop["value_ms"], where
        assert actual == values_of(runs[prop["run"]]["measurement"]), f"{where}: samples are actual-send latencies"
        assert gen.nearest_rank(intended, 99) >= prop["at_least_times_reported"] * gen.nearest_rank(actual, 99), where
    elif kind == "attempt_counts":
        run = runs[prop["run"]]
        attempts = run["measurement"]["attempts"]
        for key in ("started", "observed", "timed_out", "excluded"):
            assert attempts[key]["count"]["value"] == prop[key], f"{where}: {key}"
        records = run["output"]["requests"]
        assert len(records) == prop["started"]
        assert sum(r["outcome"] == "completed" for r in records) == prop["observed"]
        assert sum(r["outcome"] == "unfinished_at_timeout" and r["latency_ms"] is None for r in records) == prop["timed_out"]
        assert prop["started"] == prop["observed"] + prop["timed_out"] + prop["excluded"]
    elif kind == "fraction_within_threshold":
        records = runs[prop["run"]]["output"]["requests"]
        within = sum(r["latency_ms"] is not None and t10(r["latency_ms"]) <= t10(prop["threshold_ms"]) for r in records)
        assert (within, len(records)) == (prop["within"], prop["of"]), where
        required = int(prop["required_fraction"].split(".")[1])
        assert (within * 100 >= required * len(records)) is prop["met"], where
    elif kind == "all_attempt_p99_at_least":
        output = runs[prop["run"]]["output"]
        filled = [t10(r["latency_ms"]) if r["latency_ms"] is not None else t10(prop["treat_unfinished_as_ms"]) for r in output["requests"]]
        assert gen.fmt(gen.nearest_rank(filled, 99)) == prop["value_ms"], where
    elif kind == "inquiry_frozen_before_results":
        inquiry = manifest["frozen_inquiry"]
        assert inquiry["frozen_before_results"] is True
        assert all(inquiry.get(field) for field in prop["fields"]), where
    elif kind == "interval_within_margin":
        envelope = runs[prop["run"]]["envelope"]
        value = envelope["assertions"][0]["value"]
        assert (value["lower"], value["upper"]) == (prop["lower_ms"], prop["upper_ms"]), where
        margin = manifest["frozen_inquiry"]["margin"]
        assert (margin["lower_ms"], margin["upper_ms"]) == (-prop["margin_ms"], prop["margin_ms"])
        inside = margin["lower_ms"] <= value["lower"] and value["upper"] <= margin["upper_ms"]
        assert (prop["outcome"] == "bounded_equivalence_supported") is inside, where
    elif kind == "comparability_limits_conclusion":
        assert prop["reasons"] == ["warmup", "workload_mix"]
    else:
        raise AssertionError(f"{where}: unknown assert {kind}")


def check_regeneration():
    files = gen.generated()
    expected_paths = set(files) | set(gen.STATIC)
    on_disk = {str(p.relative_to(FAMILY)) for p in FAMILY.rglob("*") if p.is_file()}
    assert on_disk == expected_paths, f"file set differs: {sorted(on_disk ^ expected_paths)}"
    for rel, data in files.items():
        assert (FAMILY / rel).read_bytes() == data, f"not byte-identical: {rel}"
    listing = load(FAMILY / "digests.json")
    for item in listing["files"]:
        data = (FAMILY / item["path"]).read_bytes()
        assert hashlib.sha256(data).hexdigest() == item["sha256"] and len(data) == item["bytes"], item["path"]
    return files


def check_scope():
    text = " ".join(p.read_text(encoding="utf-8").lower() for p in FAMILY.rglob("*.md"))
    for term in ("wrk2", "hdrhistogram", "criterion.rs", "jmh"):
        assert term not in text, term


if __name__ == "__main__":
    files = check_regeneration()
    totals = [check_variant(name, files) for name in VARIANTS]
    check_scope()
    print(f"Verified Scenario B: {len(VARIANTS)} variants, {sum(t[0] for t in totals)} runs, {sum(t[1] for t in totals)} properties.")
