check: format lint test boundaries neutrality hygiene audit

check-offline: format lint test boundaries neutrality hygiene (audit "--offline")

format:
    cargo fmt --all --check

lint:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# Runs every test binary in parallel with cargo-nextest when it is installed
# (doctests separately, since nextest does not run them); otherwise falls back
# to `cargo test`. Point TMPDIR at a tmpfs for a much faster run: the crash and
# recovery tests are bound by fsync, not by CPU.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    if cargo nextest --version >/dev/null 2>&1; then
        cargo nextest run --locked --workspace --all-features --no-fail-fast
        cargo test --locked --workspace --all-features --doc
    else
        cargo test --locked --workspace --all-features
    fi

boundaries:
    python3 scripts/check-boundaries.py

# The Milestone B proof's neutrality step alone (plan §20.1 X3): root and
# subcommand help, every schema, a fresh workspace and its onboard text against
# the deny-list, named tools and security terms; each allowed match names its
# plan text. About 3 s once the binary is built (the tests have built it).
neutrality:
    python3 scripts/check-neutrality.py

# Fails on tracked Python bytecode; warns, with the chmod fix, on a tracked
# file that is read-only in this worktree, which breaks a later checkout.
hygiene:
    python3 scripts/check-worktree.py

audit *options:
    cargo-deny --locked --all-features {{options}} check advisories licenses sources

# Plan §20.5 deterministic acceptance measures: runs the tests behind each
# measure and writes target/acceptance/deterministic-report.json. Not part of
# `check` (whose tests include every one it runs); CI runs it in the offline
# job. `just acceptance --quick` skips the slow trial-task rehearsals.
acceptance *options:
    python3 scripts/acceptance-measures.py {{options}}

# Plan §10.5 benchmarks on release builds; writes target/bench/report.json.
# Not part of `check` (a smoke run of the harness is). Pass options through,
# e.g. `just bench --samples 50 --fixtures target/bench/fixtures`.
bench *options:
    cargo build --release --locked -p investigation-cli -p investigation-bench
    target/release/investigation-bench {{options}}

# Plan §12.5 vault load test: about 50,000 notes over 650 cases, with and
# without the viewer features. Slow (tens of minutes to hours) and writes
# several GiB under DIR, which must be absent or empty and is left in place.
# Not part of `check`. Needs GNU `time` for peak memory. Writes
# target/bench/load-report.json; add `--smoke` for a seconds-long harness check.
# e.g. `TMPDIR=/var/tmp just load-test /var/tmp/vault-load --samples 5`.
load-test dir *options:
    cargo build --release --locked -p investigation-cli -p investigation-bench
    target/release/investigation-bench --load --fixtures {{dir}} {{options}}

# Optional real-viewer gate (W12.07, plan §13.4): drives a running Obsidian
# through its official CLI. Not part of `check`, which tests this adapter only
# against a fake CLI. `prepare DIR` needs no viewer; `run DIR` and `timings`
# call the CLI, which launches the app if it is not running, and skip with exit
# 0 when no CLI is found. See obsidian/README.md, "Integration gate".
# e.g. `just obsidian-ui prepare ~/inv-ui`, open
# ~/inv-ui/investigation-ui-check in the viewer once, then
# `just obsidian-ui run ~/inv-ui`. Reports go to target/obsidian-ui/.
obsidian-ui *options:
    cargo build --locked -p investigation-cli
    python3 -B scripts/obsidian-ui-check.py --bin target/debug/investigation {{options}}

# Release candidate package for Linux x86_64 (REL.04): builds the committed
# tree twice at different paths (locked, offline, SOURCE_DATE_EPOCH, host
# paths remapped), fails unless both are byte-identical, and writes the
# archive, SHA256SUMS and package-report.json to target/package/. Slow: two
# release builds from scratch. Not part of `check`; see docs/packaging.md.
package *options:
    python3 scripts/package-release.py {{options}}

# Installs that package into isolated prefixes (keeping an unrelated `inv` on
# PATH) and smoke-tests it in a bubblewrap clean root without network or
# checkout paths; writes target/package/smoke-report.json and the transcript.
smoke-package *options:
    python3 scripts/smoke-release.py {{options}}
