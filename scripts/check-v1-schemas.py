#!/usr/bin/env python3
"""Export and verify the frozen v1 schemas against the live CLI and a Git base."""

import argparse
from copy import deepcopy
import json
from pathlib import Path
import re
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = ROOT / "schemas/v1"
DOC_FIELDS = {"title", "description", "$comment", "examples"}
SCHEMA_VALUES = {"items", "contains", "additionalProperties", "propertyNames", "unevaluatedItems",
                 "unevaluatedProperties", "allOf", "anyOf", "oneOf", "prefixItems",
                 "not", "if", "then", "else"}
SCHEMA_MAPS = {"patternProperties", "dependentSchemas"}
# Frozen v1 enums that may gain values (owner decision, 2026-11-23; ADR 0007):
# only where every reader of the value must already tolerate one it does not
# know. Each entry names why that holds. Values may be added, never removed or
# renamed, and nothing else about the enum may change.
OPEN_ENUMS = {
    "command.schema.json/$defs/GuardedOperation": (
        "Input only: it constrains human_confirmation.confirms[].op in a command, and no response, "
        "receipt or error schema uses it. Its one reader is the binary validating a request, which "
        "refuses a value it does not know with INVALID_INPUT at that path, as for any invalid input; "
        "every request valid before stays valid."
    ),
}


class BreakingChange(ValueError):
    pass


def require(condition, path, reason):
    if not condition:
        raise BreakingChange(f"{path}: {reason}")


def compare_schema(old, new, path):
    if not isinstance(old, dict) or not isinstance(new, dict):
        if isinstance(old, list) and isinstance(new, list) and len(old) == len(new):
            for index, (earlier, later) in enumerate(zip(old, new)):
                compare_schema(earlier, later, f"{path}/{index}")
        else:
            require(old == new, path, "changed validation rule")
        return

    old_keys = old.keys() - DOC_FIELDS
    new_keys = new.keys() - DOC_FIELDS
    structural = {"properties", "$defs", "required"}
    require(old_keys - structural == new_keys - structural, path, "changed validation keywords")
    for key in sorted(old_keys - structural):
        keyword_path = f"{path}/{key}"
        if key in SCHEMA_VALUES:
            compare_schema(old[key], new[key], keyword_path)
        elif key in SCHEMA_MAPS:
            require(old[key].keys() == new[key].keys(), keyword_path, "changed validation map")
            for name in sorted(old[key]):
                compare_schema(old[key][name], new[key][name], f"{keyword_path}/{name}")
        elif key == "enum" and path in OPEN_ENUMS:
            require(isinstance(new[key], list) and len(set(map(json.dumps, new[key]))) == len(new[key])
                    and all(value in new[key] for value in old[key]),
                    keyword_path, "removed or duplicated a value of an open enum")
        else:
            require(old[key] == new[key], keyword_path, "changed validation rule")

    require(set(old.get("required", [])) == set(new.get("required", [])), path, "changed required fields")
    earlier = old.get("properties", {})
    later = new.get("properties", {})
    require(earlier.keys() <= later.keys(), path, "removed property")
    patterns = old.get("patternProperties")
    if old.get("additionalProperties", True) is not False or patterns:
        prior = old.get("additionalProperties", True)
        for name in later.keys() - earlier.keys():
            optional_schema = later[name]
            require((not patterns and optional_schema == prior and prior is not False)
                    or optional_schema in (True, {}),
                    f"{path}/{name}",
                    "constrained a previously unconstrained property")
    for name in sorted(earlier):
        compare_schema(earlier[name], later[name], f"{path}/properties/{name}")

    earlier_defs = old.get("$defs", {})
    later_defs = new.get("$defs", {})
    require(earlier_defs.keys() <= later_defs.keys(), path, "removed definition")
    for name in sorted(earlier_defs):
        compare_schema(earlier_defs[name], later_defs[name], f"{path}/$defs/{name}")


def open_enums_are_input_only(documents):
    """An open enum's definition name appears in no other published schema."""
    for location in OPEN_ENUMS:
        file, _, pointer = location.partition("/")
        name = pointer.rsplit("/", 1)[-1]
        require(file in documents and name in documents[file].get("$defs", {}), location,
                "open enum is not published there")
        for other, schema in documents.items():
            if other != file and isinstance(schema, dict):
                require(name not in schema.get("$defs", {}), f"{other}/$defs/{name}",
                        "an open enum must not appear in another published schema")


def compare_contract(old, new):
    for file, value in old.items():
        require(file in new, file, "removed published schema")
        if file != "catalog.json":
            compare_schema(value, new[file], file)
    if "catalog.json" in old:
        earlier, later = old["catalog.json"], new["catalog.json"]
        require(earlier["schema_version"] == later["schema_version"], "catalog.json", "changed version")
        for namespace in ("operations", "wire_schemas"):
            require(all(later[namespace].get(name) == path for name, path in earlier[namespace].items()),
                    namespace, "changed a published name or location")
        for reservation, namespace in (("reserved_operations", "operations"),
                                       ("reserved_wire_schemas", "wire_schemas")):
            require(set(earlier[reservation]) <= set(later[reservation]) | later[namespace].keys(),
                    reservation, "removed an operation or wire reservation")


