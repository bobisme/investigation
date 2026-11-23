//! `origin.trace` and `origin.judge_dependence` through the real operations.

use super::*;
use crate::{
    catalog::standard_catalog,
    command::Command,
    freshness::{Freshness, FreshnessIndex},
    header::SystemClock,
    identity::{IdSource, SystemIdSource},
    state::{Acceptance, AcceptedTransaction, validate_and_apply},
    wire::RecordChange,
};

struct Case {
    state: State,
    case: Option<Id>,
}

impl Case {
    fn new() -> Self {
        let mut test = Self {
            state: State::default(),
            case: None,
        };
        let accepted = test
            .submit_as(
                "agent:test",
                "case.create",
                json!({"title": "Do three references corroborate one value?", "mode": "explore"}),
            )
            .unwrap();
        test.case = Some(accepted.case_id);
        test
    }

    fn case(&self) -> Id {
        self.case.unwrap()
    }

    fn submit_as(
        &mut self,
        actor: &str,
        op: &str,
        payload: Value,
    ) -> Result<AcceptedTransaction, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("command-{sequence}"),
            "case_id": self.case,
            "actor": actor,
            "preconditions": [],
            "operations": [{"op": op, "payload": payload}],
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
        Ok(accepted)
    }

    fn one(&mut self, op: &str, payload: Value) -> RevisionRef {
        self.submit_as("agent:test", op, payload)
            .unwrap()
            .records
            .last()
            .unwrap()
            .reference()
    }

    fn current(&self, reference: RevisionRef) -> RevisionRef {
        self.state.current(reference.id).unwrap().reference()
    }

    /// Insert a record image directly, as ingest would record a Source or
    /// Artifact.
    fn image(&mut self, kind: Kind, body: Value) -> RevisionRef {
        let sequence = self.state.sequence() + 1;
        let mut header = self.state.current(self.case()).unwrap().header.clone();
        header.acceptance_sequence = sequence;
        header.id = SystemIdSource.next_id(kind).unwrap();
        let record = Record { header, body };
        let reference = record.reference();
        self.state
            .apply(&AcceptedTransaction {
                case_id: self.case(),
                sequence,
                created: vec![RecordChange {
                    client_ref: None,
                    id: reference.id,
                    revision: reference.revision,
                }],
                updated: vec![],
                records: vec![record],
                warnings: vec![],
            })
            .unwrap();
        reference
    }

    /// One separately collected document: a Source and its retained original.
    fn document(&mut self, name: &str) -> (RevisionRef, RevisionRef) {
        let source = self.image(
            Kind::Source,
            json!({
                "producer": null, "publisher": "Synthetic publisher", "identity": name,
                "acquisition": {"channel": "local_file", "acquired_at": null, "method": null, "context": null, "environment": null},
                "access": "local_only", "verification_limits": []
            }),
        );
        let artifact = self.image(Kind::Artifact, artifact(source, &[]));
        (source, artifact)
    }

    /// A derivative (extraction, screenshot, note) of one Artifact.
    fn derivative(&mut self, source: RevisionRef, input: RevisionRef) -> RevisionRef {
        self.image(Kind::Artifact, artifact(source, &[input]))
    }

    fn assertion(&mut self, statement: &str) -> RevisionRef {
        self.one(
            "observation.add",
            json!({
                "value": {"kind": "text", "statement": statement},
                "basis": {"kind": "assertion", "assertion": statement, "context": null},
                "extraction_method": null, "validity": "unknown", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": null, "run_ref": null,
            }),
        )
    }

    fn passage(&mut self, artifact: RevisionRef) -> RevisionRef {
        self.one(
            "observation.add",
            json!({
                "value": {"kind": "measurement", "value": "64", "unit": "frames"},
                "basis": {"kind": "source", "artifact_ref": artifact,
                    "selector": {"schema_version": "inv.selector.v1", "artifact_digest": DIGEST, "selector": {"type": "whole"}},
                    "text_quote": null},
                "extraction_method": "direct_read", "validity": "unknown", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": null, "run_ref": null,
            }),
        )
    }

    fn try_trace(
        &mut self,
        actor: &str,
        observation: RevisionRef,
        origin: Value,
        dimensions: Value,
    ) -> Result<RevisionRef, Failure> {
        let observation = self.current(observation);
        self.submit_as(
            actor,
            "origin.trace",
            json!({
                "observation_ref": observation, "origin": origin, "dimensions": dimensions,
                "rationale": "Compared the passage with the material it cites.",
            }),
        )
        .map(|accepted| accepted.records[0].reference())
    }

    fn original(&mut self, observation: RevisionRef, dimensions: Value) {
        self.try_trace(
            "agent:test",
            observation,
            json!({"kind": "original"}),
            dimensions,
        )
        .unwrap();
    }

    fn restates(&mut self, observation: RevisionRef, upstream: RevisionRef) {
        let upstream = self.current(upstream);
        self.try_trace(
            "agent:test",
            observation,
            json!({"kind": "restates", "observation_ref": upstream}),
            json!({}),
        )
        .unwrap();
    }

    fn try_judge(
        &mut self,
        actor: &str,
        roots: &[RevisionRef],
        judgment: &str,
        dimensions: Value,
    ) -> Result<RevisionRef, Failure> {
        let roots: Vec<RevisionRef> = roots.iter().map(|root| self.current(*root)).collect();
        self.submit_as(
            actor,
            "origin.judge_dependence",
            json!({
                "roots": roots, "judgment": judgment, "dimensions": dimensions,
                "basis": "Compared publishers, methods and test units.",
            }),
        )
        .map(|accepted| accepted.records[0].reference())
    }

    fn summary(&self, cited: &[RevisionRef]) -> OriginSummary {
        origin_summary(&self.state, self.case(), cited)
    }
}

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn artifact(source: RevisionRef, inputs: &[RevisionRef]) -> Value {
    json!({
        "digest": if inputs.is_empty() { json!(DIGEST) } else { json!(null) },
        "size_bytes": null, "format": "text/plain", "source": source,
        "role": if inputs.is_empty() { "original" } else { "derivative" },
        "transforms": inputs.iter().map(|input| json!({
            "input": input, "transformation": "example.extract", "version": "1", "input_digest": null
        })).collect::<Vec<_>>(),
        "availability": "retained", "integrity": "not_checked", "sensitivity": "internal",
    })
}

