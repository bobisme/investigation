# Obsidian

Read-only snapshot rendering and native Bases and Canvas fixtures.

The renderer itself is the `investigation-render` crate in
`crates/investigation-render/`. Its crate documentation describes the
architecture: a pure `RenderPlan` built from one verified snapshot, an
ownership-aware publisher, and a renderer lock separate from the commit lock.
This directory holds vault fixtures; rendering never requires Obsidian to be
installed or running.

Syncing a vault, using it on a phone, and the paths that must never be
synchronized are in [`docs/obsidian-sync.md`](../docs/obsidian-sync.md).

## Compatibility profile

`compatibility-profile.json` pins the supported Obsidian release (1.15.1,
verified against documentation only; the W12.07 integration gate records the
release it actually runs against), records the documentation pages and
revisions consulted, and lists every viewer feature with a tier: `baseline`
(plain Markdown), `optional` (Bases and Canvas, allowed only when the installed
release is inside the supported range) or `unsupported` (never emitted, such as
Bases row sort keys, which the syntax page does not document). An absent,
unparseable or out-of-range release resolves to the baseline.

The profile is read by `investigation_render::compat`. Its `detect` function
reads vault bytes and names the features in use, so tests check offline that
every feature a rendered vault uses is listed as emitted. A renderer change
that adds a feature must add it to the profile first. Re-check the documentation
and raise `supported_range.max` before relying on a newer release.

## Native Bases

The renderer can plan one native Base beside each static table view, at
`Cases/<case-id>/Views/<view>.base`. The comparison matrix is the exception: it
stays a static table in v1 (plan §12.5). Bases are optional, and they are off
by default. To turn them on, state the viewer release in the workspace's
`investigation.toml`:

```toml
[vault_policy]
enabled = true
viewer_release = "1.15.1"
```

The working vault plans Bases only when `compat::resolve` allows every Base
feature for that release (`bases::FEATURES`). An absent, unparseable or
out-of-range release gets no `.base` file. Plan §12.3 says "Tables are the
baseline", and it asks for "feature detection or a tested compatibility
profile, not assumptions that every user has every current view type". The
renderer never contacts the viewer, so the stated release is the detection.
Without one, the vault holds no file that only some releases can read. Bases
are never emitted as inert extra files. The static tables hold the same rows,
so a vault without Bases is complete.

Every Base uses only documented syntax: a top-level `filters` with `and`/`or`,
`properties` with `displayName` (column labels only), and one `table` view
with `order`. It never uses a row sort key, formulas, summaries or another view
type. Strings are emitted by the tested YAML escaper. The filter always starts
with `inv_case_id == "<case-id>"`, so a note of another case never appears,
whatever folder it is in. Then it adds the view's record kinds:

| Base | Notes listed (besides the exact case ID) |
|---|---|
| Questions, Hypotheses, Options, Criteria, Sources, Conclusions, Decisions | `inv_kind` of that record kind |
| Evidence | artifacts, and observations whose `inv_value_kind` is not `measurement` |
| Measurements | observations whose `inv_value_kind` is `measurement` |
| Inquiries and Runs | `inv_kind` `inquiry` or `run` |
| Reviews and Blindspots, Retrospectives and Lessons | `inv_kind` `review` or `retrospective` |
| Epistemic Frontier | notes other than views with `inv_open_gaps > 0`. These rows match the static note's "Records named by an open gap" table. |
| Timeline | every note of the case except views. These rows match the static note's "Latest revision of each record" table. |

Every record note and Overview carries `inv_open_gaps`, and every observation
note carries `inv_value_kind`, even when its content is withheld. So the
Bases select the same notes as the static tables. Each property that a Base
compares with `!=` or `>` is present on every note that the Base can reach.

