<!-- inv.migration-guidance.v1 -->
# Migrate a producer's own journal to the shared recorder

Read this if your program already keeps its own journal, manifest, run records or evidence pack and you want new evidence recorded once, by the shared recorder (`investigation docs producer`). The migration lives in your program and your tests. Investigation ships no migration module for any producer: it supplies the public capture operations, the sample manifests and fixtures, and this recipe. It does not read your files, and it never learns your name.

Goal: one authoritative shared recorder for new generic evidence. Logic that is specific to your domain or instrument stays in your program.

## The seven steps

1. **Inventory.** List what you store today and sort each item as generic (run identity, lifecycle facts, files and their hashes, completeness, receipts, assertions) or specific to your domain. Include hashing, archiving, reporting, and any budget or permission accounting. Only the generic part moves. Map each generic item to a capture field: run identity to `producer` and `external_run_id`; lifecycle to `append` facts; files to `attach` with a `role`; final state to `finalize` `execution`, `completion` and `channels`; your own extras to namespaced `metadata`.
2. **Freeze.** Copy representative historical records into your test fixtures, with their bytes. Write tests that pin your present crash and retry behavior: what happens after a crash before, during and after each write. These tests decide whether the new path is equivalent.
3. **Build a read-only, idempotent importer.** It reads old records and never changes or deletes them. It maps each old run to a new `external_run_id`, and each old file to an item `key`, deriving both from the old record alone, so a rerun sends identical requests and gets `already_committed` and `already_accepted`. Store the old-to-new ID table as a file you can regenerate. Send old files with `attach` (set `expected_digest` from your old hash), report missing files as `missing` sources, and finalize history you cannot fully vouch for with `completion` `partial` or `interrupted`. Record the original timestamps in `reported_at` and metadata, not as acceptance time.
4. **Cut over one path.** Choose one low-risk execution path. For that path the shared recorder is the only journal: each run has exactly one authoritative history. Do not write the same run to both old and new journals. If old readers must keep working, produce their view from the new record, never the reverse.
5. **Move the rest.** Move the other execution paths and your compatibility commands onto the same recording calls, one at a time, each with the frozen tests from step 2.
6. **Reconcile external enforcement.** If a budget, quota or permission system outside your program counts effects, check that after a crash it still reconciles with the durable execution references you now hold (`run_id`, `handle`, item keys, receipts). `capture recover` reports only recorded facts. A missing record is not proof that no external effect occurred; treat an unfinished step as unknown and finalize with `interrupted` and `unknown`, as the producer page says.
7. **Retire the duplicate writers.** Remove the old generic writers once parity tests and recovery tests pass. Ship a versioned rollback, or a clear refusal for the old writer, so no run ends up in two histories.

## Gates before step 7

Commands the importer and the gate tests use:

```sh
investigation docs producer
investigation schema capture --format json
investigation --workspace ./cases capture recover --handle HANDLE --format json
investigation --workspace ./cases receipt KEY --actor ACTOR --format json
investigation --workspace ./cases show RUN_ID --depth full --format json
```

Gates:

- The importer run twice yields no new records the second time.
- Every historical fixture imports, and every old file digest equals its retained digest.
- A crash injected after each call resumes without repeating an external action, and finalizes or recovers to the same result as before.
- Your producer sends every call on the `investigation docs producer` page that its runs need.
- `python3 scripts/run-conformance.py --bin BIN --producer COMMAND` passes for the migrated producer.

## Boundaries

- Use only public contracts: the CLI JSON or the `investigation-recording` crate. Never read Investigation's storage files.
- Keep your own names, formats and policies out of Investigation. Your own data goes in namespaced `metadata` that you own.
- The integration team owns and tests the real cutover. Nothing here changes the core.
