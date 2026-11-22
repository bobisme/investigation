#!/usr/bin/env python3
"""A deliberately small cold-successor check; input is one saved brief only."""
import json
import pathlib
import re
import sys


def resume(brief):
    if brief.get("ok") is not True or brief.get("result") != "brief":
        raise ValueError("Expected a successful brief")
    outcomes = brief.get("outcomes")
    if not isinstance(outcomes, list) or not outcomes:
        raise ValueError("The brief has no decision outcome")
    candidates = [outcome for outcome in outcomes if outcome.get("type") == "dec"
                  and outcome.get("effective_status", {}).get("recorded") == "accepted"
                  and outcome.get("effective_status", {}).get("state") == "conditional"]
    if len(candidates) != 1:
        raise ValueError("The brief does not identify one currently conditional accepted decision")
    content = json.dumps(candidates[0], ensure_ascii=False).lower()
    if "database" not in content or not any(word in content for word in ("loss", "lost")):
        raise ValueError("The brief does not identify the selected design and zero-loss gap")
    if not any(word in content for word in ("conditional", "condition", "unknown")):
        raise ValueError("The brief does not disclose the decision condition")
    if "2500000" not in content and "2,500,000" not in content:
        raise ValueError("The brief does not disclose the traffic reversal trigger")
    refs = candidates[0].get("evidence_refs", [])
    def valid_ref(entry, prefix):
        return (isinstance(entry, dict) and isinstance(entry.get("id"), str)
                and re.fullmatch(prefix + r"_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", entry["id"])
                and isinstance(entry.get("revision"), int) and not isinstance(entry["revision"], bool)
                and entry["revision"] > 0)
    observations = [entry for entry in refs if valid_ref(entry, "obs")]
    artifacts = [entry for entry in refs if valid_ref(entry, "art")]
    if not observations or not artifacts:
        raise ValueError("The brief omits the exact observation or retained artifact reference")
    return {"conclusion": "The database-backed queue is a conditional choice for this synthetic case.",
            "gap": "Independently verify zero acknowledged-job loss; test peak traffic and cost before relying on the choice.",
            "observation_refs": observations, "artifact_refs": artifacts}


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: scenario_c_successor.py HANDOFF.json")
    print(json.dumps(resume(json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")))))
