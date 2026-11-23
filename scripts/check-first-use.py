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
    results = load(RESULTS)
    assert results["schema"] == "first-use-results.v1"
    rows = results["assignments"]
    # Recompute every summary from the per-assignment rows.
    assert results["summary"] == ANALYSIS["summarize"](rows, results["comparison"]), "stale summary"
    for row in rows:
        assert row["arm"] in ("current", "revised") and (KEYS / f"{row['family']}.json").exists()
    return len(rows)


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
    check_grading()
    rows = check_results()
    if "--shell" in sys.argv[1:]:
        check_shell()
    print(f"First-use checks passed: {roles['comparison']} comparison and {roles['tuning']} tuning families"
          + (f", {rows} recorded assignments." if rows is not None else ", no results recorded."))
