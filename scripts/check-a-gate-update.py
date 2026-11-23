#!/usr/bin/env python3
"""Prove the same review-stage update reaches fresh successors in both arms."""

import argparse
import json
from pathlib import Path
import runpy
import tempfile


ROOT = Path(__file__).resolve().parents[1]
RUNNER = runpy.run_path(str(ROOT / "scripts/run-a-gate.py"))


def exercise(family, variant_name, binary):
    variant = next(v for v in family["variants"] if v["id"] == variant_name)
    block = {"family": family["id"], "variant": variant["id"], "domain": family["domain"],
             "model": "scripted-boundary-check", "block_seed": 91, "order": ["notes", "inv"]}
    budgets = {"investigator": {"seconds": 30, "tool_calls": 4, "input_tokens": 100, "output_tokens": 100},
               "successor": {"seconds": 30, "tool_calls": 2, "input_tokens": 100, "output_tokens": 100}}
    witnesses = []

    def backend(_path, payload, timeout):
        messages = payload["messages"]
        is_investigator = "review.ready" in messages[1]["content"]
        if is_investigator and len(messages) == 2:
            command = (
                "test ! -e update.txt && cat source.txt > NOTES.md && "
                "if command -v investigation >/dev/null; then "
                "investigation init casework --no-vault --json >/dev/null && "
                "investigation --workspace casework --actor agent:rehearsal start 'Review source' "
                "--attach - --command-id review-source --json < source.txt >/dev/null; fi && touch review.ready"
            )
        elif is_investigator:
            command = "test -s update.txt && cat update.txt >> NOTES.md && touch handoff.ready"
        elif len(messages) == 2:
            command = "test ! -e review.ready && cat update.txt"
        else:
            output = json.loads(messages[-1]["content"])
            assert output["exit"] == 0 and output["output"]
            answer = {
                "answer": output["output"].splitlines()[1],
                "citation": "update.txt line 2",
                "limitation": "Scripted check of byte delivery and the review boundary.",
                "next": "The pilot rubric checks the substantive follow-up.",
            }
            return message("submit_answer", answer)
        return message("shell", {"command": command})

    def message(name, arguments):
        return {"message": {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": name, "arguments": arguments}}]},
                "prompt_eval_count": 3, "eval_count": 2}

    with tempfile.TemporaryDirectory(prefix="investigation-a-gate-update-") as directory:
        root = Path(directory).resolve()
        for arm in block["order"]:
            output = root / arm
            output.mkdir()
            RUNNER["assignment"](block, arm, variant, output, binary, {}, budgets, backend)
            run = next(output.iterdir())
            record = json.loads((run / "run.json").read_text())
            assert record["boundary"]["review_status"] == "signaled"
            assert record["boundary"]["interruption"] == "handoff_marker"
            assert record["boundary"]["pre_update_operations"].get("case.create", 0) == int(arm == "inv")
            assert not (run / "review/update.txt").exists()
            assert not (run / "handoff/review.ready").exists()
            answer = json.loads((run / "answer.json").read_text())
            assert answer["answer"] == (run / "handoff/update.txt").read_text().splitlines()[1]
            witnesses.append((record["boundary"]["update_sha256"], answer["answer"]))
        assert witnesses[0] == witnesses[1], witnesses


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    registry = json.loads((ROOT / "fixtures/family-registry.json").read_text())
    families = {family["id"]: family for family in registry["families"]}
    exercise(families["scenario-a-timeout"], "a-gate-rehearsal", binary)
    exercise(families["field-logger-autonomy"], "missing", binary)
    print("Both arms received the same correction and new hard requirement after review.")
