# Milestone B proof

Milestone B asks for generic recording and honest knowledge. External producer fixtures record runs, not a named application. Provenance, validity, independence, criteria changes, synopsis freshness and structural blindspot review must be correct. No security- or performance-specific default may leak into the core (plan §19.3). This note records one run of every Milestone B check on a single executable, the plan items each check witnesses, and what is still open.

```sh
cargo build --locked -p investigation-cli
python3 scripts/prove_milestone_b.py target/debug/investigation --output milestone-b.json --keep milestone-b
```

[Proof driver](../scripts/prove_milestone_b.py) · [Milestone A proof](milestone-a.md) · [First-use study](first-use.md)

## How the proof runs

The driver exports the committed tree with `git archive HEAD` to a new directory and runs everything from that copy, so uncommitted files cannot change the result. It builds the test executables there and then installs the given executable at the path they were compiled against (`CARGO_BIN_EXE_investigation`). The Rust tests, the scenario scripts, the conformance runner and the vault renders therefore all run the same bytes. The neutrality guard runs before that install, because it builds the CLI itself. The tests and scripts run with a cleared environment: `PATH` holds only `python3`, no producer or viewer is installed, the binary carries only the built-in `general` pack, and bubblewrap removes the network. The driver writes one JSON report. Its exit status is 0 only when every step passed. A pending slot leaves the report marked `"complete": false`.

## Versions

| Item | Value |
| --- | --- |
| Commit | `760598b`: main `b897f1a` with this bone's scenario changes, help wording and proof driver |
| Executable SHA-256 | `33348422…aceaaaf` (debug build of `-p investigation-cli`, Rust 1.100.0, Cargo 1.100.0) |
| `RENDERER_VERSION` | `inv.vault-renderer.v18` |
| `FRESHNESS_RULES_VERSION` | 7 |
| Command, response | `inv.command.v1` (67 operations), `inv.response.v1` |
| Records, journal | `inv.record.v0-alpha`, `inv.event.v0-alpha.2`, index schema 4 |
| Capture, producer run | `inv.capture.v1`, `inv.producer-run.v1`, `inv.capture-capabilities.v1` |
| Method pack, gap and next rules | `inv.method-pack.v1`, `slice-gaps.v1`, `slice-next.v2` |
| Published schemas | 90 files under `schemas/`, checked against the binary by `check-v1-schemas.py` |

## Outcomes (one run, 2026-11-23, 126 s wall time, every step passed)

| Step | Result |
| --- | --- |
| Neutrality guard (`check-boundaries.py`) | Passed: crate direction, no frontend under a producer library, deny-list over help, schemas, the general pack, embedded docs and a fresh workspace. |
| Whole test suite, offline and minimal | 110 executables: 964 passed, 0 failed, 3 ignored (20 s). Doctests run in `just check`, not here. |
| Scenario A (debugging) | Passed. The structural audit finds the incompatible-build trial. A blindspot review dispositions all four gaps and records the scheduler question; it changes neither conclusion. The retrospective starts from `retrospective --draft` and is recorded with the closure. |
| Scenario B (performance) | Passed: 37 fixture properties, 220 decimals checked against the sources, a blindspot review that goes stale when a comparison is superseded, and a digest-based retrospective. |
| Scenario C (architecture choice) | Passed. A blindspot review of the accepted decision dispositions five gaps and adds a failure-model question. The 300 s → 60 s criterion change leaves both the validation review and the blindspot review stale (asserted from the digest). The retrospective starts from the digest. The case has no hypotheses, inquiries or conclusions. |
| Scenario D (source research) | Passed in all three variants. A blindspot review of the validated conclusion is followed by a digest-based retrospective and closure. The injected-output variant still equals the baseline. |
| Final audits | All five cases are closed: four with a recorded retrospective, and Scenario A's non-material case with a waiver. No audit claims completeness. |
| Conformance (W07.11) | Reference, Python and Rust producers each pass 20/20 cases; 40/40 normalized states equal the reference. `conformance.rs` also shows the non-idempotent producer failing exactly the four lost-response cases. |
| Four producer kinds (W07.17) | `producer_kinds.rs`: 3/3 passed. |
| Vault | All four case workspaces render complete with no viewer on `PATH` (64, 119, 60 and 69 notes). `vault_fixtures`, `vault_scenarios` and `vault_commands` passed. `scripts/vault-goldens.py` matches. |
| Drills 1–2 (W10.24) | `adversarial_drills.rs`: 4/4 passed (title-only cold start and stale synopsis, each with an explanatory and an evaluative variant). The compact brief is 14,574 B and 15,557 B, against a full traversal of 191,818 B and 160,330 B ([byte counts](../fixtures/README.md)). |
| Drills 3–4 | `drills_3_4.rs`: 6/6 passed. |
| Drill 5 | `drill_lesson_transfer.rs`: 1/1 passed. |
| Drill 6 | `drill_closure.rs` 3/3 and the store's `closure_drill_tests` 2/2 (every crash point of the closure batch) passed. |
| Neutrality scan of the binary | None of the 30+ named tools, the deny-listed terms or the 20 security terms appear in default help, the help of all 31 subcommands, the 67 operation schemas from the binary plus the 90 schema files, or a fresh workspace (with vault). The proof allows no exception. The only built-in pack is `general`. |
| Scripted first use | `--help`, `init --import-root .`, then `start --attach`: the third call leaves a persisted Source and Case whose blob matches the source digest. `receipt KEY` returns the same IDs, and the `next_write` template records a verified quote with its limitation. A fresh successor gets the observation from `brief`, finds the verified quote at decisive line 12 of the retained bytes, and reads the limitation, which matches the key. Total 0.27 s. |

