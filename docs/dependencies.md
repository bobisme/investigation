# Dependency selection

The foundation lockfile contains four first-party crates. Use the reviewed
versions below when the owning code lands and commit the resulting lockfile.
Where a release advanced past the version already available for Milestone A,
both versions are recorded. The CI audit checks the resolved graph, including
transitive and build dependencies, after each addition.

Release metadata was reviewed on 2026-11-22. Advisory status is based on the
[RustSec snapshot](https://github.com/RustSec/advisory-db/tree/55e1c3dfbd834237a05bbbf7b50ba3b5be27b662)
from 2026-11-21. “Clear” means the reviewed version has no matching advisory
in that snapshot.

| Dependency and reviewed version | License | Purpose and constraints | Advisory status |
| --- | --- | --- | --- |
| [`clap` 4.6.9](https://docs.rs/crate/clap/4.6.9) | MIT OR Apache-2.0 | CLI parsing when command dispatch lands. | Clear |
| [`serde` 1.0.229](https://docs.rs/crate/serde/1.0.229), A pin 1.0.228 | MIT OR Apache-2.0 | Typed serialization and wire validation. | Clear |
| [`serde_json` 1.0.154](https://docs.rs/crate/serde_json/1.0.154), A pin 1.0.149 | MIT OR Apache-2.0 | JSON. Reject duplicate object keys with a recursive visitor before accepting a value. Enable `float_roundtrip` for stable finite float parsing. | Clear |
| [`serde_path_to_error` 0.1.20](https://docs.rs/crate/serde_path_to_error/0.1.20) | MIT OR Apache-2.0 | Field-level input errors. | Clear |
| [`schemars` 1.2.3](https://docs.rs/crate/schemars/1.2.3) | MIT | Derive versioned JSON Schema from the operation types. | Clear |
| [`rusqlite` 0.43.0](https://docs.rs/crate/rusqlite/0.43.0), [`libsqlite3-sys` 0.40.0](https://docs.rs/crate/libsqlite3-sys/0.40.0) | MIT, with SQLite itself in the public domain | Rebuildable index. Enable `bundled`. The selected build script sets `SQLITE_ENABLE_FTS5` and includes SQLite 3.54.1. | Patched for RUSTSEC-2020-0014, RUSTSEC-2021-0128, RUSTSEC-2022-0090, and RUSTSEC-2026-0459 |
| [`sha2` 0.11.1](https://docs.rs/crate/sha2/0.11.1) | MIT OR Apache-2.0 | SHA-256 for stored event bytes and blobs. Hash the bytes that enter the journal. A canonical-JSON library would change the bytes covered by integrity checks and adds a second number encoding. | Patched for RUSTSEC-2021-0100 |
| [`uuid` 1.29.3](https://docs.rs/crate/uuid/1.29.3), A pin 1.23.1 | Apache-2.0 OR MIT | Use the `v7` feature. `Uuid::now_v7()` is provided with `std` and `v7`. | Clear |
| [`time` 0.3.55](https://docs.rs/crate/time/0.3.55), A pin 0.3.47 | MIT OR Apache-2.0 | UTC timestamp parsing and formatting. | Patched for RUSTSEC-2020-0071 and RUSTSEC-2026-0009 |
| [`thiserror` 2.0.21](https://docs.rs/crate/thiserror/2.0.21) | MIT OR Apache-2.0 | Typed errors as error surfaces grow. | Clear |
| [`tracing` 0.1.44](https://docs.rs/crate/tracing/0.1.44) | MIT | Local diagnostics without evidence excerpts or credentials by default. | Patched for RUSTSEC-2023-0078 |
| [`rustix` 1.1.6](https://docs.rs/crate/rustix/1.1.6) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | Safe Linux filesystem calls. The `fs` feature exposes `linkat` and `renameat_with` with `RenameFlags::NOREPLACE`. | Patched for RUSTSEC-2026-0486 |
| [`serde-saphyr` 1.3.0](https://docs.rs/crate/serde-saphyr/1.3.0), [2.2.0 available](https://docs.rs/crate/serde-saphyr/2.2.0) | MIT OR Apache-2.0 for 1.3.0 | W08 renderer baseline. Use ordered fields and `quote_all` to quote wikilinks, punctuation, and scalar-like strings. Upstream tests cover quoting and escape output. Confirm byte-stable output and Obsidian parsing with W08 fixtures. | Clear |
| [`tempfile` 3.27.0](https://docs.rs/crate/tempfile/3.27.0) | MIT OR Apache-2.0 | Scratch directories or staging only. Its `persist_noclobber` can fall back to link-and-unlink and does not sync the file or directory. The store owns no-replace publication and explicit sync order. | Clear |

`serde_json` rejects non-JSON literals such as `NaN` and `Infinity`, and
rejects out-of-range IEEE-754 numbers such as `1e999` in the default finite
number representation. Generic `Value` deserialization accepts repeated
keys, so validate duplicate names at each nesting level before decoding
typed input. Reject non-finite numbers generated internally before writing
them, since the default serializer can encode them as `null`.

Recheck availability, supported Rust version, enabled features, and advisory
status before adopting a later release. W02 must validate publication on the
target filesystem and run an FTS5 virtual-table smoke test against its actual
build. The W00 checks exercise the scaffold and its current lockfile.
