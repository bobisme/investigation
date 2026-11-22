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
