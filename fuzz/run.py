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
TARGETS = ("decode_command", "validate_transaction")


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
        handoff = ROOT / "fixtures/development/w01-handoff/transactions.json"
        for index, command in enumerate(json.loads(handoff.read_text(encoding="utf-8"))):
            (corpus / f"handoff-{index}.json").write_text(json.dumps(command), encoding="utf-8")
        if target == "decode_command":
            (corpus / "oversize.json").write_bytes(b" " * (512 * 1024 + 1))
        seeds = [corpus, FUZZ / "corpus" / target]
        regressions = FUZZ / "regressions" / target
        if regressions.is_dir():
            seeds.append(regressions)
        subprocess.run(
            [
                "cargo-fuzz", "run", "--target", TARGET, target, *seeds, "--",
                *BUDGETS[budget], "-timeout=8", "-rss_limit_mb=1024", "-max_len=524289",
                f"-artifact_prefix={artifacts}/",
            ],
            cwd=ROOT,
            env=env,
            check=True,
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("budget", choices=BUDGETS)
    parser.add_argument("target", choices=TARGETS)
    args = parser.parse_args()
    run(args.target, args.budget)