fn code(result: Result<RevisionRef, Failure>) -> ErrorCode {
    result.unwrap_err().error.code
}

/// Scenario D shape: a specification, an article restating it, a blog
/// repeating the article, the article's own bench test and an erratum.
struct Sources {
    spec: RevisionRef,
    article: RevisionRef,
    blog: RevisionRef,
    bench: RevisionRef,
    erratum: RevisionRef,
    spec_source: RevisionRef,
    article_source: RevisionRef,
    spec_artifact: RevisionRef,
}

fn scenario(case: &mut Case) -> Sources {
    let (spec_source, spec_artifact) = case.document("primary-specification.txt");
    let (article_source, article_artifact) = case.document("trade-article.txt");
    let (_, blog_artifact) = case.document("blog-post.txt");
    let (_, erratum_artifact) = case.document("first-party-erratum.txt");
    let sources = Sources {
        spec: case.passage(spec_artifact),
        article: case.passage(article_artifact),
        blog: case.passage(blog_artifact),
        bench: case.passage(article_artifact),
        erratum: case.passage(erratum_artifact),
        spec_source,
        article_source,
        spec_artifact,
    };
    case.original(
        sources.spec,
        json!({"upstream_source": "Corvin working group", "dependence_groups": ["working-group publications"]}),
    );
    case.restates(sources.article, sources.spec);
    case.restates(sources.blog, sources.article);
    case.original(
        sources.bench,
        json!({"upstream_source": "Article author", "method": "bench test", "environment": "one unit, build 3.1.2"}),
    );
    case.original(
        sources.erratum,
        json!({"upstream_source": "Corvin working group", "dependence_groups": ["working-group publications"]}),
    );
    sources
}

