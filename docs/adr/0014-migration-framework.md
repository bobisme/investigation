# ADR 0014 – Workspace format and migrations

Accepted with W11.15 (bn-10xc); 2026-11-24. Refines plan §3.6 and §10.7, and builds on ADR 0002, ADR 0010, ADR 0011 and ADR 0013. [Format](../../crates/investigation-store/src/workspace/format.rs) · [Migrations](../../crates/investigation-store/src/migration.rs) · [Store gate](../../crates/investigation-store/src/format_gate.rs) · [Orchestrator](../../crates/investigation-render/src/migrate.rs) · [Historical fixtures](../../fixtures/historical/README.md)

**Problem.** Plan §10.7 asks for migrations that are versioned, reversible through a backup and tested against historical fixtures. It also asks that a newer required version stops writes and that an ordinary read never migrates. Before this change, the on-disk versions were scattered and none of them could stop a write. The marker had a single version, `inv.workspace.v1`. An unknown version refused every command, reads included. Each derived store had its own version: index 5, render manifest v0, renderer v25, `inv.blob-verdict.v1` and the kept-head format. Those stores are rebuilt when their version changes. Nothing recorded which layout a workspace was at, and a newer program had no way to tell an older one to leave its workspace alone.

**No migration is needed today.** Every format written so far is read as it is. Legacy `inv.event.v0-alpha` events are upcast on read. Older indexes, render records, verdicts and kept heads are rebuilt. So this program ships an empty migration registry, and its workspace format is 1. The framework is exercised end to end by a test migration in `crates/investigation-cli/tests/migration.rs`.

## Decision 1: the format lives in the marker, as three numbers

`state/workspace.json` gains an optional `format` section, additive like `demo` and `fork`. A marker without the section is at format 1. `init` never writes the section, so a new workspace keeps the marker bytes that earlier builds wrote. Only a migration adds the section or changes it.

| Field | Meaning | A program that understands only formats up to N |
| --- | --- | --- |
| `version` | The format the workspace is at, which is the last migration applied | Above N: reads and writes it if both requirements allow |
| `read_requires` | The oldest format a program must understand to read the workspace | Above N: refuses every command, reads included (`WORKSPACE_FORMAT_NEWER`) |
| `write_requires` | The oldest format a program must understand to write the workspace | Above N: reads it and refuses every write (`WORKSPACE_FORMAT_NEWER`) |
| `migrating` | `from`, `to`, `plan_id`, `backup_export_id`, present only while a migration runs | Refuses every write (`MIGRATION_INCOMPLETE`); `check` fails |

The model is the filesystem feature set: compatible, read-only compatible and incompatible. The section is read leniently, before the strict decode of the marker. So a marker that a newer program wrote is explained, not only rejected. When a program may read a marker but may not write it, it ignores the fields it does not know. A future program must therefore raise `read_requires` for anything an older reader must not ignore. Programs from before this section existed deny unknown marker fields. They refuse a migrated workspace entirely, so they fail closed.

**Where writes are refused.** A write is refused in three places. Each place refuses with the reason, the versions and what to do next, and changes nothing:

- **The CLI.** Before any workspace command runs, every command that `read_only::EFFECTS` classifies as writing is refused. The exceptions are `export`, which only reads the workspace, and `migrate`.
- **The store.** Every journal commit reads the marker from disk under the coordination lock. This covers library callers, and a store that was opened before a migration started.
- **`init`.** `init` on an existing workspace is refused before it adds import roots.

Reads go on. `check` reports `WORKSPACE_FORMAT_NEWER` or `MIGRATION_AVAILABLE` as a warning, and `MIGRATION_INCOMPLETE` as an error. `doctor` relays these findings, and its JSON shows `format`.

## Decision 2: what a migration may change, and what it never changes

A migration (`investigation_store::migration::Migration`) has a number, `version()`. It runs on format `version() - 1`. It declares the areas it changes, and the `read_requires` and `write_requires` that the workspace has after it runs. Its `apply` must be idempotent. The `Registry` numbers migrations 2, 3, and so on, with no gaps and with requirements that never decrease.

- **May change** only the areas it declares:
  - `cache/`
  - `local/`
  - the generated vault notes and their render record, by rendering them
  - non-journal layout under `state/`

  Only the orchestrator writes the marker's `format` section.
- **Never changes:**
  - a case `genesis.json` or event file
  - a retained blob
  - a record, transaction or command ID, and so a receipt
  - `investigation.toml`
  - the human folders `Notes`, `Boards` and `Inbox`
  - `exports/` or an import root
  - the workspace ID

  The journal is canonical and append-only. If committed bytes ever had to change, that change would not be a migration. It would be a new journal format, written to a new workspace with an ID mapping, with the original archive kept (plan §10.7).

Before a migration publishes, the orchestrator checks that every case head and the owner configuration equal the backup's bytes, and that `check` passes. If any check fails, the migration does not publish. A hash chain that verifies to the same head has the same bytes, and `check` hashes every blob.

