# ADR 0023 – Validity context applies to measurements, and a recorded validity is the recorder's statement

Accepted with bn-32z3 (DF.04); 2026-11-24. Refines plan §5, §6.3 and §6.4. [Rule](../../crates/investigation-core/src/gap_rules.rs) · [Measurement test](../../crates/investigation-core/src/measurement_ops.rs) · [Gate](../../crates/investigation-core/src/explain_ops.rs) · [Contract](../schema-v1.md#measurement-validity-context) · [Dogfood note](../../notes/dogfood/2026-11-24-perf08.md)

**Problem.** The W14.10 dogfood (friction F5 and F8) recorded two real cases. The recording agent and the hosted successor hit the same four problems:

- `observation.add` accepted `validity: valid`, but `frontier` still filed `observation.validity_context_missing` (blocked by quality) until a separate `measurement.assess` existed. Nothing at `observation.add` said that the field does not count.
- The rule fired on every Observation, including code readings and a quote from a bone, which are not measurements. There were 12 such gaps after the first batch and 3 at closure that nobody could sensibly judge.
- `measurement.assess` refused `valid` without a verification. The cases had no Runs, so a `control` could not qualify, and the refusal did not say which kinds need no Run.
- `assessment.add` refused a measurement judged `partial` as support with "invalid, unverified, or unattributed". The message did not mention `partial`. Both agents dropped the evidence and cited it in prose.

**What the plan says.**
- §5 says that "measurements and extracted statements are observations". The two are different things inside one record type.
- §6.3 defines measurement validity (`valid | invalid | partial | unknown`, per observation or channel) as whether the instrument, setup and controls support interpreting a measurement. It is a property of measurements.
- §6.4 makes `measurement.assess` the step that records it, with an instrument check, a control or an equivalent verification.

**Decision.**

1. *A recorded validity is the recorder's statement, and the receipt says so.* `observation.add` and `observation.correct` still accept `validity: valid` or `partial` on a measurement, and the record keeps it with `validity_basis: recorded_only`. The receipt adds one warning: it names the validity as the recorder's own statement, says that `frontier` keeps `observation.validity_context_missing` until `measurement.assess` records an attributed judgment with an `instrument_check`, a comparable `control` or a justified `equivalent`, and says that a confirmatory basis needs that judgment too. The rule still requires the judgment.
2. *The rule reads measurements only.* An Observation is a measurement when its `value.kind` is `measurement`, it has a `measurement` block, it cites a Run (`run_ref`), or its `evidence_kind` is `interventional`. These are typed fields, so the rule makes no guess from text. A text or binary extracted statement without them, such as a quote or a code reading, gets no `observation.validity_context_missing` gap and no warning. Its diagnostic force still comes from the selector, quote and artifact checks of the evidence gate.
3. *The `measurement.assess` refusal gives a path without Runs.* A `valid` or `partial` judgment without a qualifying verification still fails with `GATE_FAILED` at `/verification`. The message now names the three kinds and says that an `instrument_check` or an `equivalent` needs no Run. When a control failed on environment, the message says that a control is compared by recorded environment: the Run each Observation cites, or the structured environment identity in its measurement fields. It ends with the honest alternative: judge the measurement `unknown` or `invalid`.
4. *The evidence gate names `partial` and what is allowed instead.* When `assessment.add` (relation `supports` or `contradicts`) or a `rejected_within_scope` disposition cites an Observation that is not effectively valid, the refusal lists each unmet condition. The first is the effective validity, for example "its effective measurement validity is partial: a channel or an assessor's judgment is not valid, so the Observation as a whole is not". It says to cite the Observation with relation `inconclusive` or `non_diagnostic`. For `partial`, it also says to record an Observation of the valid part alone, assess it and cite that. `error.actual.effective_validity` carries the value.
5. *The gap rule version moves to `slice-gaps.v2`.* Item 2 changes which subjects one rule emits for. Every gap reports one `rule_version` for all slice rules, so the version changes for all of them. The blindspot check `criterion.hard_unmeasured` keeps version `v2`, because it does not read this rule. Its line in the check set digest still changes, because that digest includes the gap rule version of every gap-rule check.

**Rejected alternatives.**
- *Count the recorded validity.* This would let one unattributed field stand for §6.4's verification. A confirmatory basis would then rest on the recorder's word, which `empirical.confirmatory_basis_incomplete` exists to prevent.
- *Refuse `validity: valid` at `observation.add`.* Under ADR 0007, a request that was valid in v1 must stay valid. The warning gives the same information without breaking producers that record a validity.
- *Decide "measurement" from the statement text or the selector type.* That is a guess. The typed fields are what a producer states, and a producer can add `value.kind: measurement` or a `measurement` block to say a reading is a measurement.
- *A separate version for this one rule.* Gaps and inventories carry one `rule_version`, and advice feedback checks it. A per-rule version would change that contract for one rule.

**Consequences.**
- *Compatibility (ADR 0007).* No schema, enum or error code changes. Every request that was valid stays valid. Receipts gain a warning, a refusal message changes, and the optional, untyped `error.actual` gains `effective_validity`. A receipt's warnings are part of the committed event, so a journal replayed from the same requests now has different event bytes and head hashes from a journal recorded before this change. The walkthroughs quote the new head hashes.
- *Fewer gaps.* Extracted statements leave `frontier`, `next` and the Epistemic Frontier. Drill 1 (the explanatory log case, whose 16 Observations are log-line quotes) goes from 17 frontier gaps to 1. Scenario A's slow-trace quote loses its gap.
- *Advice feedback.* A deferral lapses when the gap rule version changes (see the contract's Advice feedback section), so every deferral recorded under `slice-gaps.v1` resurfaces once. An `advice_feedback` request that names `slice-gaps.v1` is refused and must name the version that `frontier` reports.
- *Blindspot reviews.* A blindspot review re-runs the reviewed audit and requires the same check set digest. A review of an audit made under `slice-gaps.v1` cannot be recorded now; run `blindspots` again. Recorded reviews stay as they are. Scenario B records such a review, so its fixture workspace was rebuilt with `scripts/vault-goldens.py --rebuild-workspaces --only scenario-b`. Its IDs and hashes changed, and walkthrough B and `export_reports.rs` were updated to match.
- *Renderer.* The vault now names `slice-gaps.v2` and lists fewer gaps, so `RENDERER_VERSION` is `inv.vault-renderer.v29`. The real-viewer gate result recorded at v28 does not directly cover v29. The layout is unchanged.

**Tests.**
- `measurement_ops::tests`: `only_measurements_lack_validity_context`, `a_recorded_valid_measurement_warns_that_it_is_the_recorders_statement` and `a_judgment_without_a_verification_names_the_kinds_that_need_no_run`.
- `explain_ops::tests`: `a_partial_measurement_is_refused_by_name_and_can_be_cited_inconclusive`.
- CLI: `measurement_flags.rs`, `milestone_b_gaps_measurement.rs` and `example_packs.rs` expect the warning or a measurement value; `frontier.rs` and `advice_feedback.rs` expect `slice-gaps.v2`; `adversarial_drills.rs` and `trial_tasks_8_10.rs` expect drill 1's single gap.
- Goldens: the four golden vaults, `fixtures/development/brief-show-json.json`, and walkthroughs A, B and C.

Each fails without this change.