## First-use receipt, successor recovery and wall time (W05b.17, W05b.18)

The W05b.18 changes had not been measured. This proof re-ran the study's revised arm on main `495688d` (SHA-256 `5fe02ee0…bfbabed`). The later merges in the final build change no `investigation-cli` source, so they do not touch the first-use path. It used the unchanged protocol, runner (SHA-256 `aa4e9447…`), seed 51701, the six comparison cases, both models with the same digests, and the same budgets: 12 assignments in 15 min. Results: [results.json](../training/first-use/rerun-mb/results.json) · [raw archive](../training/first-use/rerun-mb/raw.tar.zst).

| Measure | Current (`c46441d`, recorded) | Revised (recorded) | Proof build (re-run) |
| --- | ---: | ---: | ---: |
| Source and Case receipt ≤3 calls after first CLI contact | 0/12 | 6/12 | 2/12 |
| Persisted Source and Case at any time | 0/12 | 8/12 | 8/12 |
| Source-linked handoff | 0/12 | 5/12 | 4/12 |
| Successor answered | 11/12 | 9/12 | 10/12 |
| Successor decisive citation / limitation | 9 / 10 | 8 / 9 | 10 / 8 |
| Successor used the CLI | 5/12 | 4/12 | 7/12 |
| Unsupported claims | 0/12 | 0/12 | 0/12 |
| Successor phases ended by the budget | 1/12 | 2/12 | 1/12 |
| Mean investigator-plus-successor wall time | 61.1 s | 64.9 s | 73.8 s |

The re-run is a third arm on a later day, not a paired comparison, and the proof build carries many changes besides W05b.18. With one run per cell, a difference of one or two assignments is noise. Read with that caution:

- **Successor recovery improved.** Successors answered 10/12 against 9/12 for the recorded revised build, and 7/12 used the CLI instead of 4/12. Eight of 12 successors still ran some shell command on `state/`, `cache/` or blob paths. Only one (qwen) ran out of budget, against two in the recorded revised arm, where reading raw files was the cause W05b.18 targeted.
- **Receipt within three calls fell, from 6/12 to 2/12.** Persisted receipts stayed at 8/12. The median gap from first contact to receipt was 4 calls, one over the threshold. The common extra call was `start` without `--actor`: it was refused as actor-required and repeated. Some investigators also spent their first contact on a malformed call before `--help`. Gemma never ran the CLI in 2 of 6 assignments.
- **Wall time rose.** Mean total time was 73.8 s, a ratio of 1.21 against the recorded current build. Qwen investigators again kept recording until the budget ended (4/12).

## Plan items §20.1 and §20.3

Evidence is the strongest automated witness found for each item, checked against its assertions. "Partly met" names the unchecked part.

**Cross-domain invariants**

