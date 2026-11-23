#!/usr/bin/env python3
"""Check first-use cases, keys, scheduling, grading and the checked-in results.

No model is called. `--shell` also runs one stubbed assignment through the
isolated participant shell (needs bubblewrap and strace).
"""

from collections import Counter
import json
from pathlib import Path
import runpy
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
KEYS = ROOT / "training/first-use/keys"
RESULTS = ROOT / "training/first-use/results.json"
FAMILY = runpy.run_path(str(ROOT / "scripts/check-fixture-families.py"))
RUNNER = runpy.run_path(str(ROOT / "scripts/run-first-use.py"))
ANALYSIS = runpy.run_path(str(ROOT / "scripts/analyze-first-use.py"))


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def check_cases():
    registry = load(FIXTURES / "family-registry.json")
    FAMILY["validate"](registry, FIXTURES)
    families = {family["id"]: family for family in registry["families"]}
    pilot_sources = {variant[stage]["sha256"] for family in registry["families"] if family["split"] != "development"
                     for variant in family["variants"] for stage in ("initial", "update") if stage in variant}
    roles = Counter()
    for path in sorted(KEYS.glob("*.json")):
        key = load(path)
        family = families[key["family"]]
        assert path.stem == key["family"] and family["split"] == "development", path
        assert Path(family["variants"][0]["directory"]).parts[:2] == ("development", "first-use"), path
        roles[key["role"]] += 1
        assert set(key["variants"]) == {variant["id"] for variant in family["variants"]}, path
        for variant in family["variants"]:
            assert "update" not in variant and variant["initial"]["sha256"] not in pilot_sources, path
            lines = FAMILY["source_path"](FIXTURES, variant, variant["initial"]).read_text(encoding="utf-8").splitlines()
            rubric = key["variants"][variant["id"]]
            ids = [item["id"] for item in rubric["warranted"]]
            assert {"current_answer", "decisive_citation", "material_limitation"} <= set(ids), path
            assert rubric["false_promotions"], path
            for item in rubric["warranted"] + rubric["false_promotions"]:
                if "citation" in item:
                    assert all(1 <= line <= len(lines) for line in item["citation"]["any_lines"]), path
                for pattern in item.get("all", []) + item.get("unless", []):
                    __import__("re").compile(pattern)
    assert roles["comparison"] >= 4 and roles["tuning"] >= 1, roles
    return roles


def check_schedule():
    families = [{"id": f"f{index}", "domain": "debugging", "variants": [{"id": "default"}]} for index in range(6)]
    blocks = RUNNER["schedule"](families, ["a", "b"], ("current", "revised"), 7)
    assert blocks == RUNNER["schedule"](families, ["a", "b"], ("current", "revised"), 7)
    for model in ("a", "b"):
        first = Counter(block["order"][0] for block in blocks if block["model"] == model)
        assert abs(first["current"] - first["revised"]) <= 1, first


def check_arms():
    """The help-only arm gets the first-use prompts verbatim; only the docs arm adds the page."""
    plain = RUNNER["prompts"]("help-only")
    assert plain == {"investigator": RUNNER["INVESTIGATE"], "successor": RUNNER["RESUME"]}
    assert "AGENTS.md" not in plain["investigator"] + plain["successor"]
    docs = RUNNER["prompts"]("docs", "# Page\nUse investigation onboard.\n")
    for phase in ("investigator", "successor"):
        assert docs[phase].startswith(plain[phase]) and docs[phase].endswith("Use investigation onboard.")
    families = [{"id": f"f{index}", "domain": "debugging", "variants": [{"id": "default"}]} for index in range(6)]
    old = RUNNER["schedule"](families, ["a", "b"], ("current", "revised"), 51701)
    new = RUNNER["schedule"](families, ["a", "b"], ("docs", "help-only"), 51701)
    # Same seed scheme: the same case order, block seeds and build-order pattern.
    assert [(b["family"], b["block_seed"]) for b in old] == [(b["family"], b["block_seed"]) for b in new]
    assert [b["order"][0] == "current" for b in old] == [b["order"][0] == "docs" for b in new]


def check_friction():
    verbs = ANALYSIS["invocations"]
    assert verbs("investigation --workspace ./w --actor a start 'Q' --attach f.txt") == ["start"]
    assert verbs("cd w && /tools/investigation case new 'T' | head; investigation --help") == ["case new", "--help"]
    assert verbs("investigation --format=json evidence read ART --selector-file s") == ["evidence read"]
    assert verbs("investigation hypothesis add 'x'") == ["(unknown)"]
    assert verbs("investigation case case_01a2 observe x") == ["case (unknown)"]
    cause, hint = ANALYSIS["refusal_cause"], ANALYSIS["repair_hint"]
    text = ("error: the following required arguments were not provided:\n  --on <RECORD_ID>\n\n"
            "Retry or inspect with investigation note --help --format json\n")
    assert cause(text) == "missing --on <RECORD_ID>" and hint(text) == "investigation note --help --format json"
    failure = json.dumps({"ok": False, "error": {"code": "INVALID_INPUT", "message":
                          "OBSERVATION_TEXT_QUOTE_NOT_FOUND: copy the quote", "repair": {"argv": ["investigation", "brief"]}}})
    assert cause("command_id: tx_1\n" + failure) == "OBSERVATION_TEXT_QUOTE_NOT_FOUND"
    assert hint(failure) == "investigation brief"
    assert cause("no investigation.toml found from /work up to the filesystem root.") == "no workspace found"
    targets = load(ROOT / "training/first-use/help-only/targets.json")
    assert targets["arms"] == ["docs", "help-only"] and set(targets["targets"]) == {
        "receipt", "handoff", "unsupported_claims", "refused_share", "recovery"}
    arm = {"receipt_within_three_after_contact": {"count": 7, "of": 12}, "source_linked_handoff": {"count": 4, "of": 12},
           "unwarranted_claims": {"count": 0, "of": 12}, "investigator_failed_cli_calls": 30,
           "investigator_cli_calls": 100, "refused_unstuck": {"count": 20, "of": 30}}
    base = dict(arm, receipt_within_three_after_contact={"count": 9, "of": 12}, investigator_failed_cli_calls=25)
    result = ANALYSIS["evaluate"](targets, {"docs": base, "help-only": arm}, "docs", "help-only")
    assert result["met"], result
    worse = dict(arm, unwarranted_claims={"count": 1, "of": 12}, receipt_within_three_after_contact={"count": 6, "of": 12})
    result = ANALYSIS["evaluate"](targets, {"docs": base, "help-only": worse}, "docs", "help-only")
    assert not result["met"] and not result["targets"]["receipt"]["met"]
    assert not result["targets"]["unsupported_claims"]["met"] and result["targets"]["handoff"]["met"]


