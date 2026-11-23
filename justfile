check: format lint test boundaries audit

check-offline: format lint test boundaries (audit "--offline")

format:
    cargo fmt --all --check

lint:
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --locked --workspace --all-features

boundaries:
    python3 scripts/check-boundaries.py

audit *options:
    cargo-deny --locked --all-features {{options}} check advisories licenses sources

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
