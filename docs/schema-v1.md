# Investigation v1 contracts

The public transaction envelope is `inv.command.v1`. Writes and structured errors use `inv.response.v1`. Local import manifests use `inv.ingest.v1`, and completed producer runs use `inv.producer-run.v1`. The published JSON Schemas cover commands, normal, import and producer-run receipts, producer-run envelopes, structural audit responses, structured errors, evidence selectors, and each active operation's payload. The catalog also lists reserved names so later implementations can claim them without collisions. [Published catalog](../schemas/v1/catalog.json)

## Using the contract

`investigation schema --list --include-deferred --format json` lists active and reserved names. `investigation schema OPERATION --schema-only --format json` emits the exact payload shape. With `--schema-only` omitted, discovery includes examples, lifecycle and repair guidance. A reserved name has no active write handler. The `capture` wire name stays reserved until its schemas are published. `capture begin` and `capture append` already accept the requests in the [recording guide](recording.md). The journal keeps a begin request under the tag `inv.capture.v1`; appended items are staged per handle and are not journal transactions. The synopsis role remains part of the annotation schema, and record extensions remain namespaced separately.

Put optional producer metadata in `source.metadata` on an ingest manifest. The Source record retains that object as opaque JSON and identifies the original bytes separately. Its contents carry neither additional authority nor effects. Required unsupported envelope versions produce a structured rejection before a transaction is committed.

Put a producer namespace and stable external ID in `source.acquisition_key` to name one logical acquisition. Each path or stdin name is one item of it. A later ingest of the same key and item reuses the recorded Source. If the description and bytes are unchanged, nothing is committed and the item reports `deduplicated` with a `prior` link. A changed description gives the next revision of that Source, and changed bytes give it another retained Artifact. Either one reports `revised`, never a new acquisition. An acquisition belongs to the case that first recorded it, and a reimport cannot lower its classification. Without a key, each ingest is its own acquisition. Identical bytes still share one stored blob, but acquisitions are never merged. The key is a producer claim, not verified identity. Runtime validation enforces revision preconditions, selector integrity, actor attribution and promotion rules.

## Producer runs

