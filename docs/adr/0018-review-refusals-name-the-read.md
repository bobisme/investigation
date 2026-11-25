# ADR 0018 – A review refusal names the read, not the values it returns

Accepted with bn-g2l6; 2026-11-24. Refines plan §8.7 and §10.3, and ADR 0017. [Rule](../../crates/investigation-core/src/review_ops.rs) · [Contract](../schema-v1.md#review-refusals-name-the-read-not-its-values) · [Rehearsal](../../scripts/trial-tasks.py)

**Problem.** In the W14.03 rehearsal, task 7 stage 2, a successor was limited to `brief` and `show` at title or summary depth. It still built a valid validation review. It sent a `review.submit` with a placeholder digest and an empty read set. Then it copied `error.expected` from the two `GATE_FAILED` refusals: the subject's `basis_digest`, then the full required `read_set`. The core recorded a supporting review whose basis the reviewer never read, and a validation could follow. The values a repair hands back are the values that reading the basis would have produced, so the refusal stood in for the reading.

**What the plan says.**
- §8.7 asks an error to say what to do next, and says "Read revision 4 and reassess; do not blindly replay the old validation".
- §10.3 says that at promotion "a missing or changed required read-set member produces a repairable rejection with the current required set". We read "promotion" as the gates that promote: `conclusion.request_review`, `conclusion.validate` and `decision.accept`.
- The plan says nothing about what a `review.submit` refusal carries.

**Decision.**

1. *`review.submit` withholds what reading would give.* A wrong `basis_digest`, or a validation `read_set` that leaves out part of the current closure, fails with `GATE_FAILED` at that field and with no `expected`. `repair.argv` is `review-packet ID`. Its `review_basis` holds `subject_ref`, `basis_digest` and `read_set` for a Conclusion, Decision or Comparison. The message also names `show ID --depth full`, which returns the same block for a Conclusion or Decision. No new read surface is added: both reads existed. `record` adds `--case` to the `review-packet` argv, as it does for `show`.
2. *Blindspot reviews name records, not revisions.* A blindspot review's required read set is the current revisions of records the review itself cites (evidence, assumptions, gap references). A missing one is refused at `/read_set` with `error.actual.records` (the IDs) and no `expected`. `repair.argv` reads the first one at summary depth. The reviewer already named those records, so the IDs tell it nothing new. The current revision it cites then comes from a read.
3. *The open-objection repair works at summary depth (changes ADR 0017, item 3).* The argv is `show OBJECTION --depth summary`, and the message names `--depth full`. An Objection's summary title and synopsis are its statement, so a summary-only reader can read the challenge it must answer.
4. *The promotion gates keep §10.3's required set.* The `/preconditions` refusal of `conclusion.request_review` and `conclusion.validate`, the final-state closure check of `request_review`, and the stale-review `/read_set` refusal of `conclusion.validate` and `decision.accept` still return the current required set in `expected`. A promotion needs a current review that covers that set, so copying the set skips no reading. If the set changed, the only repair is a new review, and the new review is subject to item 1.
5. *Kept elsewhere.* These `expected` values are revision checks, structural corrections or non-reading data, not values a reader must produce:
   - `REVISION_CONFLICT` gives the stale and current revisions (`state`, `review_ops::current`, likelihood, blindspot related records).
   - `RETROSPECTIVE_STALE` gives the case revision at the declared snapshot.
   - A blindspot `audit.subject_ref` correction gives the subject from the audit response.
   - An unknown `gap_id` gives the gap IDs the audit reported.
   - A method-pack adoption names the pinned digest for `replaces`.
   - The ready gate lists rival hypotheses missing from `contradiction_refs`, which is claim content.
   - The rest are confidence templates, policy and guardrail blockers, acknowledgment lists and capture byte digests.

   The lesson-consult digest check (`lessons::verify_pointer`, `/lesson/lesson_digest`) also keeps its `expected`. Lesson text is withheld until the consulting case records a consultation. `retrospective --lessons --consult` builds the pointer from the admission listing and returns the text itself. The pointer names an exact Retrospective revision, whose text cannot change. So the digest binds the pointer to the admitted content, a guard against an admission that names other content. It is not proof that the consulter read anything, and the listing already shows it without the text.

**Rejected alternatives.**
- *Withhold the required set at the promotion gates too.* This would mean amending §10.3. It also gains nothing: the review those gates need is the step that requires the reading.
- *Withhold only from readers below full depth.* The core does not know how a caller read. Sessions and depths are self-reported, and only reads made through `investigation` under a declared session are recorded (§16.1). The same refusal must serve every caller.
- *A new read that returns only the basis.* `review-packet` and `show --depth full` already return it.

**Consequences.**
- *Compatibility.* `expected` is an optional, untyped field of the frozen error schema, so leaving it out and adding `actual.records` are allowed under ADR 0007. No enum or code changes. A client that copied `expected` from these refusals must now run the named read.
- *A summary-only reader stops.* The rehearsal's task-7 stage-2 reader follows the named read. That session's read policy refuses `review-packet`, so the reader stops before recording a review in both conditions, and the grader passes it. A reader that holds the basis from a full read still meets the remaining gates: none with guardrails off, and the guardrail and the self-reported confirmation with them on, as before.
- *W17.* Every refusal still gives a runnable repair. The CLI test follows each argv and commits the review from what the read returns.

**Tests.**
- `review_ops::tests`: `review_submit_refusals_name_the_read_not_its_values` and `a_review_from_the_basis_alone_cannot_skip_an_open_objection`.
- `blindspot_review::tests`: `related_records_must_be_current_and_in_the_read_set` and `a_disposition_accepts_its_findings_related_refs_as_given`.
- `objections.rs` (CLI): `review_refusals_name_a_runnable_read_and_no_values`.
- `trial_tasks.rs`, which runs the rehearsal.

Each fails without this change.
