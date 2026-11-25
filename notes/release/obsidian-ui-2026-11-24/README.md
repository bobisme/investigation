# Real-viewer gate, 2026-11-24 (bn-1br4)

The W12.07 adapter (`scripts/obsidian-ui-check.py`, merged in `72625fb0`) ran against the owner's installed Obsidian, with the owner's approval. The owner opened the two test vaults by hand. Nothing else in the app was changed.

- **Viewer:** Obsidian 1.15.1 (installer 1.15.0, Flatpak `md.obsidian.Obsidian`). The CLI was `~/.local/bin/obsidian`, which answers `version` only while the app is running; the first call launched the app. The release is inside the bundled profile range (1.12.7..1.15.1).
- **Vault:** `~/inv-ui/investigation-ui-check`, made by `just obsidian-ui prepare ~/inv-ui`. It holds Scenarios A to D in existing-vault mode, plus the owner note and the probe Bases: 347 files. Restricted Mode was on, and no community plugin was enabled.
- **Build:** the debug binary built at `988ae6b3`.

## Results

| Check | Run 1 | Run 2 (after a CLI `reload`) |
| --- | --- | --- |
| `cli.version`, `profile.resolution` | pass | pass |
| `vault.target`, `plugins.community_disabled` | pass | pass |
| `vault.unchanged` | pass | **fail**: run 1's Canvas open made the viewer rewrite scenario A's `Map.canvas` |
| `setup.preview`, `setup.apply`, `setup.kept_after_reload`, `setup.honoured`, `setup.idempotent` | pass | pass |
| `viewer.files` (347), `notes.open` (35 note types), `notes.aliases` (17) | pass | pass |
| `links.anchors` (38 anchors in 38 notes) | pass | pass |
| `bases.rows` (24 Bases equal their static tables) | pass | pass |
| `bases.operators` (`!=`, `> 0`, frontier and-order) | pass | pass |
| `links.unresolved` | **fail**: all 23 file nodes of scenario C's `Map.canvas` were listed as unresolved | pass |
| `canvas.open` (2 Canvases, 41 file nodes resolve) | pass | pass |
| `viewer.errors` | none captured | none captured |

Raw reports: [run1-report.json](run1-report.json) and [run2-report.json](run2-report.json). In both copies the home folder is replaced by `~`.

## Findings (runs 1 and 2 at renderer v27)

1. **Opening a Canvas makes the viewer rewrite it (bn-24y4, a product defect).** Both case maps were rewritten when they were opened. The JSON content is identical after the prefix mapping, but the bytes differ. The renderer writes 2-space indentation with a trailing newline. The viewer writes tabs, puts each node and edge as one compact object on its own line, and adds no trailing newline. The manifest tracks bytes, so a person who opens a case map turns it into drift at the next render.
2. **Stale unresolved links on a freshly opened vault (bn-17wk, an adapter issue).** In run 1 the viewer listed every file node of one Canvas as unresolved, although every target exists. After `reload`, the list was empty within 10 s. The adapter has to let the metadata cache settle before it judges unresolved links.

3. **The viewer's link bookkeeping can stay stale even after a reload (bn-3ifo, an adapter issue).** On the first v28 run, 18 file nodes of scenario A's `Map.canvas` stayed in the viewer's `unresolvedLinks` for more than 90 s and through 2 reloads, while scenario C's 23 were resolved. Every target exists, and the viewer lists all of them. A read-only probe showed the viewer's own counts as 18 unresolved and 0 resolved. One call to `app.metadataCache.resolveLinks` on that file changed them to 0 unresolved and 18 resolved. So the viewer resolved that Canvas's links before it had indexed their targets, and a reload kept the cached answer.

   This is the viewer's bookkeeping, not a fault in the files. The adapter now asks the viewer to re-resolve such a file and reports it under `viewer_reresolved`. A link whose target the viewer does not list still fails.

## v28 runs (after bn-24y4, bn-17wk and bn-3ifo)

The vault was a fresh `just obsidian-ui prepare ~/inv-ui-v28` at renderer `inv.vault-renderer.v28`, and the owner opened it in the viewer.

| Run | Adapter | Result |
| --- | --- | --- |
| [v28 run 1](v28-run1-report.json) | `ea44df2c` (settle loop) | Every check passed except `links.unresolved` (finding 3). `vault.unchanged` passed, and both canvases still held their prepared bytes after `canvas.open`, so the bn-24y4 fix holds in the viewer. |
| [v28 run 2](v28-run2-report.json) | `1cda8061` (re-resolve step) | **PASS**: every check in one run. The viewer had stuck entries again, this time for scenario C's `Map.canvas` (23 unresolved, then 0 after the re-resolve), so the race also happens after a reload, not only on first open. |

Between the two runs, the lead's probe re-resolved scenario A's `Map.canvas` by hand. Run 2 found scenario C's stuck on its own.

## Claimed

On the strength of v28 run 2, `obsidian/compatibility-profile.json` now records `pinned_release.verification: integration` for 1.15.1 (profile revision 3). The run verified, in a real viewer at the pinned release:
- every Base equals its static table;
- the `!=` and `> 0` operators and the frontier ordering behave as the offline evaluator does;
- Canvas files open, every file node resolves, and the files are not rewritten;
- every generated note type opens;
- aliases and excerpt anchors resolve;
- there are no unresolved links in owned files;
- `obsidian setup` is honoured and changes nothing else.

The claim covers one viewer release on one machine, Linux with a Flatpak build. Viewer load timings for a 10,132-note vault are in [docs/vault-load.md](../../../docs/vault-load.md#viewer-timings-2026-11-24-bn-1br4).
