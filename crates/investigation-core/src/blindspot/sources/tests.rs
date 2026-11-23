use serde_json::{Value, json};

use crate::{
    blindspot::{AuditRequest, AuditResponse, Finding, audit, audit_schema},
    catalog::standard_catalog,
    command::{Command, RevisionRef},
    header::{ActorDescriptor, ActorKind, SystemClock},
    identity::{Id, IdSource, Kind, SystemIdSource},
    state::{Acceptance, AcceptedTransaction, Record, State, validate_and_apply},
    wire::{Failure, RecordChange, Snapshot},
};

const DIGEST: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

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
        test.create(
            "case.create",
            json!({"title": "Source blindspots", "mode": "record"}),
        );
        test
    }

    fn submit(&mut self, op: &str, payload: Value) -> Result<AcceptedTransaction, Failure> {
        let sequence = self.state.sequence() + 1;
        let command: Command = serde_json::from_value(json!({
            "schema_version": "inv.command.v1",
            "command_id": format!("command-{sequence}"),
            "case_id": self.case,
            "actor": "agent:test",
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
        self.state.apply(&accepted).unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        self.case.get_or_insert(accepted.case_id);
        Ok(accepted)
    }

    fn create(&mut self, op: &str, payload: Value) -> RevisionRef {
        self.submit(op, payload)
            .unwrap_or_else(|failure| panic!("{op}: {failure:#?}"))
            .records
            .last()
            .unwrap()
            .reference()
    }

    /// Store a record image directly, optionally as another actor with a
    /// self-reported descriptor.
    fn image_as(
        &mut self,
        kind: Kind,
        body: Value,
        replaces: Option<RevisionRef>,
        actor: Option<(&str, Option<&str>)>,
    ) -> RevisionRef {
        let case = self.case.unwrap();
        let sequence = self.state.sequence() + 1;
        let mut header = self
            .state
            .current(replaces.map_or(case, |reference| reference.id))
            .unwrap()
            .header
            .clone();
        header.acceptance_sequence = sequence;
        if replaces.is_some() {
            header.history.push(header.revision);
            header.revision.0 += 1;
        } else {
            header.id = SystemIdSource.next_id(kind).unwrap();
            header.revision.0 = 1;
            header.history.clear();
        }
        if let Some((actor, family)) = actor {
            header.recorded_by = actor.parse().unwrap();
            if replaces.is_none() {
                header.created_by = header.recorded_by.clone();
            }
            header.actor_descriptor_self_reported = Some(ActorDescriptor {
                kind: ActorKind::Agent,
                model_family: family.map(str::to_owned),
                model_version: None,
                harness: None,
            });
        }
        let record = Record { header, body };
        let reference = record.reference();
        let change = RecordChange {
            client_ref: None,
            id: reference.id,
            revision: reference.revision,
        };
        let (created, updated) = if replaces.is_some() {
            (vec![], vec![change])
        } else {
            (vec![change], vec![])
        };
        self.state
            .apply(&AcceptedTransaction {
                case_id: case,
                sequence,
                records: vec![record],
                created,
                updated,
                warnings: vec![],
            })
            .unwrap();
        self.state.note_verified_snapshot(self.snapshot()).unwrap();
        reference
    }

    fn image(&mut self, kind: Kind, body: Value, replaces: Option<RevisionRef>) -> RevisionRef {
        self.image_as(kind, body, replaces, None)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            sequence: self.state.sequence(),
            head_hash: format!("sha256:{:064x}", self.state.sequence())
                .parse()
                .unwrap(),
        }
    }

    fn audit(&self) -> AuditResponse {
        let response = audit(
            &self.state,
            &self.snapshot(),
            self.case.unwrap(),
            &AuditRequest::default(),
        )
        .unwrap();
        let schema = serde_json::to_value(audit_schema()).unwrap();
        let instance = serde_json::to_value(&response).unwrap();
        assert!(jsonschema::is_valid(&schema, &instance), "{instance:#}");
        response
    }

    fn body(&self, reference: RevisionRef) -> Value {
        self.state.current(reference.id).unwrap().body.clone()
    }

    /// One acquisition: a Source and its original Artifact.
    fn document(&mut self, artifact: Value) -> (RevisionRef, RevisionRef) {
        let source = self.image(Kind::Source, json!({"access": "local_only"}), None);
        let mut body = json!({
            "digest": DIGEST, "size_bytes": 64, "format": "text/plain", "source": source,
            "transforms": [], "availability": "retained", "integrity": "verified",
            "sensitivity": "internal", "role": "original",
        });
        for (key, value) in artifact.as_object().unwrap() {
            body[key] = value.clone();
        }
        let artifact = self.image(Kind::Artifact, body, None);
        (source, artifact)
    }

    /// A text Observation quoting `artifact` with the given quote state.
    fn quote(&mut self, artifact: RevisionRef, state: &str) -> RevisionRef {
        self.image(
            Kind::Observation,
            json!({
                "status": "active",
                "value": {"kind": "text", "statement": "The window is 64 frames."},
                "basis": {"kind": "source", "artifact_ref": artifact,
                    "selector": {"schema_version": "inv.selector.v1", "artifact_digest": DIGEST,
                        "selector": {"type": "byte_range", "start": 0, "end": 24}, "validation": "verified"},
                    "text_quote": {"exact": "The window is 64 frames.", "state": state}},
                "extraction_method": "Quoted the passage.",
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": null,
                "run_ref": null,
            }),
            None,
        )
    }

    /// An attributed assertion with no source selector, optionally naming a Run.
    fn assertion(&mut self, run: Option<RevisionRef>) -> RevisionRef {
        self.image(
            Kind::Observation,
            json!({
                "status": "active",
                "value": {"kind": "text", "statement": "The window is 48 frames."},
                "basis": {"kind": "assertion", "assertion": "Stated by the operator.", "context": null},
                "extraction_method": null,
                "validity": "valid", "quality": [], "limitations": [],
                "evidence_kind": "observational", "variables_changed": [], "observed_at": null,
                "run_ref": run,
            }),
            None,
        )
    }

    fn run(&mut self, coverage: Value) -> RevisionRef {
        self.image(
            Kind::Run,
            json!({
                "intent": {"kind": "exploration", "intent": "Search a register."},
                "producer": "agent:fixture", "tool": null, "environment": null,
                "started_at": null, "finished_at": null, "result_assertion": null,
                "artifact_refs": [], "manifest_digest": null, "execution_status": "completed",
                "evidence_completeness": "complete", "evidence_kind": "observational",
                "variables_changed": [], "search_coverage": coverage,
            }),
            None,
        )
    }

    fn hypothesis(&mut self) -> RevisionRef {
        let question = self.create(
            "question.create",
            json!({"wording": "What is the frame window?", "importance": "normal"}),
        );
        self.create(
            "hypothesis.create",
            json!({"question_ref": question, "statement": "The window is 64 frames.",
                "possible_falsifiers": ["An erratum changes it."], "rival_refs": [], "assumption_refs": []}),
        )
    }

    fn assessment(
        &mut self,
        subject: RevisionRef,
        evidence: &[RevisionRef],
        extra: Value,
    ) -> RevisionRef {
        let mut body = json!({
            "subject_ref": subject, "evidence_refs": evidence, "relation": "supports",
            "rationale": "The cited passages state the value.",
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        self.create("assessment.add", body)
    }

    /// An Assessment image, for evidence states the write gate refuses.
    fn assessment_image(&mut self, subject: RevisionRef, evidence: &[RevisionRef]) -> RevisionRef {
        self.image(
            Kind::Assessment,
            json!({
                "status": "active", "subject_ref": subject, "evidence_refs": evidence,
                "relation": "supports", "rival_expectations": [],
                "rationale": "The cited passages state the value.", "assumption_refs": [],
            }),
            None,
        )
    }

    fn trace(&mut self, observation: RevisionRef, origin: Value, upstream: &str) -> RevisionRef {
        let observation = self.state.current(observation.id).unwrap().reference();
        self.create(
            "origin.trace",
            json!({"observation_ref": observation, "origin": origin,
                "dimensions": {"upstream_source": upstream}, "rationale": "Traced by reading."}),
        )
    }

    fn current(&self, reference: RevisionRef) -> RevisionRef {
        self.state.current(reference.id).unwrap().reference()
    }

    /// A validated Conclusion image over `support`.
    fn validated(&mut self, support: &[RevisionRef], extra: Value) -> RevisionRef {
        let mut body = json!({
            "status": "validated", "statement": "The window is 48 frames.",
            "scope": "Edition 3.1.", "significance": "Sets the limit.",
            "support_refs": support, "contradiction_refs": [], "caveats": [],
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        self.image(Kind::Conclusion, body, None)
    }

    fn review(
        &mut self,
        subject: RevisionRef,
        read_set: &[RevisionRef],
        isolation: &str,
        actor: (&str, Option<&str>),
    ) -> RevisionRef {
        self.image_as(
            Kind::Review,
            json!({
                "kind": "validation", "status": "submitted", "subject_ref": subject,
                "basis_digest": DIGEST, "read_set": read_set, "materials_examined": [],
                "method": "Reread the cited passages.", "shared_dependencies": [], "objection_refs": [],
                "verdict": "supports", "caveats": [], "context_isolation": isolation,
                "context": {"saw_original_conclusion": {"state": "known", "value": true},
                    "gathered_new_evidence": {"state": "known", "value": false},
                    "method_or_source_changes": [], "activity": "Reread.",
                    "dependencies": "shared", "dependence_basis": "The same documents."},
            }),
            None,
            Some(actor),
        )
    }
}

fn findings<'a>(response: &'a AuditResponse, check_id: &str) -> Vec<&'a Finding> {
    response
        .checks
        .iter()
        .filter(|finding| finding.check_id == check_id)
        .collect()
}

fn only<'a>(response: &'a AuditResponse, check_id: &str) -> &'a Finding {
    let found = findings(response, check_id);
    assert_eq!(found.len(), 1, "{check_id}: {:#?}", response.checks);
    found[0]
}

fn facts(finding: &Finding) -> Vec<&str> {
    finding
        .facts
        .iter()
        .map(|fact| fact.fact.as_str())
        .collect()
}

fn fact_refs<'a>(finding: &'a Finding, name: &str) -> &'a [RevisionRef] {
    &finding
        .facts
        .iter()
        .find(|fact| fact.fact == name)
        .unwrap_or_else(|| panic!("no {name} in {finding:#?}"))
        .refs
}

