# Platform and durability support

The filesystem checks admit recognized local Linux filesystems. The evidence below qualifies specific behaviors on the tested configuration. Full power-loss and clean-machine restore qualification is still open.

`investigation doctor` reports this classification for the selected workspace, or for the working directory when there is none, together with the enforced input limits, selector validation and index, staging, lock and vault health. It writes nothing. A refused filesystem is a finding with its reason and repair, not a failure. `investigation doctor bundle --preview` lists what a redacted troubleshooting archive would hold; it leaves out evidence content, credentials and local paths, and this build writes no archive and uploads nothing.

| Platform and filesystem | Read policy | Write policy | Evidence and limits |
| --- | --- | --- | --- |
| Linux ext4 | Enabled | Enabled | Linux 7.2.8-arch1-2, ext4 with `rw,nosuid,nodev,relatime`. [LocalFs fault boundaries](../crates/investigation-store/src/real_crash_tests.rs) and [multi-process append, receipt, and revision conflicts](../crates/investigation-store/tests/production.rs) passed. Physical power loss and clean-machine archive restore are unqualified. Ext2 and ext3 share the ext-family detector magic and were outside this run. |
| Linux XFS or btrfs | Enabled | Enabled | Recognized by the runtime. Filesystem-specific crash and restore behavior is unqualified. |
| Linux tmpfs | Test use | Test use | The [LocalFs fault-boundary test](../crates/investigation-store/src/real_crash_tests.rs) passed with `/tmp` on tmpfs. RAM-backed workspaces disappear on reboot. The runtime permits tmpfs, so choose persistent storage for work you need to keep. |
| Linux NFS, SMB, and other recognized network filesystems | Refused by workspace open | Refused by workspace init and open | [Workspace refusal tests](../crates/investigation-store/src/workspace.rs) exercise the policy before layout writes. Shared locking and publication semantics are unqualified. |
| Linux FUSE | Refused by workspace open | Refused by workspace init and open | [Workspace refusal tests](../crates/investigation-store/src/workspace.rs) cover FUSE and unknown filesystem classifications. LazyFS 0.3.1 passed all three mounted cache-loss boundaries in the explicit test harness. |
| Linux cloud-synchronized canonical directories | Unsupported | Unsupported | Local filesystem detection cannot identify a sync agent. Keep canonical state outside synchronized folders. |
| macOS APFS and Windows NTFS | Unqualified | Unqualified | Publication, locking, and directory synchronization have no platform-specific test evidence. |

## Raw-file limits

Every bound on bytes the build parses from outside is listed in one table, [`investigation_store::limits`](../crates/investigation-store/src/limits.rs). Each entry refers to the constant its parser enforces, and the table serializes for diagnostic reports. A test fails if this section and the table differ.