| ID | Item | Evidence | Status |
|---|---|---|---|
| X1 | Case mode `explore` supports raw observations before hypotheses, inquiry plans or options exist. | cli/scenario_d.rs walk (explore case, observations before question); cli/claim_origin.rs::show_reports_claim_origins_and_unknown_dependence | met |
| X2 | Case mode `evaluate` supports options/criteria/comparisons/decision with zero hypotheses, zero tests and zero conclusions. | scripts/scenario_c_proof.py (no hyp_/inq_/concl_; decision accepted; mode evaluate) | met |
| X3 | Nothing in default `inv` help, on-disk default workspace, schema or binary requires a named external tool, target, permission, principal or security concept. | scripts/check-boundaries.py (help, schemas, general pack, docs, fresh workspace); cli/research_packs.rs::an_unpinned_case_shows_no_example_pack_content; MB proof neutrality scan | met: the help and the fresh workspace say "Markdown vault" and name no viewer application. The proof scans 30+ named tools, the deny-list and 20 security terms over default and subcommand help, every published schema and a fresh workspace, allows no exception, and finds none. |
| X4 | Three separately collected documents quoting the same specification keep three acquisition records and one known origin for that claim, not three independent confirmations; a second claim in one of them may have a different or unknown origin. | cli/scenario_d.rs baseline (3 acquisitions, 1 origin, 0 independent pairs; bench distinct origin); cli/claim_origin.rs | met |
| X5 | A text Observation whose `text_quote` is `not_found` is rejected; `ambiguous` is reported; a verified quote is labeled as presence, not support. | cli/cli.rs::ingest_and_read_verify_original_bytes_before_accepting_an_observation (QUOTE_NOT_FOUND); core/observation_ops.rs::text_quote_not_found_is_rejected_and_ambiguous_is_preserved | met (ambiguous unit-only; presence label asserted in vault render only) |
| X6 | Two distinct acquisitions with identical bytes remain distinct provenance identities while blob bytes deduplicate. | investigation-store/tests/acquisition.rs::identical_bytes_from_distinct_acquisitions_share_one_blob | met |
| X7 | A producer's claimed result remains attributed source data, never auto-promoted to validated Conclusion. | investigation-store/tests/producer_run.rs::exit_code_zero_and_a_reported_pass_set_no_validity_and_promote_nothing; cli/cli.rs::producer_run_envelope_ingests_against_its_fixture_directory | met |
| X8 | Retraction of original source propagates effective freshness to observations, assessments, comparisons, conclusions, decisions, reports and case Overview. | cli/freshness.rs::a_real_source_withdrawal_shows_needs_review_with_a_reason_chain_at_every_layer; core/freshness/tests/source_use.rs::real_source_withdrawal_reaches_every_dependent_layer_without_erasure | met (comparison/decision layers unit-only) |
| X9 | A source excerpt containing instructions cannot alter trusted onboarding, tool schemas, execution or pack loading. | cli/scenario_d.rs::scenario_d_injected_instructions_stay_inert_and_the_result_equals_the_baseline | met |
| X10 | A synopsis is always labeled as projection/editorial material, and index matches in synopsis text are not counted as independent source corroboration. | cli/synopsis.rs::a_real_synopsis_is_editorial_in_search_and_turns_stale_after_a_source_withdrawal | met |
| X11 | Source timestamps without attestation never become trusted sequence guarantees. | investigation-recording/tests/receipts.rs::a_mismatching_subject_or_a_timestamp_never_upgrades_chronology; store producer_run.rs::declared_format_receipts_are_structure_checked_and_never_upgrade_an_envelope_run | partly met Gap: only receipt timestamps; source started_at/observed_at not shown untrusted |

**Explanatory/debugging**

| ID | Item | Evidence | Status |
|---|---|---|---|
| X12 | Hypotheses can overlap; no enforced mutually exclusive/exhaustive probabilities. | scenario_b/scenario_a overlapping hypotheses recorded | partly met Gap: nothing asserts exclusivity/probabilities are not enforced |
| X13 | A valid control under the wrong build does not qualify the target treatment measurement. | scripts/scenario_b_proof.py (CONTROL_ENVIRONMENT_MISMATCH); core/measurement_ops.rs::a_valid_control_under_the_wrong_environment_does_not_qualify_the_target | met |
| X14 | A failed or blocked external Run cannot be interpreted as negative evidence of the question without a separate valid observation. | core/absence_gate/tests.rs::a_failed_or_blocked_run_is_not_negative_evidence | partly met Gap: unit only; blocked run and CLI path not exercised |
| X15 | A frozen predicted outcome cannot be silently amended after results to claim prospective success. | core/explain_ops.rs::confirmation_freeze_pins_rivals_and_preserves_the_frozen_plan; core/retrospective_digest.rs::digest_lists_recorded_process_facts_and_compares_recorded_expectations | met (unit only) |
| X16 | A credible counterhypothesis and material objection are visible before conclusion promotion. | cli/objections.rs::critical_objections_block_validation_and_material_ones_stay_visible; cli/drills_3_4.rs::drill_4_explanatory_* | met |
| X17 | Correction of an instrument invalidates affected interpretations without deleting historical observations. | core/freshness/tests/judgments.rs::a_verification_going_stale_reaches_dependents_incrementally; scenario_b | met |
| X18 | Observations/Runs record `evidence_kind` (`observational`/`interventional`), variables changed, and whether instrumentation changed. A causal Conclusion with only observational support displays as `candidate`, never `confirmed`. Intermittent failures are recorded as k of n before and after a change. | core/blindspot/causal/tests.rs::a_causal_conclusion_with_only_observational_support_stays_candidate; scenario_b | partly met Gap: k-of-n before/after change unchecked |
| X19 | A confirmatory plan with identical predicted outcomes for two live rivals is flagged before any run. | scenario_b inquiry.non_discriminating_plan; core/explain_ops.rs freeze warning | met |
| X20 | An executor-bound reanalysis of an already-inspected trace keeps its strong chronology *and* its known-outcome reuse; a renamed derivative counts as the same data; a separately held-out acquisition with a frozen plan stays distinguishable. | cli/evidence_reuse.rs::show_reports_reuse_deviations_and_chronology_as_separate_axes; core/evidence_reuse/tests.rs::executor_bound_reanalysis_of_an_inspected_trace_keeps_chronology_and_reuse | met (executor_bound/held-out unit-only) |
| X21 | A truly inconclusive case can close with a precise reopen condition. | scenario_c / scenario_b inconclusive close with reopen_condition | partly met Gap: stored reopen_condition never read back; vague one not rejected |

**Performance/measurement**

