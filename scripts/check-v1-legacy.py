#!/usr/bin/env python3
"""Open the exact alpha handoff journals from the pilot using the v1 binary."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "training/a-gate/archive/pilot-raw.tar.zst"


def extract_handoffs(destination):
    with subprocess.Popen(["zstd", "-dc", ARCHIVE], stdout=subprocess.PIPE) as process:
        assert process.stdout is not None
        with tarfile.open(fileobj=process.stdout, mode="r|") as archive:
            for item in archive:
                relative = PurePosixPath(item.name)
                parts = relative.parts
                if len(parts) < 4 or parts[2] != "handoff":
                    continue
                if relative.is_absolute() or ".." in parts:
                    raise ValueError(f"Unsafe pilot archive path: {relative}")
                path = destination.joinpath(*parts[1:])
                if item.isdir():
                    path.mkdir(parents=True, exist_ok=True)
                    continue
                if not item.isfile():
                    continue
                path.parent.mkdir(parents=True, exist_ok=True)
                source = archive.extractfile(item)
                assert source is not None
                with source, path.open("xb") as file:
                    shutil.copyfileobj(source, file)
        assert process.wait() == 0


def historical_copies_match(directory):
    """Each fixtures/historical/alpha-pilot-<run> is that run's casework, file for file."""
    copies = sorted((ROOT / "fixtures/historical").glob("alpha-pilot-*"))
    assert copies, "no historical alpha pilot workspace"
    for copy in copies:
        prefix = copy.name.removeprefix("alpha-pilot-")
        [run] = [run for run in directory.iterdir() if run.name.startswith(prefix)]
        source = run / "handoff/casework"
        files = lambda root: {p.relative_to(root).as_posix(): p.read_bytes()
                              for p in root.rglob("*") if p.is_file()}
        assert files(copy) == files(source), f"{copy} differs from the pilot archive"
    return len(copies)


def verify(binary, directory):
    def run(workspace, *args, input_data=None):
        proc = subprocess.run([binary, "--workspace", workspace, *args, "--format", "json"],
                              cwd=directory, input=input_data, text=True, capture_output=True)
        assert proc.returncode == 0, (args, proc.stdout, proc.stderr)
        return json.loads(proc.stdout)

    workspaces = events = 0
    for config in sorted(directory.rglob("investigation.toml")):
        workspace = config.parent
        files = sorted(workspace.glob("state/cases/*/events/*.json"))
        if not files:
            continue
        workspaces += 1
        assert run(workspace, "check")["structurally_valid"]
        run(workspace, "repair", "index")
        for file in files:
            raw = file.read_bytes()
            event = json.loads(raw)
            request = event["request"]["input"]
            assert request["schema_version"] == "inv.command.v0-alpha"
            expected_hash = "sha256:" + hashlib.sha256(raw).hexdigest()
            receipt = run(workspace, "receipt", event["command_id"], "--actor", event["actor"])
            assert receipt["schema_version"] == "inv.response.v1"
            assert receipt["transaction_id"] == event["transaction_id"]
            assert receipt["snapshot"] == {"sequence": event["sequence"], "head_hash": expected_hash}
            replay = run(workspace, "record", "--input", "-", input_data=json.dumps(request))
            assert replay["result"] == "already_committed"
            assert replay["transaction_id"] == receipt["transaction_id"] and replay["snapshot"] == receipt["snapshot"]
            assert file.read_bytes() == raw
            events += 1
        assert files == sorted(workspace.glob("state/cases/*/events/*.json"))
    assert workspaces == 2 and events == 3
    return workspaces, events


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="investigation-legacy-pilot-") as directory:
        root = Path(directory)
        extract_handoffs(root)
        copies = historical_copies_match(root)
        workspaces, events = verify(args.binary.resolve(strict=True), root)
    print(f"Reopened {workspaces} alpha pilot workspaces and replayed {events} original receipts byte for byte; "
          f"{copies} historical fixture copies equal the archive.")
