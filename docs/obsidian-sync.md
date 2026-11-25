# Vault synchronization, mobile use and what never to sync

A sync tool synchronizes files. It does not synchronize Investigation transactions. A vault copied to another machine, phone or backup is a set of generated Markdown files and your own notes. It is not a resumable investigation: `brief`, `next`, `show` and every command that writes read the workspace journal under `state/`, and a vault holds none of it. This page says what to put where. The path and ownership tables below are checked against the code by a test (`crates/investigation-cli/tests/obsidian_docs.rs`).

Nothing here is a requirement on a product. Any tool that copies files works the same way, and so does none.

## Recommended deployment

1. **One authoritative workspace.** Keep the workspace on one machine, on a local filesystem. The support matrix (`docs/support-matrix.md`) lists cloud-synchronized canonical directories as unsupported.
2. **An optional review vault outside the workspace.** Run `investigation init WORKSPACE --existing-vault VAULT [--managed-prefix FOLDER]`, or set `existing_vault` and `managed_prefix` in `investigation.toml` (see `obsidian/README.md`, "Existing-vault mode"). The workspace must not lie inside that vault, so canonical state is never synchronized as notes. Sync that vault, or only the managed prefix, as you like.
3. **Mobile for browsing and your own notes.** A phone reads generated notes and writes only in the folders that are yours. Nothing a phone does changes a record.
4. **A full workspace copy for recovery.** To recover, copy the whole workspace folder (`state/`, `investigation.toml` and the rest) while no command runs, as `docs/agent/exposure.md` describes under restore. The support matrix lists archive restore as unqualified: check a copy with `investigation check`.

Do not put the workspace's SQLite index and its write-ahead log (`cache/index.sqlite*`) under a sync tool and call that collaboration. Two machines that write the same files do not share a journal, and this build has no merge. The index is disposable and `investigation repair index` rebuilds it, but a synced copy can be torn.

## What must never be synced

These paths hold canonical state, restricted material or machine-local state. Keep them outside every sync scope. The Area column is the name `investigation exposure` reports for the path, taken from the same classifier.

| Path | Area | Why it stays out |
|---|---|---|
| `state/` | `workspace_state` | Canonical. A second writer breaks the hash chain, and `investigation check` then reports an integrity failure. |
| `state/blobs/<digest>` | `blob` | Retained evidence bytes, including blobs of restricted records. A generated note of a restricted record is a stub, but the blob is the full bytes. |
| `state/staging/<handle>` | `staging` | Capture recovery material, not cache. |
| `state/locks/commit.lock` | `workspace_state` | The commit lock. A lock file that travels to another machine coordinates nothing. |
| `local/render.lock` | `local` | The renderer lock. Same reason. |
| `cache/index.sqlite`, `cache/index.sqlite-wal` | `index` | Disposable database and write-ahead log. Search text lives in it. |
| `cache/blob-verdicts/<digest>` | `blob_verdict` | Disposable cache. |
| `local/render-manifest.json` | `render_manifest` | The vault manifest. It records the hash of every generated file and, in existing-vault mode, the vault and prefix it was written for. On another machine it would claim files it did not write. |
| `local/conflicts/` | `render_conflict` | Generated versions withheld because a vault file had drifted, and under `discarded/` and `proposed/` the edits `vault reconcile` set aside, which can hold text you meant to drop. `investigation vault clean-conflicts` lists those edits and the withheld versions the render manifest recorded and, with `--apply`, removes them. `investigation exposure` lists the edits as `reconcile_set_aside`. |
| `local/session-reads/` | `session_reads` | Machine-local record of evidence reads. |
| `investigation.toml` | `config` | Machine-specific settings, such as absolute import roots and `existing_vault`. |

Never keep a symbolic link to evidence in a vault or in a sync scope. The renderer does not follow links: it refuses a link as not a regular file (`vault remove` reports `not_a_regular_file`), and sync tools differ in how they treat links. Copy a file in as evidence with `investigation ingest` instead.

## What may be in the synced vault