Base files are tool-owned and listed in the ownership manifest. Each one
depends only on its case ID and view, so identical renders and later snapshots
produce identical bytes. Editing a Base file is drift. Editing a cell in a
Base changes the underlying note's properties, which is also drift: the note
is left in place, no longer updated, and the edit is not recorded. Each view
note that has a Base says this under `## Native Base`. When Bases are turned
off, the next render removes the Base files that were not edited.

The checks run offline. `vault_fixtures` evaluates each golden Base filter
against the golden notes' frontmatter. It confirms that the matching notes are
exactly the rows of the static table, all in one case. Scenarios A and C state
release 1.15.1. Scenario D states no release, so it shows the default.
Opening the Bases in the viewer is the job of the W12.07 integration gate.

## Case map

When the stated release allows `canvas.file`, `canvas.file_nodes` and
`canvas.labeled_edges`, each case gets `Cases/<case-id>/Map.canvas` (JSON
Canvas 1.0) with `Map Legend.md` and `Map Left Out.md` beside it. Every node is
a file node pointing at a generated note, every edge carries its relation in
words, and nothing is coloured. The columns follow the case's focus
(explanation, evaluation, research or measurement), each capped at 8 records,
with at most 96 edges; the left-out note says what the bounds dropped and links
the tables that list it. The map is tool-owned and regenerated on every render;
copy it into `Boards/` to keep an editable version.

## Existing-vault mode

By default the vault is the workspace's own `vault/` folder. To render the
casebook into a vault you already have, name that vault and one folder in it
in the workspace's `investigation.toml`:

```toml
[vault_policy]
enabled = true
existing_vault = "/home/me/Notes"
managed_prefix = "Investigation"
```

`existing_vault` is an absolute, canonical path (no symbolic links) to an
existing folder outside the workspace, and the workspace must not be inside
it, so canonical state is never synchronized as notes. `managed_prefix` is
one to four nested folder names of ASCII letters, digits, spaces, `-`, `_`
and `.`, none hidden, such as `Investigation` or `Projects/Investigation`. Its
parent folders must already exist; the renderer creates only the prefix
folder itself. The workspace's own `vault/` is then not used. `vault status`
reports the mode under `projection`.

The rules:

- **Only the prefix is touched.** Every write, rename and removal happens
  inside `<existing_vault>/<managed_prefix>/`, through directory handles that
  never follow links. Nothing outside it is written, moved, deleted or read:
  not your notes or their properties, not the viewer's configuration folder,
  its settings, themes, plugins or property types.
- **The ownership table applies inside the prefix.** `Start Here.md` and
  `Cases/` are generated; `Notes/`, `Boards/` and `Inbox/` inside the prefix
  are yours, and `vault reconcile --disposition preserve-as-note` moves an
  edit into `<prefix>/Notes/Reconciled/`. `vault copy-map --to
  Boards/<name>.canvas` writes into `<prefix>/Boards/`, never your vault's
  own `Boards/`. `Start Here.md` names the prefix.
- **Links stay inside the prefix.** Every generated wikilink, frontmatter
  link and Canvas file path, on generated maps and copied boards alike,
  starts with the prefix
  (`[[Investigation/Cases/<case-id>/Overview|...]]`), so none resolves to a
  note of yours with the same name, such as your own `Start Here.md`.
- **Your notes never appear in a generated view.** Tables are built from the
  snapshot alone, and every Base filters on the exact `inv_case_id`, which
  your notes do not carry.
- **The first render needs an empty prefix.** When the manifest records
  nothing yet, a render refuses if the prefix already holds a file the
  renderer does not own, unless the file already holds exactly the bytes
  planned for its path (a lost manifest is rebuilt that way). Empty folders
  and hidden files do not count.
- **The manifest is complete and bound to its folder.**
  `local/render-manifest.json` lists every path the renderer wrote: owned
  generated files (notes, views, `.base` files, `Map.canvas`, `Start Here.md`),
  paths released to you, and files moved into `Notes/Reconciled/` with the
  path they came from. It records the vault and prefix it was written for;
  if the setting changes while the manifest still records paths, render,
  reconcile and remove refuse. Run `vault remove` with the earlier setting
  first.
