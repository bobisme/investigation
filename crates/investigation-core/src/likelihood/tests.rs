use serde_json::json;

use super::*;
use crate::review::Probability;

fn term(name: &str) -> LikelihoodEstimate {
    serde_json::from_value(json!(name)).unwrap()
}

#[test]
fn terms_map_to_published_range_midpoints_with_a_mapping_version() {
    let expected = [
        ("almost_no_chance", 0.03),
        ("very_unlikely", 0.125),
        ("unlikely", 0.325),
        ("roughly_even_chance", 0.5),
        ("likely", 0.675),
        ("very_likely", 0.875),
        ("almost_certain", 0.97),
    ];
    for (name, probability) in expected {
        assert_eq!(
            scored_probability(term(name)),
            (probability, Some(MAPPING_VERSION)),
            "{name}"
        );
    }
    let number = LikelihoodEstimate::Number(Probability::try_from(0.42).unwrap());
    assert_eq!(scored_probability(number), (0.42, None));
}

#[test]
fn brier_is_squared_error_against_the_outcome_and_annulled_is_unscored() {
    assert_eq!(brier(0.9, ClaimOutcome::Holds), Some(0.01));
    assert_eq!(brier(0.9, ClaimOutcome::DoesNotHold), Some(0.81));
    assert_eq!(brier(0.675, ClaimOutcome::Holds), Some(0.105625));
    assert_eq!(brier(0.0, ClaimOutcome::Holds), Some(1.0));
    assert_eq!(brier(1.0, ClaimOutcome::Holds), Some(0.0));
    assert_eq!(brier(0.3, ClaimOutcome::Annulled), None);
}

fn forecast(group: &str, version: Option<&str>, p: f64, outcome: ClaimOutcome) -> ResolvedForecast {
    ResolvedForecast {
        forecast_ref: RevisionRef {
            id: "assess_019acd16-0000-7000-8000-000000000001"
                .parse()
                .unwrap(),
            revision: 1,
        },
        dependence_group: group.to_owned(),
        basis: if group.starts_with("model_family:") {
            GroupBasis::ModelFamily
        } else {
            GroupBasis::ActorFamilyUndeclared
        },
        model_version: version.map(str::to_owned),
        outcome,
        scored_probability: p,
        brier: brier(p, outcome),
    }
}

#[test]
fn calibration_shows_a_mean_only_from_the_minimum_and_lists_groups_by_key() {
    let mut forecasts = Vec::new();
    for index in 0..MIN_SCORED_FOR_MEAN {
        let outcome = if index % 2 == 0 {
            ClaimOutcome::Holds
        } else {
            ClaimOutcome::DoesNotHold
        };
        forecasts.push(forecast("model_family:zeta", Some("2"), 0.8, outcome));
    }
    forecasts.push(forecast(
        "model_family:zeta",
        None,
        0.5,
        ClaimOutcome::Annulled,
    ));
    forecasts.push(forecast(
        "model_family:alpha",
        Some("1"),
        0.9,
        ClaimOutcome::Holds,
    ));
    forecasts.push(forecast("actor:agent:x", None, 0.1, ClaimOutcome::Holds));

    let view = calibration(&forecasts, 3, 1);
    let keys: Vec<_> = view
        .groups
        .iter()
        .map(|group| group.dependence_group.as_str())
        .collect();
    assert_eq!(
        keys,
        ["actor:agent:x", "model_family:alpha", "model_family:zeta"]
    );
    assert_eq!(
        (view.scored, view.annulled, view.unresolved, view.withheld),
        (12, 1, 3, 1)
    );

    let alpha = &view.groups[1];
    assert_eq!(alpha.scored, 1);
    assert_eq!(alpha.score_state, ScoreState::BelowMinimum);
    assert_eq!(alpha.mean_brier, None);

    // Five 0.04 and five 0.64: mean 0.34.
    let zeta = &view.groups[2];
    assert_eq!((zeta.scored, zeta.annulled), (10, 1));
    assert_eq!(zeta.score_state, ScoreState::Shown);
    assert_eq!(zeta.mean_brier, Some(0.34));
    assert_eq!(zeta.versions.len(), 2);
    assert_eq!(zeta.versions[0].model_version, None);
    assert_eq!(zeta.versions[0].scored, 0);
    assert_eq!(zeta.versions[1].mean_brier, Some(0.34));
    // Actor groups carry no version breakdown.
    assert!(view.groups[0].versions.is_empty());

    let text = calibration_text(&view);
    assert!(
        text.contains("model_family:alpha: n=1, no score below 10"),
        "{text}"
    );
    assert!(
        text.contains("model_family:zeta: n=10, mean Brier 0.34"),
        "{text}"
    );
    assert!(!text.to_lowercase().contains("confidence"), "{text}");
}

#[test]
fn a_likelihood_without_a_resolution_condition_is_explained_with_confidence() {
    let original = Failure::new(ErrorCode::InvalidInput, "Field does not match");
    for (operation, payload, pointer) in [
        (
            "conclusion.create",
            json!({"likelihood": 0.7}),
            "/likelihood",
        ),
        (
            "assessment.add",
            json!({"likelihood": {"estimate": "likely"}}),
            "/likelihood",
        ),
        (
            "conclusion.amend",
            json!({"replacement": {"likelihood": {"estimate": 0.4, "resolution_condition": " "}}}),
            "/replacement/likelihood",
        ),
        (
            "likelihood.set",
            json!({"likelihood": {"estimate": "unlikely"}}),
            "/likelihood",
        ),
    ] {
        let explained = explain_decode_failure(operation, &payload, original.clone());
        assert_eq!(explained.error.code, ErrorCode::InvalidInput);
        assert!(
            explained
                .error
                .message
                .contains("stated resolution condition")
                && explained.error.message.contains("state confidence instead"),
            "{operation}"
        );
        assert_eq!(
            explained.error.path.as_deref(),
            Some(format!("{pointer}/resolution_condition").as_str())
        );
        assert!(explained.error.expected.unwrap()["or_confidence"]["level"].is_string());
    }
    // A stated condition, a null likelihood or another operation pass through.
    for (operation, payload) in [
        (
            "conclusion.create",
            json!({"likelihood": {"estimate": 0.7, "resolution_condition": "The rerun"}}),
        ),
        ("conclusion.create", json!({"likelihood": null})),
        ("case.create", json!({"likelihood": 0.7})),
    ] {
        let passed = explain_decode_failure(operation, &payload, original.clone());
        assert_eq!(passed.error.message, original.error.message);
    }
}
