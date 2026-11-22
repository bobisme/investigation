#!/usr/bin/env python3
"""Run both Milestone A case proofs against one public executable."""

import argparse
from contextlib import nullcontext
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def walk_evidence(binary, workspace, case, subject, field, expected):
    def call(*args, request=None):
        result = subprocess.run(
            [binary, "--case", case, *args, "--format", "json"], cwd=workspace,
            input=json.dumps(request) if request is not None else "", text=True,
            capture_output=True,
        )
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return json.loads(result.stdout)

    title, summary, full = (
        call("show", subject["id"], "--at", str(subject["revision"]), "--depth", depth)
        for depth in ("title", "summary", "full")
    )
    assert title["title"] == summary["title"] == full["title"]
    assert "summary" not in title and summary["summary"] and "record" not in summary
    assert title["snapshot"] == summary["snapshot"] == full["snapshot"]
    assert full["basis_revision"] == subject["revision"]
    body = full["record"]["body"]
    for ref in body[field]:
        if not ref["id"].startswith("obs_"):
            continue
        observation = call("show", ref["id"], "--at", str(ref["revision"]), "--depth", "full")
        basis = observation["record"]["body"]["basis"]
        if basis["kind"] != "source":
            continue
        artifact = basis["artifact_ref"]
        selector = {key: basis["selector"][key] for key in ("schema_version", "artifact_digest", "selector")}
        evidence = call("evidence", "read", artifact["id"], "--selector-file", "-", request=selector)
        excerpt = evidence["excerpt"]
        if expected not in (excerpt.get("text"), excerpt.get("value")):
            continue
        assert ref in summary["evidence_refs"]
        assert evidence["content_trust"] == "untrusted_evidence"
        assumptions = [
            call("show", ref["id"], "--at", str(ref["revision"]), "--depth", "full")["record"]["body"]["claim"]
            for ref in body.get("assumption_refs", [])
        ]
        return {"subject": subject, "observation": ref, "artifact": artifact,
                "selector": selector, "excerpt": expected,
                "assumptions": assumptions, "caveats": body.get("caveats", [])}
    raise AssertionError(f"Could not trace {subject} to its committed evidence excerpt")


def run(binary, output):
    scripts = Path(__file__).resolve().parent
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
    checks = [
        [scripts / "scenario_a_proof.py", binary, output / "timeout-brief.json",
         "--workspace", output / "timeout-case"],
        [scripts / "scenario_c_proof.py", "--bin", binary,
         "--handoff", output / "architecture-brief.json", "--workspace", output / "architecture-case"],
    ]
    results = []
    for command in checks:
        result = subprocess.run([sys.executable, *command], env=env, text=True, capture_output=True)
        if result.returncode:
            raise SystemExit(result.stdout + result.stderr)
        results.append(json.loads(result.stdout))
    timeout, architecture = results
    timeout["review_walk"] = walk_evidence(
        binary, output / "timeout-case", timeout["case"], timeout["validated"],
        "support_refs", "callback returned after 28 seconds",
    )
    architecture["review_walk"] = walk_evidence(
        binary, output / "architecture-case", architecture["case_id"],
        {"id": architecture["decision_id"], "revision": 2}, "evidence_refs", 110,
    )
    assert architecture["review_walk"]["assumptions"] == ["Traffic stays below 2500000 jobs per day"]
    return {"ok": True, "timeout": timeout, "architecture": architecture}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--output", type=Path, help="Keep both case workspaces and handoffs in a new directory.")
    args = parser.parse_args()
    if args.output:
        args.output.mkdir(parents=True)
    with (nullcontext(args.output.resolve()) if args.output else tempfile.TemporaryDirectory()) as output:
        result = run(args.binary.resolve(strict=True), Path(output))
        if args.output is None:
            print("Milestone A scenario and cold-handoff checks passed.")
        else:
            print(json.dumps(result, ensure_ascii=False))