| ID | Item | Evidence | Status |
|---|---|---|---|
| X22 | Measurement includes units, workload identity, observation window, sample count or explicit unknown, environment/tool identity and limitations. | core/measurement_fields.rs::observation_and_run_fields_round_trip_through_their_operations; scenario_b | met |
| X23 | Raw samples carry their repetition level (build, process, iteration) and order; a flat array is accepted but flagged as unable to support a variance analysis. | core/measurement_flags/tests.rs::flat_samples_averaged_quantiles_and_changed_instrumentation_are_flagged; scenario_b | met |
| X24 | Trial order is recorded (`interleaved`, `randomized`, `sequential`); a comparison built on A-then-B sequential runs is flagged. | scenario_b sequential_trial_order; cli/measurement_flags.rs::flags_are_advisory_on_receipts_and_shown_in_every_read | met |
| X25 | Latency measurements record the load model (open/closed loop) and whether time is measured from intended send time; a tail claim from a closed-loop generator without that is flagged for coordinated omission. | cli/measurement_flags.rs; core/measurement_flags/tests.rs::scenario_b_sequential_and_closed_loop_seeds_are_flagged_without_blocking | met |
| X26 | A percentile claim carries its sample count. The core flags a p99 claim with n < 299 (or p99.9 with n < 2,995) as unable to support a distribution-free 95% order-statistic bound. This is arithmetic on a recorded count, advisory, and not a universal sample floor. | core/measurement_flags/tests.rs::a_quantile_needs_a_known_count_at_the_order_statistic_bound; cli/measurement_flags.rs (n=120) | met |
| X27 | Percentiles from separate runs are never averaged; combining needs raw samples or mergeable histograms. | core/measurement_flags/tests.rs (averaged_run_values flag) | partly met Gap: pooled samples/merged histograms passing without flag untested |
| X28 | Started, observed, timed-out, excluded and unknown counts are preserved with their definitions; an all-attempt criterion cannot be satisfied from a completion-only percentile. | scenario_b denominators + refused accept; core/measurement_flags/tests.rs::missing_outcome_variant_flags_meets_with_the_denominators | met |
| X29 | Warmup, trial order, varying instrumentation or changed request mix are visible as possible confounders. | scenario_b warmup/instrumentation/order/mix; core/measurement_flags/tests.rs::compared_measurements_show_differing_conditions_as_possible_confounders | met |
| X30 | Two reported percentiles with different load distributions are not silently treated as comparable. | core/measurement_flags/tests.rs; scenario_b | partly met Gap: measurement.compared_load_model_differs never exercised |
| X31 | Zero detected difference under broken instrumentation is inconclusive, not evidence of performance equality. | scenario_b gated absence; core/absence_gate/tests.rs::zero_difference_under_broken_instrumentation_is_inconclusive_not_equality | met |
| X32 | Repeating a benchmark creates a distinct Run with an explicit acquisition identity and shared setup dependencies. | core/measurement_fields.rs::a_repeat_is_a_distinct_run_with_its_own_acquisition_and_shared_setup | met (unit only) |
| X33 | No p-value, confidence interval, causal claim or numeric accuracy score is invented by the core. | scenario_b no_invented_numbers; cli/confidence.rs::promotion_requires_named_reasons_and_summary_shows_concerns_without_numbers | met |

**Evaluative/architecture**

| ID | Item | Evidence | Status |
|---|---|---|---|
| Y1 | A `hard` Criterion with an unknown result cannot be treated as satisfied, nor traded against arbitrary soft-score weights. | scripts/scenario_c_proof.py (weighted excluded hard_unresolved); core/weighting/tests.rs::a_hard_failure_or_unknown_is_excluded_and_never_traded_against_soft_weights | met |
| Y2 | Criteria and choice alternatives are separate from empirical hypotheses. | scripts/scenario_c_proof.py (no hyp_/inq_/concl_ created) | met |
| Y3 | Missing cells in option × criterion table remain `unknown` with useful next-step guidance. | cli/next_rank.rs::next_groups_repeated_cells_and_every_query_path_stays_on_its_snapshot; scenario_c hard_unmeasured suggested comparison.add | met |
| Y4 | Changing criterion threshold/weight/units forces affected Comparisons and Decisions into `needs_review` (unless demonstrably irrelevant per dependency map). | scenario_c threshold change -> needs_review; core/freshness.rs::criterion_threshold_weight_and_unit_changes_require_review (units/weight/irrelevance unit-level) | met |
| Y5 | A source-authored subjective preference stays labeled preference rather than measured fact. | core/weighting/tests.rs::preferences_stay_labelled_preferences_and_never_score; render/comparison_matrix.rs::every_cell_is_stated_and_a_missing_cell_is_labelled | met |
| Y6 | A decision may be rational, conditional and accepted while one comparison remains uncertain; report explicitly preserves that condition. | scenario_c accepted conditional decision + scenario_c_successor.py | met |
| Y7 | An accepted Decision is superseded by a new Decision, not overwritten historically. | scenario_c decision.supersede; cli/vault_scenarios.rs::scenario_c_marks_its_stale_review_on_the_review_and_on_the_decision | met |
| Y8 | A sensitivity result documents which uncertain assumption could plausibly reverse an option choice, without claiming a computed probability. | vault_scenarios reversal conditions; core/weighting/tests.rs::sensitivity_reports_whether_an_order_holds_across_stated_weight_uncertainty | partly met Gap: nothing asserts no probability claimed |
| Y9 | Pareto status never prunes an option on the strength of an `unknown`, `stale` or unranked-subjective cell; `next` does not suggest soft-criterion measurement for a hard-disqualified option. | core/decision_ops.rs::generated_pareto_proofs_require_current_ranked_cells; core/next_rank.rs::soft_cells_on_a_hard_disqualified_option_are_withheld_not_suggested (unit level) | met |
| Y10 | A weight without a stated range never enters a score; adding a duplicate option does not reorder the others. | core/weighting/tests.rs::a_weight_without_a_range_never_enters_a_score, ::utilities_use_the_stated_range_so_added_or_duplicate_options_never_reorder_the_others; scenario_c enters_score | met |