| Limit | Value | Unit | Applies to | Refusal |
| --- | --- | --- | --- | --- |
| `request.bytes` | 512 KiB | bytes | A JSON request: command, ingest manifest, capture call or selector file. | `INVALID_INPUT` |
| `request.depth` | 32 | levels | Nesting of a JSON request. | `INVALID_INPUT` |
| `command.operations` | 128 | items | Operations in one command batch. | `INVALID_INPUT` |
| `ingest.items` | 32 | items | Paths in one ingest manifest. | `INVALID_INPUT` |
| `ingest.file_bytes` | 64 GiB | bytes | One copied import: a local file, standard input or a producer-run artifact. Checked before reading and again while copying, through one fixed buffer at any size. | `INVALID_INPUT` |
| `ingest.reference_bytes` | 1 TiB | bytes | One local file hashed in reference mode without copying. | `INVALID_INPUT` |
| `ingest.derivation_inputs` | 64 | items | Input artifacts named by one derived artifact. | `INVALID_INPUT` |
| `ingest.derivation_selectors` | 64 | items | Selectors recorded for one derived artifact. | `INVALID_INPUT` |
| `ingest.derivation_redactions` | 256 | items | Redaction decisions recorded for one derived artifact. | `INVALID_INPUT` |
| `ingest.metadata_bytes` | 64 KiB | bytes | Serialized opaque source metadata. | `INVALID_INPUT` |
| `ingest.metadata_depth` | 16 | levels | Nesting of one opaque metadata value. | `INVALID_INPUT` |
| `ingest.metadata_key_chars` | 128 | characters | One opaque metadata key. | `INVALID_INPUT` |
| `ingest.probe_bytes` | 1 MiB | bytes | Retained content probed for a producer envelope version. Larger content is retained without a probe; nested values are skipped without being built. | — |
| `text.buffer_bytes` | 64 KiB | bytes | Memory for the UTF-8 check of a text derivative, whatever the artifact's size. Invalid UTF-8 is reported and yields no derivative. | — |
| `producer_run.envelope_bytes` | 512 KiB | bytes | One producer-run envelope. | `INVALID_INPUT` |
| `producer_run.artifacts` | 32 | items | Artifacts in one producer-run envelope. | `INVALID_INPUT` |
| `producer_run.assertions` | 256 | items | Assertions in one producer-run envelope. | `INVALID_INPUT` |
| `producer_run.value_bytes` | 16 KiB | bytes | One serialized assertion value. | `INVALID_INPUT` |
| `capture.event_bytes` | 64 KiB | bytes | One capture event, for handles declared by this build. | `INVALID_INPUT` |
| `capture.attachment_bytes` | 64 MiB | bytes | One capture attachment, for handles declared by this build. | `INVALID_INPUT` |
| `capture.items` | 100000 | items | Items accepted by one capture handle declared by this build. | `INVALID_INPUT` |
| `selector.artifact_bytes` | 64 MiB | bytes | A retained artifact a native selector reads into memory. A larger artifact stays retained and verifiable; cite part of it through a derived artifact. | `INVALID_INPUT` |
| `selector.json_bytes` | 16 MiB | bytes | An artifact parsed by the strict json_pointer reader. | `INVALID_INPUT` |
| `selector.json_depth` | 64 | levels | Nesting of an artifact parsed by the json_pointer reader. | `INVALID_INPUT` |
| `selector.table_columns` | 4096 | items | Fields in one row of a CSV or TSV table read by table_cell. | `INVALID_INPUT` |
| `selector.excerpt_bytes` | 32 KiB | bytes | One excerpt returned by an evidence read. | `INVALID_INPUT` |
| `search.candidates` | 5000 | items | Index rows read for one search before the canonical checks. | — |
| `method_pack.bytes` | 256 KiB | bytes | One method pack file. | `INVALID_INPUT` |
| `method_pack.depth` | 16 | levels | Nesting of a method pack. | `INVALID_INPUT` |
| `journal.event_bytes` | 2 MiB | bytes | One canonical journal event read from disk. | `INTEGRITY_FAILURE` |
| `journal.event_depth` | 64 | levels | Nesting of one canonical journal event. | `INTEGRITY_FAILURE` |

Nesting levels count JSON Pointer segments below the root. An input over a bound is refused with the listed error code and nothing is committed. A dash marks a bound on the work done, which no input can exceed.

**Compressed files and archives.** Compressed files and archives are retained as opaque bytes and never expanded. Entry names inside an archive are never used as paths. No decompressor or archive reader is linked, so there is no expansion ratio, expanded size, entry count or entry size to bound. A compressed file is held to the ordinary import limit at its stored size. A test fails if a decompression, archive or launcher crate enters the dependency graph.

**Large inputs.** A copied import, from a local file or standard input, streams through one 64 KiB buffer while it is hashed into a private temporary under `state/tmp`; the UTF-8 check and a reference-mode hash stream the same way. Peak heap does not grow with the input: a [streaming test](../crates/investigation-store/tests/streaming_ingest.rs) measures one whole ingest of 2 MiB and of 72 MiB from a sparse file and from a generator, compares the retained bytes with the source, and with `INV_LARGE_INGEST_GIB=<n>` repeats this at `n` GiB. No lock is held while bytes are copied or hashed: a known blob that a reimport deduplicates against is verified before the commit lock is taken, and [another writer commits during each stream](../crates/investigation-store/src/ingest_streaming_tests.rs). A full disk at any point of the stream commits nothing, publishes no blob and removes the partial copy; a kill mid-stream leaves only the copy, which the 24-hour sweep below reclaims. With `-v`, progress goes to stderr as byte counts every 64 MiB; stdout holds only the receipt. A native selector still reads its artifact into memory, so it is held to `selector.artifact_bytes`.

