# Platform and durability support

The filesystem checks admit recognized local Linux filesystems. The evidence below qualifies specific behaviors on the tested configuration. Full power-loss and clean-machine restore qualification is still open.

| Platform and filesystem | Read policy | Write policy | Evidence and limits |
| --- | --- | --- | --- |
| Linux ext4 | Enabled | Enabled | Linux 7.2.8-arch1-2, ext4 with `rw,nosuid,nodev,relatime`. [LocalFs fault boundaries](../crates/investigation-store/src/real_crash_tests.rs) and [multi-process append, receipt, and revision conflicts](../crates/investigation-store/tests/production.rs) passed. Physical power loss and clean-machine archive restore are unqualified. Ext2 and ext3 share the ext-family detector magic and were outside this run. |
| Linux XFS or btrfs | Enabled | Enabled | Recognized by the runtime. Filesystem-specific crash and restore behavior is unqualified. |
| Linux tmpfs | Test use | Test use | The [LocalFs fault-boundary test](../crates/investigation-store/src/real_crash_tests.rs) passed with `/tmp` on tmpfs. RAM-backed workspaces disappear on reboot. The runtime permits tmpfs, so choose persistent storage for work you need to keep. |
| Linux NFS, SMB, and other recognized network filesystems | Refused by workspace open | Refused by workspace init and open | [Workspace refusal tests](../crates/investigation-store/src/workspace.rs) exercise the policy before layout writes. Shared locking and publication semantics are unqualified. |
| Linux FUSE | Refused by workspace open | Refused by workspace init and open | [Workspace refusal tests](../crates/investigation-store/src/workspace.rs) cover FUSE and unknown filesystem classifications. LazyFS 0.3.1 passed all three mounted cache-loss boundaries in the explicit test harness. |
| Linux cloud-synchronized canonical directories | Unsupported | Unsupported | Local filesystem detection cannot identify a sync agent. Keep canonical state outside synchronized folders. |
| macOS APFS and Windows NTFS | Unqualified | Unqualified | Publication, locking, and directory synchronization have no platform-specific test evidence. |

## Evidence and scope

- The [in-memory crash model](../crates/investigation-store/src/crash_tests.rs) exercises restart after faulted I/O and sync calls, lost acknowledgments, attachment-before-event ordering, same-key recovery, and rebuilding derived state. Its volatile and persisted state are a model.
- The [LocalFs boundary test](../crates/investigation-store/src/real_crash_tests.rs) uses the normal filesystem implementation and interrupts before event-file sync, before the events-directory sync after publication, and after attachment publication before event staging. Recovery verifies the chain, requires exactly one append, and checks the original receipt on retry. Its sync witnesses detect omitted calls. A normal local run leaves kernel and device caches in place.
- The [LazyFS workflow](../.github/workflows/lazyfs.yml) pins LazyFS 0.3.1 and reports the kernel, FUSE version, and backing mount options. Run `bash scripts/qualify-lazyfs.sh /path/to/lazyfs/build/lazyfs` on Linux with accessible `/dev/fuse`. On 2026-11-22, both ignored tests passed in a QEMU guest running Linux 7.2.8-arch1-2 and libfuse 3.18.3. The backing tmpfs used `rw,relatime,inode64,huge=advise`; the `fuse.lazyfs` mount used `rw,nosuid,nodev,relatime`. The canary distinguished unsynced and synced bytes, and each publication checkpoint recovered one append and its original receipt. This establishes application behavior under acknowledged userspace cache loss on that mount. Device power loss and directory metadata persistence remain outside the evidence. [LazyFS 0.3.1](https://github.com/dsrhaslab/lazyfs/releases/tag/0.3.1)

## Recovery and trust

Append publication syncs the private file before linking it into the journal and syncs the target directory before acknowledging the write. Initial case creation publishes a prepared directory with a no-replacement rename. Atomic visibility and survival of a power loss are separate properties.

On restart, Store reads verify canonical journal events before resolving receipts. A writer that reports uncertain durability may leave a warning for `investigation check`. Recover with the same actor and command ID. SQLite is a disposable index rebuilt from the verified journal, for example with `investigation repair index`.

Crash recovery preserves capture staging and unreferenced blobs. Under the commit lock, a writer first verifies the journal and then reclaims recognizable publication temporaries at least 24 hours old. The sweep preserves unrecognized entries and import preparations, which may be open outside that lock. A production recovery test covers each of these boundaries.

Restrictive permissions limit other OS users where the platform enforces them. They do not protect against arbitrary code running as the same OS user. Digests verify equality with recorded bytes and link journal entries. Independent anchoring is separate from local integrity checking.
