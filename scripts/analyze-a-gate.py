#!/usr/bin/env python3
"""Summarize frozen blinded grades and apply the paired A-gate rule."""

import argparse
from collections import Counter, defaultdict
from fractions import Fraction
import json
from pathlib import Path
import random
from statistics import fmean, median, variance


ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "training/a-gate/archive"
ARMS = ("notes", "inv")
PHASES = ("investigator", "successor")
METRICS = ("accuracy", "false_promotion_rate")
COST = ("input_tokens", "output_tokens", "tool_calls", "elapsed_ms", "discovery_bytes",
        "evidence_bytes", "note_read_bytes", "note_write_bytes", "preparation_calls")
RECORD_KIND = dict(case="case", src="source", art="artifact", obs="observation", rev="review",
                   opt="option", crit="criterion", cmp="comparison", dec="decision",
                   asm="assumption", retro="retrospective", hyp="hypothesis",
                   inq="inquiry", run="run", concl="conclusion")


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def metrics(row):
    return {
        "accuracy": Fraction(sum(row["correct"].values()), len(row["correct"])),
        "false_promotion_rate": Fraction(sum(row["unwarranted"].values()), len(row["unwarranted"])),
    }


def average(items):
    return sum(items, Fraction()) / len(items)


def family_means(pairs):
    families = defaultdict(list)
    for key, entries in pairs.items():
        families[(key[0], key[1])].append(entries)
    return {
        family: {arm: {metric: average([metrics(pair[arm])[metric] for pair in observations])
                       for metric in METRICS} for arm in ARMS}
        for family, observations in families.items()
    }


def effects(families):
    return {
        family: {metric: value["inv"][metric] - value["notes"][metric] for metric in METRICS}
        for family, value in families.items()
    }


def outcome(pairs):
    families = family_means(pairs)
    by_arm = {
        arm: {metric: float(average([row[arm][metric] for row in families.values()]))
              for metric in METRICS} for arm in ARMS
    }
    differences = effects(families)
    counts = {}
    for arm in ARMS:
        observations = [pair[arm] for pair in pairs.values()]
        counts[arm] = {
            "warranted_correct": sum(sum(row["correct"].values()) for row in observations),
            "warranted_total": sum(len(row["correct"]) for row in observations),
            "missed_warranted": sum(row["missed_warranted"] for row in observations),
            "false_promotions": sum(sum(row["unwarranted"].values()) for row in observations),
            "promotion_opportunities": sum(len(row["unwarranted"]) for row in observations),
            "all_items_and_no_promotion": sum(all(row["correct"].values()) and
                                              not any(row["unwarranted"].values()) for row in observations),
        }
    return {
        "independent_families": len(families), "paired_observations": len(pairs),
        "arms": by_arm, "counts": counts,
        "difference": {metric: float(average([row[metric] for row in differences.values()]))
                       for metric in METRICS},
    }


def interval(values):
    sorted_values = sorted(values)
    return [sorted_values[round((len(sorted_values) - 1) * q)] for q in (.025, .975)]


def clustered_intervals(family_effects):
    generator = random.Random(831304)
    strata = defaultdict(list)
    for (domain, name), effect in sorted(family_effects.items()):
        strata[domain].append(effect)
    resampled = {metric: [] for metric in METRICS}
    estimated_variance = []
    for _ in range(10_000):
        draw = [row for rows in strata.values() for row in generator.choices(rows, k=len(rows))]
        for metric in METRICS:
            resampled[metric].append(fmean(row[metric] for row in draw))
        estimated_variance.append(variance(float(row["accuracy"]) for row in draw))
    return {metric: interval(resampled[metric]) for metric in METRICS}, interval(estimated_variance)


def collect_cost(observations):
    result = {}
    for arm in ARMS:
        rows = [o for o in observations if o["assignment"]["arm"] == arm]
        phases = {phase: [o["phases"][phase] for o in rows] for phase in PHASES}
        totals = {field: sum(item[field] for parts in phases.values() for item in parts) for field in COST}
        receipts = [p["first_receipt_call"] for p in phases["investigator"] if p["first_receipt_call"] is not None]
        from_onboard = [p["calls_from_onboard_to_receipt"] for p in phases["investigator"]
                        if p["calls_from_onboard_to_receipt"] is not None]
        first_note = [p["first_note_call"] for p in phases["investigator"] if p["first_note_call"] is not None]
        result[arm] = {
            "assignments": len(rows), "totals": totals,
            "mean_per_assignment": {field: value / len(rows) for field, value in totals.items()},
            "phase_status": {phase: dict(Counter(p["status"] for p in values))
                             for phase, values in phases.items()},
            "review_checkpoints": sum(o["boundary"]["review_status"] == "signaled" for o in rows),
            "receipt_calls": receipts, "onboard_to_receipt_calls": from_onboard, "first_note_calls": first_note,
            "first_receipt_median": median(receipts) if receipts else None,
            "onboard_to_receipt_median": median(from_onboard) if from_onboard else None,
            "first_note_median": median(first_note) if first_note else None,
        }
    return result


def latest_records(workspace):
    records = {}
    for path in sorted(workspace.rglob("state/cases/*/events/*.json")):
        if path.is_symlink():
            continue
        event = read(path)
        for record in event["accepted_operations"]["records"]:
            records[record["header"]["id"]] = record
    return records