**Source research**

| ID | Item | Evidence | Status |
|---|---|---|---|
| Y11 | Quotations, extraction, screenshots and summary notes of one document do not count as independent corroboration. | cli/scenario_d.rs baseline; core/origin_ops/tests.rs::views_and_notes_of_one_document_do_not_count_as_independent | met |
| Y12 | Primary-source erratum/source currency can invalidate a prior judgment without deleting its original record. | cli/scenario_d.rs baseline (erratum -> needs_review, rev 1 kept) | met |
| Y13 | An unavailable external artifact produces `source_unavailable`, not fabricated verified bytes. | cli/reference_only.rs::a_reference_only_source_reports_source_unavailable_everywhere_it_is_read | met |
| Y14 | A source-based conclusion can be reviewed without requiring a physically executable experiment. | cli/scenario_d.rs (no inq_, only search runs) | met |

**Layered retrieval / frontier / blindspots / retrospectives**

| ID | Item | Evidence | Status |
|---|---|---|---|
| Y15 | Every supported record type yields stable `title`, `summary` and `full` views; evidence is separately opt-in. Full does not implicitly load a secret-bearing blob. | cli/cli.rs::layered_read_and_brief_use_verified_scope_and_exact_bytes; cli/synopsis.rs | partly met Gap: only case/conclusion depths checked; every record type, evidence opt-in, secret blob not checked |
| Y16 | A 40-record fixture produces a significantly smaller useful compact brief than reading all full records; measure emitted bytes and task correctness, not just compression ratio. | cli/adversarial_drills.rs::drill_1_title_only_cold_start_explanatory, ::drill_1_title_only_cold_start_evaluative; cli/brief.rs::a_compact_brief_of_a_forty_record_case_halves_a_full_traversal_and_keeps_every_caveat | met: the compact brief is at most a fifth of the full traversal (14,574 vs 191,818 B; 15,557 vs 160,330 B), and the cold path finds the open gap and its question |
| Y17 | Every compact current Conclusion or Decision exposes material caveats and effective freshness independently of optional authored synopsis text. | cli/adversarial_drills.rs::drill_1_* (every current Conclusion and Decision, with and without synopses); cli/brief.rs | met |
| Y18 | A source retraction/criterion revision marks affected synopsis bound to the old subject revision `stale`; default brief never represents the old synopsis as current. | cli/adversarial_drills.rs::drill_2_stale_synopsis_after_a_source_withdrawal_explanatory, ::drill_2_stale_synopsis_after_a_threshold_change_evaluative; cli/synopsis.rs | met: both the source-withdrawal and the criterion-revision paths |
| Y19 | An `annotation.add(role=synopsis)` is attributed, source/subject-revision-bound, separately editable by new annotation and never counted as corroboration. | cli/synopsis.rs::a_real_synopsis_is_editorial_in_search_and_turns_stale_after_a_source_withdrawal | met |
| Y20 | `inv frontier` returns the same snapshot-bound missing/blocked/stale items as the complete unranked inventory behind `inv next`, with stable rule IDs, deduplicated subjects and honest prerequisites. | cli/frontier.rs::frontier_is_the_complete_unranked_inventory_behind_next; ::explain_names_the_rule_and_version_for_any_gap_id | met |
| Y21 | `inv blindspots` discovers a missing control, a shared-origin source pair, an untested material assumption, an unknown hard criterion and an invalidated source/review from generic typed fields—without domain-specific executors. | scenario_d blindspots_reported (shared origin, invalidated source); scenario_c (assumption, hard); scenario_a (control on another build); core/blindspot/causal/tests.rs | met |
| Y22 | An audited case with no detected issue displays "no structural gap identified under checks X", never "all blindspots resolved" or "verified correct". | cli/blindspots.rs::audits_are_bounded_deterministic_pinned_and_schema_valid; core/blindspot/tests.rs::a_clean_case_reports_no_gap_under_the_named_checks_and_no_completeness | met |
| Y23 | `review.submit(kind=blindspot)` can record zero/one/many specific gaps, scope, inspected snapshot, uncertainty and exact related IDs; it never modifies a Conclusion or Decision automatically. | cli/drills_3_4.rs::drill_3_*; scenario A/C/D blindspot reviews (decision/conclusion unchanged) | met |
| Y24 | When sources/evidence change, previously recorded challenge reviews become stale where relevant; unconnected case mutations do not needlessly invalidate them. | core/freshness/tests/blindspot_review.rs::withdrawing_a_source_the_review_read_makes_it_stale_and_an_unrelated_write_does_not; scenario_c stale blindspot review | met |
| Y25 | A semantic blindspot outside the finite rules can be added as a Question/Assumption/Objection; no rule engine claims it should already have found it. | cli/drills_3_4.rs::drill_4_* ; scenario A/C/D novel gaps as Questions | met |
| Y26 | `retrospective.record` accepts useful lessons from failed, inconclusive or successful inquiries with transfer limits; allows explicit "no observed failures" without fabricated claims. | cli/drill_lesson_transfer.rs; core/retrospective_digest.rs::lessons_from_failed_inconclusive_or_successful_work_and_explicit_none_observed | met |
| Y27 | Closing a material case without a Retrospective fails with an actionable repair, while a non-material case may close with `retrospective_waived` and a reason; retrospective + close in one idempotent transaction works and preserves CAS semantics; recording-only cases remain exempt. | cli/drill_closure.rs::a_material_close_is_refused_with_a_repair_that_leads_to_one_idempotent_batch, ::a_nonmaterial_case_waives_and_a_record_case_holding_data_needs_neither; investigation-store/src/closure_drill_tests.rs | met |
| Y28 | Lessons do not silently modify method packs, prompts, agent settings, or canonical policy; restricted lessons cannot leak into public review exports. | cli/drill_lesson_transfer.rs; cli/lessons.rs::lessons_cross_cases_only_by_admission_and_explicit_consultation | partly met Gap: restricted-lesson export leak unchecked (W11.16 not built) |

