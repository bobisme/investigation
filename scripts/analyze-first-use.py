#!/usr/bin/env python3
"""Summarize first-use run directories into training/first-use/results.json."""

import argparse
from collections import defaultdict
import json
from pathlib import Path
import runpy
from statistics import fmean, median


ROOT = Path(__file__).resolve().parents[1]
RUNNER = runpy.run_path(str(ROOT / "scripts/run-first-use.py"))
ARMS = ("current", "revised")
FAILED = ("timed_out", "budget_exhausted", "crashed")


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def within(call, start, limit=3):
    return call is not None and start is not None and 0 <= call - start <= limit


def recount_contact(run, phase, counters):
    """Recompute CLI contact from the retained syscall traces (the source of truth)."""
    traces = run / f"{phase}.trace"
    if not traces.is_dir():
        return counters
    calls = sorted(int(path.stem) for path in traces.glob("*.log") if RUNNER["invoked_cli"](path))
    return dict(counters, first_contact_call=calls[0] if calls else None, cli_calls=len(calls))


def row(run):
    record = load(run / "run.json")
    a, phases, grade = record["assignment"], record["phases"], record["grade"]
    inv = recount_contact(run, "investigator", phases["investigator"])
    suc = recount_contact(run, "successor", phases["successor"])
    correct = grade["correct"]
    persisted = bool(record["handoff"]["persisted_workspaces"])
    answered = record["answer"] is not None
    return {
        "id": a["id"], "family": a["family"], "variant": a["variant"], "domain": a["domain"],
        "model": a["model"], "arm": a["arm"], "order": a["order"],
        "binary_sha256": record["environment"]["arm_binary"]["sha256"],
        "investigator": {key: inv.get(key) for key in (
            "status", "interruption", "tool_calls", "elapsed_ms", "input_tokens", "output_tokens",
            "first_contact_call", "first_onboard_call", "first_receipt_call", "source_case_call",
            "cli_calls", "cli_error_calls", "shell_timeouts", "discovery_bytes")},
        "successor": {key: suc.get(key) for key in (
            "status", "tool_calls", "elapsed_ms", "input_tokens", "output_tokens", "cli_calls",
            "cli_error_calls", "shell_timeouts")} | {"answered": answered},
        "handoff_operations": record["handoff"]["operations"],
        "persisted_source_and_case": persisted,
        "receipt_within_three_after_contact": within(inv.get("source_case_call"), inv.get("first_contact_call")),
        "receipt_within_three_after_onboard": (
            within(inv.get("source_case_call"), inv.get("first_onboard_call"))
            if inv.get("first_onboard_call") else None),
        "grade": {"answer": correct["current_answer"], "citation": correct["decisive_citation"],
                  "limitation": correct["material_limitation"],
                  "unwarranted": sorted(name for name, hit in grade["unwarranted"].items() if hit),
                  "cited_lines": grade["citations"].get("source.txt", [])},
        "source_linked_handoff": persisted and answered and correct["decisive_citation"] and correct["material_limitation"],
        "total_ms": inv["elapsed_ms"] + suc["elapsed_ms"],
    }


def upper_bound_zero(n):
    """Exact one-sided 95% upper bound for a rate with zero events in n trials."""
    return round(1 - 0.05 ** (1 / n), 3) if n else None


def count(rows, predicate):
    hits = sum(1 for item in rows if predicate(item))
    value = {"count": hits, "of": len(rows)}
    if hits == 0:
        value["upper_95"] = upper_bound_zero(len(rows))
    return value


