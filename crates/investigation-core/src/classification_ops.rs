//! `classification.raise` and `classification.downgrade`.
//!
//! Both record an attributed change as an annotation with role
//! `classification` and no evidentiary standing; the subject record is never
//! rewritten. See [`crate::classification`] for how changes take effect.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    catalog::{OperationDescriptor, OperationInfo},
    classification::{
        CHANGE_ROLE, ChangeDirection, Classification, ClassificationChange, ClassificationLevel,
        Classifier, DowngradeReviewRecord, RedactionDecision, ReviewCoverage,
        ReviewedTransformationRecord, TransformationInputRecord,
    },
    command::{Reference, RevisionRef},
    evidence::SelectorRequest,
    identity::Kind,
    state::{OperationContext, Record},
    wire::{Digest, ErrorCode, Failure},
};

const MAX_TAGS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassificationRaise {
    /// Exact current revision of the record to handle more strictly.
    pub subject_ref: Reference,
    /// Stricter than the subject's current effective level.
    pub level: ClassificationLevel,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
    #[serde(default)]
    #[schemars(length(max = 32), inner(length(min = 1, max = 64)))]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformationInput {
    /// The subject itself or one of its exact derivation inputs.
    pub input_ref: Reference,
    /// Required for an input with retained bytes; must match them.
    pub digest: Option<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewedTransformation {
    /// Every input whose effective level is above the requested level.
    #[schemars(length(min = 1, max = 64))]
    pub inputs: Vec<TransformationInput>,
    #[schemars(length(min = 1, max = 256))]
    pub transformation: String,
    #[schemars(length(min = 1, max = 128))]
    pub version: String,
    /// Portions of the inputs the transformation and review addressed.
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub selectors: Vec<SelectorRequest>,
    #[schemars(length(min = 1, max = 256))]
    pub redaction_decisions: Vec<RedactionDecision>,
    /// Required when the subject is an artifact with retained bytes.
    pub output_digest: Option<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DowngradeReview {
    #[schemars(length(min = 1, max = 2048))]
    pub method: String,
    pub coverage: ReviewCoverage,
    /// Exact records the reviewer examined; must include the subject.
    #[schemars(length(min = 1, max = 64))]
    pub examined: Vec<Reference>,
    #[serde(default)]
    #[schemars(length(max = 32))]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassificationDowngrade {
    /// Exact current revision of the derivative or record being cleared.
    pub subject_ref: Reference,
    /// Less strict than the subject's current effective level.
    pub level: ClassificationLevel,
    #[schemars(length(min = 1, max = 2048))]
    pub reason: String,
    #[serde(default)]
    #[schemars(length(max = 32), inner(length(min = 1, max = 64)))]
    pub tags: Vec<String>,
    pub transformation: ReviewedTransformation,
    pub review: DowngradeReview,
}

fn invalid(path: &str, message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidInput, message).at(path)
}

fn gate(path: &str, message: impl Into<String>) -> Failure {
    Failure::new(ErrorCode::GateFailed, message).at(path)
}

fn integrity() -> Failure {
    Failure::new(
        ErrorCode::IntegrityFailure,
        "Resolved classification subject is missing.",
    )
}

fn text(value: &str, max: usize, path: &str) -> Result<(), Failure> {
    if value.trim().is_empty() || value.chars().count() > max || value.contains('\0') {
        return Err(invalid(
            path,
            "Supply concrete text within the published length bound.",
        ));
    }
    Ok(())
}

fn tags(tags: &[String]) -> Result<(), Failure> {
    if tags.len() > MAX_TAGS {
        return Err(invalid("/tags", "List at most 32 tags."));
    }
    let mut seen = BTreeSet::new();
    for (index, tag) in tags.iter().enumerate() {
        let path = format!("/tags/{index}");
        if tag.is_empty()
            || tag.chars().count() > 64
            || tag.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(invalid(
                &path,
                "Use tags of 1–64 characters without whitespace.",
            ));
        }
        if !seen.insert(tag) {
            return Err(invalid(&path, "List each tag once."));
        }
    }
    Ok(())
}

fn raise_shape(payload: &ClassificationRaise) -> Result<(), Failure> {
    text(&payload.reason, 2048, "/reason")?;
    tags(&payload.tags)
}

