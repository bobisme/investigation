# investigation 0.1.0: release notes (DRAFT)

**This is a draft written at main `9e1aec0e` (2026-11-24). It is not a release and approves nothing.** The definition-of-done audit (REL.01a, [gap audit](2026-11-24-gap-audit.md)) recorded the verdict **not approved** on a binary built from `8093f95f`. Milestone C (bn-3q2) is open, so this build is not yet the release plan §19.3 approves for consequential investigations. REL.03 (bn-3kac) stays open: its final release notes add the built artifacts, their checksums and the audited commit.

How to read this draft:

- Each item names its evidence: a test (`crate/test_file.rs::test_name`), a `just` target, a script, or a committed report. Bracketed IDs such as [P2] are rows of the gap audit ([table](2026-11-24-gap-audit.md), [JSON](2026-11-24-gap-audit.json)), and they keep the status that audit recorded.
- "Demonstrated" means the named check passed on the audited binary or on a later main. It does not mean the guarantee holds outside what that check covers.
- Where an audit row is open or limited and a later merge changed its evidence, the item says so. The audit has not been re-run since.

## Since the audit

These merged after the audited binary (`8093f95f`):

- `76f3ba3b` (bn-f97f) adds `investigation archive restore --resume` and `investigation archive fork --resume`, to finish an interrupted restore. `restore_kill.rs` SIGKILLs a real restore process at every step. `repair journal` accepts two more witnesses, a fork's inherited head and `--archive DIR`. The v0-alpha layout is pinned by a historical fixture (`fixtures/historical/v0-alpha-2026-11-22`).
- `5f29e583` (bn-3dy5): the document checks read the documents embedded when the test binary was built. The flake behind [G1] and [G11] was a newer document paired with an older binary.
- `f34cab6e` (bn-1wre, finding F1): a stale live outcome now ranks right behind the leading one, so a tight brief budget cuts current outcomes first. Drill 2 passes at every workspace path length from 20 to 141 bytes.
- `9e1aec0e` (bn-1znz, finding F2): the Milestone B proof is green again ([report](2026-11-24-milestone-b-proof.json), run on `47770e26`): 16/16 steps, and 1,477 of 1,477 tests in a cleared environment without network. Its neutrality scan is now `just neutrality`, part of `just check`.

## Demonstrated

### One core for four kinds of case

- **Debugging, performance, source research and architecture choice run on one store and CLI**, with no hypotheses forced on a choice. Evidence: scenarios A-D in the [Milestone B proof](2026-11-24-milestone-b-proof.json); `scripts/scenario_c_proof.py` (an evaluate case with no hypotheses, inquiries or conclusions). Audit rows [B8], [B17], [X1], [X2].
- **Evidence is not a verdict, and an invalid experiment is not a negative result.** A producer's claimed pass promotes nothing. A failed or blocked Run cannot support absence. Zero difference under a broken instrument is inconclusive. Evidence:
  - `investigation-store/tests/producer_run.rs::exit_code_zero_and_a_reported_pass_set_no_validity_and_promote_nothing`;
  - `milestone_b_gaps_explanatory.rs::x14_a_failed_or_blocked_run_is_not_negative_evidence_through_the_cli`;
  - `core/absence_gate/tests.rs::zero_difference_under_broken_instrumentation_is_inconclusive_not_equality`.

  Rows [I1], [I2], [X7], [X14], [X31].

### Provenance and freshness

- **Three acquisitions of one specification stay three acquisitions with one origin, not three independent confirmations.** Identical bytes from distinct acquisitions share one blob. Evidence: `cli/scenario_d.rs`; `acquisition.rs::identical_bytes_from_distinct_acquisitions_share_one_blob`. Rows [X4], [X6], [Y11], [I4].
- **A text quote is verified against the retained bytes.** A quote that is `not_found` is rejected, and a verified quote is labelled as presence, not support. Evidence: `observation_ops.rs::text_quote_not_found_is_rejected_and_ambiguous_is_preserved`. Row [X5]; the presence label is asserted in the vault render only.
- **A source withdrawal, an erratum or a criterion change reaches every dependent layer as `needs_review`, with a reason chain, and erases nothing.** Evidence:
  - `cli/freshness.rs::a_real_source_withdrawal_shows_needs_review_with_a_reason_chain_at_every_layer`;
  - `core/freshness.rs::criterion_threshold_weight_and_unit_changes_require_review`;
  - scenarios C and D.

  Rows [X8], [X17], [Y4], [Y12], [Y24], [I3]. For [X8], the comparison and decision layers are covered at unit level only.

