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