fn ids(refs: &[RevisionRef]) -> Vec<Id> {
    refs.iter().map(|reference| reference.id).collect()
}

/// Three documents restate one specification value; a fourth passage in the
/// article is the author's own bench test.
struct Origins {
    spec: RevisionRef,
    article: RevisionRef,
    blog: RevisionRef,
    bench: RevisionRef,
    erratum: RevisionRef,
}

fn origins(case: &mut Case) -> Origins {
    let (_, spec_doc) = case.document(json!({}));
    let (_, article_doc) = case.document(json!({}));
    let (_, blog_doc) = case.document(json!({}));
    let (_, erratum_doc) = case.document(json!({}));
    let spec = case.quote(spec_doc, "verified");
    let article = case.quote(article_doc, "verified");
    let blog = case.quote(blog_doc, "verified");
    let bench = case.quote(article_doc, "verified");
    let erratum = case.quote(erratum_doc, "verified");
    case.trace(spec, json!({"kind": "original"}), "specification");
    let spec_now = case.current(spec);
    case.trace(
        article,
        json!({"kind": "restates", "observation_ref": spec_now}),
        "specification",
    );
    let article_now = case.current(article);
    case.trace(
        blog,
        json!({"kind": "restates", "observation_ref": article_now}),
        "specification",
    );
    case.trace(bench, json!({"kind": "original"}), "article bench test");
    case.trace(erratum, json!({"kind": "original"}), "erratum");
    Origins {
        spec: case.current(spec),
        article: case.current(article),
        blog: case.current(blog),
        bench: case.current(bench),
        erratum: case.current(erratum),
    }
}

