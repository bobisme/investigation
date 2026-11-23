#!/usr/bin/env python3
"""Summarize first-use run directories into a results.json.

Each row also lists the refused CLI calls of both phases (per-call friction):
the command, the cause, the repair hint the CLI printed, and whether the agent
got past the refusal. These are text heuristics over the transcripts; the
`friction` fields say exactly what they test.
"""

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re
import runpy
import shlex
from statistics import fmean, median


ROOT = Path(__file__).resolve().parents[1]
RUNNER = runpy.run_path(str(ROOT / "scripts/run-first-use.py"))
COMPARISONS = RUNNER["COMPARISONS"]
FAILED = ("timed_out", "budget_exhausted", "crashed")
# Global options of the CLI that take a value, and those that take none.
GLOBAL_VALUE = {"--workspace", "--case", "--actor", "--session", "--format"}
GLOBAL_FLAG = {"--read-only", "--no-color", "--no-pager", "--json", "-v", "-vv", "-vvv", "-q", "--verbose", "--quiet"}
COMMANDS = {"init", "record", "ingest", "start", "evidence", "capture", "schema", "onboard", "docs", "next",
            "frontier", "blindspots", "guide", "method", "receipt", "show", "brief", "explain", "review-packet",
            "retrospective", "calibration", "search", "exposure", "check", "repair", "vault", "export", "config",
            "doctor", "integration", "observe", "note", "case", "source", "help", "version", "--help"}
GROUPS = {"evidence", "capture", "method", "repair", "vault", "integration", "case", "source", "config", "doctor"}
SHELL_OPERATORS = {"|", "||", "&&", ";", "&", ">", ">>", "2>&1", "2>/dev/null", "<"}
REPAIR = re.compile(r"^(?:Retry or inspect with|See) (investigation .+)$", re.M)
REASON = re.compile(r"\b([A-Z][A-Z0-9]*(?:_[A-Z0-9]+){2,})\b")


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
        "discovery": {phase: discovery_counts(run, phase) for phase in ("investigator", "successor")},
        "friction": friction_rows(run),
    }


def invocations(command):
    """Each `investigation` invocation in a shell command, as its verb path (`start`, `case new`)."""
    try:
        tokens = shlex.split(command, comments=True)
    except ValueError:
        tokens = command.split()
    found = []
    for index, token in enumerate(tokens):
        if not (token == "investigation" or token.endswith("/investigation")):
            continue
        words, rest = [], iter(tokens[index + 1:])
        for word in rest:
            if word in SHELL_OPERATORS or word.startswith(("|", ">", "2>")):
                break
            if word in GLOBAL_VALUE:
                next(rest, None)
                continue
            if word in GLOBAL_FLAG or word.split("=", 1)[0] in GLOBAL_VALUE:
                continue
            if word in ("-h", "--help") and not words:
                words.append("--help")
                break
            if word.startswith("-"):
                if words:
                    break
                continue
            words.append(word)
            if len(words) == 2 or words[0] not in GROUPS | {"help"}:
                break
        if words and words[0] not in COMMANDS:
            words = ["(unknown)"]
        elif len(words) == 2 and not re.fullmatch(r"[a-z][a-z-]*", words[1]):
            words[1] = "(unknown)"
        found.append(" ".join(words) or "(none)")
    return found


def refusal_cause(output):
    """A short cause for a refused call: the error code and its reason or parse problem."""
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get("ok") is False:
            error = value.get("error") or {}
            problem = parse_problem(error.get("message", ""))
            if problem and error.get("code") == "INVALID_INPUT":
                return problem
            if not problem:
                problem = "{} {}".format(error.get("path") or "", error.get("message", "")[:50]).strip()
            return "{} {}".format(error.get("code", "ERROR"), problem).strip()
    lines = [line for line in output.strip().splitlines() if not line.startswith("command_id:")]
    return parse_problem(output) or (lines or [""])[0][:60]


def parse_problem(message):
    patterns = (
        (r"unexpected argument '([^']{0,40})", "unexpected argument"),
        (r"unrecognized subcommand '([^']{0,40})", "unrecognized subcommand"),
        (r"required arguments were not provided:\s*\n\s*(\S+(?: <[^>]+>)?)", "missing"),
        (r"invalid value '[^']*' for '([^' ]+)", "invalid value for"),
        (r"no investigation\.toml found", "no workspace found"),
    )
    for pattern, label in patterns:
        match = re.search(pattern, message)
        if match:
            detail = match.group(1) if match.groups() else ""
            if label == "unexpected argument" and not detail.startswith("-"):
                detail = "positional"
            if label == "unrecognized subcommand" and len(detail) > 16:
                detail = "ID or text"
            return f"{label} {detail}".strip()
    match = REASON.search(message)
    return match.group(1) if match else ""


