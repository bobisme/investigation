use serde_json::json;

use super::*;

fn context(extra: Value) -> ReviewContext {
    let mut value = json!({
        "saw_original_conclusion":{"state":"known","value":false},
        "gathered_new_evidence":{"state":"unknown"},
        "method_or_source_changes":[],"activity":"Judge the packet.",
        "dependencies":"unknown","dependence_basis":"Not compared."
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

fn declared() -> ReviewContext {
    context(json!({
        "work_kind":{"state":"unknown"},
        "prior_involvement":{"state":"known","value":[]},
        "read_other_reviews":{"state":"known","value":false}
    }))
}

fn reference(n: u8) -> RevisionRef {
    serde_json::from_value(json!({
        "id": format!("rev_019acd16-0000-7000-8000-00000000000{n}"), "revision": 1
    }))
    .unwrap()
}

fn entry(n: u8, sequence: u64, read_other: Declared<bool>) -> ReviewEntry {
    ReviewEntry {
        review_ref: reference(n),
        kind: "blindspot".into(),
        verdict: "gaps_identified".into(),
        subject_revision: 1,
        recorded_at_sequence: sequence,
        audited_sequence: None,
        recorded_by: "agent:reviewer".parse().unwrap(),
        actor_descriptor_self_reported: None,
        declarations: Declarations {
            context_isolation: ContextIsolation::FreshContextBlinded,
            saw_original_conclusion: Declared::Known(false),
            gathered_new_evidence: Declared::Unknown,
            work_kind: Declared::Undeclared,
            prior_involvement: Declared::Undeclared,
            read_other_reviews: read_other,
            dependencies: Dependence::Unknown,
        },
        undeclared: Vec::new(),
        low_independence: false,
        low_independence_reasons: Vec::new(),
        dependence_group: "actor:agent:reviewer".into(),
        reviews_read: Vec::new(),
    }
}

#[test]
fn recording_warns_but_a_complete_declaration_is_quiet() {
    assert!(submission_warnings(ContextIsolation::FreshContextBlinded, &declared()).is_empty());
    let warnings =
        submission_warnings(ContextIsolation::SameSessionSelfReview, &context(json!({})));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("low-independence"));
    assert!(warnings[1].contains("work_kind, prior_involvement, read_other_reviews"));
    // Explicit unknown is a declaration; it is not reported as missing.
    assert!(undeclared(&declared()).is_empty());
}

#[test]
fn declarations_keep_undeclared_unknown_and_known_apart() {
    assert_eq!(Declared::<bool>::from(None), Declared::Undeclared);
    assert_eq!(
        Declared::<bool>::from(Some(&Knowledge::Unknown)),
        Declared::Unknown
    );
    assert_eq!(
        Declared::from(Some(&Knowledge::Known(true))),
        Declared::Known(true)
    );
    assert_eq!(
        serde_json::to_value(Declared::<bool>::Undeclared).unwrap(),
        json!({"state":"undeclared"})
    );
}

#[test]
fn only_an_earlier_audited_basis_without_a_read_shows_independent_recording() {
    let earlier = entry(1, 10, Declared::Undeclared);

    // A recorded read wins over everything else.
    let mut later = entry(2, 12, Declared::Known(false));
    later.audited_sequence = Some(5);
    later.reviews_read = vec![earlier.review_ref];
    assert_eq!(order(&earlier, &later), PairOrder::LaterReadEarlier);

    // A declared read of other reviews also wins over an earlier basis.
    later.reviews_read.clear();
    later.declarations.read_other_reviews = Declared::Known(true);
    assert_eq!(
        order(&earlier, &later),
        PairOrder::LaterDeclaredReadingReviews
    );

    later.declarations.read_other_reviews = Declared::Unknown;
    assert_eq!(
        order(&earlier, &later),
        PairOrder::LaterBasisPredatesEarlier
    );

    // A basis at or after the earlier review shows nothing; a declaration
    // that no review was read is reported, not trusted.
    later.audited_sequence = Some(10);
    assert_eq!(
        order(&earlier, &later),
        PairOrder::LaterCouldHaveReadEarlier
    );
    later.declarations.read_other_reviews = Declared::Known(false);
    assert_eq!(order(&earlier, &later), PairOrder::LaterDeclaredNoRead);

    let together = entry(3, 10, Declared::Undeclared);
    assert_eq!(order(&earlier, &together), PairOrder::RecordedTogether);
}
