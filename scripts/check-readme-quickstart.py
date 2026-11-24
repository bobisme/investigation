#!/usr/bin/env python3
"""Run the README quickstart and docs/quickstart.md verbatim against the CLI.

Usage: check-readme-quickstart.py BINARY [--install DIR]

Each command block between `<!-- quickstart:start -->` and
`<!-- quickstart:end -->` runs as written with bash, in a new empty directory
whose PATH starts with the directory of the executable. With `--install DIR`
the install block of docs/quickstart.md runs first, as written, from the
repository root with CARGO_INSTALL_ROOT=DIR, and the quickstarts use the
executable it installed instead of BINARY.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


root = Path(__file__).resolve().parents[1]


def block(path, name):
    text = (root / path).read_text(encoding="utf-8")
    start, end = f"<!-- {name}:start -->\n```sh\n", f"```\n<!-- {name}:end -->"
    assert text.count(start) == 1 and text.count(end) == 1, f"{path}: one {name} block"
    return text.split(start, 1)[1].split(end, 1)[0]


def run(snippet, binary, cwd, env=None):
    result = subprocess.run(
        ["bash", "-eu", "-o", "pipefail", "-c", snippet],
        cwd=cwd,
        env=env or dict(os.environ, PATH=f"{binary.parent}:{os.environ['PATH']}", TMPDIR=cwd),
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"exit {result.returncode}\n{result.stdout}\n{result.stderr}"
    assert not result.stderr, result.stderr
    return result.stdout


def readme(binary):
    with tempfile.TemporaryDirectory(prefix="investigation-readme-") as parent:
        stdout = run(block("README.md", "quickstart"), binary, parent)
    init, committed, recovered, brief, guidance = map(json.loads, stdout.splitlines())
    assert all(output["ok"] for output in (init, committed, recovered, brief))
    assert isinstance(guidance["recommendations"], list)
    assert committed["transaction_id"] == recovered["transaction_id"]
    assert committed["artifacts"] == recovered["artifacts"]
    assert brief["snapshot"] == committed["snapshot"] == guidance["snapshot"]


def guide(binary):
    with tempfile.TemporaryDirectory(prefix="investigation-quickstart-") as parent:
        assert not any(Path(parent).iterdir())
        stdout = run(block("docs/quickstart.md", "quickstart"), binary, parent)
        init, started, brief, guidance, rendered, exported = map(json.loads, stdout.splitlines())
        assert all(output["ok"] for output in (init, started, brief))
        assert started["command_id"] == "first-source"
        assert brief["snapshot"] == started["snapshot"] == guidance["snapshot"]
        assert isinstance(guidance["recommendations"], list)
        assert rendered["complete"] is True and rendered["counts"]["written"] > 0
        assert (Path(parent) / "casework/vault/Start Here.md").is_file()
        assert exported["completeness"] == "complete"
        assert sorted(exported["files"]) == ["case-report.md", "export-manifest.json"]
        manifest = json.loads((Path(parent) / "report/export-manifest.json").read_text())
        assert manifest["snapshot"]["sequence"] == started["snapshot"]["sequence"]


def install(directory):
    directory = Path(directory).resolve()
    env = dict(os.environ, CARGO_INSTALL_ROOT=str(directory))
    subprocess.run(
        ["bash", "-eu", "-o", "pipefail", "-c", block("docs/quickstart.md", "install")],
        cwd=root,
        env=env,
        check=True,
    )
    binary = directory / "bin" / "investigation"
    assert binary.is_file(), f"the install block did not install {binary}"
    assert sorted(path.name for path in (directory / "bin").iterdir()) == ["investigation"]
    return binary


parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument("binary", type=Path)
parser.add_argument("--install", metavar="DIR")
arguments = parser.parse_args()
binary = arguments.binary.resolve(strict=True)
if arguments.install:
    binary = install(arguments.install)
assert binary.name == "investigation"
readme(binary)
print("README quickstart passed with the original receipt and snapshot.")
guide(binary)
print("docs/quickstart.md passed from an empty directory: case, brief, next, vault and report.")