**20.3 Neutral producer and method-pack fixtures**

| ID | Item | Evidence | Status |
|---|---|---|---|
| N1 | The exact same `producer-run.v1` schema records a synthetic benchmark, file-analysis report, simulation, and trace-capture session without core code changes. | producer_kinds.rs::four_producer_kinds_record_through_one_envelope_and_differ_only_in_data (L96); ::the_crates_hold_no_branch_on_a_fixture_producer_or_its_assertion_kinds (L377) | met |
| N2 | A standalone producer can record one Run in `record` mode with no hypotheses, Obsidian or model installed. | examples/rust-producer/tests/adapter.rs::run_finalizes_a_record_mode_run_offline (L104); investigation-recording/tests/begin.rs::begin_declares_one_run_and_returns_its_handle_protocol_limits_and_staging (L49) | met ("no Obsidian/model" rests on offline env) |
| N3 | Rust and Python/JSON producer conformance yield equivalent canonical envelopes and stable source mappings. | conformance.rs::reference_and_both_examples_pass_and_record_equal_state (L127); cases baseline/partial-evidence -> exact-acquisition-mapping, equivalent-hashes-ids | met (40 comparisons equal) |
| N4 | Begin/append/attach/finalize accept idempotent retries and recover from lost acknowledgement without duplicate runs or original acquisitions. | conformance lost-response-{begin,append,attach,finalize}, rerun-identical; conformance.rs::non_idempotent_producer_fails_the_suite (L155); recover.rs L266 | met |
| N5 | An interrupted producer returns partial/incomplete artifacts and `unknown` external effect status, never schedules rerun on recovery. | investigation-recording/tests/recover.rs::an_interrupted_producer_recovers_partial_outputs_with_unknown_execution_and_no_rerun (L179); conformance interrupted-run | met |
| N6 | Capture finalization after process failure preserves exact original error and valid partial observations; no invented overall verdict. | investigation-recording/tests/finalize.rs::a_failed_process_is_finalized_with_its_exact_error_and_what_survived (L341) | met |
| N7 | Historical or external manifest reimport is idempotent; manifest revision isn't a new trial. | investigation-store/tests/producer_run.rs::identical_content_is_one_attempt_and_a_changed_envelope_revises_it (L242); investigation-recording/tests/attempt_identity.rs::a_changed_envelope_keeps_a_terminal_status (L226) | met |
| N8 | Send position 3 when 2 is missing, retry an accepted item with changed bytes, race the last attachment with sealing, and interrupt between seal and canonical publication. Every acknowledged item survives at its position; after staging retirement an identical retry recovers its receipt and a late item cannot alter the manifest. | investigation-recording/tests/late.rs::after_staging_is_retired_an_identical_retry_recovers_its_receipt_and_a_late_item_changes_nothing (L248); late.rs::racing_the_last_attachment_with_sealing_includes_it_or_refuses_it_never_loses_it (L175); append.rs L151,L173; recover.rs L314 (skips silently if permissions not enforced, e.g. root) | met |
| N9 | Unknown source schema or namespaced metadata stays opaque with usable diagnostic; unsupported source data never silently maps to false facts. | investigation-store/tests/ingest.rs::unknown_envelope_version_is_retained_opaque_with_a_diagnostic (L147); conformance partial-evidence -> namespaced-metadata | met |
| N10 | Core compilation/run tests pass with zero installed producers, executors, task trackers, or environment credentials. | .github/workflows/check.yml offline job L45-64 (unshare --net, env -i, minimal PATH) ; local just check does not enforce. (MB proof suite step also enforces) | met: the proof runs all 106 test executables with a cleared environment, PATH holding only python3 and no network; CI has an equivalent offline job |
| N11 | A new declarative method pack can add a guidance card and custom optional fields without modifying core code. | pack_extensions.rs::a_third_party_pack_adds_a_card_and_typed_fields_that_survive_its_removal (L161); research_packs.rs::all_three_packs_load_with_the_unchanged_core_and_none_is_a_default (L189) | met (no source-scan guard for "no core change") |
| N12 | A method pack can *advise* a blindspot review or require one at a declared consequential gate only through versioned schema-bound policy; no pack runs a model, scanner or command to conduct that review. | pack_policy.rs::a_pinned_pack_gates_decision_acceptance_and_every_gate_records_its_policy (L297); investigation-core/tests/method_pack_safety.rs::pack_loading_resolving_policy_and_rendering_sources_cannot_spawn_or_reach_the_network (L456) | met (advise only via guidance card) |
| N13 | Method pack cannot create executable hooks, replace core provenance rules, grant execution permission or alter historical case policy by updating on disk. | cli method_pack_safety.rs::hostile_packs_are_refused_at_adoption_and_change_nothing (L330); ::updating_a_pack_file_on_disk_never_changes_an_adopted_pack_or_its_policy (L473) | met |
| N14 | An uninstalled/unknown method pack does not prevent valid generic recording, inspection or export. | cli method_pack_safety.rs::an_unknown_or_missing_pack_never_blocks_generic_recording_inspection_or_guidance (L521); investigation-render/tests/method_packs.rs::a_case_with_an_unavailable_pinned_pack_still_renders (L14) | met (export only as vault render) |