The vault, or the managed prefix, has one ownership table, printed on `Start Here.md` and enforced by the renderer (`investigation_render::layout::VAULT_AREAS`).

| Top-level name | Owner | In a sync scope |
|---|---|---|
| `Start Here.md` | Investigation | Yes. Generated; edit it and the edit is drift. |
| `Cases/` | Investigation | Yes. Generated notes, views, `.base` files and `Map.canvas`. |
| `Notes/` | You | Yes. Your analysis. It is not evidence until you add it to a case. |
| `Boards/` | You | Yes. Your canvases, including copies made by `vault copy-map`. |
| `Inbox/` | You | Yes. Candidate material; it becomes evidence only when you add it to a case. |
| `Attachments/` | Investigation (previews only) | Yes, but only after you decide: it holds approved previews of reviewed derivatives, and only when `include_approved_derivatives` is set. Never an original. |
| Hidden folders | You | Your choice. They hold the viewer's own settings. Rendering never writes in them. |

A sync tool can leave a conflict copy beside a generated note. A render never removes or rewrites it (area `vault_conflict_copy` in `investigation exposure`). Delete it by hand after you decide which text is right, then run `investigation vault render`.

A render is not atomic to a reader of the folder. Let a render finish, and `investigation vault status` report state `complete`, before you rely on a copy.

While a render writes, it stages up to 2,048 notes at a time as hidden files beside their targets, named `.NAME.TAG-N.inv-tmp`, and moves a retired note aside as `.NAME.TAG-N.inv-del` before removing it. Each is renamed into place or removed within the same render; a render that was killed or lost power leaves some behind, and the next render removes them. A sync client or file watcher can see them come and go. If your client takes exclusion patterns, exclude `*.inv-tmp` and `*.inv-del`. Never edit or rename them.

## Before you export or share

Choose what is shared before you render or export sensitive evidence. Removing a file from a later export or render does not retract copies already made: a sync client, a backup, a phone and a recipient each keep what they had. After `classification.raise`, a render replaces the note with a restricted stub, and the earlier copies stay as they were. `investigation exposure --text-file FILE` finds where text lives; `docs/agent/exposure.md` has the steps and the limits. No command here uploads, publishes or pushes anything, and initialization never starts a sync.

## Mobile

- A phone is a reader and a place for your own notes in `Notes/`, `Boards/` and `Inbox/`.
- Editing a generated note, including a value shown in a Base, is drift. The note stays, is no longer updated, and the edit is not recorded. `investigation vault reconcile` offers the choices.
- Bases and `Map.canvas` are written only when `[vault_policy] viewer_release` names a release the compatibility profile supports (`obsidian/README.md`). A phone that runs an older release than the one you state can show an optional file wrongly. State the oldest release you read the vault with, or leave it unset for plain Markdown, which the static tables cover completely.
- To get a phone's notes into a case, add them on the workstation with an `investigation` command. Nothing imports them automatically.

## Native setup preview

`investigation obsidian setup --vault PATH` shows the exact diff of a few native viewer settings and writes nothing. `--apply` writes only the selected keys. See `obsidian/README.md`, "Setup preview". It never installs, enables or disables a plugin and never changes Restricted Mode.

## Plugins are not required

The generated vault uses only native Markdown, properties, links, Bases and Canvas. This table says what the project does not need, by kind.

| Kind of add-on | Required | Why |
|---|---|---|
| Query-language plugins | No | Generated views are static tables and native Bases. Embedded executable queries add avoidable risk. |
| Task-list plugins | No | Useful for your own to-dos. Checking a box never changes a record. |
| Drawing plugins | No | Your own diagrams belong in `Boards/`. Native Canvas draws the generated map. |
| Local server or URI-handler plugins | No | The command line and the files are enough. No generic control server is needed. |
| Version-control plugins | No | Optional manual history of notes. Never canonical transactions. |
| A companion for Investigation | Optional, not built | A later companion would be opt-in, previewable and removable. This build has none, and `obsidian setup` installs nothing. |

Community plugins run with the viewer's own access, so installing one is your decision.
