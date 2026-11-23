#!/usr/bin/env python3
"""Verify pilot key coverage and optionally replay every retained source through the CLI."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import runpy
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"
KEYS = ROOT / "training/a-gate/evaluator"
FAMILY = runpy.run_path(str(ROOT / "scripts/check-fixture-families.py"))


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))


def validate():
    registry = load(FIXTURES / "family-registry.json")
    FAMILY["validate"](registry, FIXTURES)
    pilot = [family for family in registry["families"] if family["split"] == "pilot"]
    counts = Counter(family["domain"] for family in pilot)
    assert counts["debugging"] >= 3 and counts["architecture"] >= 3 and counts["lookup"] >= 1
    source_digests = {}
    paths = []
    for family in pilot:
        key = load(KEYS / family["key"])
        assert key["family"] == family["id"]
        assert set(key["variants"]) == {variant["id"] for variant in family["variants"]}
        sources = set()
        for variant in family["variants"]:
            rubric = key["variants"][variant["id"]]
            assert rubric["warranted"] and rubric["false_promotions"]
            assert len({item["id"] for item in rubric["warranted"]}) == len(rubric["warranted"])
            assert len({item["id"] for item in rubric["false_promotions"]}) == len(rubric["false_promotions"])
            digest = variant["initial"]["sha256"]
            sources.add(digest)
            assert source_digests.setdefault(digest, family["id"]) == family["id"]
            documents = {}
            for stage in ("initial", "update"):
                if stage in variant:
                    path = FAMILY["source_path"](FIXTURES, variant, variant[stage])
                    paths.append(path)
                    documents[path.name] = path.read_text(encoding="utf-8").splitlines()
            for item in rubric["warranted"] + rubric["false_promotions"]:
                for clause in item.get("all", []) + item.get("unless", []):
                    re.compile(clause)
                if "citation" in item:
                    citation = item["citation"]
                    lines = documents[citation["document"]]
                    assert citation["any_lines"] and all(1 <= i <= len(lines) for i in citation["any_lines"])
        if family["domain"] == "lookup":
            assert "update" not in family["variants"][0]
        else:
            assert len(sources) == 1
            assert len({v["update"]["sha256"] for v in family["variants"]}) == len(family["variants"])
    return pilot, paths


def verify_binary(binary, paths):
    with tempfile.TemporaryDirectory(prefix="investigation-pilot-fixture-") as directory:
        work = Path(directory)

        def call(*arguments, data=None):
            result = subprocess.run([binary, *arguments, "--format", "json"], cwd=work,
                                    input=data, capture_output=True)
            assert result.returncode == 0, (arguments, result.stdout, result.stderr)
            return json.loads(result.stdout)

        call("init", ".", "--no-vault")
        for index, path in enumerate(paths):
            original = path.read_bytes()
            receipt = call("--actor", "agent:fixture-qualification", "start", "Pilot fixture import",
                           "--attach", "-", "--command-id", f"fixture-{index}", data=original)
            case = next(row["id"] for row in receipt["created"] if row["id"].startswith("case_"))
            item = receipt["items"][0]
            digest = "sha256:" + hashlib.sha256(original).hexdigest()
            assert item["digest"] == digest and item["derivative_id"]
            selector = {
                "schema_version": "inv.selector.v1", "artifact_digest": digest,
                "selector": {"type": "whole"},
            }
            read = call("--case", case, "evidence", "read", item["derivative_id"], "--selector-file", "-",
                        data=json.dumps(selector).encode())
            assert read["excerpt"]["text"].encode("utf-8") == original, path
            assert read["content_trust"] == "untrusted_evidence", path


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    families, files = validate()
    if args.binary:
        verify_binary(args.binary.resolve(strict=True), files)
    print(f"Verified {len(families)} distinct pilot families and {len(files)} source files.")
