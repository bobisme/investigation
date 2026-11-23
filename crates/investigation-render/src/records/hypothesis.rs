//! Hypothesis notes: standing, rival explanations, and the assessments that
//! contradict, support or fail to discriminate it, contradicting data first.

use std::fmt::Write as _;

use investigation_core::{
    identity::{Id, Kind},
    state::Record,
};

use super::{Facts, citing, naming, refs, status_note};
use crate::{
    RenderError,
    evidence::{MAX_LISTED, prose},
    note::text_at,
    text::inline_code,
};

/// Assessment relations in reading order: what contradicts comes first.
const RELATIONS: &[(&str, &str)] = &[
    ("contradicts", "Contradicting"),
    ("supports", "Supporting"),
    (
        "non_diagnostic",
        "Non-diagnostic: expected equally under the rivals",
    ),
    ("inconclusive", "Inconclusive"),
];

pub(super) fn sections(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.word("Lifecycle", "lifecycle");
    f.reference("Question", "question_ref")?;
    f.list(
        "Possible falsifiers",
        "possible_falsifiers",
        "none recorded: nothing stated would show this hypothesis wrong",
    );
    f.text_present("Rationale", "rationale");
    f.references("Assumptions", "assumption_refs", "none recorded")?;
    f.references("Evidence cited", "evidence_refs", "none recorded")?;

    rivals(f)?;
    assessments(f)?;

    f.heading("Conclusions naming it");
    let conclusions = citing(
        f.notes,
        f.record,
        &[
            "causal_factors.hypothesis_ref",
            "causal_test.factor_ref",
            "contradiction_refs",
            "support_refs",
        ],
    );
    let id = f.record.header.id.to_string();
    f.backlinks(
        &conclusions,
        "No conclusion names this hypothesis.",
        |_, conclusion| {
            let body = &conclusion.body;
            let role = body["causal_factors"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|factor| text_at(factor, "hypothesis_ref.id") == Some(id.as_str()))
                .and_then(|factor| factor["role"].as_str());
            Ok(if let Some(role) = role {
                format!(
                    ": as causal factor, role {}",
                    inline_code(&super::wire(role))
                )
            } else if refs(body.get("contradiction_refs"))
                .iter()
                .any(|reference| reference.id.to_string() == id)
            {
                ": as a contradicting record".to_owned()
            } else {
                String::new()
            })
        },
    )
}

/// Rivals: recorded rival and alternative references both ways, and the
/// other hypotheses on the same question.
fn rivals(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.heading("Rival explanations");
    let state = f.notes.case().state();
    let this = f.record.header.id;
    let mut candidates: Vec<&Record> = Vec::new();
    for path in ["rival_refs", "alternative_to"] {
        candidates.extend(
            refs(f.record.body.get(path))
                .into_iter()
                .filter_map(|reference| state.current(reference.id)),
        );
    }
    candidates.extend(naming(
        f.notes,
        f.record,
        Kind::Hypothesis,
        &["rival_refs", "alternative_to"],
    ));
    if let Some(question) = text_at(&f.record.body, "question_ref.id") {
        candidates.extend(state.records().filter(|record| {
            record.header.id.kind() == Kind::Hypothesis
                && text_at(&record.body, "question_ref.id") == Some(question)
        }));
    }
    let mut rivals: Vec<&Record> = Vec::new();
    for candidate in candidates {
        if candidate.header.id != this
            && !rivals
                .iter()
                .any(|seen| seen.header.id == candidate.header.id)
        {
            rivals.push(candidate);
        }
    }
    if rivals.is_empty() {
        f.out.push_str(
            "No rival explanation is recorded. A hypothesis without a rival cannot be \
             discriminated by evidence.\n",
        );
        return Ok(());
    }
    for rival in rivals.iter().take(MAX_LISTED) {
        let _ = writeln!(
            f.out,
            "- {}{}",
            f.notes.link(rival.header.id)?,
            status_note(f.notes, rival)
        );
    }
    if rivals.len() > MAX_LISTED {
        let _ = writeln!(f.out, "- and {} more", rivals.len() - MAX_LISTED);
    }
    Ok(())
}