def repair_hint(output):
    for line in output.splitlines():
        if line.startswith("{"):
            try:
                value = json.loads(line)
            except ValueError:
                continue
            argv = ((value.get("error") or {}).get("repair") or {}).get("argv") if isinstance(value, dict) else None
            if argv:
                return " ".join(argv)
    match = REPAIR.search(output)
    return match.group(1).strip() if match else None


def transcript_calls(path):
    calls = []
    if not path.exists():
        return calls
    for line in path.read_text(encoding="utf-8").splitlines():
        event = json.loads(line)
        if event["event"] == "shell":
            command = event.get("command") or ""
            verbs = invocations(command)
            calls.append({"command": command, "verbs": verbs, "output": event["output"],
                          "error": bool(verbs and RUNNER["CLI_ERROR"].search(event["output"]))})
    return calls


def friction_rows(run):
    """Every refused CLI call of both phases, with its cause, hint and outcome.

    `unstuck`: a later call in the same phase ran the same verb path without a refusal.
    `hint_followed`: the next CLI call ran the verb path that the repair hint names.
    """
    rows = []
    for phase in ("investigator", "successor"):
        calls = transcript_calls(run / f"{phase}.jsonl")
        for number, call in enumerate(calls, 1):
            if not call["error"]:
                continue
            hint = repair_hint(call["output"])
            verb = call["verbs"][-1]
            later = calls[number:]
            following = next((item for item in later if item["verbs"]), None)
            hint_verbs = invocations(hint) if hint else []
            rows.append({
                "phase": phase, "call": number, "verb": verb, "command": call["command"][:240],
                "cause": refusal_cause(call["output"]), "hint": hint,
                "hint_followed": bool(following and hint_verbs and hint_verbs[0] in following["verbs"]),
                "unstuck": any(verb in item["verbs"] and not item["error"] for item in later),
            })
    return rows


def discovery_counts(run, phase):
    calls = transcript_calls(run / f"{phase}.jsonl")
    verbs = [verb for call in calls for verb in call["verbs"]]
    helps = sum(1 for call in calls if call["verbs"] and re.search(r"(^|\s)(--help|-h|help)(\s|$)", call["command"]))
    return {"help": helps, "onboard": verbs.count("onboard"),
            "schema": sum(verb == "schema" for verb in verbs),
            "docs": sum(1 for call in calls if re.search(r"investigation\S*\s+(?:\S+\s+)*?docs\s+(agents|skill)",
                                                         call["command"]))}


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
    } | friction_summary(rows)


def cause_row(rows, refused, verb, cause, calls):
    same = [item for item in refused if (item["verb"], item["cause"]) == (verb, cause)]
    return {"verb": verb, "cause": cause, "calls": calls,
            "assignments": len({r["id"] for r in rows for item in r["friction"] if item in same}),
            "with_hint": sum(item["hint"] is not None for item in same),
            "hint_followed": sum(item["hint_followed"] for item in same),
            "unstuck": sum(item["unstuck"] for item in same)}


def friction_summary(rows):
    """Refusal totals and causes; empty for results recorded before per-call friction."""
    if not rows or any("friction" not in r for r in rows):
        return {}
    refused = [item for r in rows for item in r["friction"]]
    investigator = [item for item in refused if item["phase"] == "investigator"]
    causes = Counter((item["verb"], item["cause"]) for item in refused)
    return {
        "refused_calls": {"investigator": len(investigator), "successor": len(refused) - len(investigator)},
        "refused_unstuck": count(refused, lambda item: item["unstuck"]),
        "refused_with_hint": count(refused, lambda item: item["hint"] is not None),
        "hint_followed": count([item for item in refused if item["hint"]], lambda item: item["hint_followed"]),
        "friction_causes": [cause_row(rows, refused, verb, cause, calls)
                            for (verb, cause), calls in sorted(causes.items(), key=lambda pair: (-pair[1], pair[0]))],
        "ran_docs_page": count(rows, lambda r: r["discovery"]["investigator"]["docs"] + r["discovery"]["successor"]["docs"] > 0),
        "ran_help": count(rows, lambda r: r["discovery"]["investigator"]["help"] > 0),
    }


