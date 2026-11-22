#!/usr/bin/env python3
"""Check that a standard timeout handoff carries the finding, caveat, and evidence."""

import json
from pathlib import Path
import sys


def resume(brief):
    if brief["result"] != "brief" or not brief["ok"]:
        raise ValueError("Expected a committed case handoff")
    outcome, = [
        row for row in brief["outcomes"] if row["effective_status"]["state"] == "validated"
    ]
    if "asynchronous wait" not in outcome["summary"] or "build-7c2a" not in outcome["summary"]:
        raise ValueError("The handoff omits the scoped mechanism")
    caveat = outcome["material_caveat"]["value"]
    if "incompatible-build" not in caveat:
        raise ValueError("The handoff hides the unresolved scheduler alternative")
    evidence = next(
        reference for reference in outcome["evidence_refs"]
        if reference["id"].startswith(("art_", "obs_"))
    )
    return {
        "outcome": {"id": outcome["id"], "revision": outcome["basis_revision"]},
        "evidence": evidence,
        "snapshot": brief["snapshot"],
        "finding": outcome["summary"],
        "remaining_gap": caveat,
    }


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: scenario_a_successor.py HANDOFF.json")
    print(json.dumps(resume(json.loads(Path(sys.argv[1]).read_text(encoding="utf-8")))))