#[test]
fn three_citations_of_one_origin_are_reported_and_the_bench_test_is_not() {
    let mut case = Case::new();
    let o = origins(&mut case);
    let hypothesis = case.hypothesis();
    let prior = case.assessment(hypothesis, &[o.spec, o.article, o.blog], json!({}));
    // The bench test with the erratum, judged independent on stated dimensions.
    case.create(
        "origin.judge_dependence",
        json!({"roots": [o.bench, o.erratum], "judgment": "independent_on_stated_dimensions",
            "dimensions": ["method", "upstream_source"], "basis": "A correction and a bench run."}),
    );
    let bench = case.current(o.bench);
    let erratum = case.current(o.erratum);
    let independent = case.assessment(hypothesis, &[bench, erratum], json!({}));
    // An unjudged pair of distinct origins: unknown, not reported.
    let unknown = case.assessment(hypothesis, &[bench, case.current(o.spec)], json!({}));

    let response = case.audit();
    let finding = only(&response, "origin.shared_origin_citations");
    assert_eq!(finding.subject_refs, vec![prior]);
    assert_eq!(
        finding.gap_id,
        format!("blindspot:origin.shared_origin_citations:{}", prior.id)
    );
    let shared = fact_refs(finding, "shared_origin:one_known_root");
    for observation in [o.spec, o.article, o.blog] {
        assert!(ids(shared).contains(&observation.id), "{finding:#?}");
    }
    let all: Vec<Id> = finding
        .related_refs
        .iter()
        .chain(finding.facts.iter().flat_map(|fact| &fact.refs))
        .map(|reference| reference.id)
        .collect();
    assert!(!all.contains(&o.bench.id), "{finding:#?}");
    let listed = facts(finding);
    for expected in [
        "acquisitions_of_one_origin:3",
        "known_origin_count:1",
        "acquisition_count:3",
        "independent_pairs:0",
        "cited_field:evidence_refs",
    ] {
        assert!(listed.contains(&expected), "{expected}: {listed:?}");
    }
    for clean in [independent, unknown] {
        assert!(
            !response
                .checks
                .iter()
                .any(|finding| finding.subject_refs.contains(&clean)),
            "{:#?}",
            response.checks
        );
    }

    // A recorded shared judgment between distinct roots is common provenance too.
    case.create(
        "origin.judge_dependence",
        json!({"roots": [case.current(o.bench), case.current(o.spec)], "judgment": "shared",
            "dimensions": [], "basis": "The bench run used the specification's harness."}),
    );
    let response = case.audit();
    let found = findings(&response, "origin.shared_origin_citations");
    let subjects: Vec<Id> = found
        .iter()
        .flat_map(|finding| ids(&finding.subject_refs))
        .collect();
    assert!(subjects.contains(&unknown.id), "{found:#?}");
    let pair = found
        .iter()
        .find(|finding| finding.subject_refs[0].id == unknown.id)
        .unwrap();
    assert!(
        facts(pair).contains(&"dependence_judged:shared"),
        "{pair:#?}"
    );
}