## Build-rule invariants

| Invariant | Witness on this build |
| --- | --- |
| Evidence is not a verdict | Every evidence read is labelled `untrusted_evidence` (first-use successor, Scenario C, Milestone A walk). A producer's reported pass promotes nothing (`producer_run.rs::exit_code_zero_and_a_reported_pass_set_no_validity_and_promote_nothing`). Blindspot reviews in A, C and D leave the reviewed conclusion or decision byte-identical. |
| An invalid experiment is not a negative result | Scenario A keeps the unmapped-clock run invalid and refuses the incompatible-build candidate. Scenario B gates a zero difference under broken instrumentation as inconclusive. `absence_gate` tests cover failed runs. A blocked run has a unit test only (X14). |
| A late revision invalidates stale reviews | Scenario C: the criterion change leaves the validation review and the blindspot review stale, and the digest lists both. In Scenario D, the erratum moves the earlier judgment to `needs_review`. `freshness.rs` traces a withdrawal through every layer. |
| Repeated formatting of one source is not independent confirmation | Scenario D: three acquisitions, one known origin, zero independent pairs, and the audit reports the shared origin. |
| Losing a response must not duplicate a commit | Conformance lost-response cases for all three producers. `receipt KEY` in the first-use path returns the original IDs. The closure drill's crash model retries to the same IDs. |
| Missing bytes remain missing | `reference_only.rs` reports `source_unavailable` everywhere. Interrupted and failed producers keep their partial outputs with missing positions named (`recover.rs`, `finalize.rs`, conformance `missing-attachments`). |

## Known limitations and findings

