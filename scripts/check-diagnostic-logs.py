#!/usr/bin/env python3
"""Run scenarios A and C with trace logging and scan stderr for evidence content (W05b.14).

Usage: check-diagnostic-logs.py BINARY
Fails if any fixture text line, or a local absolute path, appears in the logs.
"""

import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def main(binary):
    binary = Path(binary).resolve()
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        log = scratch / "stderr.log"
        shim = scratch / "shim.sh"
        shim.write_text(f'#!/bin/sh\nexec "{binary}" "$@" 2>>"{log}"\n')
        shim.chmod(shim.stat().st_mode | stat.S_IXUSR)
        env = {**os.environ, "INVESTIGATION_LOG": "trace"}
        subprocess.run(["python3", ROOT / "scripts/scenario_a_proof.py", shim, scratch / "handoff"],
                       check=True, env=env, capture_output=True)
        subprocess.run(["python3", ROOT / "scripts/scenario_c_proof.py", "--bin", shim],
                       check=True, env=env, capture_output=True)
        text = log.read_text()
        if "stage=" not in text:
            sys.exit("no diagnostic stage events were logged")
        leaks = []
        for fixture in (ROOT / "fixtures/development").glob("scenario-*/**/*"):
            if not fixture.is_file():
                continue
            for line in fixture.read_text(errors="ignore").splitlines():
                line = line.strip()
                if len(line) >= 20 and not line.startswith(("{", "}", '"')) and line in text:
                    leaks.append(f"{fixture.name}: {line[:60]}")
        for absolute in (str(scratch), tempfile.gettempdir() + "/"):
            if absolute in text:
                leaks.append(f"local path {absolute}")
        if leaks:
            sys.exit("diagnostic logs leak content:\n" + "\n".join(leaks))
    print("diagnostic logs ok")


if __name__ == "__main__":
    main(sys.argv[1])
