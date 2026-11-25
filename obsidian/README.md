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

## Integration gate

The offline tests show what the files say. Whether a real viewer opens them the same way is
the job of an optional gate, `scripts/obsidian-ui-check.py` (W12.07, plan §13.4: "No Obsidian
automation is part of the core headless test suite"). It drives a running desktop app through
the app's official command line interface. `just check` never runs it and needs no viewer; it
runs the adapter's own tests against a fake CLI (`scripts/test-obsidian-ui-check.py`).

```sh
just obsidian-ui prepare ~/inv-ui     # no viewer involved
# open ~/inv-ui/investigation-ui-check in the viewer once ("Open folder as vault");
# keep Restricted Mode on, so no community plugin runs
just obsidian-ui run ~/inv-ui         # report: target/obsidian-ui/report.{json,txt}
```

A sandboxed install of the viewer (a Flatpak, for example) opens only the folders it is
granted, often just your home folder, so keep DIR there.

`prepare` renders the Scenario A to D fixture workspaces into one dedicated vault, each under
its own managed prefix (`Investigation/scenario-a` and so on, existing-vault mode). It adds one
note of "the owner" with a deliberately unresolved link, and a `UI Check Probe/` folder of
probe notes and Bases. It writes no viewer configuration and records the hash of every file in
`DIR/expected.json`.

`run` checks, and the report names the viewer release, the profile resolution, the vault and
each result:

| Check | What passes |
|---|---|
| `cli.version`, `profile.resolution` | `version` names a release, and `investigation doctor --viewer-release RELEASE` resolves it to the optional features |
| `vault.target`, `vault.unchanged` | `vault=investigation-ui-check vault info=path` is exactly the prepared folder, whose files still hold their prepared bytes |
| `plugins.community_disabled` | `plugins:enabled filter=community` is empty (Restricted Mode is only read, never toggled) |
| `setup.*` | `investigation obsidian setup` previews the two keys and writes nothing; `--apply` changes only those keys of `app.json` and no other file of `.obsidian` (except the viewer's own `workspace.json`); after `reload` the viewer keeps both and creates a new link as a vault-root wikilink; a second apply changes nothing |
| `viewer.files`, `notes.open`, `notes.aliases` | the viewer lists every prepared file; one note of each generated type opens as Markdown; each title alias is reported |
| `links.anchors` | every excerpt anchor a link names is in the viewer's block cache |
| `bases.rows` | `base:query path=... view=... format=paths` of each Base returns exactly the rows of its static table |
| `bases.operators` | probe Bases: `!=` and `> 0` select as the offline evaluator does, and the frontier filter gives the same rows in either and-order; rows for an absent property are recorded, not judged |
| `links.unresolved` | `unresolved verbose` names no source inside a managed prefix, and does name the owner's planted link |
| `canvas.open` | each `Map.canvas` is listed, opens as a Canvas, and every file node resolves |

Every call is an argv list, never a shell string. `vault=<name>` comes first and every file is
named with `path=<exact vault path>`, never `file=`, as the CLI documentation advises. Three
checks (active view, block cache, new-link form) use the CLI's `eval` with fixed JavaScript
from the script; `--no-eval` skips them. Exit status: 0 when every check passes, and also when
no CLI is found or it does not answer `version` (status `skipped`, with the reason); 1 when a
check fails; 2 when the run cannot start (status `blocked`), for example when the vault has not
been opened in the viewer yet.

The commands used were checked against <https://obsidian.md/help/cli> on 2026-11-24 (the page
carries no revision). It documents commands and parameters but not exit codes, error output,
or the JSON shape of `unresolved` and `eval`; the script fails a check on output it does not
understand and keeps the raw output in the report. The CLI needs the 1.12 installer or later,
and "if Obsidian is not running, the first command you run launches Obsidian". On Linux the
app registers the CLI by copying it to `~/.local/bin/obsidian`; pass `--obsidian PATH` (and
`--obsidian-arg` for a launcher's own arguments) when the `obsidian` on `PATH` is something
else.

`timings --vault VAULT [--samples N] [--allow-restart]` measures the viewer side of the vault
load test against a vault kept by `just load-test DIR --cases 130` (pass
`DIR/viewer_1_15_1/vault`): CLI round trip, one Base query, Canvas open, search, and window
reload until every file is listed. A cold open needs `--allow-restart`, because `restart`
restarts the whole app. The report ends with a table in the form of `docs/vault-load.md`.

When the gate has passed on a release, record that release in `compatibility-profile.json`
(`pinned_release.verification` becomes `integration`).

## Native Bases

The renderer can plan one native Base beside each static table view, at
`Cases/<case-id>/Views/<view>.base`. The comparison matrix is the exception: it
stays a static table in v1 (plan §12.5). Bases are optional, and they are off
by default. To turn them on, state the viewer release with `config set`,
which refuses a release outside the profile's supported range and writes
nothing then; `config unset` turns them off again. The next render adds or
removes the files:

```sh
investigation --workspace ./casework config set vault_policy.viewer_release 1.15.1 --format json
investigation --workspace ./casework vault render --format json
```

The command writes this key of the workspace's `investigation.toml`:

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
when you create the workspace, or run the same command on an existing one:

```sh
investigation init ./casework --existing-vault ~/Notes --managed-prefix Investigation --format json
```

`--managed-prefix` defaults to `Investigation`. The command checks the
setting before it writes anything and refuses an unsafe one with a repair
command (for example a hidden folder, a missing parent folder, or a prefix
that is a file or a link), and writes these settings to the workspace's
`investigation.toml`, which you can also edit by hand:

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
  own `Boards/`. `vault snapshot-note Notes/<name>.md --on <record>` reads
  `<prefix>/Notes/<name>.md` (never a file outside the prefix, and never
  through a link) and records its exact bytes for the record that relies on
  it. `Start Here.md` names the prefix.
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
  if the setting changes while the manifest still owns generated files,
  render, reconcile and remove refuse, and `init --existing-vault` refuses
  the change. Run `vault remove` with the earlier setting first.
- **Removal deletes only owned files.** `investigation vault remove` deletes
  each owned file only while it holds exactly the bytes recorded for it, then
  the folders that leaves empty, and the prefix folder last when it is empty.
  Edited generated files stay (and stay listed by `vault conflicts`), as do
  released paths, `Notes/Reconciled/`, your own files and anything the
  manifest does not own. Nothing is deleted recursively. `--dry-run` lists
  what would happen. The workspace journal is unchanged, and `vault render`
  projects the casebook again.

- **Moved notes are looked for only inside the prefix.** When a generated
  note disappears, the next render looks for its bytes elsewhere under the
  prefix and reports it as renamed. It never lists or reads the rest of your
  vault, so a note moved out of the prefix is reported as missing. The search
  is bounded (`vault.moved_note_bytes`, `vault.moved_search_files` and
  `vault.moved_search_bytes` in `docs/support-matrix.md`); past a bound an
  unfound note is also reported as missing.

Changing the managed prefix renames and moves nothing. Remove the projection
under the current setting, write the new one, then render:

```sh
investigation --workspace ./casework vault remove --dry-run --format json
investigation --workspace ./casework vault remove --format json
investigation init ./casework --existing-vault ~/Notes --managed-prefix Projects/Investigation --format json
investigation --workspace ./casework vault render --format json
```

Generated files you edited survive the removal and stay owned, so the init
refuses until each is resolved: `vault reconcile PATH --disposition
release-path` keeps it in place as yours (or `preserve-as-note`), then run
`vault remove` again. Everything that stays in the old prefix after that
(your `Notes/`, `Boards/` and `Inbox/`, released paths and
`Notes/Reconciled/`) is yours: the renderer forgets those paths when the new
prefix is bound and never touches them again. Move them yourself if you want
them in the new prefix. There is no move command, because a move would have
to relocate your own files.

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