### Measurement validity

- **The core flags measurement claims it cannot support, without inventing numbers.**
  - Percentile claims without the sample count they need are flagged.
  - Percentiles from separate runs are never averaged.
  - Denominators and exclusions are kept.
  - Warmup, trial order, instrumentation and mix show as confounders.
  - A control under the wrong build does not qualify a treatment.
  - No p-value, interval or accuracy score is invented.

  Evidence: `scripts/scenario_b_proof.py` (37 fixture properties, 220 decimals checked against the sources); `core/measurement_flags/tests.rs`. Rows [X13], [X22]-[X29], [X31]-[X33]. [X32] is witnessed at unit level only.

### Evaluation

- **A hard criterion with an unknown or failing result is never satisfied or traded away.**
  - A weight without a stated range never enters a score.
  - Pareto pruning needs current, ranked cells.
  - Preferences stay labelled as preferences.
  - A changed threshold sends dependent comparisons and decisions to `needs_review`.
  - An accepted decision is superseded, never overwritten.

  Evidence: `scripts/scenario_c_proof.py`; `core/weighting/tests.rs`; `core/decision_ops.rs::generated_pareto_proofs_require_current_ranked_cells`. Rows [Y1]-[Y10].

### Reading a case

- **Every record type has stable `title`, `summary` and `full` views, and evidence bytes stay opt-in.** Evidence: `milestone_b_gaps_layered.rs::every_record_type_has_stable_layered_views_and_evidence_stays_opt_in`. Row [Y15].
- **A compact brief of a 40-record case is much smaller than reading every record in full, and it still shows each current conclusion's and decision's caveats and freshness.** Evidence: `adversarial_drills.rs::drill_1_title_only_cold_start_explanatory` and `::drill_1_title_only_cold_start_evaluative`; `just acceptance` (context efficiency). Rows [Y16], [Y17]. Whether a smaller brief yields equally good answers is a trial claim; see [C9] under Limited.
- **A synopsis is editorial, attributed and revision-bound, and it turns stale when its source changes.** Evidence: `cli/synopsis.rs::a_real_synopsis_is_editorial_in_search_and_turns_stale_after_a_source_withdrawal`. Rows [Y19], [X10]. The audit recorded [Y18] as limited because of F1. Both drill 2 variants pass at main since `f34cab6e`.

### Gaps, challenge reviews and retrospectives

- **`investigation frontier` is the complete, unranked gap inventory behind `investigation next`.** `investigation blindspots` finds seeded structural gaps and never claims completeness. Evidence: `frontier.rs::frontier_is_the_complete_unranked_inventory_behind_next`; `blindspots.rs::audits_are_bounded_deterministic_pinned_and_schema_valid`; `just acceptance` (blindspot calibration: 26 of 26 seeded gaps detected, 0 completeness claims, [report](2026-11-24-gap-audit/acceptance-report.json)). Rows [Y20]-[Y25], [C10], [B6].
- **A material case cannot close without a Retrospective; the refusal carries its repair.** Lessons never change method packs, prompts or policy on their own. Restricted lessons are not revealed across cases or in public exports. Evidence: `drill_closure.rs::a_material_close_is_refused_with_a_repair_that_leads_to_one_idempotent_batch`; `drill_lesson_transfer.rs`; `lessons.rs`. Rows [Y26]-[Y28].

### Recording external work