#[test]
fn a_non_diagnostic_reading_of_one_origin_is_not_corroboration_and_is_not_reported() {
    let mut case = Case::new();
    let o = origins(&mut case);
    let hypothesis = case.hypothesis();
    case.assessment(
        hypothesis,
        &[o.spec, o.article, o.blog],
        json!({"relation": "non_diagnostic"}),
    );
    assert!(findings(&case.audit(), "origin.shared_origin_citations").is_empty());
}

#[test]
fn a_promoted_record_on_unverified_quotes_unsourced_claims_or_unverified_references_is_reported() {
    let mut case = Case::new();
    let (_, document) = case.document(json!({}));
    let verified = case.quote(document, "verified");
    let ambiguous = case.quote(document, "ambiguous");
    let unchecked = case.quote(document, "unverified");
    let unsourced = case.assertion(None);
    let run = case.run(Value::Null);
    let attributed = case.assertion(Some(run));
    let (_, reference) = case.document(json!({
        "availability": "external", "integrity": "not_checked",
        "external": {"locator": "register://entry/4", "digest_basis": "declared"},
    }));
    let referenced = case.quote(reference, "unavailable");
    let hypothesis = case.hypothesis();
    // The write gate refuses support from unverified or unattributed
    // readings; the audit reads whatever state the records reached.
    let assessment = case.assessment_image(
        hypothesis,
        &[
            verified, ambiguous, unchecked, unsourced, attributed, referenced,
        ],
    );
    let candidate_like = case.validated(&[assessment], json!({"status": "candidate"}));
    let conclusion = case.validated(&[assessment], json!({}));

    let response = case.audit();
    let quotes = only(&response, "quote.promoted_basis_unverified");
    assert_eq!(quotes.subject_refs, vec![conclusion]);
    assert_eq!(fact_refs(quotes, "text_quote:ambiguous"), [ambiguous]);
    assert_eq!(fact_refs(quotes, "text_quote:unverified"), [unchecked]);
    assert_eq!(fact_refs(quotes, "text_quote:unavailable"), [referenced]);
    assert!(!ids(&quotes.related_refs).contains(&verified.id));

    let claims = only(&response, "source.claim_without_selector_or_attribution");
    assert_eq!(claims.subject_refs, vec![conclusion]);
    assert_eq!(
        fact_refs(claims, "observation_basis:assertion_without_run"),
        [unsourced]
    );

    let retention = only(&response, "source.reference_only_unverified");
    assert_eq!(retention.subject_refs, vec![conclusion]);
    let mut expected = vec![reference, referenced];
    expected.sort_by_key(|reference| reference.id);
    assert_eq!(
        fact_refs(retention, "retention:external_unverified"),
        expected
    );
    // Only promoted records are audited for what they rest on.
    for finding in &response.checks {
        assert!(!finding.subject_refs.contains(&candidate_like));
    }

    // A recorded reverification clears the retention finding.
    let mut body = case.body(reference);
    body["external"]["verifications"] = json!([{
        "outcome": "verified", "method": "supplied_bytes", "observed_digest": DIGEST,
        "observed_size": 64, "verified_at": "2026-11-01T00:00:00Z", "verified_by": "agent:test",
        "command_id": "reverify-1",
    }]);
    case.image(Kind::Artifact, body, Some(reference));
    assert!(findings(&case.audit(), "source.reference_only_unverified").is_empty());
}