**Paths.** A local import opens beneath a configured import root with `openat2` and `RESOLVE_BENEATH`, `RESOLVE_NO_SYMLINKS` and `RESOLVE_NO_MAGICLINKS`. The import refuses `..` components, symlinks at any depth, files with more than one hard link, and anything that is not a regular file. It opens a FIFO non-blocking and refuses it. Standard input names, capture keys and artifact names are labels: staging files are named by position and digest. The capture staging directory is accepted only as `state/staging/<run-id>` for its own Run, even when it is read from the journal.

**Execution.** No code path starts a process, and none chooses a parser from a MIME type or file extension. These values are recorded as hints. A [structural test](../crates/investigation-store/tests/no_execution.rs) scans the core, store, recording, render and CLI sources for process APIs. Each reviewed exception has a recorded reason. The [hardening fixtures](../crates/investigation-store/tests/raw_file_hardening.rs) cover a nested gzip bomb, an archive with traversal, absolute and link entries, deep nesting, path traversal, huge lines and invalid UTF-8. They assert the refusal and measure peak heap use.

## Evidence and scope

- The [in-memory crash model](../crates/investigation-store/src/crash_tests.rs) exercises restart after faulted I/O and sync calls, lost acknowledgments, attachment-before-event ordering, same-key recovery, and rebuilding derived state. Its volatile and persisted state are a model.
- The [LocalFs boundary test](../crates/investigation-store/src/real_crash_tests.rs) uses the normal filesystem implementation and interrupts before event-file sync, before the events-directory sync after publication, and after attachment publication before event staging. Recovery verifies the chain, requires exactly one append, and checks the original receipt on retry. Its sync witnesses detect omitted calls. A normal local run leaves kernel and device caches in place.
- The [LazyFS workflow](../.github/workflows/lazyfs.yml) pins LazyFS 0.3.1 and reports the kernel, FUSE version, and backing mount options. Run `bash scripts/qualify-lazyfs.sh /path/to/lazyfs/build/lazyfs` on Linux with accessible `/dev/fuse`. On 2026-11-22, both ignored tests passed in a QEMU guest running Linux 7.2.8-arch1-2 and libfuse 3.18.3. The backing tmpfs used `rw,relatime,inode64,huge=advise`; the `fuse.lazyfs` mount used `rw,nosuid,nodev,relatime`. The canary distinguished unsynced and synced bytes, and each publication checkpoint recovered one append and its original receipt. This establishes application behavior under acknowledged userspace cache loss on that mount. Device power loss and directory metadata persistence remain outside the evidence. [LazyFS 0.3.1](https://github.com/dsrhaslab/lazyfs/releases/tag/0.3.1)

## Recovery and trust

Append publication syncs the private file before linking it into the journal and syncs the target directory before acknowledging the write. Initial case creation publishes a prepared directory with a no-replacement rename. Atomic visibility and survival of a power loss are separate properties.

On restart, Store reads verify canonical journal events before resolving receipts. A writer that reports uncertain durability may leave a warning for `investigation check`. Recover with the same actor and command ID. SQLite is a disposable index rebuilt from the verified journal, for example with `investigation repair index`.

Crash recovery preserves capture staging and unreferenced blobs. Only `investigation repair staging` removes staging, and only for a finalized Run whose bytes the canonical store holds; a kill or power loss during it leaves the full staging or none. Under the commit lock, a writer first verifies the journal and then reclaims recognizable publication temporaries at least 24 hours old. These include the private copies of import bytes that an ingest or a finalization killed mid-stream leaves under `state/tmp`. A copy modified within 24 hours may belong to a writer still streaming and is kept. Blobs are published only under that lock, so a reclaimed copy was never published; a writer stalled past the grace period finds its copy gone and fails without committing. The sweep preserves unrecognized entries. A production recovery test covers each of these boundaries.

Restrictive permissions limit other OS users where the platform enforces them. They do not protect against arbitrary code running as the same OS user. Digests verify equality with recorded bytes and link journal entries. Independent anchoring is separate from local integrity checking.