- **One neutral `producer-run.v1` envelope records a synthetic benchmark, a file-analysis report, a simulation and a test run.** Begin, append, attach and finalize retry idempotently. An interrupted or failed producer keeps its partial outputs, its exact error and an `unknown` effect status. Evidence: `producer_kinds.rs::four_producer_kinds_record_through_one_envelope_and_differ_only_in_data`; `investigation-recording/tests/recover.rs`; `finalize.rs`; `late.rs`. Rows [N1]-[N9].
- **Conformance: the reference, Python and Rust producers each pass 20 of 20 cases, and 40 of 40 normalized states equal the reference.** A non-idempotent producer fails the suite. Evidence: `scripts/run-conformance.py`; `conformance.rs`; the [Milestone B proof](2026-11-24-milestone-b-proof.json). Rows [N3], [N4], [G7]. The generic producers are the project's own examples.
- **Method packs are declarative.** A third-party pack can add a card and typed fields. Hostile packs are refused at adoption. A missing pack never blocks recording, inspection or export. Evidence: `pack_extensions.rs`; `method_pack_safety.rs::hostile_packs_are_refused_at_adoption_and_change_nothing`; `pack_policy.rs`. Rows [N11]-[N14], [B14].

### Write safety and integrity

- **A write retried after a lost response is recorded exactly once.** The same command ID with a changed payload is refused, also after an index rebuild. Competing updates get a clear revision conflict. A malformed operation commits none of its batch. Evidence:
  - `acceptance_measures.rs::every_write_command_retried_after_a_lost_response_is_recorded_exactly_once`;
  - `production.rs::competing_processes_return_the_winning_revision_and_bounded_diff`;
  - `just acceptance` (lost and duplicate writes, 33 tests).

  Rows [P2]-[P4], [P7], [P10], [C6], [I5], [B16]. For [P3], the after-rebuild case is shown for `start` only.
- **Journal damage is diagnosed in place, not repaired silently.** Missing or corrupt events, duplicate sequences, hash mismatches and non-canonical bytes are all reported. `investigation repair journal` restores bytes only where a witness proves them (ADR 0016). Rebuilding the index keeps state and receipts. Evidence: `production.rs::replay_reports_duplicate_sequences_and_unknown_versions_in_place`; `canonical_journal.rs`; `journal_repair.rs`; `cli.rs::check_and_index_repair_preserve_receipts`. Rows [P12], [P13], [B10]. [P13] is checked piece by piece, with no single whole-state comparison.
- **Missing bytes stay missing.** A changed external file fails re-verification. Ingest streams large files in bounded memory and keeps their exact bytes. Paths cannot leave the import root. Evidence:
  - `reference_only.rs::a_reference_only_source_reports_source_unavailable_everywhere_it_is_read`;
  - `read_path_hash_tests.rs::an_overwrite_in_place_is_caught_on_the_next_read`;
  - `streaming_ingest.rs::a_file_past_the_old_limit_streams_in_bounded_memory_and_keeps_exact_bytes` (72 MiB with a 2 MiB peak heap; multi-GiB runs were not run);
  - `raw_file_hardening.rs`.

  Rows [P14]-[P17], [I6], [Y13].
- **A full workspace archive restored in a clean root has the same state, the same evidence digests and the same vault.** A restore killed at any step finishes with `--resume` from its own marked staging. Evidence: `clean_restore.rs::a_full_archive_restored_in_a_clean_root_has_the_same_state_digests_and_vault`; `archive_restore.rs`; `restore_kill.rs`, which merged after the audited binary. Row [P18]: Linux only.

### Casebook and exports

- **The vault is plain Markdown with native Bases and Canvas.**
  - It needs no community plugin or viewer installed to render.
  - Identical snapshots give identical bytes.
  - Title changes keep file names and links.
  - Human folders are left byte-identical.
  - Edits to generated files are reported as drift.
  - A failed render says it is partial while canonical reads continue.

  Evidence: `vault_fixtures.rs`; `vault_scenarios.rs`; `render/drift.rs`; `generations.rs::a_failed_render_advertises_partial_while_canonical_reads_continue`. Rows [V2], [V3], [V5]-[V9]. These are file-level checks; see Limited for the viewer.
- **Review exports leave out the raw journal and secrets, across every kind, audience and identifier mode.** A conclusion whose decisive objection is withheld is never shown clean. Evidence: `export_leaks.rs::scenario_a_leaks_nothing_through_any_kind_audience_or_identifier_mode` (and scenarios B-D and hostile text); `audience_policy.rs::a_conclusion_whose_decisive_objection_is_hidden_is_never_shown_clean`. Row [V12]. [V13] is open; see Limited.

### Neutral, offline core