A producer that finished a run can describe it in one `inv.producer-run.v1` envelope and submit it with `investigation ingest --input ENVELOPE`. The `producer-run` and `producer-run-receipt` wire schemas publish the envelope and its receipt, and `investigation schema producer-run` shows a schematic example. [Envelope contract](recording.md#one-shot-envelope-invproducer-runv1)

## Measurement fields

Observations and Runs accept an optional `measurement` block: unit, workload, observation window, warm-up, sample count, raw samples, trial order (`interleaved`, `randomized` or `sequential`), load model, attempt counts, environment and tool identity, whether instrumentation changed, and limitations. Each field is optional, and each can be recorded as `{"state":"unknown"}` without rejecting the record. Raw samples are a flat array or a leveled list in which each sample states its build, process, iteration and order. Started, observed, timed-out, excluded and unknown counts each keep their definition. The core stores what the recorder states and does not derive one field from another. A known `measurement.unit` must equal the unit of a measurement value. A Run that repeats a procedure records `repetition`: a shared series, its own acquisition (`namespace` and `external_id`), and setup references pinned to exact revisions. A second Run with the same acquisition is rejected. When the assessed Observation or its Run records a structured environment or tool identity, a `measurement.assess` control qualifies only if it records every known component with the same value.

`measurement.statistic` states what the reported value summarizes: `quantile` (with a `quantile` level strictly between 0 and 1, such as `0.99`, with at most six fractional digits), `mean`, `minimum`, `maximum` or `other`. It can also state the `population` (`completed_attempts` or `all_attempts`) and how values from separate runs were combined (`single_run`, `pooled_samples`, `merged_histograms`, `averaged_run_values` or `other`). The core never computes the statistic.

### Advisory measurement flags

The core flags recorded measurement conditions under rule version `measurement-flags.v1`. A flag never rejects a record and never changes recorded or effective validity. Each flag has a stable `flag_id`, `advisory: true`, the record it is about, the exact fields it read (record, path and recorded value), and a reason. The rules fire only from recorded fields:

- `measurement.quantile_sample_count_unrecorded`: a quantile has no known sample count.
- `measurement.quantile_below_order_statistic_bound`: the recorded count is below the smallest n with level^n ≤ 0.05. Below that count, the sample maximum cannot be a distribution-free 95% upper bound on the quantile. This needs 299 samples for 0.99 and 2,995 for 0.999. The check is arithmetic on the recorded count. It is not a universal sample floor.
- `measurement.flat_samples`: flat raw samples cannot support a variance analysis across repetition levels.
- `measurement.sequential_trial_order`: the compared trials ran sequentially.
- `measurement.closed_loop_without_intended_send_timing`: load came from a closed loop, and the durations are not known to be measured from the intended send time (coordinated omission).
- `measurement.quantiles_combined_across_runs`: per-run quantiles were averaged or combined in some other way. Combining needs raw samples or mergeable histograms.
- `measurement.completion_only_statistic`: the statistic covers completed attempts only, or its population is unrecorded while the recorded counts show attempts without an observed outcome. The reason lists the recorded counts.
- `measurement.instrumentation_changed`: instrumentation changed relative to the compared runs.
- `measurement.compared_workload_differs`, `measurement.compared_load_model_differs`, `measurement.compared_warmup_differs`, `measurement.compared_tool_identity_differs`: two measurements that are compared differ in the field, or only one of them records it. A pair is compared when it is a `measurement.assess` control and its target, or when a record cites both.
- `measurement.outcome_from_completion_only_statistic`: a Comparison records `meets` or `partially_meets` from a completion-only statistic. A criterion over all attempts cannot be satisfied that way, and the reason names the recorded counts.

`show` and `brief` include `measurement_flags` for Observations and for Assessments, Comparisons, Conclusions and Decisions. For a record that cites Observations, the report covers the exact cited revisions. A report lists at most 32 flags and counts the rest in `omitted`. A receipt adds a warning for each flag about the record it wrote. `next` and `frontier` list each flag as a gap whose `rule_id` is the flag ID.

Put `derived_from` on an ingest manifest when the items are outputs of a transformation of recorded Artifacts, such as a redaction, a text extraction, a plot or an aggregation. Name each exact input revision, the transformation identity and version, and optionally the input digests, the selectors used, the redaction decisions and how the output was reviewed. A supplied input digest must match the recorded one. The output is recorded as a derivative Artifact with role `derivative`; no Source or acquisition is created, and each item reports `derived` with its lineage. A derivative's `source` names the acquisition of its first input. Its `transforms` name every input with the digest the transformation read. The inputs and their bytes are never changed. `source.title` labels the output, and `source.format` gives its format. A derivative is ingested into the existing case that holds its inputs and cannot carry an acquisition key, publisher or identity. Every Artifact records whether it is an `original`, a `derivative` or a `manifest`. `show` follows a derivative, or the Artifact an Observation cites, through derivatives of derivatives to the original acquisitions. It lists the other Artifacts that share those origins: they are views of the same captured bytes, not independent confirmations. It also reports when an original is no longer retained, so the derivation cannot be re-inspected. Classification and freshness follow the same input references.

## Structural blindspot audit

`investigation blindspots` returns an `inv.audit.v1` object, published as the `audit` wire schema. It reads one committed snapshot of one case and writes nothing. It does not call a model and does not record a Review. The fields are:

- `snapshot`, `check_version` (`builtin-blindspot.v1`) and `check_set_digest`. The digest is a SHA-256 hash of each check ID and version that ran, so any change to the check set changes it.
- `scope`: the case, the optional `--subject` revision with its `subject_read_set`, and the audience that decides what is withheld.
- `checks[]`: one finding per detected gap. Each finding has its `check_id`, `check_version`, `gap_id`, `state` (`gap_detected_in_recorded_structure`), `severity` with a `severity_reason`, the `subject_refs` and `related_refs` it read, machine-readable `facts`, a fixed `why`, a `suggested_schema` and a `guide` card, and argv to inspect them.
- `checks_run[]`: every check, with its version, source, subject kinds, the fields it reads, its condition and its number of findings.
- `outcome`, `statement` and `limits`. A result with no finding reads "No structural gap identified under checks X." The limits state that the result is not proof of completeness.

A `gap_id` is stable for the same check and subject IDs. A check built on a gap rule keeps the `gap:` ID that `next` and `frontier` use, and `rule_id` names that rule. Other checks use `blindspot:CHECK:IDS`. Messages are fixed text for each check and never quote a record. A referenced record that the default audience may not see is listed in `withheld_refs` as an ID only.

Checks of `builtin-blindspot.v1`:

- `promoted.open_critical_objection`: a validated Conclusion or accepted Decision has an open critical Objection on itself or on its promotion basis.
- `promoted.basis_not_current`: a validated Conclusion or accepted Decision is `needs_review` or `disputed` for a reason that is not an Objection. The facts name each freshness trigger and its cause.
- `measurement.advisory_flags`: an advisory measurement flag. `rule_id` is the flag ID.
- `conclusion.causal_claim_without_rival_account`, `decision.accepted_with_unresolved_hard_cell` and `case.ready_to_close_without_retrospective`: the gap rules of the same names.

To add a check, add a `CheckSpec` to a family in `crates/investigation-core/src/blindspot/` and list the family in `FAMILIES`. The module documentation gives the rules, and its tests check every registered spec.

## Classification

Records are `internal` unless a stricter level is requested. Renderers use the effective level, not the stored label. A derivative is at least as strict as its inputs, and a case raised above `internal` raises every record in it. `classification.raise` applies to every revision of its subject. `classification.downgrade` clears one exact revision. It requires a reviewed transformation: exact inputs and digests, the transformation and version, redaction decisions, the output digest and a whole-output review. Pattern matching alone is rejected. The downgrade stops applying if a covered input later becomes stricter. Both changes are annotations with role `classification`; the subject record is not rewritten. `show`, `brief` and `evidence read` use the default `internal` audience. Classification is handling policy for this tool's outputs, not access control.

## Source assessment and withdrawal

`source.assess` records one attributed judgment of a Source in a new revision. Authenticity and attribution, reliability (track record, competence, access), origin role and currency are separate dimensions. Each has its own rationale, `cannot_judge` is allowed, and no combined grade is computed. Reliability of the source is judged separately from the credibility of any datum it reports. Judgments are kept, and the latest one per assessor and dimension is current, so assessors can disagree.

`source.withdraw` and `artifact.withdraw` record an attributed withdrawal with a reason and a scope. An `entire` withdrawal sets `status: withdrawn` and removes the record from further evidentiary use. A `partial` withdrawal names the part it covers and leaves the rest usable. Withdrawal is not erasure. The digest, availability, retained bytes and earlier revisions stay unchanged. `evidence read` still returns withdrawn bytes and labels them in `evidentiary_use`, and `show` labels Sources, Artifacts and Observations the same way.

Dependent observations, assessments, comparisons, conclusions and decisions become `needs_review` when a withdrawal is recorded or when a judgment newly raises an authenticity, reliability or currency concern, for example after an erratum. Other judgments do not change freshness. New diagnostic assessments, direct support and new Observations cannot rest on withdrawn material. A reimport of the same acquisition keeps its withdrawals and judgments.

`show`, `brief` and `frontier` read freshness from one dependency index. Each view's `freshness` gives `effective` (`current`, `needs_review` or `disputed`), a `label` such as `validated / needs_review`, and `reasons`: each reason names its trigger, the current revision that caused it, and the dependency path from the record to it. The case view adds an `overview` of every record still in evidentiary use. A `measurement.assess` judgment reaches dependents only when it lowers the Observation's effective validity, and classification changes never change freshness. A Review that read a Source still covers it after judgments that raise no concern, so they do not block promotion. A Source revision outside its use fields, such as a reimport with a changed description, is a material change: dependents need review and promotion needs the current revision. Gap rule `freshness.needs_review` lists live assessments, comparisons, conclusions and decisions that need review, with the same reasons.

## Compatibility

The v1 readers accept the alpha command and ingest tags used by the pilot. Existing alpha receipts remain readable where serialized locally. Resubmitting identical alpha bytes under the original actor and command ID returns the original transaction and snapshot. Submit new requests with v1. A retry keeps the original envelope version as part of the original request. The journal retains its versioned storage encoding, event bytes, hashes and historical record revisions. Other auxiliary read payloads carry their own version tags.

Published v1 schemas allow additive optional properties that preserve every existing valid request. Required fields, existing value constraints and published operation names are stable. Incompatible changes need a new version and a documented migration. Reserved names may become active. The CI workflow regenerates the catalog from the binary, checks published files, and checks compatibility against the push or pull-request base. [Publication check](../scripts/check-v1-schemas.py)

The v1 qualification also extracts the original pilot handoffs, verifies their journals, rebuilds the index, and resolves and resubmits the archived commands. [Alpha replay check](../scripts/check-v1-legacy.py) · [Pilot archive and provenance](../training/a-gate/replay.md)
