# Release candidate package

The release candidate is packaged for one platform, Linux x86_64 with glibc, because that is the only platform with build and test evidence. macOS, Windows, Linux aarch64 and musl are not packaged; `PACKAGE.json` in the archive lists each with the reason. Nothing here publishes or uploads anything.

## Build: `just package`

`scripts/package-release.py` builds the package from the committed tree of `HEAD` (`git archive`), never from the working directory. Uncommitted changes are left out, and the report says whether there were any. It runs the build twice, from source copies at two different paths, each with its own target directory:

```sh
cargo build --release --locked --offline --target x86_64-unknown-linux-gnu -p investigation-cli --bin investigation
```

Each build uses this environment:

- `SOURCE_DATE_EPOCH` is the commit time.
- `RUSTFLAGS` remaps the source, target, Cargo and rustup paths with `--remap-path-prefix`.
- `CFLAGS` remaps the same paths with `-ffile-prefix-map`, for the bundled SQLite.
- `CARGO_INCREMENTAL=0`, `LC_ALL=C` and `TZ=UTC` are set, and only the toolchain variables are kept.

The archive is written deterministically:

- Entries are sorted.
- Owners are 0, with empty user and group names.
- Every mtime is the commit time.
- Modes are fixed at 0755 or 0644.
- The gzip header carries no file name and uses the commit time.

The run fails unless both builds produce byte-identical binaries and archives. When they differ, `package-report.json` names the first differing offset of each. No network is used: a crate missing from the local Cargo cache fails the offline build.

The archive `investigation-VERSION-x86_64-unknown-linux-gnu.tar.gz` holds:

| File | Content |
| --- | --- |
| `bin/investigation` | The executable. |
| `install.sh` | Installs into one prefix (below). |
| `LICENSE` | MIT, the license of this project. |
| `NOTICES.md` | Third-party notices, generated offline from `Cargo.lock` with `cargo metadata --locked --offline`. It covers the normal dependency graph of the CLI crate for the target: the linked crates and the procedural macros that generate part of them. Each entry has the declared license expression and repository, and each distinct license file text appears once. A crate that ships no license file fails the build. `python3 scripts/package-release.py notices` prints it. |
| `PACKAGE.json` | Source commit and tree, `SOURCE_DATE_EPOCH`, `rustc -vV`, `cargo -V`, the C compiler, the newest glibc symbol version the binary needs, and the platforms that are not packaged. |
| `README.txt` | What the package is, how to install it, and its platform. |
| `SHA256SUMS` | Digests of every file above. |

Beside the archive, `target/package/` gets `SHA256SUMS` for the archive and `package-report.json`, which holds both builds' binary and archive digests and whether they matched.

Reproducibility is shown for the same toolchain on one machine: two clean builds from different paths. A different `rustc`, C compiler, linker or glibc headers can produce different bytes, so record `PACKAGE.json` with any digest you compare.

## Install: `install.sh`

```sh
./install.sh --prefix DIR [--alias-inv] [--replace]
```

The script checks every packaged file against `SHA256SUMS` before it copies anything, then installs `DIR/bin/investigation` by copying to a temporary name and renaming it. It changes nothing outside `DIR`: no shell start-up file, no `PATH` entry. It refuses an existing `DIR/bin/investigation` unless `--replace` is given, and it always refuses a symbolic link there.

`inv` is only an optional alias. With `--alias-inv`, the script adds `DIR/bin/inv`, a link to `investigation`, only when there is no `inv` in `DIR/bin` and none on `PATH`. Otherwise it names the `inv` it kept and still succeeds. It never replaces or removes an `inv`.

The test `crates/investigation-cli/tests/release_install.rs` runs this script in `just check` against the debug binary. It covers an unrelated `inv` on `PATH`, an `inv` already in the prefix, the alias where none exists, a refused second install and a refused mismatched package.

## Smoke test: `just smoke-package`

`scripts/smoke-release.py` takes the archive from `target/package/` and needs bubblewrap; it refuses to run without it. Every step runs in a clean root:

- No network namespace and a cleared environment.
- Read-only `/usr` and a private `/tmp`.
- An empty `/work` as the working directory.
- None of the checkout, the home directory or the Cargo and rustup homes visible.

A probe checks each of these from inside and records the result.

1. **Install.** The extracted package is mounted read-only. An unrelated `inv` program sits earlier on `PATH`. After `install.sh --prefix /opt/investigation --alias-inv`, that `inv` must be unchanged and still found first. A prefix whose `bin/inv` already exists keeps it. A prefix with no `inv` anywhere gets the alias. A second install without `--replace` is refused.
2. **Smoke.** Each command runs as a fresh process with the prefix mounted read-only:
   1. `version` and `init`.
   2. `start` with standard input attached.
   3. `record` of the transaction example that `schema question.create` prints, with the returned case ID filled in.
   4. `receipt` for both command IDs, then `brief`.
   5. `show` for every ID the receipts returned.

The run writes `smoke-transcript.json` (every argv, exit code, standard-input digest and standard output) and `smoke-report.json` (archive, binary and transcript digests, `PACKAGE.json`, the host, the isolation probe and each install's output).

This is a manual release step and not part of CI. It costs two release builds from scratch: about 2 minutes 20 seconds on a 64-thread machine, and several times that on a hosted runner. Bubblewrap also needs unprivileged user namespaces, which the hosted Ubuntu 24.04 image restricts. The install-script test above does run in CI.

## Candidate evidence

[`notes/release/2026-11-24-candidate/`](../notes/release/2026-11-24-candidate/) holds the package report, smoke report, transcript and checksums of the candidate built on 2026-11-24 from commit `3ceb437aa831` (tree `beef90f69365`). That commit was later rebased onto newer main commits, so it is not in the history and the merged binary differs; the evidence shows that the procedure works, not which bytes ship. That run is summarized below.

| Item | Value |
| --- | --- |
| Archive | `investigation-0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| Platform | Linux 7.2.8-arch1-2 x86_64; the binary needs glibc 2.34 or newer |
| Toolchain | rustc 1.100.0 (0ef1bc386 2026-11-09), LLVM 23.1.5; cargo 1.100.0; cc (GCC) 16.2.1 |
| Archive SHA-256 | `648115b2debb7fe005c18a87e4578d6960d0289c329dbc26c36eda7f8b7b0043` |
| Binary SHA-256 | `102261988b0a639791a2292e8d93f2ad9e5c4cdac6e396277d591022fa5ccae1` |
| Reproducible | Yes: two builds from different paths gave identical binary and archive bytes. |
| Smoke | Passed under bubblewrap: 14 commands, 6 returned IDs followed, unrelated `inv` kept; transcript SHA-256 `f6b8b33fdeb15af2f0e6c8d2438e2c899b3a3d5a4b7c257918d9cf899684fdfc` |

The archive digest covers `PACKAGE.json`, which names the source commit. Any new commit therefore gives a new archive digest even when the binary bytes stay the same. Hand the docs check and the release audit the archive that `just package` writes at the release commit, together with its `package-report.json` and `smoke-report.json`.