fn downgrade_shape(payload: &ClassificationDowngrade) -> Result<(), Failure> {
    text(&payload.reason, 2048, "/reason")?;
    tags(&payload.tags)?;
    let transformation = &payload.transformation;
    if transformation.inputs.is_empty() {
        return Err(invalid(
            "/transformation/inputs",
            "Name the exact inputs of the reviewed transformation.",
        ));
    }
    text(
        &transformation.transformation,
        256,
        "/transformation/transformation",
    )?;
    text(&transformation.version, 128, "/transformation/version")?;
    for (index, selector) in transformation.selectors.iter().enumerate() {
        selector.selector.validate_shape().map_err(|blocker| {
            invalid(
                &format!("/transformation/selectors/{index}"),
                blocker.code(),
            )
        })?;
    }
    if transformation.redaction_decisions.is_empty() {
        return Err(invalid(
            "/transformation/redaction_decisions",
            "Record at least one redaction decision, including what was retained and why.",
        ));
    }
    for (index, decision) in transformation.redaction_decisions.iter().enumerate() {
        let path = format!("/transformation/redaction_decisions/{index}");
        text(&decision.scope, 512, &format!("{path}/scope"))?;
        text(&decision.rationale, 2048, &format!("{path}/rationale"))?;
    }
    text(&payload.review.method, 2048, "/review/method")?;
    if payload.review.examined.is_empty() {
        return Err(invalid(
            "/review/examined",
            "List the exact records the reviewer examined.",
        ));
    }
    for (index, limitation) in payload.review.limitations.iter().enumerate() {
        text(limitation, 2048, &format!("/review/limitations/{index}"))?;
    }
    Ok(())
}

/// Resolve an exact, current, same-case subject that is not itself a change.
fn subject<'s>(
    ctx: &'s OperationContext<'_>,
    reference: &Reference,
) -> Result<(RevisionRef, &'s Record), Failure> {
    let exact = ctx.resolve_reference(reference, "/subject_ref")?;
    let record = ctx.exact(exact).ok_or_else(integrity)?;
    let current = ctx.current(exact.id).ok_or_else(integrity)?;
    if current.reference() != exact {
        let mut failure = Failure::new(
            ErrorCode::RevisionConflict,
            "Classify the subject's current revision.",
        )
        .at("/subject_ref");
        failure.error.actual = Some(json!(current.reference()));
        return Err(failure);
    }
    if ClassificationChange::from_record(record).is_some() {
        return Err(gate(
            "/subject_ref",
            "Classify the record itself, not a classification change.",
        ));
    }
    Ok((exact, record))
}

fn record_change(
    ctx: &mut OperationContext<'_>,
    change: ClassificationChange,
) -> Result<(), Failure> {
    // The change describes the stricter of the two states, so it is labelled
    // at that level.
    let classification = Classification {
        level: change.from.max(change.to),
        reason: Some(change.reason.clone()),
        tags: change.tags.clone(),
    };
    let body = json!({
        "subject_refs": [change.subject_ref],
        "role": CHANGE_ROLE,
        "evidentiary_status": "none",
        "classification_change": change,
    });
    ctx.create_classified(Kind::Annotation, body, classification)?;
    Ok(())
}

fn raise(payload: ClassificationRaise, ctx: &mut OperationContext<'_>) -> Result<(), Failure> {
    let (subject_ref, record) = subject(ctx, &payload.subject_ref)?;
    let classifier = Classifier::new(ctx.state());
    let current = classifier.effective(record);
    if payload.level <= current.level {
        let mut failure = gate(
            "/level",
            "The record is already handled at this level or stricter.",
        );
        failure.error.actual = Some(json!(current.level));
        return Err(failure);
    }
    record_change(
        ctx,
        ClassificationChange {
            direction: ChangeDirection::Raise,
            subject_ref,
            from: current.level,
            to: payload.level,
            base_level: classifier.base_level(record),
            reason: payload.reason,
            tags: payload.tags,
            transformation: None,
            review: None,
            guardrail: None,
        },
    )
}

