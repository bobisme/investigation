"""Verify crate direction and scan the shipped default surface."""

import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ALLOWED = {
    "investigation-core": set(),
    "investigation-store": {"investigation-core"},
    "investigation-recording": {"investigation-core", "investigation-store"},
    "investigation-cli": {"investigation-core", "investigation-store", "investigation-recording"},
    "investigation-mcp": {
        "investigation-core", "investigation-store", "investigation-recording", "investigation-cli"
    },
}
ROOT = Path(__file__).resolve().parent.parent


def cargo(*args):
    return subprocess.check_output(["cargo", *args], cwd=ROOT, text=True)


def dependency_errors():
    metadata = json.loads(cargo("metadata", "--no-deps", "--locked", "--format-version", "1"))
    for package in metadata["packages"]:
        name = package["name"]
        manifest = Path(package["manifest_path"]).relative_to(ROOT)
        if name not in ALLOWED:
            yield f"{manifest}: no dependency direction policy for {name}"
            continue
        dependencies = {dependency["name"] for dependency in package["dependencies"]}
        for dependency in sorted((dependencies & ALLOWED.keys()) - ALLOWED[name]):
            yield f"{manifest}: {name} cannot depend on {dependency}"


def neutral_surfaces():
    command = ("run", "--locked", "--quiet", "--package", "investigation-cli", "--")
    help_text = cargo(*command, "--help")
    yield "investigation --help", help_text
    paths = [
        *(ROOT / "schemas").rglob("*"),
        *(ROOT / "methods/general").rglob("*"),
        ROOT / "methods/general.toml",
    ]
    for path in sorted(paths):
        if path.is_file() and path.suffix.lower() != ".md":
            yield str(path.relative_to(ROOT)), path.read_text(encoding="utf-8")

    # Exercise the normal entry point once init is part of the public command.
    if re.search(r"(?m)^\s+init(?:\s|$)", help_text):
        with tempfile.TemporaryDirectory() as scratch:
            workspace = Path(scratch) / "workspace"
            cargo(*command, "init", str(workspace), "--no-vault")
            paths = sorted(path for path in workspace.rglob("*") if path.is_file())
            if not paths:
                raise RuntimeError("investigation init created no workspace files")
            for path in paths:
                content = path.read_bytes().decode("utf-8", errors="replace")
                yield f"default workspace/{path.relative_to(workspace)}", content


def neutrality_errors():
    denylist = (ROOT / "scripts/neutrality-denylist.txt").read_text(encoding="utf-8")
    terms = [line for line in map(str.strip, denylist.splitlines()) if line and not line.startswith("#")]
    patterns = [(term, re.compile(rf"\b{re.escape(term)}\b", re.IGNORECASE)) for term in terms]
    for path, content in neutral_surfaces():
        for line_number, line in enumerate(content.splitlines(), start=1):
            for term, pattern in patterns:
                if pattern.search(line):
                    yield f"{path}:{line_number}: default contains deny-listed term {term!r}"


if __name__ == "__main__":
    errors = list(dependency_errors()) or list(neutrality_errors())
    for error in errors:
        print(error, file=sys.stderr)
    sys.exit(bool(errors))
