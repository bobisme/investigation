# Historical workspaces

One workspace per released on-disk format, kept byte for byte forever (plan §10.7, W11.15, [ADR 0014](../../docs/adr/0014-migration-framework.md)). Every build must open, replay, `check` and render them, and reads on them must change no file. `crates/investigation-cli/tests/migration.rs` does this in `cargo test`, and runs the migration framework on them.

**Never regenerate or edit a file here.** `SHA256SUMS` lists every file, and a test fails when a file differs, is missing or is not listed. When a format changes, add a new directory with a workspace the last release wrote, and add its files to `SHA256SUMS`; leave the older directories as they are. Git keeps no empty directory, so the tests copy a workspace and create the empty layout directories (`state/staging`, `exports` and so on) before opening it.

| Directory | Written by | Format | What it covers |
| --- | --- | --- | --- |
| `alpha-pilot-332e69ec` | The alpha build of the A-gate pilot, 2026-11-22 | marker `inv.workspace.v1`, events `inv.event.v0-alpha.2`, index version 1 | One case with one event; an index from before versions 2 to 5 |
| `alpha-pilot-bb3ab553` | Same | Same | Two cases; files the participant left in the workspace root |
| `alpha-pilot-37b1a42c` | Same | Same, no case | A workspace that was initialized and never written; a file under `local/` |
| `v1-2026-11-24` | This repository at the W11.15 change (renderer `inv.vault-renderer.v25`, render manifest `inv.render-manifest.v0`) | marker `inv.workspace.v1` without a `format` section (format 1), events `inv.event.v0-alpha.2`, index version 5 | The Scenario C workspace with retained blobs, a rendered vault and its render record, human notes in `Notes`, `Boards` and `Inbox`, recorded blob verdicts |

The three `alpha-pilot-*` directories are the `handoff/casework` directories of pilot runs `332e69ec-…`, `bb3ab553-…` and `37b1a42c-…` in [`training/a-gate/archive/pilot-raw.tar.zst`](../../training/a-gate/archive/), copied file for file. `scripts/check-v1-legacy.py` checks that each file equals its archive member.

`v1-2026-11-24` was made once: a copy of `fixtures/vault-goldens/scenario-c/workspace` with the empty layout directories, three human notes, then `vault render` and `repair index`. Unlike the golden vaults, it is not re-rendered when the renderer changes: its vault and render record stay those of renderer v25, so later builds are tested against a vault an older renderer wrote.