def run(binary, cwd, *args):
    result = subprocess.run([binary, "schema", *args, "--format", "json", "--no-color", "--no-pager"],
                            cwd=cwd, check=True, capture_output=True, text=True)
    if result.stderr:
        raise ValueError(result.stderr)
    return json.loads(result.stdout)


def generate(binary):
    with tempfile.TemporaryDirectory(prefix="investigation-schema-export-") as cwd:
        listing = run(binary, cwd, "--list", "--include-deferred")
        operations = {op: f"operations/{op}.schema.json"
                      for stage in listing["stages"] for op in stage["operations"]}
        wire = {name: ("command" if name == "record" else name) + ".schema.json"
                for name in listing["wire_schemas"]}
        reserved_operations = listing["reserved_operations"]
        reserved_wire_schemas = listing["reserved_wire_schemas"]
        assert not operations.keys() & set(reserved_operations)
        assert not wire.keys() & set(reserved_wire_schemas)
        assert not (operations.keys() | set(reserved_operations)) & (wire.keys() | set(reserved_wire_schemas))
        documents = {}
        for namespace, contracts in (("wire", wire), ("operation", operations)):
            for name, file in contracts.items():
                if not re.fullmatch(r"[a-z][a-z0-9._-]*", name):
                    raise ValueError(f"Invalid catalog schema name: {name}")
                schema = run(binary, cwd, name, "--schema-only")
                schema["$id"] = f"urn:investigation:schema:v1:{namespace}:{name}"
                documents[file] = schema
        documents["catalog.json"] = {
            "schema_version": "inv.catalog.v1", "wire_schemas": wire, "operations": operations,
            "stages": listing["stages"], "reserved_operations": reserved_operations,
            "reserved_wire_schemas": reserved_wire_schemas,
        }
        return documents


def committed(base):
    if not base or base == "0" * 40:
        return {}
    if not re.fullmatch(r"[0-9a-f]{7,40}", base):
        raise ValueError(f"Expected a Git revision, got {base!r}")
    files = subprocess.check_output(
        ["git", "-C", ROOT, "ls-tree", "-r", "--name-only", base, "--", "schemas/v1"],
        text=True,
    ).splitlines()
    return {
        file.removeprefix("schemas/v1/"): json.loads(subprocess.check_output(
            ["git", "-C", ROOT, "show", f"{base}:{file}"], text=True,
        ))
        for file in files if file.endswith(".json")
    }


def self_test():
    base = {
        "type": "object", "additionalProperties": False, "required": ["id"],
        "properties": {"id": {"type": "string", "enum": ["wire"]}},
    }
    additional = deepcopy(base)
    additional["properties"]["elsewhere"] = {"type": "string"}
    compare_schema(base, additional, "optional")
    for rejected in (
        dict(additional, required=["id", "elsewhere"]),
        dict(base, properties={"id": {"type": "number"}}),
        dict(base, properties={"id": {"type": "string", "enum": ["wire", "other"]}}),
        dict(base, properties={}),
    ):
        try:
            compare_schema(base, rejected, "breaking")
        except BreakingChange:
            pass
        else:
            raise AssertionError(f"Breaking contract change accepted: {rejected}")
    open_path = next(iter(OPEN_ENUMS))
    guarded = {"type": "string", "enum": ["a", "b"]}
    compare_schema(guarded, dict(guarded, enum=["a", "new", "b"]), open_path)
    for rejected in (dict(guarded, enum=["a"]), dict(guarded, enum=["a", "c"]),
                     dict(guarded, enum=["a", "b", "b"]), dict(guarded, enum=["a", "b", "c"], type="number")):
        try:
            compare_schema(guarded, rejected, open_path)
        except BreakingChange:
            pass
        else:
            raise AssertionError(f"Breaking change to an open enum accepted: {rejected}")
    try:
        compare_schema(guarded, dict(guarded, enum=["a", "b", "c"]), "closed.schema.json/$defs/Other")
    except BreakingChange:
        pass
    else:
        raise AssertionError("A value added to a closed enum was accepted.")
    try:
        compare_schema({}, {"properties": {"typed": {"type": "string"}}}, "open")
    except BreakingChange:
        pass
    else:
        raise AssertionError("A new restriction on an open property was accepted.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--against", help="Compare v1 with schemas from this Git commit.")
    parser.add_argument("--write", action="store_true", help="Explicitly regenerate published JSON.")
    args = parser.parse_args()
    self_test()
    generated = generate(args.binary.resolve(strict=True))
    open_enums_are_input_only(generated)
    compare_contract(committed(args.against), generated)
    existing = {file.relative_to(DIRECTORY).as_posix(): json.loads(file.read_text(encoding="utf-8"))
                for file in DIRECTORY.rglob("*.json")}
    if args.write:
        if existing.keys() - generated.keys():
            raise ValueError(f"Review old publications: {existing.keys() - generated.keys()}")
        for relative, data in sorted(generated.items()):
            file = DIRECTORY / relative
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text(json.dumps(data, indent=2, ensure_ascii=False, sort_keys=True) + "\n", encoding="utf-8")
    elif existing != generated:
        differing = sorted(name for name in existing.keys() | generated.keys()
                           if existing.get(name) != generated.get(name))
        raise ValueError(f"Published v1 schemas differ from the live catalog: {differing}")
    print(f"Verified {len(generated) - 1} published v1 schemas and the operation reservations.")