fn assessments(f: &mut Facts<'_, '_, '_>) -> Result<(), RenderError> {
    f.heading("Evidence assessed against it");
    let assessed = citing(f.notes, f.record, &["subject_ref"]);
    let assessed: Vec<_> = assessed
        .into_iter()
        .filter(|(record, _)| record.header.id.kind() == Kind::Assessment)
        .collect();
    let withheld = withheld_assessments(f);
    if assessed.is_empty() && withheld.is_empty() {
        f.out
            .push_str("No assessment relates any evidence to this hypothesis yet.\n");
    }
    for &(relation, label) in RELATIONS {
        let group: Vec<_> = assessed
            .iter()
            .filter(|(record, _)| {
                f.notes.visible(record) && text_at(&record.body, "relation") == Some(relation)
            })
            .copied()
            .collect();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(f.out, "{label}:\n");
        f.backlinks(&group, "", |notes, assessment| {
            let evidence: Vec<String> = refs(assessment.body.get("evidence_refs"))
                .into_iter()
                .take(MAX_LISTED)
                .map(|reference| notes.link(reference.id))
                .collect::<Result<_, _>>()?;
            Ok(format!(
                ": {}; evidence {}",
                prose(text_at(&assessment.body, "rationale").unwrap_or_default()),
                if evidence.is_empty() {
                    "none cited".to_owned()
                } else {
                    evidence.join(", ")
                }
            ))
        })?;
        f.out.push('\n');
    }
    // Withheld assessments may contradict it: they stay as ID-only
    // placeholders so filtering never makes the hypothesis read stronger.
    if !withheld.is_empty() {
        let links: Vec<String> = withheld
            .iter()
            .take(MAX_LISTED)
            .map(|id| f.notes.link(*id))
            .collect::<Result<_, _>>()?;
        let _ = write!(
            f.out,
            "{} further assessment{} withheld by the view policy: {}",
            withheld.len(),
            if withheld.len() == 1 { " is" } else { "s are" },
            links.join(", ")
        );
        if withheld.len() > MAX_LISTED {
            let _ = write!(f.out, " and {} more", withheld.len() - MAX_LISTED);
        }
        f.out.push_str(".\n");
    }
    let weighed = naming(
        f.notes,
        f.record,
        Kind::Assessment,
        &["rival_expectations.rival_ref"],
    );
    let weighed: Vec<&Record> = weighed
        .into_iter()
        .chain(
            citing(f.notes, f.record, &["rival_expectations.rival_ref"])
                .into_iter()
                .map(|(record, _)| record),
        )
        .collect();
    if !weighed.is_empty() {
        f.out
            .push_str("\nAssessments that weigh this hypothesis as a rival:\n\n");
        let id = f.record.header.id.to_string();
        for assessment in weighed.iter().take(MAX_LISTED) {
            let _ = write!(f.out, "- {}", f.notes.link(assessment.header.id)?);
            if f.notes.visible(assessment)
                && let Some(expectation) = assessment.body["rival_expectations"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|item| text_at(item, "rival_ref.id") == Some(id.as_str()))
            {
                let _ = write!(
                    f.out,
                    ": evidence {} under this hypothesis. {}",
                    inline_code(&super::wire(
                        expectation["expectation"].as_str().unwrap_or("unknown")
                    )),
                    prose(expectation["rationale"].as_str().unwrap_or_default())
                );
            }
            f.out.push('\n');
        }
    }
    Ok(())
}

/// Withheld assessments of this hypothesis that may appear as placeholders,
/// in record order, each once.
fn withheld_assessments(f: &Facts<'_, '_, '_>) -> Vec<Id> {
    let state = f.notes.case().state();
    let mut found: Vec<Id> = Vec::new();
    for citation in f.notes.citations(f.record.header.id) {
        if citation.by.kind() != Kind::Assessment
            || citation.field != "subject_ref"
            || found.contains(&citation.by)
        {
            continue;
        }
        if state
            .current(citation.by)
            .is_some_and(|assessment| f.notes.placeholder(assessment))
        {
            found.push(citation.by);
        }
    }
    found
}