#[test]
fn three_acquisitions_of_one_restated_value_are_one_known_origin() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let summary = case.summary(&[s.spec, s.article, s.blog]);
    assert_eq!(summary.observations, 3);
    assert_eq!(summary.acquisition_count, 3);
    assert_eq!(summary.known_origin_count, 1);
    assert_eq!(summary.unknown_origin_count, 0);
    assert_eq!(summary.origins.len(), 1);
    assert_eq!(summary.origins[0].root.unwrap().id, s.spec.id);
    assert_eq!(summary.origins[0].observations.len(), 3);
    assert_eq!(summary.origins[0].acquisitions.len(), 3);
    assert!(summary.pairs.is_empty());
    assert_eq!(summary.independent_pairs, 0);
    // Every acquisition stays its own record.
    assert_eq!(
        case.state
            .records()
            .filter(|record| record.header.id.kind() == Kind::Source)
            .count(),
        4
    );

    // The blog follows the article to the specification.
    let blog = case.state.current(s.blog.id).unwrap();
    let origin = effective_origin(&case.state, blog);
    assert_eq!(origin.state, OriginState::Known);
    assert_eq!(
        origin.chain.iter().map(|step| step.id).collect::<Vec<_>>(),
        vec![s.article.id, s.spec.id]
    );
}

#[test]
fn a_second_claim_in_one_document_keeps_its_own_or_unknown_origin() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let summary = case.summary(&[s.article, s.bench]);
    // One document, two claims, two origins.
    assert_eq!(summary.acquisition_count, 1);
    assert_eq!(summary.acquisitions, vec![s.article_source.id]);
    assert_eq!(summary.known_origin_count, 2);
    let pair = &summary.pairs[0];
    assert_eq!(pair.dependence, Dependence::Unknown);
    assert!(pair.judgments.is_empty());
    // The roots were collected through different acquisitions.
    assert!(!pair.recorded_shared.contains(&OriginDimension::Acquisition));
    assert!(!pair.independent_corroboration);

    // An untraced claim in the same article has an unknown origin.
    let article = case.state.current(s.article.id).unwrap().body.clone();
    let artifact: RevisionRef = input::decode(&article["basis"]["artifact_ref"]).unwrap();
    let untraced = case.passage(artifact);
    let summary = case.summary(&[s.article, untraced]);
    assert_eq!(summary.known_origin_count, 1);
    assert_eq!(summary.unknown_origin_count, 1);
    let unknown = &summary.origins[1];
    assert_eq!(unknown.state, OriginState::Unknown);
    let view = observation_origin(&case.state, case.state.current(untraced.id).unwrap()).unwrap();
    assert_eq!(view.origin.reason, Some(UnknownReason::NotTraced));
    assert_eq!(view.acquisitions, vec![s.article_source.id]);
}