- **The shipped crates import no named producer and start no process or socket.** No named tool or security concept is required by default help, the schemas or a fresh workspace. Evidence:
  - `scripts/check-producer-imports.py` ([report](2026-11-24-gap-audit/producer-imports.json));
  - `scripts/check-boundaries.py` (`just boundaries`);
  - `scripts/check-neutrality.py` (`just neutrality`), which reports its two plan-cited allowances: the `obsidian` subcommand line and the plan's "completeness and security guarantees" phrase.

  Rows [G7], [B2], [B3], [C12]. The audit recorded [X3] as open (F2); it is green since `9e1aec0e`.
- **The whole test suite passes offline, in a cleared environment with only `python3` and `openssl` on `PATH`.** `openssl` seals the release-trial bundle. Evidence: [Milestone B proof](2026-11-24-milestone-b-proof.json); the CI offline job. The audit recorded [N10] as limited (F1, F2); it is green since `9e1aec0e`.

### Documentation and package

- **Documentation is checked against the binary.** Executable examples run in fresh workspaces with the IDs they return. The quickstarts run from an empty directory. Every document under `docs/` and `methods/`, and the READMEs, names only commands the binary has. Evidence: `walkthroughs.rs`; `scripts/check-readme-quickstart.py`; `scripts/check-cli-goldens.py`; `docs.rs::every_document_names_only_what_the_binary_has`. Rows [G2], [G11]; [G1] is limited.
- **A reproducible release-candidate package for Linux x86_64 with glibc.**
  - Two builds from `git archive` give byte-identical binaries and archives.
  - The install never replaces an unrelated `inv`.
  - A smoke test ran 14 documented commands in a clean root without network.

  Evidence: `just package`, `just smoke-package`; reports for `8093f95f` in [the audit folder](2026-11-24-gap-audit/) (`package-report.json`, `smoke-report.json`). Versions, toolchain and filesystems: [support matrix](../../docs/support-matrix.md) [G6].

## Limited

Each of these works within the stated limit and no further.

- **Platform: Linux x86_64 only.**
  - Tested on Linux 7.2.8 with ext4 and tmpfs, and on the GitHub-hosted `ubuntu-24.04` runner.
  - XFS and btrfs are accepted but unqualified.
  - Network filesystems and FUSE are refused.
  - macOS, Windows, aarch64 and musl are unqualified and not packaged.

  The package needs glibc 2.34 or later. See the [support matrix](../../docs/support-matrix.md) and [packaging](../../docs/packaging.md). Rows [G6], [P18].
- **Durability is qualified against models, not physical power loss.**
  - Writer crashes are simulated with the in-memory crash model or injected errors. No test SIGKILLs a committing writer; the real SIGKILL test covers restore only.
  - The real-filesystem test covers three publication points, and its LazyFS variant is ignored in the normal suite.
  - LazyFS 0.3.1 passed the three journal cache-loss boundaries in a QEMU guest on 2026-11-22.
  - Batched vault publication (PERF.10) is checked against a power-loss model, not against LazyFS or dm-flakey ([ADR 0012](../../docs/adr/0012-batched-vault-durability.md)); bn-1i3a tracks that qualification.
  - An injected fsync error is not a kernel EIO.

  Rows [P1], [P8], [P9], [P19], [C6].
- **Concurrency and input bounds are partly unit-level.**
  - No test races a promotion against a retraction or a criterion change [P5].
  - There is no end-to-end test of a late criterion turning an accepted basis `needs_review` [P6].
  - No test asserts that an error is itself size-bounded [P11], [B11].
- **The vault was never opened in the viewer.**
  - Bases and Canvas get syntax and filter checks only, against the viewer's documentation.
  - Inert hostile text is judged by a CommonMark parse.
  - Nobody has judged the vault useful with plugins disabled, or read a review copy offline or on a phone.

  Rows [V1], [V4], [V11], [V14], [B15]; bn-3d9f, bn-3ijp.
- **`investigation obsidian setup` previews or applies two named native settings.** It has no uninstall or revert, and applying into a custom configuration folder is untested (preview only) [V15]. No companion exists [V10].
- **Export audience policy: audit row [V13] is open.**
  - The leak tests pass.
  - The derived-title leak the audit names (bn-31an) is closed, with `export_policy.rs::a_withheld_option_title_never_reaches_a_report_through_a_decision_title`.
  - Whether public copies may show revision counts, and supersession by a withheld decision, awaits the owner (bn-3aa9, bn-266b).
  - The CLI leak matrix runs at head only.
