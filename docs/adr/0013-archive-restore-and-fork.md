# ADR 0013 – Archive restore and fork

Accepted with W11.18 (bn-36sl); 2026-11-23. Refines plan §10.4 and §10.7, ADR 0002 and ADR 0010. [Restore](../../crates/investigation-render/src/export/restore.rs) · [Fork marker](../../crates/investigation-store/src/workspace/fork.rs) · [Journal writers](../../crates/investigation-store/src/journal.rs)

**Problem.** A workspace archive (W11.08) must become a workspace again. Plan §10.7 asks for two kinds of result. An exact restore keeps the original IDs. A deliberate fork gets a new workspace identity and an explicit origin snapshot. Two forks must not pass for one coordinated writer. Every event names its workspace (`workspace_id`), and the verifier refused any event that names another workspace. A fork that keeps the origin's bytes would therefore fail verification. A fork that rewrites them would change every hash, head and receipt it claims to keep.

**Decision 1: restore publishes only the copy that verification checked.**

1. PATH must be a new directory whose parent exists, or an empty directory. PATH must not lie inside the archive. A non-empty PATH is refused, and so is a PATH that holds an interrupted restore.
2. `verify_into` runs the checks of `archive verify` without changes: members, manifest, index, restore and `check`. The checked copy is kept in `PATH/.restore-staging/workspace`, on the destination filesystem, instead of in a removed scratch copy. If any check fails, nothing is published. The staging is removed, and PATH is left as it was found.
3. The index is rebuilt from the journal. Then `check` must pass on the staged copy, with every case head equal to the archive's. Cache, index and vault are rebuilt and never copied. The archive does not hold them.
4. Every staged directory is synced. Each top-level entry is then moved into PATH with a no-replace rename. `state/`, which holds the workspace marker, is moved last. Before that rename, PATH has no marker, so `check` and every other command refuse PATH. After that rename, PATH is the checked workspace. The empty staging folder is removed afterwards. The vault is then rendered like any `vault render`. A render never writes an existing vault outside PATH.

Nothing is written outside PATH. A kill at any step leaves one of two results. Either PATH is refused as incomplete, or PATH is complete and checked. Test `a_kill_at_every_step_leaves_path_refused_or_complete_and_a_retry_succeeds` covers each step in process. A plain retry still refuses the interrupted PATH; `--resume` (Decision 3) or a new or emptied PATH retries. A plain restore never deletes what it did not create in the same run.

**Decision 3 (bn-f97f): ownership marking and `--resume`.**

- *Marking.* Right after the staging folder is made, the restore writes `owner.json` in it: kind `investigation.restore-staging`, the archive's `export_id` and the mode. It is written to a temporary name, synced, renamed and the folder synced. Before the first rename into PATH, the restore writes `moved.json` the same way: every top-level entry publication will move, `state/` last. The restore holds an exclusive `flock` on the staging folder for its whole run. The kernel drops that lock when the process dies.
- *Resume.* `archive restore|fork DIR PATH --resume` on a new or empty PATH restores as usual. Otherwise it clears and restores again only when all of these hold:
  - PATH holds a real `.restore-staging` directory, and no running restore holds its lock. A lock that is held gives `WORKSPACE_BUSY`, exit 7.
  - `owner.json` names DIR's `export_id` and the same mode.
  - The staging folder holds nothing but the marker files and the staged `workspace`.
  - Every other entry of PATH is listed in `moved.json`.

  It removes the listed entries, then the staged copy, then the markers, `owner.json` last. A resume killed part way can therefore be resumed again. If `state/` was already published, it removes only the leftover staging folder. It does this after it checks that PATH opens as the workspace this archive restores or forks and passes `check`, and it reports `already_published`.