## Decision 3: dry run, verified backup, marker, apply, check, publish

`investigation migrate` is a dry run. It runs `check`, then lists the pending migrations in order, with what each one changes and what none changes. It prints a plan ID, which is a digest of these values:

- the workspace
- the from and to formats
- each migration
- the recovery head over every case head

The dry run writes nothing. `migrate --apply --plan ID` runs only the plan with that ID. If anything differs, it refuses before it writes. The steps are:

1. **Backup.** The command writes a W11.08 workspace archive of the planned heads. It is written by default to `exports/migration-<from>-to-<to>-<recovery head>`, or to `--backup DIR`, which must be new or empty. The command then verifies the archive as `archive verify` does, with a private restore and `check`. A backup that does not verify, or that holds other heads, stops the run before any change.
2. **Mark.** The command takes the coordination lock. If every case head still equals the backup's, it replaces the marker's format with one that holds `migrating`. The new marker is written to `state/tmp`, synced, and renamed over the old one, and then `state/` is synced.
3. **Apply** each migration in order.
4. **Rebuild** the index from the journal.
5. **Check**, as in Decision 2.
6. **Publish.** Under the lock, the command writes the new format without `migrating`. The CLI then renders the vault, as a restore does.

**Interruption.** A kill before step 2 leaves the workspace unchanged, and `check` accepts it. A kill from step 2 through step 6 leaves `migrating` in the marker. `check` then refuses the workspace and gives the `migrate` command, writes are refused, and reads go on. In both cases the same `migrate --apply --plan ID` finishes the migration:

- The plan ID does not depend on the marker.
- The backup is found again by its export ID, next to the default name, and verified again.
- The migrations run again.

A backup directory that an interrupted run left without a manifest is never removed. The next free name, `-2` and so on, is used instead. Test `a_migration_stopped_at_any_step_is_accepted_or_refused_by_check_and_a_retry_finishes` stops the run at every step.

**Reverse.** `archive restore BACKUP PATH` (W11.18) restores the workspace as it was before step 2. Every canonical file is the same, byte for byte, the marker included. Use the restored workspace in place of the migrated one, never beside it, because they share a workspace ID. The backup is never removed automatically.

## Decision 4: reads never migrate, and historical workspaces are kept forever

`fixtures/historical/` holds one workspace for each released format:

- Three `handoff/casework` directories of the alpha A-gate pilot. They have index version 1, and one has no case. `check-v1-legacy.py` checks them against the pilot archive.
- A frozen v1 workspace. It has blobs, a vault rendered by renderer v25 with its render record, human notes, blob verdicts and an index at version 5.

A `SHA256SUMS` test keeps every file of these workspaces unchanged. For each workspace, the tests check that it:

- opens and passes `check`
- returns every original receipt, and the original transaction when a request is sent again
- rebuilds its index
- renders, with the human folders byte-identical

The tests also check that `brief`, `show`, `next`, `check`, `migrate` and `doctor` change no file, and that the migration runs on these workspaces.

One distinction matters for "`brief` changes no file". Each entry in `cache/blob-verdicts` records the identity (device, inode, times) of the blob file it hashed. A copied workspace has new identities, so the first read of a copy hashes the blobs again and records verdicts. This is not a migration, and the test allows it once, only in that directory. After that, every read writes nothing. To make that hold, a verdict that is already recorded is no longer written again (`record_verdict`). Before this change, `check` rewrote every verdict on every run.

## Rejected alternatives

- **A separate `state/format.json`.** Every program released so far ignores an unknown file, so it would write a workspace in a newer format. The marker is the one file that every program reads, and that older programs refuse when it has unknown fields.
- **One version number.** One number cannot say "read, but do not write", so it gives either no forward compatibility or unsafe writes.
- **Migrate on open, or on the first write.** Plan §10.7 forbids silent migration. Also, a read would then need a backup, a lock and a rebuild.
- **A copy format just for migration backups.** The workspace archive already has verification, pinning, restore and fork (ADR 0013). A second format would need its own verification and its own reverse.
- **Reverse in place by rewriting the marker back.** This holds only for the marker. A migration may change other areas, and only the verified archive is known to be the state before the migration.
- **A plan ID that includes the marker.** A retry after step 2 would then need a different plan, which the person never reviewed.

## Consequences

- The marker is no longer written once. `migrate` replaces its `format` section by an atomic rename under the coordination lock. The workspace ID, the `demo` and `fork` sections, and every other field stay the same. A fork of a migrated workspace keeps the format.
- Every commit now reads the marker once under the lock, which is one small file.
- `check` JSON always carries `format`. Format 1 appears as `{"version": 1, "read_requires": 1, "write_requires": 1}`.
- `migrate` is a new command (`ReadUnlessFlag(apply)`). With the built-in registry it reports `current`.
- The first real migration adds itself to `Registry::builtin()` and raises `FORMAT_VERSION`. It needs a new directory in `fixtures/historical/` with a workspace from the last release.
