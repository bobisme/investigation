# ADR 0020 – A proposed Decision is replaced by supersession, and a choice is made eligible before it is compared

Accepted with bn-30vf; 2026-11-24. Refines plan §9.4 (`decision.*`) and §10.3, and ADR 0018. [Rule](../../crates/investigation-core/src/decision_ops.rs) · [Contract](../schema-v1.md#replacing-a-proposed-decision-and-the-eligibility-order) · [Tests](../../crates/investigation-cli/tests/decision_lifecycle.rs)

**Problem.** In the PERF.08 dogfood (F15, F16 and F17 in `notes/dogfood/2026-11-24-perf08.md`), five Decisions were proposed before one could be accepted:
- `decision.supersede` refused a proposed Decision, and nothing else could withdraw one, so each stale proposal stayed live.
- `decision.accept` needs the chosen option to be `eligible`, but `option.create` makes it `proposed`. Making it eligible after the comparisons had been taken gave the option a new revision. That staled every Comparison that pinned the option, the Decision that cited them, and its review. Nothing said what order avoids this.
- The accept receipt carried 22 "uncertain comparison" warnings, one per comparison. The decision record put the four stale proposals before the accepted Decision.

**What the plan says.**
- A Decision is `proposed`, `accepted` or `superseded`. Accepted content is immutable, and a correction is "a new superseding Decision with links" (plan line 536, §9.4 row `decision.propose / accept / supersede`).
- The plan has no `withdrawn` state for a Decision. A choice may be `defer {reason}`.
- §10.3 makes the scoped option set a membership of the review, and a Comparison pins the exact option revision.

**Decision.**

1. *A proposal is superseded like an accepted Decision.* `decision.supersede` accepts a `proposed` or `accepted` Decision. The old one keeps its content. It becomes `superseded`, with `change_reason` and `superseded_by`, and the replacement is a new proposal with `supersedes`. A `superseded` Decision is refused at `/decision_ref`, and the message names its successor as the record to supersede. Withdrawing a choice with no replacement choice is a supersession whose replacement has `choice: {"kind": "defer", "reason": ...}`. No new lifecycle value or operation is added, so ADR 0007's v1 freeze is not touched. `decision.propose` warns, once per other live proposal for the same question, that it stays proposed and that `decision.supersede` replaces it.
2. *The order is stated, not relaxed.* An option disposition stays a substantive change: it changes the scoped option set that a review covers, and no rule makes a lifecycle-only revision benign to a Comparison or review. Making it benign would let a review stay current over an option set it did not read (ADR 0017, ADR 0018). Instead, `decision.propose` (and the replacement in `decision.supersede`) warns when the selected option is not `eligible`. The `decision.accept` refusal at `/choice` names the option and its lifecycle in `actual`, and gives the order: `option.disposition` to `eligible`, `comparison.revise` of each Comparison of the option, `decision.supersede` with the new revisions, a fresh `review.submit`, then `decision.accept`. `repair.argv` is `schema option.disposition`, and `repair.schema` stays `decision.accept`, as for every gate. No warning is given at `comparison.add`: one warning per Comparison is the noise item 4 removes.
3. *A validation `read_set` refusal names the records the reviewer cited (refines ADR 0018, item 1).* When the only records missing from `read_set` are ones the review cites in `objection_refs` or `materials_examined`, the refusal says so and lists their IDs in `error.actual.records`, as item 2 of ADR 0018 does for blindspot reviews. The reviewer named those records, so the IDs tell it nothing new. When a member of the subject's dependency closure is missing, the refusal is unchanged: it names `review-packet`, and it names no closure member, even if cited records are listed too.
4. *One warning for the uncertain comparisons.* The accept receipt gives one warning that counts them, lists their IDs and says what makes a Comparison uncertain: its outcome is `unknown`, or no cited Observation is a valid, verified measurement whose value matches. The stored `acceptance_challenges.uncertain_comparisons` is unchanged. The decision record export lists accepted Decisions first, then live proposals. Superseded ones stay in the supersession history.

**Rejected alternatives.**
- *A `withdrawn` Decision lifecycle or a separate withdraw operation for Decisions.* The plan has neither. `DecisionLifecycle` is a closed v1 enum in records and responses, so it is not one that ADR 0007 opens. A `defer` replacement records the same outcome with a reason and a link.
- *Let a review taken after the disposition count, or treat a lifecycle-only disposition as benign.* Either way, a review would stay current over a scoped option set that changed after it read the set.
- *Refuse a non-eligible choice at `decision.propose`.* A proposal may still be revised before review, as for an unconditioned hard criterion, so it warns.
- *Count a Comparison as uncertain only when it states a doubt.* `uncertainty` is required and non-empty on every Comparison, so that rule would mark every one uncertain. The rule stays as it was: an unknown outcome, or no verified matching measurement. The warning now says this.

**Consequences.**
- *Compatibility.* No schema shape, enum or error code changes. Field descriptions were added to `review.submit` (`subject_ref`, `basis_digest`, `read_set`, `materials_examined`, `objection_refs`), which `check-v1-schemas.py` treats as documentation. `actual` is an untyped optional field. Receipts recorded before this change keep their per-comparison warnings in the journal. A replay does not rewrite them.
- *Next work.* `next` does not yet put an option's eligibility before its comparisons (bn-30vf follow-up).

**Tests.** `decision_lifecycle.rs` (CLI):
- `a_proposed_decision_is_replaced_with_a_reason_and_reads_show_only_the_live_one`
- `a_choice_still_proposed_warns_at_propose_and_the_accept_refusal_gives_the_order`
- `accepting_over_uncertain_comparisons_gives_one_warning_that_explains_them`
- `the_read_set_refusal_names_the_cited_records_it_lacks_and_no_closure_member`
- `the_decision_record_leads_with_the_accepted_decision`
- `packet_help_and_the_review_schema_state_what_a_reviewer_must_cite`

Each fails without its part of this change.