- **Agent evidence is development evidence, not the release trial.**
  - The [A-gate pilot](../../docs/a-gate.md) met its directional rule: successor accuracy was 64.2% with notes and 76.7% with the CLI, and false promotions tied at 2 of 48 per arm.
  - The [first-use study](../../docs/first-use.md) is a development check on 6 cases with one run per cell; in its own words, "differences of one or two assignments are noise". Its help-only arm missed preregistered target 1, with 4/12 and 5/12 receipts within three calls against at least 6. The re-measure is bn-gef3.
  - The deterministic halves of [C2], [C5], [C7], [C9] and [C11] pass in `just acceptance`. Their agent counts need the trial.
  - Scenarios 1-10 [C1] are scripted rehearsals on this binary.
- **Performance: plan §10.5 latency targets are missed.**
  - The [2026-11-23 baseline](../bench/2026-11-23-baseline.json) missed 8, for example the large `brief` at 17,617 ms p95 against 250 ms.
  - W08.19 later brought the large brief to about 2.1 s, and PERF.10 brought an unchanged 650-case render to about 2 s.
  - The targets have not been re-measured as a whole (bn-9muo).
- **Documentation execution [G1]:** most documented commands are parsed against the command tree, not executed. Only the quickstarts, walkthroughs, agent page, help examples and the package smoke run execute.
- **Measurement [X30]:** only open- versus closed-loop load and intended-send timing are compared. Other load differences show only through workload identity.
- **Record-mode closure [B7]:** a record case with a failed or invalid Run needs a Retrospective. This is stricter than one reading of plan §17.7, and it is kept pending the owner ([dispositions](../../docs/milestone-b.md#reviewer-dispositions)).
- **Known open defects:**
  - the Overview lists one source at two revisions (bn-bn1m);
  - audit references cannot be copied into `gap_dispositions` as given (bn-dem8);
  - `artifact.withdraw` has no guardrail like `source.withdraw` (bn-6fpb). Its dependents still go `needs_review`.

## Not yet demonstrated

These audit rows are open, and nothing in this build claims them.

- **The sealed, paired release trial** [C0], [C3], [C4], [C8], [B18]: no sealed run exists. The trial-task instances must be authored by a non-builder and sealed by the owner (bn-1jpq). The run (bn-1ktn) and the analysis under the preregistered rule (bn-2lvt) follow ([preregistration](../../docs/release-trial.md)).
- **The human experience gate** [H1]-[H10], [G8]: no session with a reviewer who did not create the case is recorded (bn-3ijp).
- **A dogfooded real investigation** [G10] (bn-6n1p).
- **The Milestone C gates as a whole** [G5] (bn-3q2), and **these release notes** [G12] (bn-3kac): this draft is the start of that row, not its completion.

## Planned or deferred

- **Deferred, not in this build:** the [deferred-feature register](../../docs/deferred.md) lists each item with its reason and entry condition: plan §22, and the deferrals of plan revision 4.1 and §19.3. In particular:
  - The MCP frontend (W13) is owner-deferred outside every milestone (plan §9.7, §19.3). This binary has no `mcp` command and refuses one as unrecognized, committing nothing [G3].
  - The optional Obsidian companion plugin (W15) and the W16 items (local action claims, SDK conveniences, pack-authoring docs, portability) are Milestone D. Each waits for the human gate or a demonstrated need.
  - Also deferred: distributed multiwriter sync, event-text physical purge, receipt signature verification, selector and receipt exporters, PDF export, semantic embeddings, typed `causal_role` tags, and a recovery archive that includes unfinalized capture staging.
- **Planned before the release (Milestone C):** the open rows above, plus:
  - the viewer checks of Bases, Canvas and previews (bn-3d9f);
  - LazyFS or dm-flakey qualification of batched publication (bn-1i3a);
  - the latency targets (bn-9muo);
  - the owner decisions listed under Limited;
  - the final REL.03 manifest: artifacts of the audited commit with verified checksums, source revision, license and notices.
- **No publishing step runs automatically.** Publishing is a separate, explicit action by the project owner.
