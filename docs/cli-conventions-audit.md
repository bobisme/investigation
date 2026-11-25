# CLI conventions audit

This audit compares the `investigation` CLI with
[cli-conventions.md](../.agents/edict/design/cli-conventions.md), one row
per rule. It was done for W17.01 (bn-2ygl) on 2026-11-23 against main at
`abddc9b` plus the fixes in the same change.

Status values:

- **met**: the CLI follows the rule.
- **fixed**: the CLI did not follow the rule; this change fixes it.
- **exception**: the CLI does not follow the rule on purpose. The decision
  column gives the reason.
- **filed**: a larger gap, filed as a bone under W17 (bn-3hdd).
- **help gap**: a help-text gap, handed to W17.02 (bn-27qy), which owns
  help content and the help lint. See [Help gaps for W17.02](#help-gaps-for-w1702).

The frozen v1 surfaces cannot change incompatibly: the `inv.response.v1`
envelope, the command names, the published schemas and the exit-code table
of plan §9.6. Additive changes, hidden aliases and new optional output
formats are allowed.

The evidence was produced in a scratch directory that holds `results.log`,
with `investigation init ./casework --no-vault --import-root .` and one
`start`. `<WS>` stands for the absolute workspace path that the CLI prints.

## Summary

| Status | Rules |
|---|---|
| met | 17 |
| fixed | 4 |
| exception | 9 |
| filed | 3 (bn-ahn4, bn-1nac, bn-1m44) |
| help gap | 5 (W17.02) |
| total | 38 |

E2 is met for text output and an exception for JSON output. It is counted
once, as an exception.

This change makes seven fixes. Fixes 1–6 close the four fixed rules (F1,
S3, H6, E3). Fix 7 improves I1, which was already met.

1. `--format pretty` (and `--format=pretty`, `FORMAT=pretty`) is accepted and
   prints the text form. Before, it was refused with exit 2.
2. A hidden `version` subcommand prints the same as `--version`, in text and
   JSON.
3. A refused argument repairs to the help of the subcommand that it was given
   to (`investigation doctor --help --format json`), not always to the root
   help.
4. `show` and `explain` with an unknown or malformed record ID repair to the
   case `brief`, which lists the record IDs.
5. The missing-workspace message has its missing full stop ("…filesystem
   root. Run investigation init PATH…").
6. An ingest path error gives the OS reason ("No such file or directory (os
   error 2)"), not a Rust debug dump (`Io(Os { code: 2, kind: NotFound, … })`).
   A failure at the document root no longer starts with ": ".
7. In text, the receipt of an idempotent retry says so on its first line
   (`tx_…  already_committed`). Before, it was the same as the first commit's
   receipt (I1).

`crates/investigation-cli/tests/cli_conventions.rs` pins each fix.
`crates/investigation-store/tests/raw_file_hardening.rs` now checks the
readable import reasons instead of the debug variant names.

## Command structure

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| C1 | Commands are `<tool> <nouns> <verb>` | exception | `investigation --help` lists bare verbs: `init`, `record`, `ingest`, `start`, `next`, `brief`, `show`, `check`, `search` … | Owner-approved exception (2026-11-23, bn-2ygl): `investigation` is the main noun, so the top-level verbs act on it. They are not renamed or aliased. The noun groups that exist (`case`, `source`, `evidence`, `capture`, `method`, `vault`, `repair`, `integration`) keep the noun-verb form. |
| C2 | Nouns are plural | exception | `investigation case new 'T'`, `investigation source add FILE`, `investigation method list` | The command names are frozen v1 surfaces. Each group acts on one item in one case (`case new`, `source add`), where the singular reads better; the conventions allow that exception. No plural aliases are added: aliases would add names that agents must learn and that help does not show. |
| C3 | Renames keep aliases and a deprecation note | met | No command was renamed. | Nothing to do. A later rename needs a hidden alias and a deprecation note. |

## Output formats

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| F1 | `--format text\|pretty\|json` | fixed | Before: `investigation --workspace ./casework next --format pretty` → stderr `error: invalid value 'pretty' for '--format <FORMAT>'  [possible values: text, json]`, exit 2. After: `investigation --workspace ./casework check --format=pretty` → `verified  2 case(s)  2 verified blob(s)`, exit 0. Help: `--format <FORMAT>   Select text or a single machine-readable JSON object; pretty prints the text form [possible values: text, json, pretty]` | `pretty` is the text rendering. The text form is already human-readable and never colored, so no second renderer was written. |
| F2 | `text` is the default for pipes and agents | met | `investigation --workspace ./casework next \| cat` → `case case_01a2…  snapshot 1  rules slice-next.v2` | Nothing to do. |
| F3 | `pretty` is the default on a TTY | exception | `script -qc 'investigation schema --list' /dev/null` and `investigation schema --list \| cat` give the same SHA-256 (`1c843073…`) and no `\x1b` byte. | One text rendering for terminals and pipes. Agents often run in a pseudo-terminal, and identical bytes keep their output deterministic. `pretty` is accepted (F1). A decorated terminal view would add a third surface for no agent benefit. |
| F4 | Hidden `--json` alias | met | `investigation --workspace ./casework next --json` → `{"case_id":"case_01a2…","counts":{…},…}`; `investigation --help \| grep -c -- --json` → `0` | Pinned by `tests/cli.rs` (`json_discovery_and_usage_are_single_objects`). |
| F5 | Order: flag or `--json` > `FORMAT` > TTY | met | `FORMAT=json investigation --workspace ./casework next` → JSON; `FORMAT=json investigation --workspace ./casework next --format text` → `case case_01a2…  snapshot 1 …` | `FORMAT=pretty` and `FORMAT=text` give text. The last explicit flag wins (`--format pretty --json` → JSON). |
| F6 | JSON is always one object | met | `next`, `brief`, `check`, `frontier`, `blindspots`, `onboard`, `doctor`, `schema --list`, `method list`, `search`, `vault status`, `retrospective`, `calibration`, `--version`, `--help`, `docs agents` and every failure each print one `dict` with `--format json`. | `scripts/check-cli-goldens.py` asserts this for help, discovery and failures. |
| F7 | JSON has an `advice` array and a named collection key | exception | `investigation --workspace ./casework check --format json` → `{"schema_version":"inv.response.v1","ok":true,"result":"checked","heads":…,"issues":[]…}`; a receipt has `warnings` and `next`; a failure has `error.repair` | The `inv.response.v1` envelope (`schema_version`, `ok`, `result`, plan §9.5–9.6) is frozen. Its `warnings`, `next` and `error.repair.argv` do the work of `advice`, with runnable argv instead of prose. Read views use named keys (`recommendations`, `gaps`, `checks`, `results`, `packs`), never `data`. Copying `warnings` into an `advice` key would add bytes and a second source of truth. |
| T1 | Text is ID-first with two-space fields | fixed | Receipt: `tx_01a2…` then `case_01a2…  revision 1`; `check` → `verified  1 case(s)  2 verified blob(s)`; `show ID` → `case_01a2…  title [authored_data]: "Why did requests time out?"`. Before: `investigation --workspace ./casework brief` printed `{` then 135 lines of indented JSON, and `show ID --depth full` printed 125 lines of JSON. After: the one-source brief is 11 lines (`case_01a2…  active / current  snapshot 1  level standard`, … `Next: investigation … show case_… --depth full … --format json`). For the Scenario A case, the brief goes from 770 to 51 lines and `show CASE --depth full` from 193 to 19 lines. | bn-ahn4: text is rendered from the same bounded view. `--format json` is unchanged byte for byte, and `tests/brief_text.rs` pins this against `fixtures/development/brief-show-json.json`. |
| T2 | No prose in text | exception | `next` → `No detected gaps means no gap was emitted by this rule version; it does not mean the investigation is complete.`; `search` → `Search results are pointers, not evidence. …` | These notices state the limits of a result. The plan requires them so that an agent does not read "no gap" as "done" or a search hit as evidence. They are not decoration ("Successfully created!"), and no such decoration was found. |
| T3 | Text suggests the next command | met | `start` → `Next: investigation --workspace <WS> show case_… --case case_… --format json` and `Record what the source shows, quoting it exactly: investigation … observe STATEMENT --source src_… --quote 'EXACT TEXT' …`; `init` prints the `start` line; `next` prints the `frontier` route | Nothing to do. |

## Help

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| H1 | Each command has a one-line description | help gap | Each of the 51 help pages has one, but `show --help` lists `<ID>` with an empty description, and `--depth` (`show`) and `--limit` (`next`) have none. | W17.02. |
| H2 | Each command has a usage line | met | clap prints `Usage: investigation show [OPTIONS] <ID>` on every page. | Nothing to do. |
| H3 | Each command has examples | help gap | Leaf pages without an example: `ingest`, `evidence read`, `onboard`, `next`, `show`, `observe`, `note`, `case new`, `source add`. Group pages without one: `evidence`, `capture`, `method`, `repair`, `case`, `source`. | W17.02. |
| H4 | Help shows the agent workflow | help gap | Only the root help ("Continue, record and hand off:") and `schema` show a sequence. No leaf page names the command to run next. | W17.02. |
| H5 | Help documents exit codes | help gap | `investigation --help` has no exit-code table. The table is in plan §9.6 only; `doctor` and `capture finalize` mention one exit status each. | W17.02: put the §9.6 table in the root help or the `docs agents` page. Fixed in W17.03 (bn-1tzg): the root help ends with the exit codes. |
| H6 | Wrong usage fails fast with a helpful error | fixed | Before: `investigation doctor --fix` → `error: unexpected argument '--fix' found … Retry or inspect with investigation --help --format json`. After: `… Retry or inspect with investigation doctor --help --format json`; `investigation --format json vault render --bogus` → `"repair":{"argv":["investigation","vault","render","--help","--format","json"]}` | The repair names the help page of the deepest subcommand on the command line. Global options and their values are skipped. |

## Standard commands

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| S1 | `doctor` checks prerequisites and configuration | met | `investigation --workspace ./casework doctor` → `doctor  healthy` / `binary  investigation 0.1.0` / `filesystem  workspace  tmpfs  test_use` / `workspace  ws_…  1 case(s)  index current  lock free` | There are no external services to test: the core invokes no external tool (neutrality). |
| S1a | `doctor --fix` repairs | exception | `investigation doctor --fix` → `error: unexpected argument '--fix' found`, exit 2 | `doctor` is read-only by contract (`read_only.rs`: `("doctor", Effect::Read)`; help: "It writes nothing"), so it also works under `--read-only`. Each finding names its explicit repair (`repair index`, `repair staging`, `init PATH --import-root DIR`). An automatic repair could hide a journal fork, which plan §9.6 forbids. |
| S1b | `doctor` exits non-zero when a check fails | exception | `investigation --workspace ./nope doctor` → `doctor  1 finding(s)` / `error  WORKSPACE_NOT_FOUND …`, exit 0 | The help documents exit 0: doctor must report on a workspace that it cannot open. `check` is the gating command and exits 6 (`INTEGRITY_FAILURE`) on a failed verification. |
| S2 | `config` command | filed | No `config` command; settings are in `investigation.toml` (`import_roots`, `[vault_policy]`, optional `actor`). Only `init PATH --import-root DIR` changes one. | bn-1m44: read-only `config list\|get\|path`. Coordinate the actor key with bn-6vlm. Resolved by W17.06: `investigation config list\|get\|path` reports each effective setting and its source. bn-1pa3 adds `config set\|unset` for the one key with no evidentiary meaning, `vault_policy.viewer_release`, checked against the bundled compatibility profile; import roots, the actor and the existing vault stay on `init`. |
| S3 | `version` command | fixed | `investigation --version` → `investigation 0.1.0` (met before). Before: `investigation version` → `error: unrecognized subcommand 'version'`, exit 2. After: `investigation 0.1.0`; `--format json` → `{"ok":true,"program":"investigation","result":"version","schema_version":"inv.response.v1","version":"0.1.0"}` | The subcommand is hidden. `--version` stays the documented form, and the help and goldens list no new command. |

## Exit codes and errors

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| E1 | Exit 0 success, 1 user error, 2 system error | exception | `show obs_nope` → exit 2 (`INVALID_INPUT`); `next` with no workspace → exit 3 (`WORKSPACE_REQUIRED`); a reused command ID → exit 4; `record --input nope.json` → exit 7 (`IO_FAILURE`) | The v1 exit table (plan §9.6, `ErrorCode::exit_code`) is frozen and more detailed: 0 success, 2 invalid input, 3 missing reference, 4 conflict, 5 gate, 6 integrity, 7 IO or unknown durability. It never uses 1. The JSON `error.code` names the cause. Documenting the table is H5. |
| E2 | Errors go to stderr | exception | Text: `investigation next` → stdout empty, stderr `no investigation.toml found …`, exit 3. JSON: `show obs_nope --format json` → stdout `{"commit_status":"not_committed","error":{…},"ok":false,…}`, stderr empty | Met for text. For `--format json` the failure envelope goes to stdout on purpose: it is the `inv.response.v1` response (plan §9.6), and `check-cli-goldens.py` asserts that stderr stays empty. Stdout still holds exactly one parseable object. |
| E3 | Errors say what went wrong and how to fix it | fixed | Most failures carry `error.repair.argv` or `repair.schema` (text: `Retry or inspect with …`, `See investigation schema ingest`). Gaps fixed here: H6; `show`/`explain` with an unknown ID had no repair and now give `Retry or inspect with investigation --workspace <WS> --case case_… brief --format json`; "…filesystem root Run investigation init…" now has its full stop; ingest said `path "missing.txt": Io(Os { code: 2, kind: NotFound, message: "No such file or directory" })` and now says `path "missing.txt": No such file or directory (os error 2)`; a root-level parse failure printed `: Malformed JSON` and now prints `Malformed JSON`. | `review-packet` keeps its repair-free unknown-ID error: a repair pointing at `brief` would show the conclusions that a blinded packet hides. |
| E4 | Batch operations report partial failure | met | A manifest with `"paths":["a.txt","missing.txt"]` → `/paths/1: … No such file or directory (os error 2)`, exit 2; then `investigation --actor agent:auditor receipt audit-batch` → `audit-batch  not_found_at_snapshot` | Every write is one atomic transaction (plan §9.5): it commits all or nothing, so no write can succeed on part of a batch. |

## Safety, retries and previews

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| D1 | Destructive operations have levels 1–3 | met | No command deletes canonical data. The journal is append-only; retract and supersede add revisions. `integration remove` restores the original bytes and takes `--dry-run`. `repair staging` removes only staging whose bytes are verified canonical ("Retiring again changes nothing"). `repair journal` replaces a journal file only with bytes that decode to the same content and whose hash the next file or the index already records (ADR 0016), and takes `--dry-run`. `archive restore|fork --resume` removes only what an interrupted run of the same archive listed in its own ownership-marked staging folder, under that run's lock, and refuses anything else, naming it (ADR 0013). `vault render` keeps human folders and reports drift. | Everything is level 1, so no `--yes` and no level-3 prompt. A future command that destroys data needs one. bn-1pa3: `vault clean-conflicts` deletes the edits `vault reconcile` set aside under `local/conflicts/{discarded,proposed}/`, which cannot be recovered, so it is level 2: a preview unless `--apply` is given (as `retention reclaim`), and it removes a file only while its bytes still hash to the name reconcile kept it under. `vault unrelease` is level 1: it touches no file of the person's, and `release-path` undoes it. |
| I1 | Commands are safe to retry | met | `init ./casework …` again → `ws_…  already_initialized`; `case new 'Retry probe' --command-id audit-retry-1` twice → the same `tx_…` and `case_…`; the same ID with a changed title → `/command_id: This actor has committed a different request with the same command ID.` and `Retry or inspect with investigation --workspace <WS> receipt audit-retry-1 --actor agent:auditor --format json`, exit 4; a generated ID is printed on stderr before the write (`command_id: tx_…`) | Retrying was already safe. In text the retry's receipt was the same as the first one, so this change appends `  already_committed` to its first line. JSON already had `"result":"already_committed"`. |
| P1 | Mutations support `--dry-run` | filed | Have a preview: `integration apply\|remove --dry-run`, `--print-envelope` on `observe`, `note`, `case new` and `source add` (prints the request without validating it against the workspace), `doctor bundle --preview`. No preview: `record`, `start`, `ingest`, `capture finalize`, `vault render`, `repair` (`record --dry-run` → `error: unexpected argument '--dry-run' found`). | bn-1nac: `--dry-run` that validates against the current snapshot and commits nothing. Landed for `record`, `ingest`, `start`, `observe`, `note`, `case new` and `source add` ([Dry runs](schema-v1.md#dry-runs)); the capture writes, `vault render` and `repair` still have none. |

## Agent environment

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| A1 | No persistent environment variables are needed | met | Each variable has a flag: `AGENT` → `--actor`, `INVESTIGATION_SESSION` → `--session`, `FORMAT` → `--format`, `INVESTIGATION_READ_ONLY` → `--read-only`, `INVESTIGATION_LOG` → `-v`/`-q`. No help page contains `export`. | Nothing to do. |
| A2 | No persistent working directory is needed | help gap | `--workspace PATH` works on every command, and no help page contains `cd`. A missed discovery repairs itself: `investigation next` in the parent directory → `… A workspace exists at ./casework; pass --workspace ./casework.` But the root help's "Continue" examples `case new`, `source add`, `schema case.create`, `observe` and `note` leave out `--workspace` and depend on the working directory. | W17.02: every example names `--workspace`. The README quickstart's `cd "$scratch"` is a human shell script, not help. Fixed in W17.03 (bn-1tzg): the four root-help shortcut examples now name `--workspace ./casework`. |
| A3 | `--cwd` is validated | met | No `--cwd`. `--workspace` opens only a directory that holds `investigation.toml`, and file reads are limited to the configured import roots. | Nothing to do. |

## Styling and tokens

| # | Rule | Status | Evidence | Decision |
|---|---|---|---|---|
| Y1 | No emoji | met | A scan of all 51 help pages and `docs/agent/*.md` for emoji code points found none. The outputs use ASCII and a few typographic glyphs (`—`). | Nothing to do. |
| Y2 | Respect `--no-color`, `NO_COLOR`, `TERM=dumb` and non-TTY | met | `investigation schema --list` with no setting, with `NO_COLOR=1`, with `TERM=dumb --no-color` and under `script` (a TTY) gives the same SHA-256 and 0 `\x1b` bytes. clap runs with `ColorChoice::Never`. | The CLI never colors, so every preference is honoured. `--no-color` and `--no-pager` are accepted but do nothing: there is no color and no pager (see help gaps). |
| Y3 | Progress goes to stderr | met | There are no progress bars. `-v` diagnostics and generated command IDs go to stderr; `start … 2>/dev/null` leaves only the receipt on stdout. | Nothing to do. |
| K1 | Output is a tl;dr with the next step | met | Receipts end in `Next: …`; `next` and `frontier` give routes. Next lines repeat the absolute `--workspace <WS>`. | The absolute path lets the argv run from any directory, because agents have no persistent working directory (A2). The long JSON text of `brief` was T1, fixed by bn-ahn4. |

## Help gaps for W17.02

W17.02 (bn-27qy) owns the help text and its lint. This audit did not
rewrite help text, apart from one clause on `--format` for the fix in F1.

1. Add examples to the pages listed in H3.
2. Add an agent-workflow line (the next command to run) to each leaf page (H4).
3. Put the exit-code table of plan §9.6 in the root help or the `docs agents`
   page (H5).
4. Describe `show <ID>`, `show --depth` and `next --limit` (H1).
5. Add `--workspace` to every root-help example (A2).
6. Reword `--no-color` ("Suppress terminal colors") and `--no-pager` ("Write
   directly to stdout"): the CLI never colors and never pages, so both flags
   are accepted for compatibility and change nothing.
7. `ingest --input` says "- for a path-only manifest on stdin". Lines of paths
   on stdin are refused with `Malformed JSON`; say that the manifest is JSON
   and point to `investigation schema ingest`.
8. The shortcut pages (`case new`, `source add`, `observe`, `note`) mix global
   options with the catalog fields, so the operation's own flags are hard to
   find.

W17.03 (bn-1tzg) fixed gaps 3, 5, 6 and 7 in the help text: the root help
ends with the exit-code table, every root-help example names `--workspace`,
`--no-color` and `--no-pager` say that they change nothing, and
`ingest --input` says that the manifest is JSON and names
`investigation schema ingest`.
