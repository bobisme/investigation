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