#[test]
fn absent_linkage_is_unknown_and_only_attributed_judgments_change_it() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let cited = [s.spec, s.article, s.blog, s.bench, s.erratum];
    let summary = case.summary(&cited);
    assert_eq!(summary.acquisition_count, 4);
    assert_eq!(summary.known_origin_count, 3);
    assert_eq!(summary.pairs.len(), 3);
    assert_eq!(summary.unknown_pairs, 3);
    assert_eq!(summary.independent_pairs, 0);
    assert!(
        summary
            .pairs
            .iter()
            .all(|pair| pair.dependence == Dependence::Unknown && pair.judgments.is_empty())
    );
    // The specification and erratum record one dependence group; that is a
    // computed shared fact, and the judgment still stays unknown.
    let index = |root: RevisionRef| {
        summary
            .origins
            .iter()
            .find(|group| group.root.map(|r| r.id) == Some(root.id))
            .unwrap()
            .index
    };
    let find = |summary: &OriginSummary, a: usize, b: usize| {
        summary
            .pairs
            .iter()
            .find(|pair| pair.origins == [a.min(b), a.max(b)])
            .unwrap()
            .clone()
    };
    let (spec, bench, erratum) = (index(s.spec), index(s.bench), index(s.erratum));
    let spec_erratum = find(&summary, spec, erratum);
    assert_eq!(spec_erratum.dependence, Dependence::Unknown);
    assert_eq!(
        spec_erratum.recorded_shared,
        vec![
            OriginDimension::UpstreamSource,
            OriginDimension::DependenceGroup
        ]
    );

    case.try_judge(
        "agent:reviewer",
        &[s.bench, s.erratum],
        "independent_on_stated_dimensions",
        json!(["method", "environment"]),
    )
    .unwrap();
    case.try_judge(
        "agent:reviewer",
        &[s.spec, s.erratum],
        "shared",
        json!(["upstream_source"]),
    )
    .unwrap();
    let summary = case.summary(&cited);
    let bench_erratum = find(&summary, bench, erratum);
    assert_eq!(
        bench_erratum.dependence,
        Dependence::IndependentOnStatedDimensions
    );
    assert_eq!(
        bench_erratum.judgments[0].assessor.to_string(),
        "agent:reviewer"
    );
    assert!(bench_erratum.independent_corroboration);
    assert_eq!(find(&summary, spec, erratum).dependence, Dependence::Shared);
    // Nobody judged the specification against the bench test.
    assert_eq!(find(&summary, spec, bench).dependence, Dependence::Unknown);
    assert_eq!(summary.independent_pairs, 1);
    assert_eq!(summary.unknown_pairs, 1);

    // A second assessor disagrees: the pair is unknown again, both kept.
    case.try_judge(
        "agent:second",
        &[s.bench, s.erratum],
        "partly_shared",
        json!(["environment"]),
    )
    .unwrap();
    let summary = case.summary(&cited);
    let disputed = find(&summary, bench, erratum);
    assert_eq!(disputed.dependence, Dependence::Unknown);
    assert!(disputed.disagreement);
    assert_eq!(disputed.judgments.len(), 2);
    assert!(!disputed.independent_corroboration);

    // The Observation view lists the judgments that link its root.
    let view = observation_origin(&case.state, case.state.current(s.erratum.id).unwrap()).unwrap();
    assert_eq!(view.dependence.len(), 3);
}

#[test]
fn views_and_notes_of_one_document_do_not_count_as_independent() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let source = s.spec_source;
    let extraction = case.derivative(source, s.spec_artifact);
    let screenshot = case.derivative(source, s.spec_artifact);
    let note = case.derivative(source, extraction);
    let quote = case.passage(s.spec_artifact);
    let extracted = case.passage(extraction);
    let pictured = case.passage(screenshot);
    let noted = case.passage(note);

    // Untraced views of one document form one unknown origin group.
    let summary = case.summary(&[quote, extracted, pictured, noted]);
    assert_eq!(summary.acquisition_count, 1);
    assert_eq!(summary.known_origin_count, 0);
    assert_eq!(summary.unknown_origin_count, 4);
    assert_eq!(summary.origins.len(), 1);
    assert_eq!(summary.origins[0].observations.len(), 4);
    assert!(summary.pairs.is_empty());
    assert_eq!(summary.independent_pairs, 0);

    // Beside the traced specification claim they share its acquisition.
    let summary = case.summary(&[s.spec, pictured]);
    assert_eq!(summary.known_origin_count, 1);
    let pair = &summary.pairs[0];
    assert!(pair.recorded_shared.contains(&OriginDimension::Acquisition));
    assert_eq!(pair.dependence, Dependence::Unknown);

    // Even a claimed independence between two views of one acquisition is
    // not independent corroboration, and stating it on the shared
    // acquisition is flagged as a conflict.
    case.original(pictured, json!({}));
    case.try_judge(
        "agent:careless",
        &[s.spec, pictured],
        "independent_on_stated_dimensions",
        json!(["method", "acquisition"]),
    )
    .unwrap();
    let summary = case.summary(&[s.spec, pictured]);
    let pair = &summary.pairs[0];
    assert_eq!(pair.dependence, Dependence::IndependentOnStatedDimensions);
    assert_eq!(pair.conflicts, vec![OriginDimension::Acquisition]);
    assert!(!pair.independent_corroboration);
    assert_eq!(summary.independent_pairs, 0);
}