def arm_summary(rows):
    inv = [item["investigator"] for item in rows]
    receipt_calls = [item["source_case_call"] for item in inv if item["source_case_call"] is not None]
    contact_gaps = [item["source_case_call"] - item["first_contact_call"] for item in inv
                    if item["source_case_call"] is not None and item["first_contact_call"] is not None]
    return {
        "assignments": len(rows),
        "receipt_within_three_after_contact": count(rows, lambda r: r["receipt_within_three_after_contact"]),
        "receipt_within_three_after_onboard": count(
            [r for r in rows if r["receipt_within_three_after_onboard"] is not None],
            lambda r: r["receipt_within_three_after_onboard"]),
        "ran_onboard": count(rows, lambda r: r["investigator"]["first_onboard_call"] is not None),
        "persisted_source_and_case": count(rows, lambda r: r["persisted_source_and_case"]),
        "source_linked_handoff": count(rows, lambda r: r["source_linked_handoff"]),
        "successor_answered": count(rows, lambda r: r["successor"]["answered"]),
        "successor_citation": count(rows, lambda r: r["grade"]["citation"]),
        "successor_limitation": count(rows, lambda r: r["grade"]["limitation"]),
        "successor_answer": count(rows, lambda r: r["grade"]["answer"]),
        "unwarranted_claims": count(rows, lambda r: bool(r["grade"]["unwarranted"])),
        "successor_used_cli": count(rows, lambda r: (r["successor"]["cli_calls"] or 0) > 0),
        "investigator_failed_cli_calls": sum(item["cli_error_calls"] or 0 for item in inv),
        "investigator_cli_calls": sum(item["cli_calls"] or 0 for item in inv),
        "investigator_budget_or_timeout": count(rows, lambda r: r["investigator"]["status"] in FAILED),
        "successor_budget_or_timeout": count(rows, lambda r: r["successor"]["status"] in FAILED),
        "shell_call_timeouts": sum((r["investigator"]["shell_timeouts"] or 0) + (r["successor"]["shell_timeouts"] or 0)
                                   for r in rows),
        "median_source_case_call": median(receipt_calls) if receipt_calls else None,
        "median_calls_contact_to_source_case": median(contact_gaps) if contact_gaps else None,
        "mean_total_seconds": round(fmean(r["total_ms"] for r in rows) / 1000, 1) if rows else None,
        "mean_investigator_calls": round(fmean(item["tool_calls"] for item in inv), 1) if rows else None,
    }


def summarize(rows, comparison):
    by_arm = {arm: [r for r in rows if r["arm"] == arm] for arm in ARMS}
    summary = {"by_arm": {arm: arm_summary(items) for arm, items in by_arm.items() if items}}
    models = sorted({r["model"] for r in rows})
    summary["by_model"] = {model: {arm: arm_summary([r for r in items if r["model"] == model])
                                   for arm, items in by_arm.items() if any(r["model"] == model for r in items)}
                           for model in models}
    pairs = defaultdict(dict)
    for r in rows:
        pairs[(r["family"], r["variant"], r["model"])][r["arm"]] = r
    matched = [pair for pair in pairs.values() if set(pair) == set(ARMS)]
    if matched:
        differences = [pair["revised"]["total_ms"] - pair["current"]["total_ms"] for pair in matched]
        current = fmean(pair["current"]["total_ms"] for pair in matched)
        revised = fmean(pair["revised"]["total_ms"] for pair in matched)
        summary["paired"] = {
            "pairs": len(matched),
            "mean_total_seconds": {"current": round(current / 1000, 1), "revised": round(revised / 1000, 1)},
            "mean_difference_seconds": round(fmean(differences) / 1000, 1),
            "revised_to_current_time_ratio": round(revised / current, 3),
            "pairs_revised_faster": sum(difference < 0 for difference in differences),
            "source_linked_handoff": {arm: sum(pair[arm]["source_linked_handoff"] for pair in matched) for arm in ARMS},
            "successor_citation_and_limitation": {
                arm: sum(pair[arm]["grade"]["citation"] and pair[arm]["grade"]["limitation"] for pair in matched)
                for arm in ARMS},
        }
    summary["comparison"] = comparison
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", type=Path, nargs="+", help="Run output directories from run-first-use.py")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    rows, manifests = [], []
    for directory in args.runs:
        manifest = load(directory / "manifest.json")
        manifests.append({"directory": directory.name, "role": manifest["role"], "arms": manifest["arms"],
                          "seed": manifest["seed"], "models": manifest["environment"]["models"],
                          "binaries": manifest["environment"]["binaries"],
                          "runner_sha256": manifest["environment"]["runner_sha256"],
                          "fixture_commit": manifest["environment"]["fixture_commit"],
                          "blocks": manifest["blocks"]})
        rows += [row(run.parent) for run in sorted(directory.glob("*/run.json"))]
    rows.sort(key=lambda r: (r["model"], r["family"], r["order"]))
    comparison = {"runs": [m["directory"] for m in manifests]}
    value = {"schema": "first-use-results.v1", "runs": manifests, "assignments": rows,
             "comparison": comparison, "summary": summarize(rows, comparison)}
    args.output.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