#[test]
fn a_conclusion_reviewed_only_by_same_session_self_review_is_low_independence() {
    let mut case = Case::new();
    let (_, document) = case.document(json!({}));
    let quote = case.quote(document, "verified");
    let hypothesis = case.hypothesis();
    let assessment = case.assessment(hypothesis, &[quote], json!({}));
    let conclusion = case.validated(&[assessment], json!({}));
    let review = case.review(
        conclusion,
        &[assessment, quote],
        "same_session_self_review",
        ("agent:author", Some("family-a")),
    );

    let response = case.audit();
    let finding = only(&response, "review.promoted_self_review_only");
    assert_eq!(finding.subject_refs, vec![conclusion]);
    assert_eq!(
        fact_refs(finding, "low_independence:same_session_self_review"),
        [review]
    );
    assert!(findings(&response, "review.promoted_single_dependence_group").is_empty());

    // A second same-family review in a fresh context is no longer self-review
    // only, but both reviews are one dependence group.
    let second = case.review(
        conclusion,
        &[assessment, quote],
        "fresh_context_blinded",
        ("agent:reviewer", Some("Family-A")),
    );
    let response = case.audit();
    assert!(findings(&response, "review.promoted_self_review_only").is_empty());
    let group = only(&response, "review.promoted_single_dependence_group");
    assert_eq!(group.subject_refs, vec![conclusion]);
    let mut both = vec![review, second];
    both.sort_by_key(|reference| reference.id);
    assert_eq!(
        fact_refs(group, "dependence_group_basis:model_family"),
        both
    );
    assert!(facts(group).contains(&"review_count:2"));

    // A review from another declared family forms a second group.
    case.review(
        conclusion,
        &[assessment, quote],
        "fresh_context_blinded",
        ("agent:third", Some("family-b")),
    );
    assert!(findings(&case.audit(), "review.promoted_single_dependence_group").is_empty());
}

#[test]
fn a_withdrawn_source_reports_the_unpromoted_judgment_and_the_review_that_read_it() {
    let mut case = Case::new();
    let (source, document) = case.document(json!({}));
    let quote = case.quote(document, "verified");
    let hypothesis = case.hypothesis();
    let assessment = case.assessment(hypothesis, &[quote], json!({}));
    let conclusion = case.validated(&[assessment], json!({"status": "candidate"}));
    let review = case.review(
        conclusion,
        &[assessment, quote],
        "fresh_context_unblinded",
        ("agent:reviewer", None),
    );
    assert!(findings(&case.audit(), "source.invalidated_basis_unreviewed").is_empty());

    let mut body = case.body(source);
    body["status"] = json!("withdrawn");
    let withdrawn = case.image(Kind::Source, body, Some(source));
    let response = case.audit();
    let judged = findings(&response, "source.invalidated_basis_unreviewed");
    let subjects: Vec<Id> = judged
        .iter()
        .flat_map(|finding| ids(&finding.subject_refs))
        .collect();
    assert!(subjects.contains(&assessment.id), "{judged:#?}");
    assert!(subjects.contains(&conclusion.id), "{judged:#?}");
    let finding = judged
        .iter()
        .find(|finding| finding.subject_refs == [assessment])
        .unwrap();
    assert_eq!(
        fact_refs(finding, "freshness_trigger:withdrawn"),
        [withdrawn]
    );
    assert!(facts(finding).contains(&"freshness:needs_review"));

    let stale = only(&response, "review.source_invalidated");
    assert_eq!(stale.subject_refs, vec![review]);
    assert_eq!(fact_refs(stale, "freshness_trigger:withdrawn"), [withdrawn]);
}

