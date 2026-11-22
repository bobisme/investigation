#!/usr/bin/env python3
"""Run the README quickstart against the built CLI and check its receipt."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


root = Path(__file__).resolve().parents[1]
readme = (root / "README.md").read_text(encoding="utf-8")
snippet = readme.split("<!-- quickstart:start -->\n```sh\n", 1)[1]
snippet = snippet.split("```\n<!-- quickstart:end -->", 1)[0]
binary = Path(sys.argv[1]).resolve(strict=True)
assert binary.name == "investigation"
with tempfile.TemporaryDirectory(prefix="investigation-readme-") as parent:
    result = subprocess.run(
        ["bash", "-eu", "-o", "pipefail", "-c", snippet],
        cwd=parent,
        env=dict(os.environ, PATH=f"{binary.parent}:{os.environ['PATH']}", TMPDIR=parent),
        capture_output=True,
        check=True,
        text=True,
    )
    init, committed, recovered, brief, guidance = map(json.loads, result.stdout.splitlines())
    assert not result.stderr, result.stderr
    assert all(output["ok"] for output in (init, committed, recovered, brief))
    assert isinstance(guidance["recommendations"], list)
    assert committed["transaction_id"] == recovered["transaction_id"]
    assert committed["artifacts"] == recovered["artifacts"]
    assert brief["snapshot"] == committed["snapshot"] == guidance["snapshot"]
print("README quickstart passed with the original receipt and snapshot.")