1. **Resolved: Obsidian named in default surfaces.** The `init` help line, the `--no-vault` help and the comment in a fresh `investigation.toml` used to name Obsidian. They now say "an optional Markdown vault" and "do not open or sync it as a notes vault" (lead commit `ff05dc1`, CLI goldens regenerated). The proof's neutrality scan now allows no named tool, and it passes.
2. **Resolved by W10.27 (`b897f1a`, bn-1nk1): resolving a question with its validated answer marked that answer `needs_review`.** Under freshness rules version 7, a question's resolution evidence is no longer a material revision for the conclusion that the resolution cites. The Scenario D conclusion and its review now read `validated / current`, the audit no longer reports `promoted.basis_not_current`, and Scenario D now fails if it does. By design, a hypothesis under the resolved question that the answer does not rest on still goes `needs_review`; bn-3lhs tracks whether to keep that. Renderer v18 also fills the Scenario D Overview Sources section.
3. **Audit references cannot be copied into a review as they are (bn-dem8).** A finding such as `promoted.basis_not_current` gives the revision the basis read, and one finding can list two revisions of a record. Copying its `related_refs` into `gap_dispositions` is then refused with `REVISION_CONFLICT` or "Supply one revision per record". Scenario D maps every related reference to its current revision, as the repair asks.
4. A second review from the same undeclared actor family raises the advisory `review.promoted_single_dependence_group`. Scenario A asserts this.
5. The golden vault workspaces (`fixtures/vault-goldens/`) predate the new W10 steps. Rebuilding them would change every ID. The proof renders the four new workspaces live instead.
6. The items marked "partly met" in the table remain open. The restricted-lesson export check waits for the review export (W11.16).
7. **Most §10.5 latency targets are missed.** §10.5 calls them engineering targets, so they do not block this milestone. The top-level goal bn-9muo (PERF.01–05) tracks them. The W05b.15 baseline ([2026-11-23](../notes/bench/2026-11-23-baseline.json), run with `just bench`) set targets of 250 ms p95 for warm reads and 150 ms for one-operation mutations, and missed 8 of them:

   | Fixture | Measurement | p95 | Target |
   | --- | --- | ---: | ---: |
   | Sparse choice (1,000 × 200) | `brief` | 2,318 ms | 250 ms |
   | Sparse choice | `next` (top three) | 2,070 ms | 250 ms |
   | Sparse choice | `frontier` first page | 708 ms | 250 ms |
   | Sparse choice | group drill-down first page | 736 ms | 250 ms |
   | Large | `brief` | 17,617 ms | 250 ms |
   | Large | `next` (top three) | 2,021 ms | 250 ms |
   | Large | `frontier` first page | 1,296 ms | 250 ms |
   | Large | one-operation `record` | 413 ms | 150 ms |

   W08.19, which merged after the baseline, reduced the large brief to about 2.1 s and the large vault render from 205 s to about 4 s. The vault render has no target. Both are still above the read target, and the baseline has not been re-measured since.
8. **First-use time target missed.** See above and the dispositions below.
9. **`start` without `--actor` costs a refused call (bn-6vlm).** In the first-use re-run, this was the most common extra call before a receipt. The refusal gives the exact retry, but the call still counts against the three-call measure.
10. **The Overview lists one source at two revisions (bn-bn1m).** Since renderer v18, the Scenario D Overview Sources section lists some sources twice: at the revision read and at an older one.

## Reviewer dispositions

These are the dispositions of the lead (investigation-dev), acting as tech lead for this milestone. The project owner can override them.

- **W05b.17 time target (missed):** revised 64.9 s vs current 61.1 s mean investigator+successor (ratio 1.06; 5/12 pairs faster); successor answer rate 9/12 vs 11/12.
  - Disposition: accepted as a non-blocking miss for Milestone B. The primary first-use gains were achieved (receipt within 3 calls 6/12 vs 0/12; source-linked handoff 5/12 vs 0/12) with no unsupported claims in either arm. Wall time is dominated by investigators recording until their budget ends, not by CLI latency. The successor regression is attributed to successors reading raw `state/` files; W05b.18 addressed that (workspace README pointer, toml header).
  - Condition: the W05b.18 changes must be measured. If this proof re-ran the study, cite its numbers here. If not, a re-run is required before Milestone C sign-off.
  - **Re-run numbers (this proof):** the revised arm was re-run on the proof build with the same protocol, cases, seed, models and budgets. Successor answer rate 10/12 (recorded revised 9/12, current 11/12). Successor used the CLI 7/12 (recorded 4/12). Successor budget exhaustion 1/12 (recorded 2/12). Source-linked handoff 4/12 (recorded 5/12). Receipt within 3 calls of first contact 2/12 (recorded 6/12; persisted receipts 8/12 in both). Unsupported claims 0/12. Mean investigator-plus-successor wall time 73.8 s (recorded 64.9 s revised, 61.1 s current). The re-run is unpaired, one run per cell. It suggests the successor fix helped. The receipt-within-three and time measures did not improve; the most common extra call was a `start` refused for a missing `--actor`. [Re-run results](../training/first-use/rerun-mb/results.json)
  - Lead re-assessment: the re-run does not confirm the receipt-within-three gain (2/12 vs 6/12 recorded) and wall time rose (73.8 s); the miss remains non-blocking for Milestone B because persisted receipts (8/12), source-linked handoff (4/12) and zero unsupported claims held and the main extra call is an actionable `--actor` refusal (bn-6vlm), but this disposition now rests on weaker evidence than when it was written and is explicitly flagged for the project owner.
- **record-mode closure exemption:** today a `record` case is exempt from the retrospective requirement only while it is non-material; a record case with a failed or invalid Run requires one. Plan §17.7 ("record mode remains exempt") and §10 ("record-mode data-only sessions are exempt") read differently.
  - Disposition: keep the current, stricter behaviour for Milestone B (it fails safe), and flag it for the project owner's decision.
- **artifact.withdraw guardrail (bn-6fpb):** blocked by the v1 schema freeze (adding a value to the frozen GuardedOperation enum). Moved out of the milestone pending the owner's schema-evolution decision. Mitigation: an unguarded artifact withdrawal is visible, since dependents go needs_review.

Both findings this proof raised for disposition are resolved: Known limitations 1 and 2.

Still awaiting the project owner (the lead's dispositions above apply until then): the W05b.17 first-use miss, the record-mode closure exemption, and how a frozen v1 enum may widen for the artifact.withdraw guardrail (bn-6fpb).
