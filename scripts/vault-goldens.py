#!/usr/bin/env python3
"""Check or regenerate the golden vaults of the Scenario A, C and D fixture workspaces.

Each scenario under fixtures/vault-goldens/ holds a checked-in canonical workspace
(`workspace/`: investigation.toml and state/) and the vault the renderer publishes from
it (`vault/`: Start Here.md and Cases/). `cargo test` compares the two and never writes
either; only this script with an explicit flag does:

  python3 scripts/vault-goldens.py BINARY                       # check, write nothing
  python3 scripts/vault-goldens.py BINARY --write               # re-render vault/ from workspace/
  python3 scripts/vault-goldens.py BINARY --rebuild-workspaces  # rerun the scenario proofs, then --write

Re-render after an intended renderer change, together with a RENDERER_VERSION bump.
Rebuild the workspaces only when a scenario proof gains records worth projecting: every
ID and timestamp changes, so the whole golden changes.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
GOLDENS = ROOT / "fixtures/vault-goldens"
SCENARIOS = ("scenario-a", "scenario-c", "scenario-d")
# Directories a workspace must have; git keeps no empty directory.
REQUIRED = (
    "state/cases", "state/events", "state/blobs/sha256", "state/checkpoints", "state/staging",
    "state/locks", "state/tmp", "cache", "local", "exports", "vault/Notes", "vault/Boards",
    "vault/Inbox",
)
HUMAN = {"Notes", "Boards", "Inbox"}
# Scenarios whose workspace states a supported viewer release, so their golden vaults
# also hold the optional native Base files and case maps; the others show the plain-Markdown
# default.
VIEWER_RELEASE = {"scenario-a": "1.15.1", "scenario-c": "1.15.1"}


def prepare(source, target):
    shutil.copytree(source, target)
    for directory in REQUIRED:
        (target / directory).mkdir(parents=True, exist_ok=True)


def render(binary, workspace):
    # No viewer program, a fixed locale and zone: nothing here depends on them.
    result = subprocess.run(
        [str(binary), "--workspace", str(workspace), "vault", "render", "--format", "json"],
        env={"PATH": "", "TZ": "UTC", "LC_ALL": "C"},
        capture_output=True, text=True, timeout=60, check=False,
    )
    report = json.loads(result.stdout)
    if result.returncode != 0 or report.get("state") != "complete":
        raise SystemExit(f"vault render failed for {workspace}:\n{result.stdout}\n{result.stderr}")
    return report


def owned(vault):
    """Every generated file, by vault-relative path; human and hidden folders are not ours."""
    files = {}
    for path in sorted(vault.rglob("*")):
        relative = path.relative_to(vault)
        top = relative.parts[0]
        if top in HUMAN or top.startswith(".") or not path.is_file():
            continue
        files[relative.as_posix()] = path.read_bytes()
    return files


def rendered(binary, scenario, scratch):
    workspace = scratch / scenario
    prepare(GOLDENS / scenario / "workspace", workspace)
    render(binary, workspace)
    return owned(workspace / "vault")


def keep_workspace(source, scenario):
    target = GOLDENS / scenario / "workspace"
    if target.exists():
        shutil.rmtree(target)
    target.mkdir(parents=True)
    config = (source / "investigation.toml").read_text(encoding="utf-8")
    if "enabled = false" not in config:
        raise SystemExit(f"{source}: unexpected vault policy\n{config}")
    config = config.replace("enabled = false", "enabled = true", 1)
    if scenario in VIEWER_RELEASE:
        config = config.replace("preserve_human_folders = true\n",
                                f"preserve_human_folders = true\nviewer_release = \"{VIEWER_RELEASE[scenario]}\"\n", 1)
        if "viewer_release" not in config:
            raise SystemExit(f"{source}: could not state the viewer release\n{config}")
    (target / "investigation.toml").write_text(config, encoding="utf-8")
    shutil.copytree(source / "state", target / "state")


def rebuild_workspaces(binary, scratch):
    proofs = scratch / "proofs"
    proofs.mkdir()
    subprocess.run(
        [sys.executable, ROOT / "scripts/scenario_a_proof.py", binary, proofs / "handoff.json",
         "--workspace", proofs / "scenario-a"],
        cwd=ROOT, check=True, stdout=subprocess.DEVNULL,
    )
    subprocess.run(
        [sys.executable, ROOT / "scripts/scenario_c_proof.py", "--bin", binary,
         "--workspace", proofs / "scenario-c"],
        cwd=ROOT, check=True, stdout=subprocess.DEVNULL,
    )
    subprocess.run(
        ["cargo", "test", "--locked", "--quiet", "-p", "investigation-cli", "--test", "scenario_d",
         "scenario_d_baseline_conditional_conclusion_from_sources_alone", "--", "--exact"],
        cwd=ROOT, check=True, stdout=subprocess.DEVNULL,
        env={**os.environ, "INVESTIGATION_KEEP_SCENARIO_D": str(proofs / "scenario-d")},
    )
    for scenario in SCENARIOS:
        keep_workspace(proofs / scenario, scenario)


def write(scenario, files):
    vault = GOLDENS / scenario / "vault"
    if vault.exists():
        shutil.rmtree(vault)
    for relative, content in files.items():
        path = vault / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)


def differences(scenario, files):
    expected = owned(GOLDENS / scenario / "vault")
    found = []
    for path in sorted(expected.keys() | files.keys()):
        if path not in files:
            found.append(f"{scenario}: {path} is no longer rendered")
        elif path not in expected:
            found.append(f"{scenario}: {path} is new")
        elif files[path] != expected[path]:
            found.append(f"{scenario}: {path} differs")
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--write", action="store_true", help="Replace each golden vault with the current render.")
    parser.add_argument("--rebuild-workspaces", action="store_true",
                        help="Rerun the scenario proofs, replace the fixture workspaces, then --write.")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="vault-goldens-") as directory:
        scratch = Path(directory)
        if args.rebuild_workspaces:
            rebuild_workspaces(binary, scratch)
        problems = []
        for scenario in SCENARIOS:
            files = rendered(binary, scenario, scratch / "render")
            if args.write or args.rebuild_workspaces:
                write(scenario, files)
                print(f"{scenario}: wrote {len(files)} files")
            else:
                problems.extend(differences(scenario, files))
    if problems:
        print("\n".join(problems), file=sys.stderr)
        raise SystemExit("Golden vaults differ. If the change is intended, bump RENDERER_VERSION and run "
                         "python3 scripts/vault-goldens.py target/debug/investigation --write")
    if not (args.write or args.rebuild_workspaces):
        print("Golden vaults match.")


if __name__ == "__main__":
    main()