fn downgrade(
    payload: ClassificationDowngrade,
    ctx: &mut OperationContext<'_>,
) -> Result<(), Failure> {
    let (subject_ref, record) = subject(ctx, &payload.subject_ref)?;
    let classifier = Classifier::new(ctx.state());
    let current = classifier.effective(record);
    if payload.level >= current.level {
        let mut failure = gate(
            "/level",
            "A downgrade names a level below the record's effective level. Use classification.raise for stricter handling.",
        );
        failure.error.actual = Some(json!(current.level));
        return Err(failure);
    }
    if payload.review.coverage == ReviewCoverage::PatternMatchingOnly {
        return Err(gate(
            "/review/coverage",
            "Pattern matching alone never proves a derivative safe. Review the whole transformed output before downgrading.",
        ));
    }

    let allowed: BTreeSet<RevisionRef> =
        crate::classification::derivation_inputs(ctx.state(), record)
            .into_iter()
            .chain([subject_ref])
            .collect();
    let mut inputs = Vec::new();
    let mut listed = BTreeSet::new();
    for (index, input) in payload.transformation.inputs.iter().enumerate() {
        let path = format!("/transformation/inputs/{index}");
        let exact = ctx.resolve_reference(&input.input_ref, &format!("{path}/input_ref"))?;
        if !allowed.contains(&exact) {
            let mut failure = gate(
                &format!("{path}/input_ref"),
                "List only the subject or its exact derivation inputs.",
            );
            failure.error.expected = Some(json!(allowed));
            return Err(failure);
        }
        if !listed.insert(exact) {
            return Err(invalid(
                &format!("{path}/input_ref"),
                "List each transformation input once.",
            ));
        }
        let input_record = ctx.exact(exact).ok_or_else(integrity)?;
        let retained = retained_digest(input_record);
        if input.digest != retained {
            let mut failure = gate(
                &format!("{path}/digest"),
                "Bind each input to the digest of its retained bytes, or omit it when none are retained.",
            );
            failure.error.expected = Some(json!(retained));
            return Err(failure);
        }
        inputs.push(TransformationInputRecord {
            input_ref: exact,
            digest: retained,
            level: classifier.effective(input_record).level,
        });
    }
    let uncovered: Vec<_> = classifier
        .inputs_above(record, payload.level)
        .into_iter()
        .filter(|input| !listed.contains(input))
        .collect();
    if !uncovered.is_empty() {
        let mut failure = gate(
            "/transformation/inputs",
            "The review must cover every input that is more sensitive than the requested level.",
        );
        failure.error.expected = Some(json!(uncovered));
        return Err(failure);
    }
    let output = retained_digest(record);
    if payload.transformation.output_digest != output {
        let mut failure = gate(
            "/transformation/output_digest",
            "Bind the review to the digest of the subject's retained bytes, or omit it when none are retained.",
        );
        failure.error.expected = Some(json!(output));
        return Err(failure);
    }
    let digests: BTreeSet<_> = inputs
        .iter()
        .filter_map(|input| input.digest.clone())
        .chain(output.clone())
        .collect();
    for (index, selector) in payload.transformation.selectors.iter().enumerate() {
        if !digests.contains(&selector.artifact_digest) {
            return Err(gate(
                &format!("/transformation/selectors/{index}/artifact_digest"),
                "A selector must address the bytes of a listed input or the output.",
            ));
        }
    }
    let mut examined = Vec::new();
    for (index, reference) in payload.review.examined.iter().enumerate() {
        examined.push(ctx.resolve_reference(reference, &format!("/review/examined/{index}"))?);
    }
    if !examined.contains(&subject_ref) {
        return Err(gate(
            "/review/examined",
            "The reviewer must examine the exact subject revision being downgraded.",
        ));
    }
    let base_level = classifier.base_level(record);
    let from = current.level;
    let subject_record = record.clone();
    let guardrail = crate::guardrails::enforce(
        ctx,
        crate::guardrails::GuardedOperation::ClassificationDowngrade,
        &subject_record,
    )?;
    let ReviewedTransformation {
        transformation,
        version,
        selectors,
        redaction_decisions,
        ..
    } = payload.transformation;
    let change = ClassificationChange {
        direction: ChangeDirection::Downgrade,
        subject_ref,
        from,
        to: payload.level,
        base_level,
        reason: payload.reason,
        tags: payload.tags,
        transformation: Some(ReviewedTransformationRecord {
            inputs,
            transformation,
            version,
            selectors,
            redaction_decisions,
            output_digest: output,
        }),
        review: Some(DowngradeReviewRecord {
            method: payload.review.method,
            coverage: payload.review.coverage,
            examined,
            limitations: payload.review.limitations,
        }),
        guardrail,
    };
    record_change(ctx, change)
}

