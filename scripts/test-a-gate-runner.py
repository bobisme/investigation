#!/usr/bin/env python3
"""Check pilot scheduling, the durable review boundary, and a capped successor."""

from collections import Counter
import json
from pathlib import Path
import runpy
import tempfile


ROOT = Path(__file__).resolve().parents[1]
RUNNER = runpy.run_path(str(ROOT / "scripts/run-a-gate.py"))
REGISTRY = json.loads((ROOT / "fixtures/family-registry.json").read_text())


def test_schedule():
    families = [{"id": f"f{i}", "domain": domain, "variants": [{"id": "case"}]}
                for i, domain in enumerate(("debugging",) * 5 + ("architecture",) * 4)]
    models = ["a", "b"]
    schedule = RUNNER["schedule"]
    blocks = schedule(families, models, 1)
    assert len(blocks) == len(families) * len(models)
    assert blocks == schedule(families, models, 1)
    order = Counter((block["domain"], block["model"], block["order"][0]) for block in blocks)
    for model in models:
        for domain in ("debugging", "architecture"):
            assert abs(order[domain, model, "inv"] - order[domain, model, "notes"]) <= 1
    assert all(set(block["order"]) == {"inv", "notes"} for block in blocks)


def test_handoff():
    family = next(f for f in REGISTRY["families"] if f["id"] == "scenario-a-timeout")
    variant = next(v for v in family["variants"] if v["id"] == "a-gate-rehearsal")
    block = {"family": family["id"], "variant": variant["id"], "domain": family["domain"],
             "model": "rehearsal", "block_seed": 11, "order": ["notes", "inv"]}
    phases = {
        "investigator": {"seconds": 20, "tool_calls": 5, "input_tokens": 100, "output_tokens": 100},
        "successor": {"seconds": 20, "tool_calls": 1, "input_tokens": 100, "output_tokens": 100},
    }
    submitted = {name: name for name in RUNNER["ANSWER_FIELDS"]}

    def stub(_path, payload, timeout):
        count = len(payload["messages"])
        investigator = "review.ready" in payload["messages"][1]["content"]
        if investigator:
            name = "shell"
            command = ("test ! -e update.txt && printf 'reviewed' > NOTES.md && touch review.ready handoff.ready"
                       if count == 2 else "test -s update.txt && touch handoff.ready")
            arguments = {"command": command}
        elif count == 2:
            name = "shell"
            arguments = {"command": "test -s NOTES.md && test -s update.txt && test ! -e review.ready"}
        else:
            assert [tool["function"]["name"] for tool in payload["tools"]] == ["submit_answer"]
            name, arguments = "submit_answer", submitted
        return {"message": {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": name, "arguments": arguments}}]},
                "prompt_eval_count": 3, "eval_count": 2}

    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory).resolve()
        RUNNER["assignment"](block, "notes", variant, output, None, {}, phases, stub)
        run = next(output.iterdir())
        metadata = json.loads((run / "run.json").read_text())
        states = metadata["phases"]
        assert states["investigator"]["session_id"] != states["successor"]["session_id"]
        assert states["investigator"]["tool_calls"] == 2 and states["successor"]["tool_calls"] == 1
        assert metadata["boundary"]["interruption"] == "handoff_marker"
        assert metadata["boundary"]["update_sha256"] == variant["update"]["sha256"]
        assert "premature_handoff" in (run / "investigator.jsonl").read_text()
        assert not (run / "review/update.txt").exists()
        assert (run / "handoff/update.txt").is_file()
        assert json.loads((run / "answer.json").read_text()) == submitted


if __name__ == "__main__":
    import sys

    test_schedule()
    if "--shell" in sys.argv[1:]:
        test_handoff()
    print("A-gate runner checks passed.")