- *Unmarked staging.* An empty staging folder, or one holding only `owner.json.tmp`, is a restore killed before its marker was in place. Resume clears it, and only when PATH holds nothing else. Any other unmarked staging folder is refused.
- *Refusals* name what was found, remove nothing, and exit 2 with `error.path` `PATH`. Each one has a test in `restore_kill.rs`:
  - a user's folder;
  - a staging name that is a link;
  - an unmarked folder that holds data;
  - a forged marker;
  - another archive;
  - the other mode;
  - an added entry in PATH or in the staging folder;
  - a running restore.
- *Real kills.* A build with debug assertions compiles in a test pause. With `INVESTIGATION_TEST_PAUSE_RESTORE=<Step>`, the process marks the file `INVESTIGATION_TEST_PAUSE_READY` names at that step and waits. Release builds have no such code. `a_restore_killed_at_every_step_is_resumed_to_the_same_workspace` runs the binary with each step set, both restore and fork, and sends SIGKILL. The steps are Staging, Verified, Forked, Checked, Moving (one entry moved), Publishing and Published. After the kill, PATH holds no marker and only the restore's own entries, or it is the complete checked workspace. A plain run refuses it. A resume killed after clearing PATH (step Cleared) is resumed again. The final resume gives the archive's heads with the expected identity.

**Decision 2: what a fork keeps, and what it changes.**

- *Kept, byte for byte:* every case journal, so the case heads, record IDs, transaction IDs, command IDs and receipts at the fork are the origin's. Also kept: the retained blobs, the owner configuration, the human notes and a demo marker, if there is one. An old command ID returns its original receipt in the fork.
- *Changed:* the workspace ID is new. `state/workspace.json` gets an optional `fork` section, which is additive like `demo`. The section names the origin workspace, the archive's `export_id` and recovery head, and every inherited case. For each case it gives the head at the fork and the runs of events by the workspace that wrote them. A fork of a fork lists both writers.
- *Verification:* the journal verifier takes `JournalWriters`, which is the reading workspace and an optional inherited prefix. The origin's identity is accepted only for the inherited bytes. The event at the inherited sequence must hash to the recorded head, and a journal that ends before that sequence is an integrity failure. Every later event, and every case the fork creates, must name the fork. An ordinary workspace passes its own ID, and nothing changes for it.
- *Never confusable:* every event the fork commits, its `check` report, and the manifest and `archive.json` of its own workspace archives name the fork's workspace ID. `check` also reports the fork section. Two forks of one archive have two IDs, so their later events can never be read as one writer's. An exact restore keeps the original ID. Its receipt says to use it instead of the original, never beside it.

**Refusals.** A tampered, incomplete or non-archive export is refused before anything is published, with the problems that `archive verify` reports. A non-empty or occupied PATH, a missing parent and a PATH inside the archive are refused before anything is written. A live archive pin in the source workspace needs no refusal. A fork reads only the finished archive directory and never reads the source workspace. A pin exists only while its archive is written, and an archive in progress has no manifest yet, so it is refused as `MANIFEST_MISSING`. A finished archive holds and verifies every byte it needs, so a later reclamation in the source cannot change it.

**Rejected alternatives.**

- *Rewrite the journals of a fork with the new workspace ID.* Every hash, head, receipt and transaction ID chain would change. The origin snapshot would no longer be verifiable against the archive.
- *Keep the original workspace ID in a fork and add a separate fork ID.* Later events of two forks would carry the same `workspace_id` and pass for one writer. Plan §10.7 forbids this.
- *Restore in place and publish the marker last, without staging.* `check` needs a workspace with a marker, so it could not run before publication. Plan §10.7 requires verification before publication.
- *Stage beside PATH and rename the whole directory.* This writes outside PATH.

**Consequences.** The storage format is unchanged. The workspace marker gains one optional section. Three journal replay call sites pass `Workspace::writers(case)` instead of the workspace ID. A restore holds the staged copy and the published workspace on one filesystem, and it never holds two copies at once. Verification copies the archive once. `archive verify` alone still removes its own copy.