#[test]
fn disagreeing_traces_are_disputed_and_never_resolve_to_a_root() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    case.try_trace(
        "agent:second",
        s.blog,
        json!({"kind": "original"}),
        json!({}),
    )
    .unwrap();
    let blog = case.state.current(s.blog.id).unwrap();
    let origin = effective_origin(&case.state, blog);
    assert_eq!(origin.state, OriginState::Disputed);
    assert_eq!(origin.reason, Some(UnknownReason::AssessorsDisagree));
    let summary = case.summary(&[s.spec, s.blog]);
    assert_eq!(summary.known_origin_count, 1);
    assert_eq!(summary.unknown_origin_count, 1);
    assert_eq!(summary.origins[1].state, OriginState::Disputed);
    // A disputed claim cannot be linked as a root.
    assert_eq!(
        code(case.try_judge("agent:test", &[s.spec, s.blog], "shared", json!([]))),
        ErrorCode::GateFailed
    );
    // The same assessor's later trace replaces its earlier one.
    case.try_trace(
        "agent:second",
        s.blog,
        json!({"kind": "restates", "observation_ref": case.current(s.article)}),
        json!({}),
    )
    .unwrap();
    let blog = case.state.current(s.blog.id).unwrap();
    assert_eq!(
        effective_origin(&case.state, blog).root.unwrap().id,
        s.spec.id
    );
    let view = observation_origin(&case.state, blog).unwrap();
    assert_eq!(view.traces.len(), 2);
    assert_eq!(view.traces_kept, 3);
}

#[test]
fn dependence_links_only_known_roots_and_needs_stated_dimensions() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let untraced = case.assertion("An operator remembers a 64 frame limit.");
    assert_eq!(
        code(case.try_judge("agent:test", &[s.spec, s.article], "shared", json!([]))),
        ErrorCode::GateFailed
    );
    assert_eq!(
        code(case.try_judge("agent:test", &[s.spec, untraced], "shared", json!([]))),
        ErrorCode::GateFailed
    );
    assert_eq!(
        code(case.try_judge(
            "agent:test",
            &[s.spec, s.bench],
            "independent_on_stated_dimensions",
            json!([])
        )),
        ErrorCode::InvalidInput
    );
    assert_eq!(
        code(case.try_judge("agent:test", &[s.spec], "shared", json!([]))),
        ErrorCode::InvalidInput
    );
    // A stale root revision is a conflict, not a silent rewrite.
    let stale = case.current(s.spec);
    case.original(s.spec, json!({}));
    let result = case.submit_as(
        "agent:test",
        "origin.judge_dependence",
        json!({"roots": [stale, case.current(s.bench)], "judgment": "shared", "basis": "Same lab."}),
    );
    assert_eq!(result.unwrap_err().error.code, ErrorCode::RevisionConflict);
    // An explicit unknown judgment is recorded and shown as unknown.
    case.try_judge("agent:test", &[s.spec, s.bench], "unknown", json!([]))
        .unwrap();
    let summary = case.summary(&[s.spec, s.bench]);
    assert_eq!(summary.pairs[0].dependence, Dependence::Unknown);
    assert_eq!(summary.pairs[0].judgments.len(), 1);
}

