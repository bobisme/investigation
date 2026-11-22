# investigation

![Investigation — Trace conclusions back to their sources. A hooded investigator aligns a carved stone fragment with its matching broken edge above a gorge.](docs/investigation.webp)

**Status: design stage.** This repository holds an implementation plan and its reviews. There is no code yet, and nothing described here works today.

`investigation` will be an offline, agent-native workbench for questions that need more than one look: why a request intermittently hangs, which change caused a p99 regression, whether three "independent" sources really agree, or which of several designs to choose. It records evidence, competing explanations or options, and the reasoning that links them. A fresh agent or person can then resume the case without the original chat, and can trust that nothing important silently vanished.

## What it is, and what it is not

It **records and structures**. It does not act. Agents, people and external tools do the thinking and run the experiments; `investigation` keeps:

- **Sources and artifacts.** The exact bytes, content-addressed, with where they came from.
- **Observations.** Narrow claims tied to a precise selector in those bytes, with quoted text checked mechanically.
- **Hypotheses** (possible explanations) and **options** (possible choices), kept as distinct kinds of record.
- **Assessments, comparisons, conclusions and decisions.** Each is attributed, revision-bound, and marked stale when its basis changes.
- **Reviews, objections and retrospectives.** These keep disagreement, blindspots and method lessons visible instead of letting them be silently resolved.

It is not an agent harness, scanner, benchmark runner, debugger, task tracker or model client. Nothing in it calls a model or runs an external command.

## Planned shape

- A standalone Rust CLI (`investigation`, optionally aliased `inv`) and library. It needs no server, model, network or Obsidian.
- An append-only, hash-chained event journal plus content-addressed blobs as canonical state. A rebuildable SQLite index serves search and queries.
- Transactional, schema-validated JSON writes with durable idempotency and revision checks; no parsing of agent prose.
- Layered reads (title → summary → full record → exact evidence) so agents can skim cheaply without losing caveats.
- Deterministic guidance (`next`, `frontier`, `blindspots`) that never invents a test or a score.
- A generated Obsidian casebook using native Bases and Canvas, with human-owned notes that rendering never overwrites.
- A neutral recording protocol, so any tool in any language can record runs and artifacts.
- An optional, compact MCP server over the same engine.

## The open question it must answer first

No published controlled study shows that structured evidence tracking helps agents more than a plain notes file. The plan therefore puts a controlled comparison at the first milestone. Agents investigate with `investigation` or with Markdown notes, and a successor is graded on what it concludes. The rest of the build depends on that result.

## Repository contents

| Path | What it is |
|---|---|
| [`notes/plan.md`](notes/plan.md) | Implementation specification, revision 4. Ends with a **Disposition of reviews** table. |
| [`notes/plan.review.1.md`](notes/plan.review.1.md) | Review 1: decision science, LLM failure modes, storage, MCP overhead. |
| [`notes/plan.review.2.md`](notes/plan.review.2.md) | Review 2: an independent second review that disputes several of review 1's proposals. |
| [`notes/plan.review.3.md`](notes/plan.review.3.md) | Review 3: handoff-centered evaluation, review-basis phantoms, disclosure and retention. |
| [`fixtures/README.md`](fixtures/README.md) | Deterministic fixture layout, seed and ID rules, and split isolation. |
| [`docs/deferred.md`](docs/deferred.md) | Deliberately deferred features and their revisit conditions. |
| [`docs/support-matrix.md`](docs/support-matrix.md) | Filesystem support, durability tests, and recovery limits. |

## Where to start reading

1. `notes/plan.md`, "Read this first" and "Binding decisions".
2. The **Disposition of reviews** at the end of the plan, for what changed in revision 4 and why.
3. §19 (work packages and milestones) and §20 (acceptance gates) for the build order.
