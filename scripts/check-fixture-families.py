#!/usr/bin/env python3
"""Validate split registration and materialize a participant's explicit source allowlist."""

import argparse
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import re
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SPLITS = {"development", "pilot", "sealed"}
DOMAINS = {"debugging", "architecture", "lookup", "source-research", "measurement"}
KEY = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
SHA256 = re.compile(r"^[a-f0-9]{64}$")
EXPOSED_NAMES = {"source.txt", "update.txt"}


def within(root, relative):
    path = root / relative
    if path.resolve() != path or not path.is_relative_to(root) or ".." in Path(relative).parts:
        raise ValueError(f"Unsafe fixture path: {relative}")
    return path


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_path(root, variant, source):
    directory = within(root, variant["directory"])
    name = source["file"]
    if name not in EXPOSED_NAMES:
        raise ValueError(f"Unapproved participant file name: {name}")
    file = within(directory, name)
    if source["sha256"] != sha256(file):
        raise ValueError(f"Source checksum mismatch: {file}")
    return file


def validate(registry, fixture_root, sealed_root=None):
    families = set()
    directories = set()
    for family in registry["families"]:
        family_id = family["id"]
        if not KEY.fullmatch(family_id) or family_id in families:
            raise ValueError(f"Invalid or duplicate family ID: {family_id}")
        families.add(family_id)
        split = family["split"]
        if split not in SPLITS or family["domain"] not in DOMAINS or not family["variants"]:
            raise ValueError(f"Invalid family registration: {family}")
        if split != "development":
            if Path(family["key"]).parts != (split, f"{family_id}.json"):
                raise ValueError(f"Invalid evaluator key location: {family_id}")
        variants = set()
        for variant in family["variants"]:
            variant_id = variant["id"]
            if not KEY.fullmatch(variant_id) or variant_id in variants:
                raise ValueError(f"Invalid or duplicate variant: {family_id}/{variant_id}")
            variants.add(variant_id)
            if variant["split"] != split:
                raise ValueError(f"Variant crosses the family split: {family_id}/{variant_id}")
            relative = Path(variant["directory"])
            if relative.parts[:1] != (split,) or relative in directories:
                raise ValueError(f"Invalid or reused fixture directory: {relative}")
            directories.add(relative)
            directory = within(fixture_root, relative)
            if split == "sealed" and directory.exists():
                raise ValueError(f"Sealed source directory in checkout: {directory}")
            if split != "sealed" and not directory.is_dir():
                raise ValueError(f"Missing fixture directory: {directory}")
            if split == "development" and "initial" not in variant:
                continue
            if split == "sealed":
                # A sealed objective is part of the held-out content: register its digest only.
                if "objective" in variant or not SHA256.fullmatch(variant.get("objective_sha256", "")):
                    raise ValueError(f"Sealed objective must be registered by digest only: {variant_id}")
            elif not variant["objective"].strip():
                raise ValueError(f"Invalid initial source contract: {variant_id}")
            if variant["initial"]["file"] != "source.txt":
                raise ValueError(f"Invalid initial source contract: {variant_id}")
            if variant.get("update") and variant["update"]["file"] != "update.txt":
                raise ValueError(f"Invalid update source contract: {variant_id}")
            root = sealed_root if split == "sealed" else fixture_root
            if root is not None:
                if split == "sealed":
                    objective = within(within(root, variant["directory"]), "objective.txt")
                    if sha256(objective) != variant["objective_sha256"]:
                        raise ValueError(f"Objective checksum mismatch: {objective}")
                source_path(root, variant, variant["initial"])
                if variant.get("update"):
                    source_path(root, variant, variant["update"])


def stage_source(root, variant, workspace, stage):
    source = variant.get(stage)
    if source is None:
        return None
    data = source_path(root, variant, source).read_bytes()
    destination = workspace / source["file"]
    with destination.open("xb") as stream:
        stream.write(data)
    destination.chmod(0o444)
    return source["sha256"]


def self_test():
    def must_reject(registry, message):
        try:
            validate(registry, fixtures)
        except ValueError as error:
            assert message in str(error), str(error)
        else:
            raise AssertionError(f"Registry accepted {message}")

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory).resolve()
        fixtures = root / "fixtures"
        seeds = fixtures / "pilot/test-family/standard"
        seeds.mkdir(parents=True)
        initial = seeds / "source.txt"
        initial.write_bytes(b"source")
        (seeds / "update.txt").write_bytes(b"update")
        (seeds / "key.json").write_bytes(b"evaluator-only")
        variant = {
            "id": "standard", "split": "pilot", "directory": "pilot/test-family/standard",
            "objective": "Find the source.",
            "initial": {"file": "source.txt", "sha256": sha256(initial)},
            "update": {"file": "update.txt", "sha256": sha256(seeds / "update.txt")},
        }
        family = {
            "id": "test-family", "split": "pilot", "domain": "lookup",
            "key": "pilot/test-family.json", "variants": [variant],
        }
        original = {"families": [family]}
        validate(original, fixtures)
        must_reject({"families": [family, family]}, "duplicate family ID")
        crossed = deepcopy(original)
        crossed["families"][0]["variants"][0]["split"] = "sealed"
        must_reject(crossed, "crosses the family split")
        workspace = root / "participant"
        workspace.mkdir()
        stage_source(fixtures, variant, workspace, "initial")
        assert sorted(path.name for path in workspace.iterdir()) == ["source.txt"]
        stage_source(fixtures, variant, workspace, "update")
        assert sorted(path.name for path in workspace.iterdir()) == ["source.txt", "update.txt"]
        leak = deepcopy(original)
        leak["families"][0]["variants"][0]["initial"]["file"] = "key.json"
        must_reject(leak, "Invalid initial source")
        sealed = deepcopy(original)
        sealed["families"][0].update(split="sealed", key="sealed/test-family.json")
        sealed["families"][0]["variants"][0].update(split="sealed", directory="sealed/test-family/standard")
        must_reject(sealed, "registered by digest only")
        del sealed["families"][0]["variants"][0]["objective"]
        sealed["families"][0]["variants"][0]["objective_sha256"] = hashlib.sha256(b"Find the source.\n").hexdigest()
        validate(sealed, fixtures)
        (fixtures / "sealed/test-family/standard").mkdir(parents=True)
        must_reject(sealed, "Sealed source directory in checkout")
        initial.write_bytes(b"modified")
        must_reject(original, "checksum mismatch")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sealed-root", type=Path)
    args = parser.parse_args()
    fixtures = ROOT / "fixtures"
    registry = json.loads((fixtures / "family-registry.json").read_text(encoding="utf-8"))
    validate(registry, fixtures, args.sealed_root.resolve() if args.sealed_root else None)
    self_test()
    print(f"Verified {len(registry['families'])} case family registrations and participant allowlists.")
