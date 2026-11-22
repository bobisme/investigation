"""Run the same bounded libFuzzer budgets locally and in CI."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

FUZZ = Path(__file__).resolve().parent
ROOT = FUZZ.parent
NIGHTLY = "nightly-2026-08-21"
TARGET = "x86_64-unknown-linux-gnu"
BUDGETS = {
    "quick": ["-runs=25000", "-max_total_time=30"],
    "scheduled": ["-max_total_time=600"],
}
MAX_LENGTH = {
    "decode_command": 512 * 1024 + 1,
    "validate_transaction": 512 * 1024 + 1,
    "decode_event": 2 * 1024 * 1024 + 1,
    "replay_journal": 32 * 1024 + 1,
    "parse_selector": 512 * 1024 + 1,
    "resolve_selector": 32 * 1024,
}


def run(target, budget):
    env = dict(os.environ, RUSTUP_TOOLCHAIN=NIGHTLY)
    subprocess.run(
        ["cargo", "fetch", "--locked", "--manifest-path", FUZZ / "Cargo.toml", "--target", TARGET],
        cwd=ROOT,
        env=env,
        check=True,
    )
    env["CARGO_NET_OFFLINE"] = "true"
    artifacts = FUZZ / "artifacts" / target
    artifacts.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f"investigation-{target}-") as temporary:
        corpus = Path(temporary)
        if target in {"decode_command", "validate_transaction"}:
            handoff = ROOT / "fixtures/development/w01-handoff/transactions.json"
            for index, command in enumerate(json.loads(handoff.read_text(encoding="utf-8"))):
                (corpus / f"handoff-{index}.json").write_text(json.dumps(command), encoding="utf-8")
        if target in {"decode_command", "decode_event"}:
            (corpus / "oversize.json").write_bytes(b" " * MAX_LENGTH[target])
        if target == "decode_event":
            append = FUZZ / "fixtures/w02-journal/events/000000000002.json"
            (corpus / "valid-append.json").write_bytes(append.read_bytes())
        seeds = [corpus, FUZZ / "corpus" / target]
        regressions = FUZZ / "regressions" / target
        if regressions.is_dir():
            seeds.append(regressions)
        subprocess.run(
            [
                "cargo-fuzz", "run", "--target", TARGET, target, *seeds, "--",
                *BUDGETS[budget], "-timeout=8", "-rss_limit_mb=1024", f"-max_len={MAX_LENGTH[target]}",
                f"-artifact_prefix={artifacts}/",
            ],
            cwd=ROOT,
            env=env,
            check=True,
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("budget", choices=BUDGETS)
    parser.add_argument("target", choices=MAX_LENGTH)
    args = parser.parse_args()
    run(args.target, args.budget)
