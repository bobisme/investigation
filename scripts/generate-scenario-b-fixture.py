#!/usr/bin/env python3
"""Generate the Scenario B fixture from a fixed seed. Pure integer arithmetic; byte-identical on every run.

Usage: generate-scenario-b-fixture.py [--write]
Without --write it only reports whether the files on disk match.
"""

import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FAMILY_DIR = "fixtures/development/scenario-b-latency-regression"
FAMILY = ROOT / FAMILY_DIR
STATIC = ("README.md", "roles/investigator.md", "roles/reviewer.md")
MASK = (1 << 64) - 1
BASE_SEED = 20261122
QUESTION = (
    "After the Marlin runtime upgrade from 1.8 to 1.9, did the change to the Ledgerd allocator cause a "
    "p99 latency regression, and does optimization A (arena reuse) improve p99 meaningfully?"
)
PRODUCER = {"name": "tidepool-bench", "version": "0.4.0", "instance": "fixture-a"}
ARTIFACT_NOTE = "synthetic bytes generated from a fixed seed with integer arithmetic"


class Rng:
    """xorshift64* over integers only, so no platform float behavior is involved."""

    def __init__(self, seed):
        self.x = ((seed * 0x9E3779B97F4A7C15) + 1) & MASK or 1

    def next(self):
        x = self.x
        x ^= x >> 12
        x ^= (x << 25) & MASK
        x ^= x >> 27
        self.x = x
        return ((x * 0x2545F4914F6CDD1D) & MASK) >> 32


def fmt(t10):
    """Tenths of a millisecond as a decimal string."""
    return f"{t10 // 10}.{t10 % 10}"


def j(value):
    return (json.dumps(value, indent=2) + "\n").encode("utf-8")


def known(value):
    return {"state": "known", "value": value}


UNKNOWN = {"state": "unknown"}