def record_use(run_dirs):
    opportunities, used, retrieved = Counter(), Counter(), Counter()
    record_count, operations, post_review = Counter(), Counter(), Counter()
    hypothesis_issues = []
    retrospective_issues = []
    gate_counts = Counter()
    cache = {}
    for folder, run in run_dirs:
        assignment = run["assignment"]
        family, variant = assignment["family"], assignment["variant"]
        if assignment["arm"] != "inv" or assignment["domain"] == "lookup":
            continue
        boundary = run["boundary"]
        operations.update(boundary["handoff_operations"])
        post_review["investigator_review_submit_before_update"] += boundary["pre_update_operations"].get("review.submit", 0)
        post_review["investigator_decision_accept_before_update"] += boundary["pre_update_operations"].get("decision.accept", 0)
        if family not in cache:
            cache[family] = read(ROOT / f"training/a-gate/evaluator/pilot/{family}.json")
        offered = cache[family]["variants"][variant]["record_opportunities"]
        opportunities.update(offered)
        before = latest_records(folder / "handoff")
        successor = [json.loads(line) for line in (folder / "successor.jsonl").read_text().splitlines()]
        output = "\n".join(event["output"] for event in successor if event["event"] == "shell")
        kinds_present, kinds_retrieved = set(), set()
        for identifier, record in before.items():
            kind = RECORD_KIND.get(identifier.split("_", 1)[0])
            if kind is None:
                continue
            record_count[kind] += 1
            kinds_present.add(kind)
            if identifier in output:
                kinds_retrieved.add(kind)
            body = record["body"]
            if kind == "hypothesis":
                gate_counts["hypotheses"] += 1
                if not body.get("possible_falsifiers"):
                    hypothesis_issues.append(identifier)
            if kind == "retrospective":
                gate_counts["retrospectives"] += 1
                if not any(step.get("support_refs") for step in body.get("effective_steps", [])):
                    retrospective_issues.append(identifier)
        used.update(set(offered) & kinds_present)
        retrieved.update(set(offered) & kinds_retrieved)
    return {
        "record_count": dict(record_count), "opportunities": dict(opportunities),
        "used_at_opportunity": dict(used), "retrieved_at_opportunity": dict(retrieved),
        "omitted_at_opportunity": {name: count - used[name] for name, count in opportunities.items()},
        "operations": dict(operations), "boundary_operations": dict(post_review),
        "hypothesis_count": gate_counts["hypotheses"], "hypotheses_without_falsifier": hypothesis_issues,
        "retrospective_count": gate_counts["retrospectives"],
        "retrospectives_without_checkable_step": retrospective_issues,
    }


def analyze(raw):
    mapping = read(ARCHIVE / "assignment-map.json")
    grades = {entry["id"]: entry for entry in read(ARCHIVE / "blinded-grades.json")}
    assert grades.keys() == mapping.keys()
    paired = defaultdict(dict)
    run_dirs = []
    for identifier, assignment in mapping.items():
        key = tuple(assignment[field] for field in ("domain", "family", "variant", "model"))
        arm = assignment["arm"]
        assert arm not in paired[key]
        paired[key][arm] = grades[identifier]
        folder = raw / assignment["source_run"]
        run = read(folder / "run.json")
        assert run["assignment"]["arm"] == arm and run["assignment"]["family"] == key[1]
        run_dirs.append((folder, run))
    assert all(set(pair) == set(ARMS) for pair in paired.values())
    substantive = {key: value for key, value in paired.items() if key[0] != "lookup"}
    primary = outcome(substantive)
    per_family = effects(family_means(substantive))
    intervals, variance_interval = clustered_intervals(per_family)
    accuracy_variance = variance(float(row["accuracy"]) for row in per_family.values())
    exact = {metric: average([row[metric] for row in per_family.values()]) for metric in METRICS}
    relevant_runs = [run for _, run in run_dirs if run["assignment"]["domain"] != "lookup"]
    lookup_runs = [run for _, run in run_dirs if run["assignment"]["domain"] == "lookup"]
    return {
        "substantive": primary, "intervals": intervals,
        "between_family_accuracy_variance": accuracy_variance,
        "between_family_accuracy_variance_interval": variance_interval,
        "case_effects": {name: {metric: float(value) for metric, value in effect.items()}
                         for (_, name), effect in per_family.items()},
        "model_breakdown": {model: outcome({k: v for k, v in substantive.items() if k[3] == model})
                            for model in sorted({key[3] for key in substantive})},
        "domain_breakdown": {domain: outcome({k: v for k, v in substantive.items() if k[0] == domain})
                             for domain in ("architecture", "debugging")},
        "lookup": outcome({key: value for key, value in paired.items() if key[0] == "lookup"}),
        "costs": collect_cost(relevant_runs), "lookup_costs": collect_cost(lookup_runs),
        "record_use": record_use(run_dirs),
        "rule": {"accuracy_improves": exact["accuracy"] > 0,
                 "false_promotions_improve": exact["false_promotion_rate"] < 0,
                 "false_promotions_tie": exact["false_promotion_rate"] == 0,
                 "directional_gain": exact["accuracy"] > 0 or exact["false_promotion_rate"] < 0},
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("raw", type=Path)
    args = parser.parse_args()
    print(json.dumps(analyze(args.raw), indent=2, sort_keys=True))
