#!/usr/bin/env python3
"""Run the deterministic acceptance measures of plan §20.5 and report them as one JSON object.

    python3 scripts/acceptance-measures.py [--output PATH] [--jobs N] [--offline]

Each measure has a fixed target that needs no agent trial: correct case targeting, zero
lost or duplicate writes, zero false promotions in invalid-control and stale-basis
fixtures, repairability of ordinary errors, context efficiency of compact reads,
blindspot calibration and domain neutrality. The table below names, for each measure,
the tests that prove it: most were written by earlier work and are run here as they
are; `crates/investigation-cli/tests/acceptance_measures.rs` adds what no earlier test
measured whole.

The script compiles the test executables with the flags `just test` uses (so after
`just check` nothing is rebuilt), runs only the named tests with
INV_ACCEPTANCE_OBSERVATIONS pointing at a scratch directory, runs the named scripts,
and writes the report (default target/acceptance/deterministic-report.json): per
measure, the target, the observed value the tests recorded, pass or fail, and every
test behind it with its outcome. A named test that no longer exists fails its measure.
Nothing here uses the network, an agent or the wall clock. Exit status 0 means every
measure passed.

These are deterministic checks, not release-trial results. A deterministic zero is a
count over fixed fixtures; an agent trial's "zero observed" is a sample and is reported
by scripts/release-trial.py with its upper bound.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
SCHEMA = "inv.acceptance.deterministic-report.v1"
CLI, CORE, STORE, RECORDING, RENDER = (
    "investigation-cli", "investigation-core", "investigation-store", "investigation-recording",
    "investigation-render")
STORE_LIB = (STORE, "investigation_store", "lib")
CORE_LIB = (CORE, "investigation_core", "lib")
NEW = "acceptance_measures"


def test(package, target, name, why, slow=False):
    """A test selector: libtest substring filter `name` in one test executable. `slow`
    tests (over about ten seconds) are skipped by --quick."""
    kind = "lib" if (package, target) in ((STORE, "investigation_store"), (CORE, "investigation_core")) else "test"
    return {"package": package, "target": target, "kind": kind, "filter": name, "why": why,
            "origin": "new" if target == NEW else "existing", "slow": slow}


def summed(observations, prefix, fields):
    return {field: sum(value.get(field, 0) for key, value in observations.items() if key.startswith(prefix))
            for field in fields}


# ------------------------------------------------------------------------- observed values

def observe_targeting(observations):
    shards = {key: value for key, value in observations.items() if key.startswith("targeting.fuzz-shard-")}
    if len(shards) != 3 or "targeting.retention-reclaim" not in observations:
        return None, "missing fuzz shard or retention reclaim observations"
    # The fuzz shards and the journal-less retention reclaim probes.
    total = summed(observations, "targeting.",
                   ("probes", "accepted", "refused", "misdirected", "cross_case_citations", "refusals_that_wrote"))
    wrong = total["misdirected"] + total["cross_case_citations"]
    total["correct_targeting_rate"] = None if not total["accepted"] else (total["accepted"] - wrong) / total["accepted"]
    total["cross_case_writes"] = wrong + total["refusals_that_wrote"]
    total["seeds"] = sorted({value["seed"] for value in shards.values()})
    total["commands_fuzzed"] = next(iter(shards.values()))["commands_fuzzed"]
    total["commands_probed_without_a_journal"] = ["retention reclaim"]
    total["commands_not_case_targeted"] = next(iter(shards.values()))["commands_not_case_targeted"]
    ok = total["accepted"] > 0 and total["correct_targeting_rate"] == 1 and total["cross_case_writes"] == 0
    return total, None if ok else "a write landed outside the case it named"


def observe_lost(observations):
    cli = observations.get("lost_duplicate_writes.cli-lost-response")
    if cli is None:
        return None, "no lost-response observation"
    observed = {"write_commands_retried": cli["commands"], "lost": cli["lost"], "duplicated": cli["duplicated"],
                "changed_requests_under_a_reused_key_not_refused": cli["changed_requests_not_refused"],
                "per_command": cli["per_command"],
                "process_termination": "crash schedules at every filesystem call of the commit, capture and "
                                       "closure paths (store crash, closure-drill and capture qualification tests)"}
    ok = cli["lost"] == 0 and cli["duplicated"] == 0 and cli["changed_requests_not_refused"] == 0
    return observed, None if ok else "a write was lost or duplicated"


def observe_false_promotion(observations, results):
    fixtures = [entry for entry in results if entry["outcome"] not in ("missing", "skipped")]
    failed = [entry["test"] for entry in fixtures if entry["outcome"] != "ok"]
    observed = {"fixture_tests": len(fixtures), "fixture_tests_failed": len(failed),
                "false_promotions": len(failed),
                "derivation": "each fixture test fails on any promotion of an invalid control or a stale "
                              "basis, so the count of failed fixture tests bounds false promotions from "
                              "above; warranted controls in the same tests still promote"}
    return observed, None if not failed else "a fixture promoted an invalid control or a stale basis"


def observe_repairability(observations):
    table = observations.get("repairability.error-table")
    if table is None:
        return None, "no repair table observation"
    observed = {"codes_published": table["codes_published"],
                "resolved_by_following_the_repair": table["resolved_by_following_the_repair"],
                "resolved_in_other_tests": table["resolved_in_other_tests"],
                "not_ordinary": table["not_ordinary"], "unresolved": table["unresolved"]}
    return observed, None if table["unresolved"] == 0 else "an ordinary error was not resolved by its repair"


def observe_context(observations):
    drills = {key.split(".", 1)[1]: value for key, value in observations.items()
              if key.startswith("context_efficiency.")}
    if len(drills) < 2:
        return None, "missing drill 1 observations"
    worst = max(value["compact_to_full_ratio"] for value in drills.values())
    smallest = min(value["records"] for value in drills.values())
    observed = {"fixtures": drills, "largest_compact_to_full_ratio": round(worst, 4),
                "byte_reduction": round(1 - worst, 4), "smallest_fixture_records": smallest,
                "caveats_retained": "drill 1 asserts the compact path shows every live Conclusion and "
                                    "Decision with its material caveat and names the open gap"}
    ok = worst <= 0.5 and smallest >= 40
    return observed, None if ok else "the compact read is not at least 50% smaller on a 40-record fixture"


def observe_blindspots(observations):
    audits = {key.split(".", 1)[1]: value for key, value in observations.items()
              if key.startswith("blindspot_calibration.")}
    if not audits:
        return None, "no drill audit observations"
    seeded = sum(len(value["seeded"]) for value in audits.values())
    detected = sum(value["detected"] for value in audits.values())
    claims = sum(value["completeness_claims"] for value in audits.values())
    observed = {"pinned_audits": len(audits), "seeded_gaps": seeded, "detected": detected,
                "completeness_claims": claims,
                "also_checked": "each registered check against its matched fixture (core blindspot "
                                "tests), task 9 seeded gaps reported once (trial-tasks-8-10 self-check), "
                                "and reviewer-added gaps outside the checks (drill 4)"}
    ok = seeded > 0 and detected == seeded and claims == 0
    return observed, None if ok else "a seeded gap was missed or completeness was claimed"


def observe_neutrality(observations):
    walk = observations.get("domain_neutrality.walkthroughs")
    if walk is None:
        return None, "no walkthrough observation"
    observed = {"walkthroughs": walk["walkthroughs"], "terms_scanned": walk["terms"],
                "findings": walk["findings"],
                "default_surfaces": "scripts/check-boundaries.py scans help, schemas, the general pack, "
                                    "agent docs and a fresh workspace for scripts/neutrality-denylist.txt"}
    return observed, None if not walk["findings"] else "a walkthrough needed a security abstraction or test run"


MEASURES = [
    {
        "id": "targeting",
        "measure": "Correct case/object targeting",
        "target": "100%; no cross-case writes.",
        "observe": observe_targeting,
        "tests": [
            test(CLI, NEW, "targeting_fuzz_shard_", "seeded fuzz of every case-targeting write command against a journal oracle"),
            test(CLI, NEW, "retention_reclaim_removes_only_bytes_of_the_case_it_names", "retention reclaim, which records nothing, removes only bytes of the case it names"),
            test(CLI, "retention", "a_workspace_sweep_is_a_dry_run_until_applied_and_names_no_record", "retention reclaim --all names no case: it refuses --case, is a dry run until --apply and names no record"),
            test(CLI, NEW, "every_write_command_is_fuzzed_for_targeting_or_names_no_case", "every write command in read_only::EFFECTS is fuzzed or names no case"),
            test(CLI, "cli", "record_retries_and_rejects_input_without_changing_the_case", "a --case that disagrees with the request is refused"),
            test(CLI, "note_on_case", "", "notes land on the selected case or the named record"),
            test(CLI, "lessons", "lessons_cross_cases_only_by_admission_and_explicit_consultation", "retrospective --consult crosses cases only through admitted lessons"),
            test(*STORE_LIB[:2], "store::scope::tests::an_id_recorded_in_an_unloaded_case_is_refused", "an ID of another case is refused"),
            test(*STORE_LIB[:2], "store::scope::tests::a_key_recorded_in_another_case_is_found_and_verified_through_the_index", "a command ID is unique across cases"),
        ],
    },
    {
        "id": "lost_duplicate_writes",
        "measure": "Lost/duplicate writes",
        "target": "Zero with injected response loss, retries and process termination.",
        "observe": observe_lost,
        "tests": [
            test(CLI, NEW, "every_write_command_retried_after_a_lost_response_is_recorded_exactly_once", "every write command: response dropped, identical retry, changed retry"),
            test(*STORE_LIB[:2], "store::crash_tests::", "seeded crash schedules over the commit path keep idempotency and atomic publication"),
            test(*STORE_LIB[:2], "store::closure_drill_tests::", "a crash at every filesystem call of a closure batch, then retry"),
            test(*STORE_LIB[:2], "store::capture::qualification_tests::", "capture protocol under crash, retry and interleaving"),
            test(*STORE_LIB[:2], "store::capture::finalize::tests::every_crash_point_of_a_finalization_is_completed_by_the_identical_retry", "finalize crash points"),
            test(*STORE_LIB[:2], "store::capture::items::tests::every_acknowledged_item_survives_a_crash_at_its_position", "acknowledged items survive a crash"),
            test(*STORE_LIB[:2], "store::capture::attach::tests::every_acknowledged_attachment_survives_a_crash_with_its_bytes", "acknowledged attachments survive a crash"),
            test(*STORE_LIB[:2], "store::real_crash_tests::local_filesystem_publication_boundaries_and_retry", "publication boundaries on the local filesystem"),
            test(*STORE_LIB[:2], "durable::tests::crash_boundaries_preserve_only_complete_published_bytes", "only complete bytes are published"),
            test(CLI, "vault_reconcile", "a_retry_under_the_same_command_id_returns_the_original_receipt", "a reconcile retry after a stop before the render, and after a lost response"),
            test(CLI, "drill_closure", "", "closure kill-and-retry drill through the binary"),
            test(CLI, "trial_tasks", "", "trial task 6 rehearsal: an injected lost response is retried without a duplicate", slow=True),
            test(CLI, "conformance", "", "a producer that re-records after a lost response fails conformance", slow=True),
            test(RECORDING, "append", "", "a resent append after a lost response returns the original receipt"),
        ],
    },
    {
        "id": "false_promotion",
        "measure": "False promotion and unnecessary abstention (deterministic fixtures)",
        "target": "Zero in deterministic invalid-control and stale-basis fixtures.",
        "observe_results": observe_false_promotion,
        "tests": [
            # Invalid controls.
            test(CLI, "cli", "measurement_validity_is_attributed_gates_support_and_is_shown_beside_each_use", "invalid-control: unknown or invalid measurements cannot support review; a valid one can"),
            test(STORE, "producer_run", "exit_code_zero_and_a_reported_pass_set_no_validity_and_promote_nothing", "invalid-control: a passing exit code promotes nothing"),
            test(*CORE_LIB[:2], "blindspot::causal::tests::scenario_a_refutation_on_a_different_build_yields_the_comparability_gap", "invalid-control: a refutation on a different build is a gap, not evidence"),
            test(CLI, "drills_3_4", "drill_3_explanatory_root_cause_ignores_a_corrupted_clock", "invalid-control: a corrupted clock trace stays a visible gap"),
            test(CLI, "objections", "critical_objections_block_validation_and_material_ones_stay_visible", "a critical objection blocks validation"),
            test(CLI, "guardrails", "the_session_that_read_untrusted_evidence_cannot_validate_and_solo_work_is_unchanged", "untrusted evidence cannot validate its own reader's claim"),
            # Stale bases.
            test(*CORE_LIB[:2], "evaluate_ops::tests::commits_exact_comparison_and_rejects_mismatch_and_stale", "stale-basis: a stale comparison is refused"),
            test(*CORE_LIB[:2], "decision_ops::tests::generated_unknown_tables_never_prove_dominance_and_threshold_changes_stale_basis", "stale-basis: a threshold change stales the decision basis"),
            test(STORE, "freshness_oracle", "a_rejected_stale_edit_changes_nothing", "stale-basis: a stale edit is refused and changes nothing"),
            test(*STORE_LIB[:2], "store::closure_drill_tests::a_batch_that_never_committed_cannot_be_retried_past_a_concurrent_revision_or_new_evidence", "stale-basis: a retry cannot pass a newer revision"),
            test(CLI, "adversarial_drills", "drill_2_", "stale-basis: a stale synopsis is never the current answer"),
            test(CLI, "freshness", "a_real_source_withdrawal_shows_needs_review_with_a_reason_chain_at_every_layer", "stale-basis: a withdrawn source turns dependents to needs_review"),
            test(RENDER, "export_reports", "a_stale_reviewed_conclusion_is_never_printed_as_simply_validated", "stale-basis: exports never print a stale review as validated"),
            test(CLI, "vault_scenarios", "scenario_c_marks_its_stale_review_on_the_review_and_on_the_decision", "stale-basis: Scenario C's changed criterion marks the decision"),
        ],
    },
    {
        "id": "repairability",
        "measure": "Repairability",
        "target": "Ordinary schema/CAS error resolved without bespoke intervention.",
        "observe": observe_repairability,
        "tests": [
            test(CLI, NEW, "each_ordinary_error_is_resolved_by_following_the_repair_it_returns", "every published error code: produced, unchanged workspace, repair followed, resolved"),
            test(CLI, "cli", "onboarding_and_budget_repairs_follow_the_public_cli_contract", "workspace, case and budget repairs"),
            test(CLI, "cli_conventions", "an_unknown_record_id_repairs_to_the_case_brief", "unknown record repair"),
            test(CLI, "drill_closure", "a_material_close_is_refused_with_a_repair_that_leads_to_one_idempotent_batch", "RETROSPECTIVE_REQUIRED repair"),
            test(CLI, "drill_closure", "new_material_evidence_after_the_account_requires_an_updated_one", "RETROSPECTIVE_STALE repair"),
            test(*STORE_LIB[:2], "store::capture::tests::a_retry_allocates_staging_that_a_crash_left_unallocated", "OBJECT_REQUIRED repair"),
            test(CLI, "doctor", "a_stale_index_is_a_specific_finding_whose_repair_restores_health", "a stale index repairs"),
        ],
    },
    {
        "id": "context_efficiency",
        "measure": "Context efficiency (compression regression check)",
        "target": "A compact 40-record fixture emits at least 50% fewer bytes than a full-record traversal, "
                  "keeping the necessary caveats.",
        "observe": observe_context,
        "tests": [
            test(CLI, "adversarial_drills", "drill_1_", "drill 1: compact brief, frontier and title reads against a full traversal of 40+ records"),
            test(CLI, "brief", "the_default_brief_of_a_large_stale_case_stays_within_budget_and_reports_omissions", "the default brief stays in budget and reports omissions"),
            test(CLI, "trial_tasks_8_10", "", "trial task 8 rehearsal: reference bytes and the read budget", slow=True),
        ],
    },
    {
        "id": "blindspot_calibration",
        "measure": "Blindspot calibration",
        "target": "All seeded structural gaps detected in matched fixtures; no claim of completeness for "
                  "unknown semantic gaps; a reviewer can record additional gaps safely.",
        "observe": observe_blindspots,
        "tests": [
            test(CLI, "drills_3_4", "", "drills 3-4: pinned seeded gaps, completeness language, reviewer-added gaps"),
            test(*CORE_LIB[:2], "blindspot::", "every registered check against its matched fixtures, and no completeness claim on a clean case"),
            test(*CORE_LIB[:2], "blindspot_review::", "reviews record novel gaps and can never assert exhaustive coverage"),
            test(CLI, "blindspots", "", "the blindspots command through the binary"),
            test(CLI, "trial_tasks_8_10", "", "trial task 9 rehearsal: every seeded gap reported once", slow=True),
        ],
    },
    {
        "id": "domain_neutrality",
        "measure": "Domain neutrality",
        "target": "Architecture and research tasks need no security abstractions or unrelated test-run ceremony.",
        "observe": observe_neutrality,
        "tests": [
            test(CLI, NEW, "architecture_and_research_walkthroughs_need_no_security_abstraction_or_test_run_ceremony", "Scenario C and D journals and their first reads"),
        ],
        "scripts": [["scripts/check-boundaries.py"]],
    },
]

# Rows of the §20.5 table that only an agent trial can measure.
TRIAL_MEASURES = {
    "First use and total burden": "release trial cost records (scripts/release-trial.py)",
    "Task success": "release trial, sealed cases, preregistered decision rule",
    "False promotion and unnecessary abstention (agent trials)": "release trial counts with denominators and upper bounds",
    "Continuation": "release trial successor answers",
    "Context efficiency (product claim)": "release trial selective reads and tokens against the notes arm",
    "Learning discipline": "release trial retrospectives (graded by scripts/grade-gate-content.py)",
}


# ------------------------------------------------------------------------- running

def package_name(manifest):
    found = re.search(r'(?m)^name\s*=\s*"([^"]+)"', Path(manifest).read_text(encoding="utf-8"))
    return found.group(1)


def build(offline):
    command = ["cargo", "test", "--locked", "--workspace", "--all-features", "--no-run",
               "--message-format=json-render-diagnostics"]
    if offline:
        command.append("--offline")
    child = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if child.returncode:
        sys.exit(f"cargo test --no-run failed:\n{child.stderr[-4000:]}")
    executables = {}
    for line in child.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if not message["profile"]["test"]:
            continue
        target = message["target"]
        key = (package_name(message["manifest_path"]), target["name"], target["kind"][0])
        executables[key] = {"path": message["executable"], "cwd": str(Path(message["manifest_path"]).parent)}
    return executables


def run_executable(executable, filters, env):
    started = time.monotonic()
    child = subprocess.run([executable["path"], "--color", "never", *filters], cwd=executable["cwd"], env=env,
                           capture_output=True, text=True)
    outcomes = dict(re.findall(r"(?m)^test (\S+) \.\.\. (ok|FAILED|ignored)", child.stdout))
    return {"exit": child.returncode, "outcomes": outcomes, "seconds": round(time.monotonic() - started, 2),
            "output": None if child.returncode == 0 else (child.stdout + child.stderr)[-6000:]}


def run_script(argv, env):
    started = time.monotonic()
    child = subprocess.run([sys.executable, *argv], cwd=ROOT, env=env, capture_output=True, text=True)
    return {"script": " ".join(["python3", *argv]), "outcome": "ok" if child.returncode == 0 else "FAILED",
            "seconds": round(time.monotonic() - started, 2),
            "output": None if child.returncode == 0 else (child.stdout + child.stderr)[-4000:]}


def versions():
    def tool(*command):
        try:
            return subprocess.run(command, cwd=ROOT, capture_output=True, text=True).stdout.strip()
        except OSError:
            return None
    return {"commit": tool("git", "rev-parse", "HEAD"),
            "uncommitted_changes": bool(tool("git", "status", "--porcelain", "--untracked-files=no")),
            "rustc": tool("rustc", "-V"), "python": platform.python_version()}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--output", type=Path, default=ROOT / "target/acceptance/deterministic-report.json")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    parser.add_argument("--offline", action="store_true", help="pass --offline to cargo")
    parser.add_argument("--quick", action="store_true",
                        help="skip the slow rehearsal tests; the report says which were skipped and is not complete")
    args = parser.parse_args()
    started = time.monotonic()
    executables = build(args.offline)

    wanted = {}
    for measure in MEASURES:
        for selector in measure["tests"]:
            if args.quick and selector["slow"]:
                continue
            key = (selector["package"], selector["target"], selector["kind"])
            wanted.setdefault(key, set()).add(selector["filter"])

    with tempfile.TemporaryDirectory(prefix="acceptance-observations-") as scratch:
        env = dict(os.environ, INV_ACCEPTANCE_OBSERVATIONS=scratch)
        if args.offline:
            env["CARGO_NET_OFFLINE"] = "true"
        runs = {}
        with ThreadPoolExecutor(max(1, args.jobs)) as pool:
            futures = {}
            for key, filters in wanted.items():
                if key not in executables:
                    runs[key] = None
                    continue
                # An empty filter runs the whole executable.
                chosen = [] if "" in filters else sorted(filters)
                futures[key] = pool.submit(run_executable, executables[key], chosen, env)
            scripts = {tuple(argv): pool.submit(run_script, argv, env)
                       for measure in MEASURES for argv in measure.get("scripts", [])}
            for key, future in futures.items():
                runs[key] = future.result()
            script_results = {key: future.result() for key, future in scripts.items()}
        observations = {}
        for path in sorted(Path(scratch).glob("*.json")):
            observations[path.stem] = json.loads(path.read_text(encoding="utf-8"))

    report = {
        "schema": SCHEMA,
        "kind": "deterministic_checks",
        "statement": "Deterministic acceptance checks of plan §20.5: scripted, offline checks of fixed "
                     "fixtures against fixed targets, through the tests named under each measure. They are "
                     "not release-trial results. A zero here is a count over fixed fixtures; an agent "
                     "trial's 'zero observed' is a sample, reported by scripts/release-trial.py with its "
                     "upper bound.",
        "plan": "notes/plan.md §20.5",
        "generated_by": "scripts/acceptance-measures.py",
        "versions": versions(),
        "measures": {},
        "trial_measures_not_covered": TRIAL_MEASURES,
    }
    for measure in MEASURES:
        results = []
        problems = []
        for selector in measure["tests"]:
            key = (selector["package"], selector["target"], selector["kind"])
            label = f"{selector['package']}/{selector['target']}"
            if args.quick and selector["slow"]:
                results.append({"test": label, "filter": selector["filter"], "outcome": "skipped",
                                "origin": selector["origin"], "why": selector["why"]})
                continue
            run = runs.get(key)
            if run is None:
                problems.append(f"no test executable {label}")
                results.append({"test": label, "filter": selector["filter"], "outcome": "missing",
                                "origin": selector["origin"], "why": selector["why"]})
                continue
            matched = {name: outcome for name, outcome in run["outcomes"].items() if selector["filter"] in name}
            if not matched:
                problems.append(f"{label}: no test matches {selector['filter']!r}")
                results.append({"test": label, "filter": selector["filter"], "outcome": "missing",
                                "origin": selector["origin"], "why": selector["why"]})
            for name, outcome in sorted(matched.items()):
                results.append({"test": f"{label}::{name}", "outcome": outcome, "origin": selector["origin"],
                                "why": selector["why"]})
                if outcome == "FAILED":
                    problems.append(f"{label}::{name} failed")
            if run["exit"] and not any(outcome == "FAILED" for outcome in run["outcomes"].values()):
                problems.append(f"{label} exited {run['exit']}: {run['output']}")
        for argv in measure.get("scripts", []):
            result = script_results[tuple(argv)]
            results.append({"test": result["script"], "outcome": result["outcome"], "origin": "existing",
                             "why": "deny-listed terms absent from the default surfaces"})
            if result["outcome"] != "ok":
                problems.append(f"{result['script']} failed: {result['output']}")
        if "observe_results" in measure:
            observed, problem = measure["observe_results"](observations, results)
        else:
            observed, problem = measure["observe"](observations)
        if problem:
            problems.append(problem)
        report["measures"][measure["id"]] = {
            "measure": measure["measure"],
            "target": measure["target"],
            "observed": observed,
            "pass": not problems,
            "problems": problems,
            "tests": results,
        }
        print(f"{'pass' if not problems else 'FAIL'}  {measure['id']}: {len(results)} tests"
              + ("" if not problems else f"; {problems[0][:300]}"), file=sys.stderr)
    report["passed"] = all(entry["pass"] for entry in report["measures"].values())
    report["complete"] = not args.quick
    report["skipped"] = sorted({entry["test"] for measure in report["measures"].values()
                                for entry in measure["tests"] if entry["outcome"] == "skipped"})
    report["seconds"] = round(time.monotonic() - started, 1)
    report["test_seconds"] = {f"{package}/{target}": run["seconds"] for (package, target, _), run in sorted(runs.items())
                              if run is not None}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"{'ACCEPTANCE-MEASURES-OK' if report['passed'] else 'ACCEPTANCE-MEASURES-FAILED'} "
          f"{args.output.relative_to(ROOT) if args.output.is_relative_to(ROOT) else args.output} "
          f"({report['seconds']} s)", file=sys.stderr)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
