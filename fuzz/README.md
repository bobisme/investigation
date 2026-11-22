# Fuzzing

The standalone fuzz workspace shares the production core. It uses Rust
`nightly-2026-08-21`, `cargo-fuzz 0.13.2` and `libfuzzer-sys 0.4.13`.
The runner targets Linux x86-64 with a native C++ compiler and Python 3.
The root workspace uses its normal stable toolchain.

```sh
rustup toolchain install nightly-2026-08-21 --profile minimal
cargo +nightly-2026-08-21 install --locked cargo-fuzz --version 0.13.2
python3 fuzz/run.py quick decode_command
python3 fuzz/run.py quick validate_transaction
```

Pull requests use the quick budget, up to 25,000 inputs or 30 seconds per
target. The weekly job runs the `scheduled` budget for ten minutes per
target. These times cover input processing after the build. Both budgets
enable AddressSanitizer, an 8-second per-input timeout and a 1 GiB libFuzzer
RSS limit. The decoder includes an input one byte above the 512 KiB bound.
The runner fetches the locked graph before fuzzing and builds offline from
that cache. Set `CARGO_NET_OFFLINE=true` before calling the runner to use a
previously fetched graph for both steps.

- `decode_command` exercises the real strict command decoder and envelope
  validation. Errors must be typed rejections with a JSON Pointer.
- `validate_transaction` exercises the real command catalog and transaction
  engine against the seeded W01 handoff state. The clock and ID source are
  deterministic. Rejected commands leave state unchanged, and accepted
  after-images replay with the exact issued IDs and revisions.

The runner splits the shared worked handoff into command inputs and adds
the target's checked-in seeds from `corpus/<target>/`. New inputs accumulate
in a temporary corpus for the current run. Crash artifacts are written to
`fuzz/artifacts/<target>/`, which the CI job uploads on failure.

## Reproduce and keep a regression

From the repository root, replay and minimize the reported artifact.

```sh
RUSTUP_TOOLCHAIN=nightly-2026-08-21 cargo-fuzz run decode_command fuzz/artifacts/decode_command/crash-…
RUSTUP_TOOLCHAIN=nightly-2026-08-21 cargo-fuzz tmin decode_command fuzz/artifacts/decode_command/crash-…
```

Check in the smallest reproducer under `fuzz/regressions/<target>/` in the
fix's first commit, before the fix itself. Preserve raw bytes when the
failure depends on encoding or malformed JSON. The runner includes this
directory as an additional seed corpus on every subsequent launch.

## Add a target

For journal replay (W02.20) or capture state (W06a.07), declare a binary under
`fuzz_targets/` in `fuzz/Cargo.toml` and add its name to the runner and CI
matrix. Feed the ordinary parser or state machine and check its invariants
with deterministic fixtures. Put seed inputs under `corpus/<target>/` and
commit any lockfile updates. If a target needs different shared seeds or
input budgets, select those explicitly when adding the target.

See the [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
and [libFuzzer options](https://llvm.org/docs/LibFuzzer.html#options).