#[test]
fn traces_reject_self_reference_cycles_and_retracted_claims() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let spec = case.current(s.spec);
    assert_eq!(
        code(case.try_trace(
            "agent:test",
            s.spec,
            json!({"kind": "restates", "observation_ref": spec}),
            json!({}),
        )),
        ErrorCode::InvalidInput
    );
    let blog = case.current(s.blog);
    assert_eq!(
        code(case.try_trace(
            "agent:other",
            s.spec,
            json!({"kind": "restates", "observation_ref": blog}),
            json!({}),
        )),
        ErrorCode::GateFailed
    );
    assert_eq!(
        code(case.try_trace(
            "agent:test",
            s.bench,
            json!({"kind": "original"}),
            json!({"dependence_groups": ["lab", " lab "]}),
        )),
        ErrorCode::InvalidInput
    );
    let unknown = case.assertion("Someone said 64.");
    case.one(
        "observation.retract",
        json!({"observation_ref": unknown, "revision_reason": "Out of scope."}),
    );
    assert_eq!(
        code(case.try_trace("agent:test", unknown, json!({"kind": "unknown"}), json!({}))),
        ErrorCode::GateFailed
    );
}

#[test]
fn origin_judgments_do_not_change_freshness_but_upstream_changes_do() {
    let mut case = Case::new();
    let s = scenario(&mut case);
    let question = case.one(
        "question.create",
        json!({"wording": "What is the frame window at build 3.1.4?", "importance": "normal"}),
    );
    let hypothesis = case.one(
        "hypothesis.create",
        json!({"question_ref": question, "statement": "The window is 64 frames.",
            "possible_falsifiers": ["A first-party erratum changes the value."],
            "rival_refs": [], "assumption_refs": []}),
    );
    let cited = [s.spec, s.article, s.blog].map(|item| case.current(item));
    let assessment = case.one(
        "assessment.add",
        json!({"subject_ref": hypothesis, "evidence_refs": cited, "relation": "inconclusive",
            "rationale": "Three documents state 64 frames."}),
    );
    let fresh = |case: &Case, id: Id| {
        FreshnessIndex::build(&case.state, case.case())
            .get(id)
            .unwrap()
            .freshness
    };
    assert_eq!(fresh(&case, assessment.id), Freshness::Current);
    // New origin revisions of cited claims are not material revisions.
    case.try_judge("agent:test", &[s.spec, s.bench], "unknown", json!([]))
        .unwrap();
    case.try_trace(
        "agent:other",
        s.blog,
        json!({"kind": "restates", "observation_ref": case.current(s.article)}),
        json!({}),
    )
    .unwrap();
    assert_eq!(fresh(&case, assessment.id), Freshness::Current);
    let edges = crate::freshness::dependency_edges(case.state.current(s.blog.id).unwrap()).unwrap();
    assert!(edges.iter().any(|edge| {
        edge.field == "claim_origin.traces.origin.observation_ref" && edge.target.id == s.article.id
    }));
    let edges = crate::freshness::dependency_edges(case.state.current(s.spec.id).unwrap()).unwrap();
    assert!(edges.iter().any(|edge| {
        edge.field == "claim_origin.dependence.roots" && edge.target.id == s.bench.id
    }));
    // Retracting the restated claim makes the restatement's origin need review.
    let article = case.current(s.article);
    case.one(
        "observation.retract",
        json!({"observation_ref": article, "revision_reason": "Misquoted."}),
    );
    assert_eq!(fresh(&case, s.blog.id), Freshness::NeedsReview);

    // The origin summary appears on the assessment's cited evidence.
    let record = case.state.current(assessment.id).unwrap();
    let origins = cited_origins(&case.state, record);
    assert_eq!(origins.len(), 1);
    assert_eq!(origins[0].field, "evidence_refs");
    assert_eq!(origins[0].summary.acquisition_count, 3);
    assert_eq!(origins[0].summary.known_origin_count, 1);
}

#[test]
fn examples_decode_and_payloads_reject_unknown_fields() {
    let catalog = standard_catalog();
    for name in ["origin.trace", "origin.judge_dependence"] {
        let descriptor = catalog.get(name).unwrap();
        assert!(descriptor.example().is_some());
    }
    assert!(
        input::decode::<OriginTracePayload>(&json!({
            "observation_ref": {"id": "obs_019acd16-0000-7000-8000-000000000001", "revision": 1},
            "origin": {"kind": "original"}, "rationale": "x", "independent": true
        }))
        .is_err()
    );
}