def summarize(rows, comparison):
    baseline, treatment = comparison.get("arms", COMPARISONS[0])
    arms = (baseline, treatment)
    by_arm = {arm: [r for r in rows if r["arm"] == arm] for arm in arms}
    summary = {"by_arm": {arm: arm_summary(items) for arm, items in by_arm.items() if items}}
    models = sorted({r["model"] for r in rows})
    summary["by_model"] = {model: {arm: arm_summary([r for r in items if r["model"] == model])
                                   for arm, items in by_arm.items() if any(r["model"] == model for r in items)}
                           for model in models}
    pairs = defaultdict(dict)
    for r in rows:
        pairs[(r["family"], r["variant"], r["model"])][r["arm"]] = r
    matched = [pair for pair in pairs.values() if set(arms) <= set(pair)]
    if matched:
        differences = [pair[treatment]["total_ms"] - pair[baseline]["total_ms"] for pair in matched]
        before = fmean(pair[baseline]["total_ms"] for pair in matched)
        after = fmean(pair[treatment]["total_ms"] for pair in matched)
        summary["paired"] = {
            "pairs": len(matched),
            "mean_total_seconds": {baseline: round(before / 1000, 1), treatment: round(after / 1000, 1)},
            "mean_difference_seconds": round(fmean(differences) / 1000, 1),
            f"{treatment}_to_{baseline}_time_ratio": round(after / before, 3),
            f"pairs_{treatment}_faster": sum(difference < 0 for difference in differences),
            "source_linked_handoff": {arm: sum(pair[arm]["source_linked_handoff"] for pair in matched) for arm in arms},
            "successor_citation_and_limitation": {
                arm: sum(pair[arm]["grade"]["citation"] and pair[arm]["grade"]["limitation"] for pair in matched)
                for arm in arms},
        }
    if "targets" in comparison:
        summary["targets"] = evaluate(comparison["targets"], summary["by_arm"], baseline, treatment)
    summary["comparison"] = comparison
    return summary


def evaluate(targets, by_arm, baseline, treatment):
    """Apply preregistered targets (training/first-use/help-only/targets.json) to the arm summaries."""
    if baseline not in by_arm or treatment not in by_arm:
        return {"met": None, "reason": "both arms are needed"}
    base, arm = by_arm[baseline], by_arm[treatment]
    results = {}
    for name, target in targets["targets"].items():
        measure = target["measure"]
        if measure == "refused_unstuck":
            share = lambda s: s[measure]["count"] / s[measure]["of"] if s[measure]["of"] else 1.0
            value = round(share(arm) - share(base), 3)
            met = value >= -target["max_decrease"]
            detail = {"treatment": arm[measure], "baseline": base[measure]}
        elif measure == "refused_call_share":
            share = lambda s: s["investigator_failed_cli_calls"] / s["investigator_cli_calls"] if s["investigator_cli_calls"] else 0.0
            value = round(share(arm) - share(base), 3)
            met = value <= target["max_increase"]
            detail = {"treatment": round(share(arm), 3), "baseline": round(share(base), 3)}
        else:
            ours, theirs = arm[measure]["count"], base[measure]["count"]
            met = True
            if "max" in target:
                met = met and ours <= target["max"]
            if "margin" in target:
                met = met and ours >= theirs - target["margin"]
            if "min" in target:
                met = met and ours >= target["min"]
            value = ours
            detail = {"treatment": arm[measure], "baseline": base[measure]}
        results[name] = {"met": met, "value": value} | detail
    return {"met": all(item["met"] for item in results.values()), "targets": results}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", type=Path, nargs="+", help="Run output directories from run-first-use.py")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--targets", type=Path, help="Preregistered targets to evaluate, as JSON.")
    args = parser.parse_args()
    rows, manifests = [], []
    for directory in args.runs:
        manifest = load(directory / "manifest.json")
        manifests.append({"directory": directory.name, "role": manifest["role"], "arms": manifest["arms"],
                          "seed": manifest["seed"], "models": manifest["environment"]["models"],
                          "binaries": manifest["environment"]["binaries"],
                          "runner_sha256": manifest["environment"]["runner_sha256"],
                          "fixture_commit": manifest["environment"]["fixture_commit"],
                          "blocks": manifest["blocks"]}
                         | ({"docs_page": manifest["environment"]["docs_page"]}
                            if "docs_page" in manifest["environment"] else {}))
        rows += [row(run.parent) for run in sorted(directory.glob("*/run.json"))]
    rows.sort(key=lambda r: (r["model"], r["family"], r["order"]))
    arms = {tuple(m["arms"]) for m in manifests}
    comparison = {"runs": [m["directory"] for m in manifests]}
    pair = next((pair for pair in COMPARISONS if any(set(pair) == set(a) for a in arms)), None)
    if pair is not None and pair != COMPARISONS[0]:
        comparison["arms"] = list(pair)
    if args.targets:
        comparison["targets"] = load(args.targets)
    value = {"schema": "first-use-results.v1", "runs": manifests, "assignments": rows,
             "comparison": comparison, "summary": summarize(rows, comparison)}
    args.output.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
