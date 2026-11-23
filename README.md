# investigation

![Investigation — Trace conclusions back to their sources. A hooded investigator aligns a carved stone fragment with its matching broken edge above a gorge.](docs/investigation.webp)

**The A-gate pilot met its decision rule.** With the CLI available, successor accuracy was 76.7% versus 64.2% with Markdown notes. The command and response contracts are frozen at v1. [Pilot results](docs/a-gate.md) · [v1 contracts](docs/schema-v1.md)

`investigation` is an offline workbench for questions that need multiple observations, competing explanations or options, and a handoff. It retains original bytes, binds observations to selectors, and records attributed claims, comparisons, reviews, objections, and retrospectives. A successor can trace a finding to the exact revisions and select the next useful observation from the committed case.

## Build and check

```sh
cargo build --locked -p investigation-cli
just check
```

The executable is `target/debug/investigation`. Use the pinned Rust toolchain and a local Linux workspace. See the filesystem support matrix for the tested configurations. [Support matrix](docs/support-matrix.md)

## Quickstart

Put the built executable on `PATH`. These commands start an exploratory case, retain the first source from standard input in the same transaction, and recover its receipt and handoff.

<!-- quickstart:start -->
```sh
scratch=$(mktemp -d)
cd "$scratch"
investigation init casework --no-vault --format json
printf '%s\n' 'The connection stayed checked out while the callback waited.' |
  investigation --workspace casework --actor agent:quickstart start 'What held the connection?' --attach - --command-id first-source --format json
investigation --workspace casework --actor agent:quickstart receipt first-source --format json
investigation --workspace casework brief --level standard --format json
investigation --workspace casework next --format json
```
<!-- quickstart:end -->

To retain a file that is already in the working directory, create the workspace with `investigation init ./casework --no-vault --import-root .` and pass `--attach FILE` to `start`. Import roots are read-only inputs; a fresh workspace has none unless `--import-root` names them. Running `init` again on an existing workspace with `--import-root DIR` adds DIR.

Run `investigation onboard` from an existing workspace, the directory that contains it, or an empty directory. `investigation schema --list` lists active operations. `investigation schema observation.add --format json` describes the payload and its repair guidance. Supply an exact revision for changes to an existing record and reuse the original command ID after a lost response.

`investigation docs agents` prints the versioned agent instructions for an `AGENTS.md` file, and `investigation docs skill` prints an optional installable skill. Both pages are built into the binary and work offline; `onboard` reports their version. Producers that only record program runs read `investigation docs producer`; a program with its own journal reads `investigation docs migration`.

Nothing installs them for you. `investigation integration show --target agents-block` prints the exact managed block, and `investigation integration apply --target agents-block --path AGENTS.md` adds it to the file you name, between begin and end markers that carry the version. Apply prints a diff, accepts `--dry-run`, changes nothing outside the block, and a second apply changes nothing. `investigation integration remove --target agents-block --path AGENTS.md` restores the file's original bytes. The `skill-file` target writes a file that holds only the skill. Both refuse symbolic links and paths inside a workspace, where a file named `AGENTS.md` is evidence. `investigation integration inspect` lists the targets, versions, limits, selector validation and built-in method packs.

`investigation next` ranks up to three open gaps by ordinal factors read from recorded structure, such as whether a result could change a hard-criterion disposition or an option's Pareto status, and shows each factor with its source. It is a heuristic, not a probability or score. It groups repeated cells for one criterion, withholds soft-criterion cells on options that already fail a hard criterion, and returns snapshot-bound `frontier` routes for groups and for blocked and deferred gaps. `investigation frontier --format json` lists every gap at the same committed snapshot, unranked, with `--status`, `--kind`, `--rule` and `--reads` filters and a cursor that stays on the first page's snapshot. `investigation frontier --explain GAP_ID` names the rule and rule version that emits a gap and the fields it reads. A gap is missing or stale recorded information. It is not an assignment, and neither command dispatches work.

`investigation blindspots --format json` is a read-only audit of the recorded structure at one committed snapshot. It returns an `inv.audit.v1` response that names every check that ran and, for each finding, its check version, a stable `gap_id`, the exact subject revisions and a schema or method card. `--subject ID` narrows it to one conclusion, comparison or decision and the records it cites, and `--snapshot SEQUENCE` re-reads an earlier snapshot. A result with no finding reads "no structural gap identified under checks X". It is not proof that nothing is missing.

A recommendation from `next` can carry one method card, chosen by the rule that produced it. `investigation guide NAME` prints one card with its pack ID, version and digest, and `investigation method list` lists the packs and their cards by stage. Both work outside a workspace with the built-in `general` pack. Inside a case, a pack pinned with `method_pack.adopt` can also supply cards. A pack file that is not pinned is never used.

`investigation search WORDS --format json` finds records whose recorded text contains every word, in the selected case unless you pass `--all-cases`. Results are pointers, not evidence. Each match is labelled `editorial_summary`, `authored_text` or `recorded_evidence`, and only `recorded_evidence` counts as a source hit. Excerpts are quoted data. Records above the default audience stay out of results unless you pass `--include-restricted`. Verify a claim with `show --depth full` or `evidence read` with a selector.

Before you close a substantial case, run `investigation --case ID retrospective --draft --format json`. It reads the case at its verified snapshot and lists recorded process facts: invalid, failed or unfinished runs, plans amended after freezing, rejected hypotheses, options that were ruled out, stale reviews, and superseded or retracted conclusions. Each fact has a stable item ID and a record ID to cite. The command also puts each recorded expectation beside the recorded outcome, and reports "no expectation recorded" where there is none. The digest covers recorded work only. `retrospective.record` rejects a step or lesson that cites nothing, and the repair points to this command. `investigation --case ID retrospective --format json` reads the recorded accounts and the review status of each lesson. Another case sees a lesson only after `lesson.admit`, and reads its text with `retrospective --lessons --consult LESSON`, which records the consultation.

## Worked cases

```sh
python3 scripts/prove_milestone_a.py target/debug/investigation
```

Use `--output PATH` to keep the case workspaces and their handoffs in a new directory. The proof records a scoped timeout explanation, checks a conditional architecture choice under a changed requirement, and tests both handoffs. The proof note records the independent cold reads and durability evidence. [Milestone A proof](docs/milestone-a.md)

`python3 scripts/prove_milestone_b.py target/debug/investigation` runs the Milestone B checks on one executable from a clean copy of the committed tree. It covers four domains with their blindspot reviews and retrospectives, the producer conformance suite, the vault renders, drills 1–6 and neutrality, and writes one JSON report. [Milestone B proof](docs/milestone-b.md)

## Repository contents

| Path | Contents |
| --- | --- |
| [`crates/`](crates/) | Rust core, storage, recording boundary, and CLI |
| [`fixtures/`](fixtures/) | Synthetic sources, development cases, and adversarial variants |
| [`schemas/`](schemas/) | Published v1 envelopes, operation schemas, and reserved names |
| [`scripts/`](scripts/) | Worked proofs, local qualification, and repository checks |
| [`docs/adr/`](docs/adr/) | Architecture and product decisions |
| [`docs/deferred.md`](docs/deferred.md) | Deferred features and revisit conditions |
| [`docs/support-matrix.md`](docs/support-matrix.md) | Filesystem support, durability tests, and recovery limits |
| [`notes/plan.md`](notes/plan.md) | Implementation specification, review dispositions, and acceptance gates |

The sealed release trial follows the A-gate pilot. Its preregistration fixes the outcome, burden limit and sample size before sealed cases are opened. [Release trial](docs/release-trial.md)