- **Removal deletes only owned files.** `investigation vault remove` deletes
  each owned file only while it holds exactly the bytes recorded for it, then
  the folders that leaves empty, and the prefix folder last when it is empty.
  Edited generated files stay (and stay listed by `vault conflicts`), as do
  released paths, `Notes/Reconciled/`, your own files and anything the
  manifest does not own. Nothing is deleted recursively. `--dry-run` lists
  what would happen. The workspace journal is unchanged, and `vault render`
  projects the casebook again.

Changing the managed prefix renames nothing: remove the projection, change
the setting, then render.

### Property types

Every generated property is `inv_`-prefixed, so vault-wide property types you
set for your own notes never apply to them. The casebook expects these types.
If your vault already assigns a different type to one of these names, the
viewer may show a warning on generated notes; nothing breaks.

| Type | Properties |
|---|---|
| Text | `inv_artifact_role`, `inv_availability`, `inv_basis`, `inv_case_id`, `inv_disposition`, `inv_effective`, `inv_evidentiary_use`, `inv_focus`, `inv_freshness`, `inv_generation`, `inv_id`, `inv_integrity`, `inv_kind`, `inv_mode`, `inv_quote_state`, `inv_retention`, `inv_review_freshness`, `inv_review_kind`, `inv_schema`, `inv_selector_validation`, `inv_severity`, `inv_snapshot`, `inv_source_access`, `inv_status`, `inv_summary`, `inv_value_kind`, `inv_verdict`, `inv_view` |
| Text holding one link | `inv_case` |
| Number | `inv_basis_sequence`, `inv_lineage_depth`, `inv_open_gaps`, `inv_render_sequence`, `inv_revision`, `inv_rows` |
| Checkbox | `inv_assessed`, `inv_withheld` |
| Date & time | `inv_updated` |

Generated notes also carry the viewer's own `aliases` and `tags` lists.

## Setup preview

`investigation obsidian setup --vault PATH` shows, and only with `--apply`
writes, a few native viewer settings in the vault's configuration folder. It
is for the native baseline only: it never installs, enables or disables a
plugin, never changes Restricted Mode, and never opens `community-plugins.json`,
`plugins/`, `themes/`, `snippets/`, `hotkeys.json` or `workspace.json`.

- **Without `--apply` nothing is written.** The response lists each proposed
  key with its current value, an action (`add`, `change` or `unchanged`), the
  exact unified diff, and the command that would apply it. `--read-only`
  accepts the preview and refuses `--apply`.
- **Apply changes only the previewed keys.** It splices the values of the
  selected keys into `app.json`'s own text, so every other byte of the file
  is as it was, and no other file is opened. It then checks that every other
  key parses to the value it had. `--setting KEY` (repeatable) limits the
  keys; without it every key in the table is proposed. A second apply changes
  nothing.
- **The configuration folder** is `.obsidian` when it exists. Otherwise it is
  the one hidden folder of the vault that holds an `app.json`; two such
  folders are refused as ambiguous. `--config-dir NAME` names a custom folder.
  The folder must exist: setup creates no folder, so open the vault in the
  viewer once first. The vault path and the folders it names must not be
  symbolic links, and `app.json` must be a UTF-8 JSON object.
- **The write is atomic.** A temporary file beside `app.json` is renamed over
  it only if `app.json` still holds the bytes the diff was computed from.

The settings it can propose:

| Key | Proposed value | Why |
|---|---|---|
| `useMarkdownLinks` | `false` | Generated notes link with wikilinks; links you create then use the same form. |
| `newLinkFormat` | `"absolute"` | Generated links name the path from the vault root, so a link you create cannot resolve to a generated note of the same name. |

A test keeps this table equal to the `SETTINGS` table in
`crates/investigation-cli/src/obsidian_setup.rs`.