fn retained_digest(record: &Record) -> Option<Digest> {
    if record.header.id.kind() != Kind::Artifact {
        return None;
    }
    serde_json::from_value(record.body.get("digest")?.clone()).ok()
}

pub(crate) fn operations() -> [OperationDescriptor; 2] {
    let info =
        |name, summary, when_to_use, effects, revision_rule, repair, related| OperationInfo {
            name,
            stage: "record",
            summary,
            when_to_use,
            lifecycle: "An existing case and an exact current same-case subject.",
            effects,
            returns: "Annotation ID (role classification) and revision 1.",
            revision_rule,
            idempotency: "Retry the same request with the original actor and command ID.",
            repair,
            related,
        };
    let digest = |fill: &str| format!("sha256:{}", fill.repeat(64));
    [
        OperationDescriptor::active(
            info(
                "classification.raise",
                "Handle a record and everything derived from it more strictly.",
                "A record, or content derived from it, needs stricter handling than its current level.",
                "Record an attributed change; every revision of the subject and its derivatives take the stricter level.",
                "Name the subject's exact current revision.",
                "Inspect the subject's effective level and request a stricter one.",
                &["classification.downgrade", "annotation.add"],
            ),
            json!({
                "subject_ref":{"id":"art_019acd16-0000-7000-8000-000000000001","revision":1},
                "level":"restricted",
                "reason":"Contains unpublished figures.",
                "tags":["unpublished"]
            }),
            raise_shape,
            raise,
        ),
        OperationDescriptor::active(
            info(
                "classification.downgrade",
                "Clear one exact derivative for a less strict audience after a reviewed transformation.",
                "A reviewer examined a transformed derivative and decided it may be handled at a lower level.",
                "Record the reviewed transformation; only the exact subject revision takes the lower level, and only while nothing it covered becomes stricter.",
                "Name the subject's exact current revision and exact input revisions with their digests.",
                "List every more sensitive input, bind input and output digests, record redaction decisions and review the whole output.",
                &["classification.raise"],
            ),
            json!({
                "subject_ref":{"id":"art_019acd16-0000-7000-8000-000000000002","revision":1},
                "level":"internal",
                "reason":"Identifying values were removed and the result was reviewed.",
                "transformation":{
                    "inputs":[{"input_ref":{"id":"art_019acd16-0000-7000-8000-000000000001","revision":1},
                        "digest":digest("1")}],
                    "transformation":"manual_redaction",
                    "version":"1",
                    "redaction_decisions":[{"scope":"Named individuals","action":"removed",
                        "rationale":"Not needed to interpret the result."}],
                    "output_digest":digest("2")
                },
                "review":{
                    "method":"Read the whole output against the original.",
                    "coverage":"whole_output",
                    "examined":[{"id":"art_019acd16-0000-7000-8000-000000000002","revision":1}]
                }
            }),
            downgrade_shape,
            downgrade,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::{
        catalog::standard_catalog,
        classification::{
            Audience, AudiencePolicy, ClassificationBasis, Field, effective_classification,
        },
        command::Command,
        header::SystemClock,
        identity::{IdSource, SystemIdSource},
        state::{Acceptance, AcceptedTransaction, State, validate_and_apply},
        wire::RecordChange,
    };

    struct Case {
        state: State,
        case: crate::identity::Id,
        sequence: u64,
    }

    fn sha(fill: char) -> Value {
        json!(format!("sha256:{}", fill.to_string().repeat(64)))
    }

    impl Case {
        fn new() -> Self {
            let mut case = Self {
                state: State::default(),
                case: SystemIdSource.next_id(Kind::Case).unwrap(),
                sequence: 0,
            };
            let accepted = case
                .submit(None, vec![json!({"op":"case.create","payload":{"title":"Classification fixture","mode":"explore"}})])
                .unwrap();
            case.case = accepted.case_id;
            case
        }

        fn submit(
            &mut self,
            case: Option<crate::identity::Id>,
            operations: Vec<Value>,
        ) -> Result<AcceptedTransaction, Failure> {
            let sequence = self.state.sequence() + 1;
            let command: Command = serde_json::from_value(json!({
                "schema_version":"inv.command.v1","command_id":format!("class-{sequence}"),
                "case_id":case,"actor":"agent:reviewer","preconditions":[],"operations":operations,
            }))
            .unwrap();
            let accepted = validate_and_apply(
                &self.state,
                &command,
                &standard_catalog(),
                Acceptance {
                    sequence,
                    clock: &mut SystemClock,
                    ids: &mut SystemIdSource,
                },
            )?;
            self.state.apply(&accepted)?;
            self.sequence = sequence;
            Ok(accepted)
        }

        fn op(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
            self.submit(Some(self.case), vec![json!({"op":op,"payload":payload})])
        }

        /// Ingest-shaped image: ingest is the only producer of sources and artifacts.
        fn image(
            &mut self,
            kind: Kind,
            level: ClassificationLevel,
            body: Value,
            provenance: Vec<crate::identity::Id>,
        ) -> RevisionRef {
            let sequence = self.state.sequence() + 1;
            let mut header = self.state.current(self.case).unwrap().header.clone();
            header.id = SystemIdSource.next_id(kind).unwrap();
            header.acceptance_sequence = sequence;
            header.classification = Classification {
                level,
                ..Classification::default()
            };
            header.provenance.references = provenance;
            let record = Record { header, body };
            let reference = record.reference();
            self.state
                .apply(&AcceptedTransaction {
                    case_id: self.case,
                    sequence,
                    records: vec![record],
                    created: vec![RecordChange {
                        id: reference.id,
                        revision: 1,
                        client_ref: None,
                    }],
                    updated: vec![],
                    warnings: vec![],
                })
                .unwrap();
            reference
        }

        fn level(&self, reference: RevisionRef) -> ClassificationLevel {
            effective_classification(&self.state, self.state.revision(reference).unwrap()).level
        }

        /// Restricted original, an unlabelled-as-restricted text derivative and
        /// an observation quoting the derivative.
        fn restricted_lineage(&mut self) -> (RevisionRef, RevisionRef, RevisionRef, RevisionRef) {
            let restricted = ClassificationLevel::Restricted;
            let source = self.image(
                Kind::Source,
                restricted,
                json!({"access":"restricted"}),
                vec![],
            );
            let original = self.image(
                Kind::Artifact,
                restricted,
                json!({
                    "digest":sha('1'),"size_bytes":10,"format":null,"source":source,"transforms":[],
                    "availability":"retained","integrity":"verified","sensitivity":"restricted"
                }),
                vec![source.id],
            );
            // Even a derivative whose own label is lower inherits the original's level.
            let derivative = self.image(
                Kind::Artifact,
                ClassificationLevel::Internal,
                json!({
                    "digest":sha('2'),"size_bytes":8,"format":"text/plain","source":source,
                    "transforms":[{"input":original,"transformation":"redact","version":"1"}],
                    "availability":"retained","integrity":"verified","sensitivity":"internal"
                }),
                vec![source.id, original.id],
            );
            let observation = self.image(Kind::Observation, ClassificationLevel::Internal, json!({
                "value":{"kind":"text","statement":"A sensitive derived statement."},
                "basis":{"kind":"source","artifact_ref":derivative,"selector":{"schema_version":"inv.selector.v1",
                    "artifact_digest":sha('2'),"selector":{"type":"whole"},"validation":"verified"},
                    "text_quote":{"state":"not_applicable"}},
                "validity":"valid","quality":[],"limitations":[],"variables_changed":[]
            }), vec![]);
            (source, original, derivative, observation)
        }
    }

    fn downgrade_payload(subject: RevisionRef, inputs: Value, output: Value) -> Value {
        json!({
            "subject_ref":subject,"level":"internal","reason":"Identifying values removed.",
            "transformation":{"inputs":inputs,"transformation":"manual_redaction","version":"1",
                "redaction_decisions":[{"scope":"Identifiers","action":"removed","rationale":"Not needed."}],
                "output_digest":output},
            "review":{"method":"Read the whole output.","coverage":"whole_output","examined":[subject]}
        })
    }

    #[test]
    fn derivatives_of_restricted_artifacts_stay_restricted_until_a_reviewed_downgrade() {
        let mut case = Case::new();
        let (source, original, derivative, observation) = case.restricted_lineage();
        let classifier = Classifier::new(&case.state);
        let effective = classifier.effective(case.state.revision(derivative).unwrap());
        assert_eq!(effective.level, ClassificationLevel::Restricted);
        assert_eq!(effective.recorded, ClassificationLevel::Internal);
        assert!(matches!(
            effective.basis,
            ClassificationBasis::Inherited { .. }
        ));
        assert_eq!(case.level(observation), ClassificationLevel::Restricted);
        let policy = AudiencePolicy::default();
        let record = case.state.revision(observation).unwrap();
        assert!(!classifier.may_show(record, Field::Title, &policy));
        assert!(classifier.may_show(record, Field::Identity, &policy));
        assert!(!classifier.may_show(
            record,
            Field::Identity,
            &AudiencePolicy::strict(Audience::Internal)
        ));

        let accepted = case
            .op(
                "classification.downgrade",
                downgrade_payload(
                    derivative,
                    json!([{"input_ref":original,"digest":sha('1')},{"input_ref":source,"digest":null}]),
                    sha('2'),
                ),
            )
            .unwrap();
        let change = &accepted.records[0];
        assert_eq!(change.body["role"], "classification");
        assert_eq!(change.body["evidentiary_status"], "none");
        // The change describes restricted material and is labelled accordingly.
        assert_eq!(
            change.header.classification.level,
            ClassificationLevel::Restricted
        );
        assert_eq!(case.level(derivative), ClassificationLevel::Internal);
        assert_eq!(case.level(observation), ClassificationLevel::Internal);
        assert_eq!(case.level(original), ClassificationLevel::Restricted);
        assert_eq!(
            case.level(change.reference()),
            ClassificationLevel::Restricted
        );

        // A further reviewed downgrade builds on the first one, and must still
        // cover the restricted inputs.
        let mut public = downgrade_payload(
            derivative,
            json!([{"input_ref":derivative,"digest":sha('2')}]),
            sha('2'),
        );
        public["level"] = json!("public");
        let failure = case
            .op("classification.downgrade", public.clone())
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::GateFailed, "{failure:?}");
        public["transformation"]["inputs"] =
            json!([{"input_ref":original,"digest":sha('1')},{"input_ref":source,"digest":null}]);
        case.op("classification.downgrade", public).unwrap();
        assert_eq!(case.level(derivative), ClassificationLevel::Public);
        // The observation keeps its own recorded default label.
        assert_eq!(case.level(observation), ClassificationLevel::Internal);
        let classifier = Classifier::new(&case.state);
        let record = case.state.revision(derivative).unwrap();
        assert!(classifier.may_show(record, Field::Title, &policy));
        // The derivative may be shown; its lineage to the restricted original may not,
        // unless placeholders are allowed.
        let strict = AudiencePolicy::strict(Audience::Internal);
        assert!(classifier.may_show(record, Field::Title, &strict));
        assert!(!classifier.may_show(record, Field::Lineage, &strict));

        // A later raise of the subject overrides the reviewed downgrade.
        case.op(
            "classification.raise",
            json!({"subject_ref":derivative,"level":"restricted","reason":"New concern."}),
        )
        .unwrap();
        assert_eq!(case.level(derivative), ClassificationLevel::Restricted);
    }

    #[test]
    fn downgrade_without_a_reviewed_transformation_is_rejected() {
        let mut case = Case::new();
        let (source, original, derivative, _) = case.restricted_lineage();
        let valid = downgrade_payload(
            derivative,
            json!([{"input_ref":original,"digest":sha('1')},{"input_ref":source,"digest":null}]),
            sha('2'),
        );
        let before = case.state.clone();
        let reject = |case: &mut Case, payload: Value| {
            case.op("classification.downgrade", payload).unwrap_err()
        };

        for missing in ["transformation", "review"] {
            let mut payload = valid.clone();
            payload.as_object_mut().unwrap().remove(missing);
            assert_eq!(
                reject(&mut case, payload).error.code,
                ErrorCode::InvalidInput
            );
        }
        let mut payload = valid.clone();
        payload["transformation"]["redaction_decisions"] = json!([]);
        assert_eq!(
            reject(&mut case, payload).error.code,
            ErrorCode::InvalidInput
        );

        let mut payload = valid.clone();
        payload["review"]["coverage"] = json!("pattern_matching_only");
        let failure = reject(&mut case, payload);
        assert_eq!(failure.error.code, ErrorCode::GateFailed);
        assert!(failure.error.message.contains("Pattern matching alone"));

        // The restricted original and source must be covered by the review.
        let mut payload = valid.clone();
        payload["transformation"]["inputs"] = json!([{"input_ref":derivative,"digest":sha('2')}]);
        let failure = reject(&mut case, payload);
        assert_eq!(failure.error.code, ErrorCode::GateFailed);
        assert_eq!(
            failure.error.path.as_deref(),
            Some("/operations/0/payload/transformation/inputs")
        );
        assert!(
            failure
                .error
                .expected
                .unwrap()
                .as_array()
                .unwrap()
                .contains(&json!(original))
        );

        let mut payload = valid.clone();
        payload["transformation"]["inputs"][0]["digest"] = sha('3');
        assert_eq!(reject(&mut case, payload).error.code, ErrorCode::GateFailed);

        let mut payload = valid.clone();
        payload["transformation"]["output_digest"] = Value::Null;
        assert_eq!(reject(&mut case, payload).error.code, ErrorCode::GateFailed);

        let mut payload = valid.clone();
        payload["review"]["examined"] = json!([original]);
        assert_eq!(reject(&mut case, payload).error.code, ErrorCode::GateFailed);

        let mut payload = valid.clone();
        payload["level"] = json!("restricted");
        assert_eq!(reject(&mut case, payload).error.code, ErrorCode::GateFailed);

        assert_eq!(case.state, before);
        assert_eq!(case.level(derivative), ClassificationLevel::Restricted);
        assert!(case.op("classification.downgrade", valid).is_ok());
    }

    #[test]
    fn raises_propagate_and_stricter_inputs_make_a_downgrade_stale() {
        let mut case = Case::new();
        let internal = ClassificationLevel::Internal;
        let source = case.image(
            Kind::Source,
            internal,
            json!({"access":"local_only"}),
            vec![],
        );
        let original = case.image(
            Kind::Artifact,
            internal,
            json!({
                "digest":sha('1'),"size_bytes":10,"format":null,"source":source,"transforms":[],
                "availability":"retained","integrity":"verified","sensitivity":"internal"
            }),
            vec![source.id],
        );
        let derivative = case.image(
            Kind::Artifact,
            internal,
            json!({
                "digest":sha('2'),"size_bytes":8,"format":null,"source":source,
                "transforms":[{"input":original,"transformation":"decode_utf8","version":"1"}],
                "availability":"retained","integrity":"verified","sensitivity":"internal"
            }),
            vec![source.id, original.id],
        );
        let mut downgrade = downgrade_payload(
            derivative,
            json!([{"input_ref":original,"digest":sha('1')},{"input_ref":source,"digest":null}]),
            sha('2'),
        );
        downgrade["level"] = json!("public");
        case.op("classification.downgrade", downgrade).unwrap();
        assert_eq!(case.level(derivative), ClassificationLevel::Public);

        // A raise is not a downgrade and needs no transformation, but cannot
        // repeat or lower the current level.
        let failure = case
            .op(
                "classification.raise",
                json!({"subject_ref":original,"level":"internal","reason":"Same."}),
            )
            .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::GateFailed);
        case.op(
            "classification.raise",
            json!({"subject_ref":original,"level":"restricted","reason":"Contains identifiers.","tags":["identifiers"]}),
        )
        .unwrap();
        assert_eq!(case.level(original), ClassificationLevel::Restricted);
        // The reviewed derivative was cleared from internal inputs; the stricter
        // original is no longer covered by that review.
        let classifier = Classifier::new(&case.state);
        let effective = classifier.effective(case.state.revision(derivative).unwrap());
        assert_eq!(effective.level, ClassificationLevel::Restricted);
        assert!(effective.stale_downgrade.is_some());

        // Raising the case raises every record in it.
        let case_ref = case.state.current(case.case).unwrap().reference();
        let note = case
            .op(
                "annotation.add",
                json!({"subject_refs":[case_ref],"content":{"kind":"text","text":"Plain note."}}),
            )
            .unwrap()
            .records[0]
            .reference();
        assert_eq!(case.level(note), ClassificationLevel::Internal);
        case.op(
            "classification.raise",
            json!({"subject_ref":case_ref,"level":"restricted","reason":"Whole case is sensitive."}),
        )
        .unwrap();
        assert_eq!(case.level(note), ClassificationLevel::Restricted);
        assert_eq!(case.level(source), ClassificationLevel::Restricted);
    }
}
