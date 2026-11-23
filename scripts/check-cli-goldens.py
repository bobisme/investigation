#!/usr/bin/env python3
"""Check stable machine-readable help, schema discovery, and error envelopes."""

import argparse
import json
import os
from pathlib import Path
import pty
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "fixtures/development/cli-goldens.json"
SCOPES = (
    ("init",), ("schema",), ("record",), ("ingest",), ("start",), ("receipt",), ("show",),
    ("brief",), ("explain",), ("review-packet",), ("next",), ("frontier",), ("blindspots",), ("retrospective",), ("search",), ("guide",), ("method", "list"), ("onboard",), ("docs",), ("check",), ("exposure",), ("repair", "index"), ("repair", "staging"), ("evidence", "read"),
    ("case", "new"), ("source", "add"), ("observe",), ("note",), ("capture", "begin"),
    ("capture", "append"), ("capture", "attach"), ("vault", "render"), ("vault", "status"),
    ("vault", "conflicts"), ("vault", "reconcile"), ("vault", "copy-map"), ("vault", "remove"), ("export",), ("archive", "verify"), ("config", "list"), ("config", "get"), ("config", "path"), ("doctor",), ("doctor", "bundle"),
    ("integration", "show"), ("integration", "apply"), ("integration", "remove"), ("integration", "inspect"), ("obsidian", "setup"),
)


def invoke(binary, cwd, args, expected_exit, stdin=subprocess.DEVNULL):
    environment = dict(os.environ)
    for key in ("AGENT", "FORMAT"):
        environment.pop(key, None)
    result = subprocess.run(
        [binary, *args, "--no-color", "--no-pager", "--format", "json"],
        cwd=cwd,
        env=environment,
        stdin=stdin,
        text=True,
        capture_output=True,
        timeout=10,
    )
    assert result.returncode == expected_exit, (args, result.stdout, result.stderr)
    assert "\x1b" not in result.stdout and not result.stderr, (args, result.stderr)
    value = json.loads(result.stdout)
    assert isinstance(value, dict), (args, value)
    return value


def capture(binary, cwd):
    goldens = {
        "help": invoke(binary, cwd, ["--help"], 0),
        "schema_list": invoke(binary, cwd, ["schema", "--list"], 0),
        "evidence_selector": invoke(binary, cwd, ["schema", "evidence-selector", "--schema-only"], 0),
    }
    assert "observe" in goldens["help"]["help"], "shortcut commands must appear in help"
    for scope in SCOPES:
        failure = invoke(binary, cwd, [*scope, "--invalid-contract-test-argument"], 2)
        assert failure["commit_status"] == "not_committed", (scope, failure)
        assert failure["error"]["code"] == "INVALID_INPUT", (scope, failure)
        assert failure["error"]["repair"]["argv"], (scope, failure)
    goldens["missing_workspace"] = invoke(binary, cwd, ["next"], 3)
    message = goldens["missing_workspace"]["error"]["message"]
    goldens["missing_workspace"]["error"]["message"] = message.replace(str(cwd), "<WORKING_DIRECTORY>")
    return json.dumps(goldens, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def check_terminal(binary, cwd):
    master, terminal = pty.openpty()
    try:
        assert invoke(binary, cwd, ["onboard"], 0, stdin=terminal)["ok"]
        result = subprocess.run(
            [binary, "--help", "--format", "text", "--no-color", "--no-pager"],
            cwd=cwd, stdin=terminal, text=True, capture_output=True, timeout=10, check=True,
        )
        assert "Usage:" in result.stdout and "\x1b" not in result.stdout and not result.stderr
    finally:
        os.close(terminal)
        os.close(master)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--write", action="store_true", help="Replace the golden file with current output.")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as cwd:
        binary = args.binary.resolve(strict=True)
        actual = capture(binary, Path(cwd))
        check_terminal(binary, Path(cwd))
    if args.write:
        GOLDEN.write_text(actual, encoding="utf-8")
    else:
        assert actual == GOLDEN.read_text(encoding="utf-8"), "CLI golden output changed"
        print("CLI goldens and error envelopes passed.")