fn coverage(hits: u64) -> Value {
    json!({
        "corpus": "Errata register", "revision_bounds": "As listed on the search date",
        "queries": ["edition 3.1"], "filters": [], "inclusion_rules": [], "exclusion_rules": [],
        "total_hits": hits, "examined_hits": hits, "empty_hits": null,
        "query_results": [{"query": "edition 3.1", "hits": hits}],
    })
}

#[test]
fn absence_claims_report_a_missing_search_log_and_other_unmet_requirements_separately() {
    let mut case = Case::new();
    let hypothesis = case.hypothesis();
    // A supporting assessment cites at least one valid, verified reading.
    let (_, document) = case.document(json!({}));
    let quote = case.quote(document, "verified");
    let searched = case.run(coverage(0));
    let unbounded = case.run(Value::Null);
    // An empty, bounded search, but no expected signal.
    let empty_search = case.assessment(
        hypothesis,
        &[quote],
        json!({"absence": {"opportunity": {"description": "Register search", "refs": [searched]}}}),
    );
    // A Run with no recorded coverage and no adequacy standard.
    let no_log = case.assessment(
        hypothesis,
        &[quote],
        json!({"absence": {"expected_signal": "An erratum entry for the edition.",
            "opportunity": {"description": "Looked around", "refs": [unbounded]}}}),
    );
    // A sampled absence states its own adequacy standard; no search is expected.
    let sample = case.image(
        Kind::Observation,
        json!({
            "status": "active", "value": {"kind": "measurement", "value": "0", "unit": "stalls"},
            "basis": {"kind": "source", "artifact_ref": document,
                "selector": {"schema_version": "inv.selector.v1", "artifact_digest": DIGEST,
                    "selector": {"type": "whole"}, "validation": "verified"},
                "text_quote": {"state": "not_applicable"}},
            "extraction_method": "Counted stalls.", "validity": "valid", "quality": [],
            "limitations": [], "evidence_kind": "observational", "variables_changed": [],
            "observed_at": null, "run_ref": null,
            "measurement": {"sample_count": {"state": "known", "value": 5}},
        }),
        None,
    );
    let sampled = case.assessment(
        hypothesis,
        &[sample],
        json!({"absence": {"expected_signal": "A stall.", "justified_equivalent": "Same harness.",
            "opportunity": {"description": "Runs", "refs": [sample], "minimum_sample_count": 1}}}),
    );
    let claim = case.validated(
        &[empty_search],
        json!({"status": "candidate", "claim_form": "absence"}),
    );
    let bare = case.validated(&[], json!({"status": "candidate", "claim_form": "absence"}));

    let response = case.audit();
    let logs = findings(&response, "source.absence_without_search_log");
    let subjects: Vec<Id> = logs
        .iter()
        .flat_map(|finding| ids(&finding.subject_refs))
        .collect();
    assert_eq!(subjects, vec![no_log.id], "{logs:#?}");
    let missing_log = logs[0];
    assert!(facts(missing_log).contains(&"search_log:none_linked"));
    assert!(facts(missing_log).contains(&"absence_gate:absence.adequacy_standard_missing"));

    let incomplete = findings(&response, "source.absence_basis_incomplete");
    let by_subject = |id: Id| {
        incomplete
            .iter()
            .find(|finding| finding.subject_refs[0].id == id)
            .copied()
    };
    let gated = by_subject(empty_search.id).expect("empty search without a signal");
    assert!(facts(gated).contains(&"absence_gate:absence.expected_signal_missing"));
    assert!(by_subject(no_log.id).is_none(), "{incomplete:#?}");
    let concluded = by_subject(claim.id).expect("the absence conclusion");
    assert!(facts(concluded).contains(&"absence_gate:absence.expected_signal_missing"));
    let bare = by_subject(bare.id).expect("an absence conclusion without a basis");
    assert_eq!(facts(bare), ["absence_gate:absence.claim_without_basis"]);
    for finding in &response.checks {
        assert!(
            !finding.subject_refs.contains(&sampled) || finding.check_id.starts_with("origin."),
            "{finding:#?}"
        );
    }
    // A search that found hits is not an empty result.
    let hits = case.run(coverage(2));
    let with_hits = case.assessment(
        hypothesis,
        &[quote],
        json!({"absence": {"expected_signal": "An erratum entry.",
            "opportunity": {"description": "Register search", "refs": [hits]}}}),
    );
    let response = case.audit();
    assert!(
        findings(&response, "source.absence_without_search_log")
            .iter()
            .any(|finding| finding.subject_refs == [with_hits])
    );
}