def check_grading():
    key = load(KEYS / "river-gauge-record.json")["variants"]["default"]
    with tempfile.TemporaryDirectory() as directory:
        handoff = Path(directory)
        variant = next(v for f in load(FIXTURES / "family-registry.json")["families"]
                       if f["id"] == "river-gauge-record" for v in f["variants"])
        FAMILY["stage_source"](FIXTURES, variant, handoff, "initial")
        good = {"answer": "The record claim is not supported; 6.1 m is a provisional reading at a relocated gauge.",
                "citation": "source.txt line 16", "limitation": "Provisional data and a 2022 relocation.",
                "next": "Wait for the datum adjustment."}
        graded = RUNNER["grade"](good, key, handoff)
        assert all(graded["correct"].values()) and not any(graded["unwarranted"].values()), graded
        bad = dict(good, answer="The river reached a record 6.1 m.", citation="the newsletter")
        graded = RUNNER["grade"](bad, key, handoff)
        assert not graded["correct"]["decisive_citation"] and graded["unwarranted"]["promoted_record"], graded
        assert not any(RUNNER["grade"](None, key, handoff)["correct"].values())


def check_results():
    if not RESULTS.exists():
        return None
    count = 0
    targets = load(ROOT / "training/first-use/help-only/targets.json")
    for path in [RESULTS, *sorted((ROOT / "training/first-use/help-only").glob("run*/results.json"))]:
        results = load(path)
        assert results["schema"] == "first-use-results.v1", path
        rows = results["assignments"]
        # Recompute every summary from the per-assignment rows.
        assert results["summary"] == ANALYSIS["summarize"](rows, results["comparison"]), f"stale summary: {path}"
        arms = tuple(results["comparison"].get("arms", ("current", "revised")))
        for row in rows:
            assert row["arm"] in arms and (KEYS / f"{row['family']}.json").exists(), path
        if path != RESULTS:
            # Help-only runs are judged against the committed preregistered targets.
            assert results["comparison"]["targets"] == targets, f"targets differ from preregistration: {path}"
        count += len(rows)
    return count


def check_shell():
    family = next(f for f in load(FIXTURES / "family-registry.json")["families"] if f["id"] == "print-queue-stall")
    variant = family["variants"][0]
    key = load(KEYS / "print-queue-stall.json")["variants"]["default"]
    block = {"family": family["id"], "variant": "default", "domain": family["domain"], "model": "stub",
             "block_seed": 3, "order": ["current"]}
    budgets = {"investigator": {"seconds": 20, "tool_calls": 3, "input_tokens": 100, "output_tokens": 100},
               "successor": {"seconds": 20, "tool_calls": 1, "input_tokens": 100, "output_tokens": 100}}
    answer = {"answer": "Driver 4.2 likely caused the holds.", "citation": "source.txt line 13",
              "limitation": "Only the PS-2 log was exported.", "next": "Reinstall 4.2 on one server."}

    def stub(_path, payload, timeout):
        investigator = "handoff.ready" in payload["messages"][1]["content"]
        if investigator:
            call = {"name": "shell", "arguments": {"command": "printf x > NOTES.md && touch handoff.ready"}}
        elif len(payload["messages"]) == 2:
            call = {"name": "shell", "arguments": {"command": "test -s NOTES.md && test ! -e handoff.ready"}}
        else:
            call = {"name": "submit_answer", "arguments": answer}
        return {"message": {"role": "assistant", "content": "", "tool_calls": [{"function": call}]},
                "prompt_eval_count": 3, "eval_count": 2}

    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory).resolve()
        environment = {"binaries": {"current": {"sha256": "stub"}}}
        record = RUNNER["assignment"](block, "current", variant, key, output, None, environment, budgets, stub)
        assert record["phases"]["investigator"]["interruption"] == "handoff_marker", record
        assert record["phases"]["successor"]["tool_calls"] == 1
        assert record["handoff"]["persisted_workspaces"] == []
        assert record["grade"]["correct"] == {"current_answer": True, "decisive_citation": True,
                                              "material_limitation": True}


if __name__ == "__main__":
    roles = check_cases()
    check_schedule()
    check_arms()
    check_friction()
    check_grading()
    rows = check_results()
    if "--shell" in sys.argv[1:]:
        check_shell()
    print(f"First-use checks passed: {roles['comparison']} comparison and {roles['tuning']} tuning families"
          + (f", {rows} recorded assignments." if rows is not None else ", no results recorded."))