def nearest_rank(values, percent):
    ordered = sorted(values)
    return ordered[(percent * len(ordered) + 99) // 100 - 1]


def latencies(seed, n, base, spread, tail_den, tail_floor, tail_span, drift=0):
    rng = Rng(seed)
    out = []
    for i in range(n):
        v = base + rng.next() % spread + rng.next() % spread + rng.next() % spread
        if rng.next() % tail_den == 0:
            v += tail_floor + rng.next() % tail_span
        if i < drift:
            v += (drift - i) * 2
        out.append(v)
    return out


def leveled(values, build):
    per_process = max(len(values) // 4, 1)
    return {
        "shape": "leveled",
        "unit": "ms",
        "samples": [
            {
                "value": fmt(v),
                "order": known(i),
                "build": known(build),
                "process": known(f"p{i // per_process}"),
                "iteration": known(i % per_process),
            }
            for i, v in enumerate(values)
        ],
    }


def flat(values):
    return {"shape": "flat", "unit": "ms", "values": [fmt(v) for v in values]}


class Builder:
    def __init__(self, variant):
        self.variant = variant
        self.files = {}
        self.runs = []

    def put(self, rel, data):
        self.files[f"{self.variant}/{rel}"] = data
        return data

    def run(self, run_id, role, output, envelope, measurement, flags, intended_window_s=None):
        base = f"runs/{run_id}"
        raw = self.put(f"{base}/output.json", j(output))
        envelope["external_run_id"] = f"{self.variant}-{run_id}"
        envelope["artifacts"] = [
            {
                "role": "original-output",
                "path": "./output.json",
                "expected_sha256": hashlib.sha256(raw).hexdigest(),
                "media_type": "application/json",
                "classification": "internal",
            }
        ]
        env_bytes = self.put(f"{base}/envelope.json", j(envelope))
        meas_bytes = self.put(f"{base}/measurement.json", j(measurement))
        entry = {
            "id": run_id,
            "role": role,
            "envelope": f"{base}/envelope.json",
            "measurement": f"{base}/measurement.json",
            "output": f"{base}/output.json",
            "sha256": {
                "envelope": hashlib.sha256(env_bytes).hexdigest(),
                "measurement": hashlib.sha256(meas_bytes).hexdigest(),
                "output": hashlib.sha256(raw).hexdigest(),
            },
        }
        if intended_window_s is not None:
            entry["intended_observation_window_s"] = intended_window_s
        self.runs.append(entry)
        return flags


def envelope(variant, kind, revision, workload, mix, warmup_s, p99=None, controls=None, limitations=None, label=None):
    assertions = []
    if p99 is not None:
        assertions.append(
            {"kind": "reported_metric", "name": "latency.p99", "value": p99, "unit": "ms", "validity_claim": "producer_reported"}
        )
    return {
        "schema_version": "inv.producer-run.v1",
        "producer": dict(PRODUCER),
        "external_run_id": "",
        "intent": {"label": label or f"{kind} latency capture", "external_plan_digest": None},
        "execution": {
            "state": "completed",
            "exit_code": 0,
            "started_at": None,
            "ended_at": None,
            "clock_quality": "unverified",
        },
        "environment": {
            "revision": revision,
            "workload": workload,
            "workload_mix": mix,
            "warmup_s": str(warmup_s),
            "machine": "fixture-host-1",
        },
        "artifacts": [],
        "assertions": assertions,
        "controls": controls or [{"name": "steady-load", "producer_result": "reported_pass"}],
        "receipts": [],
        "limitations": limitations or [],
    }


def meas(n, build_label, samples, warmup_s, window_s, trial_order, loop="open", from_intended=True, population="all_attempts", **extra):
    block = {
        "unit": known("ms"),
        "workload": known("ledgerd-mixed-requests"),
        "observation_window": known({"value": str(window_s), "unit": "s"}),
        "warmup": known({"value": str(warmup_s), "unit": "s"}),
        "sample_count": known(n),
        "statistic": known(
            {"kind": "quantile", "quantile": "0.99", "population": known(population), "combination": known("single_run")}
        ),
        "samples": samples,
        "trial_order": known(trial_order),
        "load_model": known({"loop": known(loop), "timed_from_intended_send": known(from_intended)}),
        "environment_identity": known(
            {
                "runtime": known(build_label["runtime"]),
                "allocator": known(build_label["allocator"]),
                "host": known("fixture-host-1"),
            }
        ),
        "tool_identity": known({"harness": known("tidepool-bench 0.4.0"), "monitor": known("tidepool-monitor 0.3.1")}),
        "instrumentation_changed": known(False),
    }
    block.update(extra)
    return block


OLD = {"runtime": "marlin-1.8", "allocator": "slab-v1"}
NEW = {"runtime": "marlin-1.9", "allocator": "slab-v2"}
NEW_A = {"runtime": "marlin-1.9", "allocator": "slab-v2+arena-reuse"}


def baseline():
    b = Builder("baseline")
    specs = [
        ("control", "Control: runtime 1.8, old allocator. Comparable to treatment.", OLD, 11, 120, 40, 40, 100, 300, 30, "read90-write10", 0),
        ("treatment", "Treatment: runtime 1.9, new allocator. The comparable partner of control.", NEW, 12, 120, 40, 20, 160, 300, 30, "read90-write10", 0),
        ("treatment-debug-logging", "Confounded treatment: same build as treatment with extra debug logging switched on.", NEW, 13, 120, 40, 12, 300, 400, 30, "read90-write10", 0),
        ("optimization-a", "Optimization A on the new runtime. Short warm-up and a different workload mix.", NEW_A, 14, 120, 40, 40, 100, 300, 5, "read70-write30", 0),
    ]
    flags, p99s = {}, {}
    for run_id, role, build, off, base, spread, den, floor, span, warmup, mix, drift in specs:
        values = latencies(BASE_SEED + off, 300, base, spread, den, floor, span, drift)
        p99 = nearest_rank(values, 99)
        p99s[run_id] = fmt(p99)
        confounded = run_id == "treatment-debug-logging"
        extra = {"instrumentation_changed": known(True)} if confounded else {}
        if confounded:
            extra["limitations"] = ["Debug logging was enabled for this capture only; comparison with other runs is confounded."]
        m = meas(300, build, leveled(values, build["runtime"]), warmup, 120, "interleaved", **extra)
        env = envelope(
            "baseline", run_id, build["runtime"], "ledgerd-mixed-requests", mix, warmup, fmt(p99),
            limitations=["Single synthetic workload; not a production profile."]
            + (["Debug logging enabled during capture."] if confounded else []),
        )
        flags[run_id] = b.run(run_id, role, {"schema": "tidepool-bench.raw.v1", "unit": "ms", "latency_ms": [fmt(v) for v in values]}, env, m,
                              ["measurement.instrumentation_changed"] if confounded else [])
    props = [
        {"id": "p99_values_recomputable", "assert": "nearest_rank_p99", "unit": "ms", "per_run": p99s},
        {"id": "control_treatment_comparable", "assert": "comparable_pair", "runs": ["control", "treatment"],
         "same": ["workload", "workload_mix", "warmup", "observation_window", "load_model", "trial_order", "tool_identity"],
         "differs": ["runtime", "allocator"]},
        {"id": "regression_visible_in_comparable_pair", "assert": "p99_greater", "greater": "treatment", "lesser": "control", "min_difference_ms": "5.0",
         "scope": "this workload and these builds only"},
        {"id": "confounded_run_not_pooled", "assert": "run_confounded", "run": "treatment-debug-logging", "reason": "measurement.instrumentation_changed",
         "must_not_pool_with": ["treatment"], "same_build_as": "treatment", "p99_higher_than": "treatment"},
        {"id": "warmup_difference_noticeable", "assert": "recorded_field_differs", "runs": ["treatment", "optimization-a"], "field": "warmup", "values_s": [30, 5]},
        {"id": "workload_mix_difference_noticeable", "assert": "environment_differs", "runs": ["treatment", "optimization-a"], "field": "workload_mix",
         "values": ["read90-write10", "read70-write30"]},
        {"id": "optimization_a_not_comparable_to_treatment", "assert": "comparability_limits_conclusion", "runs": ["treatment", "optimization-a"],
         "reasons": ["warmup", "workload_mix"], "conclusion_ceiling": "bounded_to_workload_and_unresolved_for_effect"},
        {"id": "exit_zero_is_not_validity", "assert": "exit_does_not_imply_validity", "runs_exit_zero": sorted(flags), "run_not_valid": "treatment-debug-logging"},
        {"id": "advisory_flags", "assert": "derived_flags_equal", "per_run": flags},
        {"id": "pair_flags", "assert": "derived_pair_flags_equal", "pairs": [
            {"runs": ["control", "treatment"], "flags": []},
            {"runs": ["treatment", "optimization-a"], "flags": ["measurement.compared_warmup_differs"]},
        ]},
        {"id": "statistic_recorded", "assert": "statistic_recorded", "kind": "quantile", "quantile": "0.99", "population": "all_attempts",
         "runs": sorted(flags), "note": "Samples per run meet the order-statistic minimum, so no quantile flag fires."},
    ]
    traps = ["confounded_measurement", "warmup_mismatch", "workload_mix_mismatch", "process_exit_is_not_validity"]
    return b, props, traps, (
        "Treating the debug-logging run as an ordinary treatment sample, or comparing optimization A with the treatment as if "
        "warm-up and workload mix matched."
    )


def negative_short_window():
    b = Builder("negative-short-window")
    flags = {}
    p99s = {}
    for run_id, role, build, off, den in (
        ("control-short", "Control, 8 s window, 40 samples.", NEW, 21, 10**9),
        ("optimization-a-short", "Optimization A, 8 s window, 40 samples, same monitor.", NEW_A, 22, 10**9),
    ):
        values = latencies(BASE_SEED + off, 40, 120, 40, den, 160, 300)
        p99 = nearest_rank(values, 99)
        p99s[run_id] = fmt(p99)
        m = meas(40, build, flat(values), 5, 8, "interleaved",
                 tool_identity=known({"harness": known("tidepool-bench 0.4.0"), "monitor": known("tidepool-monitor 0.3.1"), "clock_source": UNKNOWN}),
                 limitations=["The monitor dropped samples; the capture ended after 8 s of a planned 300 s."])
        env = envelope("negative-short-window", run_id, build["runtime"], "ledgerd-mixed-requests", "read90-write10", 5, fmt(p99),
                       controls=[{"name": "monitor-self-check", "producer_result": "reported_fail"}],
                       limitations=["Monitor self-check failed.", "Capture stopped early; 8 s observed of 300 s planned."])
        flags[run_id] = b.run(run_id, role, {"schema": "tidepool-bench.raw.v1", "unit": "ms", "latency_ms": [fmt(v) for v in values]}, env, m,
                              ["measurement.flat_samples", "fixture.monitor_self_check_failed", "fixture.observation_window_below_intended",
                               "measurement.quantile_below_order_statistic_bound"], intended_window_s=300)
    props = [
        {"id": "p99_values_recomputable", "assert": "nearest_rank_p99", "unit": "ms", "per_run": p99s},
        {"id": "no_improvement_is_not_equality", "assert": "equality_claim_forbidden", "runs": ["control-short", "optimization-a-short"],
         "reasons": ["fixture.monitor_self_check_failed", "fixture.observation_window_below_intended", "measurement.quantile_below_order_statistic_bound"],
         "conclusion_ceiling": "inconclusive"},
        {"id": "no_fabricated_test_statistic", "assert": "inputs_contain_no_key", "keys": ["p_value", "power", "confidence_interval"],
         "note": "The core must not invent a p-value, power or interval; the fixture supplies none."},
        {"id": "negative_trial_preserved", "assert": "negative_result_retained", "runs": ["control-short", "optimization-a-short"]},
        {"id": "advisory_flags", "assert": "derived_flags_equal", "per_run": flags},
    ]
    return b, props, ["negative_without_adequate_conditions"], (
        "Recording 'no improvement detected' as evidence that optimization A has no effect."
    )


def sequential_order():
    b = Builder("sequential-order")
    flags, p99s, firsts = {}, {}, {}
    for run_id, role, build, off, drift, start in (
        ("a-first-control", "Run A (control), executed first; cold effects decay over its first 60 samples.", OLD, 31, 60, "2026-11-01T10:00:00Z"),
        ("b-second-treatment", "Run B (treatment), executed immediately after A on the same host.", NEW, 32, 0, "2026-11-01T10:05:00Z"),
    ):
        values = latencies(BASE_SEED + off, 300, 120, 40, 40 if off == 31 else 28, 100, 300, drift)
        p99 = nearest_rank(values, 99)
        p99s[run_id] = fmt(p99)
        quarter = len(values) // 4
        firsts[run_id] = {"first_quarter_mean_t10": sum(values[:quarter]) // quarter, "last_quarter_mean_t10": sum(values[-quarter:]) // quarter}
        m = meas(300, build, leveled(values, build["runtime"]), 30, 120, "sequential")
        env = envelope("sequential-order", run_id, build["runtime"], "ledgerd-mixed-requests", "read90-write10", 30, fmt(p99),
                       limitations=["Run order was A then B with no interleaving or repeat."])
        env["execution"]["started_at"] = start
        flags[run_id] = b.run(run_id, role, {"schema": "tidepool-bench.raw.v1", "unit": "ms", "latency_ms": [fmt(v) for v in values]}, env, m,
                              ["measurement.sequential_trial_order"])
    props = [
        {"id": "p99_values_recomputable", "assert": "nearest_rank_p99", "unit": "ms", "per_run": p99s},
        {"id": "sequential_order_flagged", "assert": "flag_present", "runs": ["a-first-control", "b-second-treatment"], "flag": "measurement.sequential_trial_order"},
        {"id": "order_confounds_difference", "assert": "order_effect_visible", "run": "a-first-control", "quarters": firsts["a-first-control"],
         "first_quarter_mean_exceeds_last_by_at_least_t10": 30,
         "meaning": "Early samples of the first run are slower, so any A-versus-B difference includes run order."},
        {"id": "no_paired_claim", "assert": "conclusion_ceiling", "runs": ["a-first-control", "b-second-treatment"], "ceiling": "order_confounded_not_paired_evidence"},
        {"id": "advisory_flags", "assert": "derived_flags_equal", "per_run": flags},
    ]
    return b, props, ["measurement.sequential_trial_order"], "Treating a single A-then-B comparison as paired or interleaved evidence of a regression."


def closed_loop():
    b = Builder("closed-loop-omission")
    rng = Rng(BASE_SEED + 41)
    n, interval, stall_at = 400, 100, 200
    records, done_prev = [], 0
    for i in range(n):
        service = 40 + rng.next() % 3 * 10 + rng.next() % 3 * 10 + rng.next() % 3 * 10
        if i == stall_at:
            service = 6000
        intended = i * interval
        actual = max(intended, done_prev)
        done = actual + service
        records.append({"seq": i, "intended_send_ms": fmt(intended), "actual_send_ms": fmt(actual), "done_ms": fmt(done)})
        done_prev = done
    from_actual = [int(r["done_ms"].replace(".", "")) - int(r["actual_send_ms"].replace(".", "")) for r in records]
    from_intended = [int(r["done_ms"].replace(".", "")) - int(r["intended_send_ms"].replace(".", "")) for r in records]
    p_actual, p_intended = nearest_rank(from_actual, 99), nearest_rank(from_intended, 99)
    m = meas(n, NEW, flat(from_actual), 30, 40, "interleaved", loop="closed", from_intended=False,
             limitations=["Generator waits for each response before sending the next request."])
    env = envelope("closed-loop-omission", "closed-loop-capture", NEW["runtime"], "ledgerd-mixed-requests", "read90-write10", 30, fmt(p_actual),
                   limitations=["Latency measured from actual send time."])
    flags = b.run("closed-loop-capture", "Closed-loop capture, latency from actual send time; one 600 ms stall at request 200 delays the schedule.",
                  {"schema": "tidepool-bench.raw.v1", "unit": "ms", "intended_interval_ms": "10.0", "requests": records}, env, m,
                  ["measurement.closed_loop_without_intended_send_timing", "measurement.flat_samples"])
    props = [
        {"id": "closed_loop_actual_send_flagged", "assert": "flag_present", "runs": ["closed-loop-capture"], "flag": "measurement.closed_loop_without_intended_send_timing"},
        {"id": "reported_p99_from_actual_send", "assert": "nearest_rank_p99", "unit": "ms", "per_run": {"closed-loop-capture": fmt(p_actual)}},
        {"id": "intended_send_p99_recomputable", "assert": "p99_from_intended_send", "run": "closed-loop-capture", "value_ms": fmt(p_intended),
         "at_least_times_reported": 5,
         "meaning": "Measured from the intended schedule in output.json the tail is far larger; the reported p99 omits the delayed requests."},
        {"id": "reported_p99_not_a_tail_bound", "assert": "conclusion_ceiling", "runs": ["closed-loop-capture"], "ceiling": "understates_tail_latency"},
        {"id": "advisory_flags", "assert": "derived_flags_equal", "per_run": {"closed-loop-capture": flags}},
    ]
    return b, props, ["coordinated_omission"], "Reporting the actual-send p99 of a closed-loop capture as the latency users would see."


def missing_outcome():
    b = Builder("missing-outcome")
    records = [{"id": i, "outcome": "completed", "latency_ms": "10.0"} for i in range(95)]
    records += [{"id": 95 + i, "outcome": "unfinished_at_timeout", "latency_ms": None} for i in range(5)]
    values = [100] * 95
    m = meas(95, NEW, flat(values), 30, 60, "interleaved",
             population="completed_attempts",
             attempts={
                 "started": {"count": known(100), "definition": known("requests sent during the observation window")},
                 "observed": {"count": known(95), "definition": known("requests with a completion before the timeout")},
                 "timed_out": {"count": known(5), "definition": known("requests with no completion at the 1000 ms timeout")},
                 "excluded": {"count": known(0), "definition": known("requests removed from analysis by the producer")},
             },
             limitations=["Unfinished requests carry no latency value; the timeout is 1000 ms."])
    env = envelope("missing-outcome", "timeout-capture", NEW["runtime"], "ledgerd-mixed-requests", "read90-write10", 30, "10.0",
                   limitations=["Reported p99 covers completed requests only."])
    flags = b.run("timeout-capture", "100 requests started, 95 finish in 10 ms, 5 unfinished at the 1 s timeout.",
                  {"schema": "tidepool-bench.raw.v1", "unit": "ms", "timeout_ms": "1000", "requests": records}, env, m,
                  ["measurement.completion_only_statistic", "measurement.flat_samples", "measurement.quantile_below_order_statistic_bound"])
    props = [
        {"id": "attempt_counts_preserved", "assert": "attempt_counts", "run": "timeout-capture",
         "started": 100, "observed": 95, "timed_out": 5, "excluded": 0},
        {"id": "completion_only_p99", "assert": "nearest_rank_p99", "unit": "ms", "per_run": {"timeout-capture": "10.0"}},
        {"id": "all_attempt_criterion_not_met", "assert": "fraction_within_threshold", "run": "timeout-capture", "threshold_ms": "25.0",
         "denominator": "started", "within": 95, "of": 100, "required_fraction": "0.99", "met": False},
        {"id": "completion_only_percentile_cannot_satisfy_criterion", "assert": "conclusion_ceiling", "runs": ["timeout-capture"],
         "ceiling": "criterion_unmet_or_unresolved_never_satisfied", "forbidden_claim": "99% of requests complete within 25 ms"},
        {"id": "all_attempt_p99_is_censored", "assert": "all_attempt_p99_at_least", "run": "timeout-capture", "treat_unfinished_as_ms": "1000.0", "value_ms": "1000.0"},
        {"id": "advisory_flags", "assert": "derived_flags_equal", "per_run": {"timeout-capture": flags}},
    ]
    return b, props, ["missing_outcomes_timeouts"], "Answering the '99% within 25 ms' requirement from the completion-only p99 of 10 ms."


def practical_equivalence():
    b = Builder("practical-equivalence")
    props_cmp = []
    for run_id, role, low, high in (
        ("interval-inside-band", "Externally supplied compatible interval for the p99 difference: [-2, +3] ms.", -2, 3),
        ("interval-crosses-band", "Externally supplied compatible interval for the p99 difference: [-8, +3] ms.", -8, 3),
    ):
        env = envelope("practical-equivalence", run_id, NEW_A["runtime"], "ledgerd-mixed-requests", "read90-write10", 30, None,
                       label="externally computed interval for the p99 difference, optimization A minus treatment")
        env["producer"] = {"name": "tidepool-stats", "version": "0.2.0", "instance": "fixture-a"}
        env["assertions"] = [{
            "kind": "reported_interval", "name": "latency.p99.difference", "unit": "ms",
            "value": {"lower": low, "upper": high, "contrast": "optimization-a minus treatment"}, "validity_claim": "producer_reported",
        }]
        env["controls"] = []
        env["limitations"] = ["Interval computed outside Investigation; method and confidence level not stated by the producer."]
        m = {"unit": known("ms"), "workload": known("ledgerd-mixed-requests"), "sample_count": UNKNOWN,
             "limitations": ["Only the supplied interval is recorded; no raw samples accompany it."]}
        b.run(run_id, role, {"schema": "tidepool-stats.interval.v1", "unit": "ms", "lower": str(low), "upper": str(high)}, env, m, [])
        props_cmp.append(run_id)
    props = [
        {"id": "frozen_inquiry_precedes_results", "assert": "inquiry_frozen_before_results", "fields": [
            "population", "unit", "aggregation", "replication_unit", "timeout_exclusion_policy", "margin"]},
        {"id": "inside_band_supports_bounded_equivalence", "assert": "interval_within_margin", "run": "interval-inside-band",
         "lower_ms": -2, "upper_ms": 3, "margin_ms": 5, "outcome": "bounded_equivalence_supported"},
        {"id": "crossing_band_leaves_equivalence_unresolved", "assert": "interval_within_margin", "run": "interval-crosses-band",
         "lower_ms": -8, "upper_ms": 3, "margin_ms": 5, "outcome": "equivalence_unresolved"},
        {"id": "no_invented_interval", "assert": "inputs_contain_no_key", "keys": ["p_value", "power"],
         "note": "The core invents neither interval; both intervals arrive in producer envelopes."},
        {"id": "advisory_flags", "assert": "derived_flags_equal", "per_run": {r: [] for r in props_cmp}},
    ]
    extra = {"frozen_inquiry": {
        "frozen_before_results": True,
        "population": "ledgerd mixed requests on fixture-host-1 at the steady-load level",
        "unit": "ms",
        "aggregation": "p99 per capture, nearest rank",
        "replication_unit": "one capture (process level)",
        "timeout_exclusion_policy": "requests unfinished at 1000 ms are counted as attempts and never dropped",
        "margin": {"lower_ms": -5, "upper_ms": 5},
    }}
    return b, props, ["practical_equivalence_margin"], "Treating 'no significant difference' as equivalence, or accepting the wider interval as inside the band.", extra


VARIANTS = (
    ("baseline", baseline, "Control and treatment comparable; one confounded debug-logging run; an optimization run with mismatched warm-up and workload mix."),
    ("negative-short-window", negative_short_window, "No improvement detected under a failed monitor and too short a window."),
    ("sequential-order", sequential_order, "One comparison executed A then B sequentially."),
    ("closed-loop-omission", closed_loop, "Closed-loop capture measured from actual send time."),
    ("missing-outcome", missing_outcome, "100 started, 95 finish in 10 ms, 5 unfinished at a 1 s timeout."),
    ("practical-equivalence", practical_equivalence, "Prespecified +-5 ms band with supplied intervals [-2, +3] and [-8, +3]."),
)


def build():
    files = {}
    for name, fn, summary in VARIANTS:
        result = fn()
        builder, props, traps, mistake = result[:4]
        extra = result[4] if len(result) > 4 else {}
        files.update(builder.files)
        manifest = {
            "schema_version": "inv.fixture.manifest.v1",
            "scenario": "B",
            "variant": name,
            "split": "development",
            "synthetic": True,
            "offline": True,
            "provider_specific_inputs": False,
            "immutable_sources": True,
            "fixture_environment": "deterministic offline synthetic records; no external service or real producer",
            "generator": {"script": "scripts/generate-scenario-b-fixture.py", "seed": BASE_SEED, "algorithm": "xorshift64*, integer arithmetic only", "note": ARTIFACT_NOTE},
            "question": QUESTION,
            "summary": summary,
            "mistake_offered": mistake,
            "traps": traps,
            "input_format": "inv.producer-run.v1 envelope per run, with the raw output as its artifact; measurement.json holds the typed measurement fields an investigator records for the run",
            "runs": builder.runs,
        }
        manifest.update(extra)
        files[f"{name}/manifest.json"] = j(manifest)
        expected = {"schema_version": "inv.fixture.expected.v1", "scenario": "B", "variant": name, "question": QUESTION, "properties": props}
        files[f"{name}/expected.json"] = j(expected)
    return files


def digests(files):
    entries = {path: data for path, data in files.items()}
    for rel in STATIC:
        entries[rel] = (FAMILY / rel).read_bytes()
    listing = [{"path": p, "sha256": hashlib.sha256(d).hexdigest(), "bytes": len(d)} for p, d in sorted(entries.items())]
    return j({"schema_version": "inv.fixture.digests.v1", "scope": "scenario-b-latency-regression", "algorithm": "sha256", "files": listing})


def generated():
    files = build()
    files["digests.json"] = digests(files)
    return files


if __name__ == "__main__":
    files = generated()
    if "--write" in sys.argv:
        for rel, data in files.items():
            path = FAMILY / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        print(f"Wrote {len(files)} files under {FAMILY_DIR}.")
    else:
        stale = [rel for rel, data in files.items() if not (FAMILY / rel).is_file() or (FAMILY / rel).read_bytes() != data]
        print("Up to date." if not stale else "Stale: " + ", ".join(stale))
        sys.exit(1 if stale else 0)
