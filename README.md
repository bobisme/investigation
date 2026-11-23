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

Run `investigation onboard` from an existing workspace or an empty directory. `investigation schema --list` lists active operations. `investigation schema observation.add --format json` describes the payload and its repair guidance. Supply an exact revision for changes to an existing record and reuse the original command ID after a lost response.

## Worked cases

```sh
python3 scripts/prove_milestone_a.py target/debug/investigation
```

Use `--output PATH` to keep the case workspaces and their handoffs in a new directory. The proof records a scoped timeout explanation, checks a conditional architecture choice under a changed requirement, and tests both handoffs. The proof note records the independent cold reads and durability evidence. [Milestone A proof](docs/milestone-a.md)

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
