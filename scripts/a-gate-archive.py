#!/usr/bin/env python3
"""Report progress or verify the completeness and integrity of a pilot archive."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
PHASES = ("investigator", "successor")
STATUSES = {"completed", "timed_out", "budget_exhausted", "crashed", "invalidated"}


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def digest(file):
    with file.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def workspace_digest(work):
    manifest = [[file.relative_to(work).as_posix(), digest(file)] for file in sorted(work.rglob("*"))
                if file.is_file() and not file.is_symlink()]
    return hashlib.sha256(json.dumps(manifest).encode()).hexdigest()


def planned_assignments(manifest):
    planned = {}
    for block in manifest["blocks"]:
        assert set(block["order"]) == {"notes", "inv"} and len(block["order"]) == 2
        for position, arm in enumerate(block["order"], 1):
            key = (block["family"], block["variant"], block["model"], arm)
            assert key not in planned, key
            planned[key] = block["block_seed"], position
    return planned


def records(archive, manifest):
    planned = planned_assignments(manifest)
    present = {}
    for file in sorted(archive.glob("*/run.json")):
        data = load(file)
        assignment = data["assignment"]
        key = (assignment["family"], assignment["variant"], data["environment"]["model"], assignment["arm"])
        assert key not in present and key in planned, key
        assert (assignment["block_seed"], assignment["order"]) == planned[key], key
        present[key] = (data, file.parent)
    return planned, present


def progress(planned, present):
    counts = Counter()
    for _, _, model, _ in present:
        counts[model] += 1
    return {"planned": len(planned), "terminal": len(present), "by_model": dict(counts)}


def validate_phase(directory, phase, record, budget, sessions):
    session_id = record["session_id"]
    assert session_id not in sessions
    sessions.add(session_id)
    assert record["status"] in STATUSES
    assert record["tool_calls"] <= budget["tool_calls"]
    path = directory / record["transcript"]
    events = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    assert events[0]["event"] == "started" and events[0]["session_id"] == session_id
    assert events[0]["phase"] == phase
    assert events[-1]["event"] == "finished" and events[-1]["status"] == record["status"]
    shell = [event for event in events if event["event"] == "shell"]
    assert len(shell) == record["tool_calls"]
    for field in ("discovery_bytes", "evidence_bytes", "note_read_bytes", "note_write_bytes", "preparation_calls"):
        assert sum(event["accounting"].get(field, 0) for event in shell) == record[field], (phase, field)
    for event in shell:
        if event["trace"]:
            assert (path.with_suffix(".trace") / event["trace"]).is_file(), path
    return events[0]["messages"]


def validate(manifest, planned, present, binary):
    assert present.keys() == planned.keys(), sorted(planned.keys() - present.keys())
    environment = manifest["environment"]
    subprocess.run(["git", "-C", ROOT, "merge-base", "--is-ancestor", environment["decision_adr_commit"],
                    environment["fixture_commit"]], check=True)
    if binary is not None:
        assert digest(binary) == environment["binary_sha256"], binary
    registry = load(ROOT / "fixtures/family-registry.json")
    variants = {(family["id"], variant["id"]): variant for family in registry["families"]
                for variant in family["variants"]}
    sessions = set()
    prompts = {}
    dispositions = Counter()
    for key, (data, directory) in present.items():
        assert data["assignment"]["id"] == directory.name, key
        assert data["budgets"] == manifest["budgets"], key
        assert data["environment"] == dict(environment, model=key[2]), key
        handoff = directory / "handoff"
        assert workspace_digest(handoff) == data["boundary"]["work_digest"], key
        assert not any((handoff / name).exists() for name in ("review.ready", "handoff.ready")), key
        variant = variants[key[:2]]
        assert digest(handoff / "source.txt") == variant["initial"]["sha256"], key
        update = variant.get("update")
        if update:
            assert digest(handoff / "update.txt") == update["sha256"] == data["boundary"]["update_sha256"], key
        else:
            assert not (handoff / "update.txt").exists()
        review = directory / "review"
        if data["boundary"]["review_status"] == "signaled":
            assert workspace_digest(review) == data["boundary"]["review_digest"], key
            assert not (review / "update.txt").exists(), key
        for phase in PHASES:
            record = data["phases"][phase]
            initial = validate_phase(directory, phase, record, data["budgets"][phase], sessions)
            prompt_key = (*key[:3], phase)
            assert prompts.setdefault(prompt_key, initial) == initial, prompt_key
            dispositions[phase, record["status"]] += 1
        answer = load(directory / "answer.json")
        if answer is not None:
            assert set(answer) == {"answer", "citation", "limitation", "next"}, key
            assert all(isinstance(value, str) and value for value in answer.values()), key
            dispositions["successor", "answer"] += 1
    return {phase: {status: count for (p, status), count in sorted(dispositions.items()) if p == phase}
            for phase in PHASES}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--progress", action="store_true")
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    manifest = load(args.archive / "manifest.json")
    planned, present = records(args.archive, manifest)
    result = progress(planned, present)
    if not args.progress:
        result["validated"] = validate(manifest, planned, present, args.binary)
    print(json.dumps(result, indent=2))
