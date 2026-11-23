# Fixture conventions

Fixtures are deterministic, offline, and reviewable from their committed inputs. Organize them by scenario and variant:

```text
fixtures/
  development/
    scenario-a-timeout/
      baseline/
        sources/         # immutable original source bytes
        manifest.json    # source identities, acquisition context, and seed inputs
        expected.json    # outcome properties and required visible limitations
      invalid-clock-alignment/
      ...
    scenario-b-p99/
    scenario-c-architecture-choice/
    scenario-d-source-research/
  pilot/                 # separate pilot families; never mixed into development cases
```

The tree is a layout convention, not a claim that all scenario data is present. Keep sealed evaluation keys and agent-inaccessible answer material outside this repository's agent-visible tree; see AG.04 and §20.5. Do not copy held-out answers into prompts, manifests, or source labels.

## Reproducibility and identity

- Commit seed source bytes and manifests. Treat a published seed as immutable; a changed source is a new revision/variant with a recorded relationship, not an in-place edit.
- Record source provenance, acquisition context, fixture/environment, known dependence groups, and the digest of every retained original. No network access or runtime-generated remote input is needed to replay a fixture.
- Expected results describe properties: required distinctions, supported and unsupported claims, visible counterevidence, unknowns, and invalid-result handling. Do not pin generated case/object IDs in expected outcomes.
- Exercises may contain IDs for objects already committed into a tutorial case. Generate those IDs from the fixture seed during case creation and print/consume the actual returned values; never teach users to paste fabricated example IDs.
- Keep positive/control, counterevidence, and invalid-result variants explicit and independently replayable. An invalid result must remain visible with its limitation and must not be promoted as valid evidence.
- Keep development, pilot, and sealed evaluation families disjoint. A variant or source family cannot cross a split. Sealed materials do not live in this repository's agent-visible tree.

Register every family and twin in [family-registry.json](family-registry.json). Run `python3 scripts/check-fixture-families.py` to verify the split and explicit participant source allowlists. Register later sealed cases by identifier and key locator with their bytes in a separate evaluator directory.

## Expected-outcome shape

An `expected.json` should state outcome properties rather than prescribe a single narrative or generated identifier. At minimum it names the questions/hypotheses/options in scope, source properties and selectors that matter, expected limitation/counterevidence visibility, acceptable bounded outcomes, and prohibited promotions. For a choice fixture, encode constraints and evidence-cell status so every choice consistent with the declared preferences can pass; do not bake in a universal winner.

Validate fixtures mechanically against the implemented schemas and replay them through public CLI/MCP paths as those interfaces become available. Until then, label examples as schematic rather than accepted input. Fixture correctness is not established solely by a successful command or by fixture-specific recognition in application code.

## Initial Scenario A and C source shapes

These are candidate input shapes from plan §17, not canonical JSON schemas. W01's operation catalog and accepted schemas remain authoritative.

| Family | Seed source shape | Required trap or limitation |
|---|---|---|
| Scenario A — timeout | Two request traces with comparable request/span IDs, build/deployment identity, monotonic timestamps and stated wall-clock mapping; pool metrics over matching intervals; a source-code commit reference; and a small reproduction record with control and changed-transaction variants. | A third trace deliberately has missing or incompatible clock alignment. Keep its bytes and selector, but mark it unusable for ordering claims; expected properties require the gap remain visible and not count as evidence against a rival. Include an adversarial twin where the apparent refutation uses another build. |
| Scenario C — architecture choice | Three option source bundles (database-backed queue, managed queue, dedicated broker): workload and traffic projections, raw small-benchmark samples with build/environment/warmup/order/sample count, versioned operational cost assumptions, and reliability/failure-mode evidence tied to the specified recoverable-job failure model. | Explicitly encode the hard recoverability requirement, soft cost/latency/team-complexity criteria, and unknown option/criterion cells. Include a sensitivity input that can reverse the choice. An adversarial variant changes the hard threshold after decision acceptance and expects dependent comparisons/decision to need review while source evidence remains. |

For each source, the manifest should preserve origin/revision, acquisition, digest, selector support, fixture environment, and any shared provenance/dependence group. Raw benchmark observations should retain their denominators and sample-level values; do not seed only an aggregate that hides exclusions or ordering.

## Scenario D source shape

Scenario D (`development/scenario-d-source-research/`) is a source-only family: a primary specification, a trade article that restates it and reports its own bench test, a blog that repeats the article, and a first-party erratum tied to one edition. Each manifest lists `claim_origins` so three acquisitions can trace to one originating claim while every Source and selector stays intact. Variants: `baseline`, `later-edition-twin` (the erratum applies only to a later edition than the named build), and `injected-output` (crawler output with planted instructions that must stay inert data). Run `python3 scripts/check-scenario-d-fixture.py` to evaluate digests, selectors, origin structure, search coverage and the injection sample. See the family's README for roles.

## Adversarial drills 3 and 4

`adversarial-drills/` holds scripted drills from plan §17.7. Each file is a list of steps: `find` (bind a record by ID kind and body fields), `record` (one `inv.command.v1` transaction), `cli`, `audit` (`blindspots`) and `surface` (`next` or `frontier`). Each step can pin expected values. Placeholders such as `{"$": "audit/checks/[check_id=criterion.weight_without_range]/gap_id"}` read earlier bindings, and `{name/id}` inside a path reads one first (`[gap_id=blindspot:criterion.hard_unmeasured:{recovery_guarantee/id}]`), so no file pins a generated ID. The format is documented in `crates/investigation-cli/tests/drills_3_4.rs`, which replays every file through the CLI.

- `drill-3-explanatory.json` and `drill-3-evaluative.json` start from the checked-in Scenario A and C workspaces in `vault-goldens/`. They add a variant (an unassessed clock-alignment assumption under the validated root cause, or a queue choice that ignores the unknown recovery guarantee), pin the audit's check IDs (in Scenario C also the recovery-time criterion, whose cells cite the revision before its threshold changed) and record a scripted blindspot Review with a Question and an Assumption. No Conclusion or Decision changes.
- `drill-4-explanatory.json` and `drill-4-evaluative.json` build small cases whose audit is clean. A scripted fresh reviewer then adds a rival Hypothesis or Option and a material Objection through a blindspot review. `next` and `frontier` show the rival, the subject reads as disputed, and the clean audit replays byte for byte at its own snapshot.

The responses are scripted transactions, not model runs. MCP parity (W13.05) and W14 can replay the same files.
