//! Positive inclusion and caveats for exports (`inv.export-policy.v2`). Pure.
//!
//! Every decision is made once, when [`Inclusion::new`] runs, on the full
//! pinned snapshot, and every channel of every kind asks the same answers.
//! Two layers decide what an audience gets:
//!
//! 1. **Classification.** A record is a candidate only when its effective
//!    classification permits the audience both at the pinned snapshot and
//!    today ([`Exclusion`] reasons `audience` and `audience_today`). Today's
//!    classification governs an earlier snapshot too, so a record raised
//!    since then stays out of a re-export, and a record withheld at the
//!    pinned snapshot is never shown because it was cleared later. The
//!    strict policy keeps no ID-only placeholders.
//! 2. **Dependent records.** Filtering must not make a record read stronger
//!    than it is, so each candidate's bearing material is computed on the
//!    full pinned snapshot before anything is filtered:
//!    - its *challenges*: open objections naming it, reviews and assessments
//!      of it, and, for a statement (Conclusion, Decision, Assessment,
//!      Comparison), open objections naming any record it rests on;
//!    - for a statement, its *basis*: the exact dependency closure a
//!      reviewer must read (`review_ops::required_reads`).
//!
//!    When any of that material is withheld, the record is shown only with
//!    the fixed, compiled [`Caveat`]s, and only to an audience for which
//!    [`caveats_approved`] holds. Otherwise the record itself is withheld
//!    (`challenge_withheld`, `basis_withheld`), and so, in turn, is every
//!    record that rests on or is challenged by it: the layer is iterated to
//!    a fixed point. A statement whose closure cannot be walked is withheld
//!    (`basis_unverifiable`) whenever anything is withheld at all, since a
//!    caveat could not say truthfully what it rests on.
//!
//! What each audience gets:
//!
//! | Audience | Gets | Caveats |
//! |---|---|---|
//! | `restricted` | every record | none needed: nothing is above it |
//! | `internal` | records at `internal` or below | approved: a record bearing on withheld material carries the fixed caveat |
//! | `public` | records at `public` | not approved: a record bearing on withheld material is withheld too |
//!
//! A caveat names nothing: no ID, title, kind, count, level or digest of the
//! withheld material, only that some exists and whether it challenges the
//! record or is part of what the record rests on. That the internal audience
//! may learn this much follows from the working vault, which already shows
//! it the withheld records as ID-only placeholders. Nothing grants the public
//! audience knowledge that a given public record has non-public material
//! bearing on it, so policy v2 shows the public only records whose
//! challenges and basis are public as well. Whether anything at all was
//! withheld is stated by the manifest's `completeness`; exact exclusions and
//! caveated records go only to the publisher's receipt.

use std::collections::{BTreeMap, BTreeSet};

use investigation_core::{
    classification::{self, AudiencePolicy, Classifier, Field},
    command::RevisionRef,
    identity::{Id, Kind},
    review_ops,
    state::{Record, State},
};
use serde::Serialize;
use serde_json::Value;

use crate::Audience;

/// The fixed statement a kind shows beside a record that bears on withheld
/// material. Its text is compiled into the binary and names nothing about
/// what was withheld.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Caveat {
    /// Something that challenges or reviews the record (an open objection,
    /// a review or an assessment, or an open objection to what it rests on)
    /// is withheld. Its effective state was computed with that material.
    ChallengeWithheld,
    /// Part of what the record rests on is withheld.
    BasisWithheld,
}

impl Caveat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChallengeWithheld => "challenge_withheld",
            Self::BasisWithheld => "basis_withheld",
        }
    }

    /// The approved representation shown to the audience, word for word.
    pub const fn text(self) -> &'static str {
        match self {
            Self::ChallengeWithheld => {
                "Material that challenges or reviews this record is withheld from this \
                 audience. Its effective state was computed with that material, so it does \
                 not read stronger than it is."
            }
            Self::BasisWithheld => {
                "Part of what this record rests on is withheld from this audience and is not \
                 shown here."
            }
        }
    }
}

/// Whether this audience may be told, by a [`Caveat`], that withheld
/// material bears on a record it sees. When it may not, such a record is
/// withheld instead.
pub const fn caveats_approved(audience: Audience) -> bool {
    !matches!(audience, Audience::Public)
}

/// One sentence stating the dependent-record rule for `audience`, the same
/// for every export of that audience.
pub const fn dependent_rule(audience: Audience) -> &'static str {
    if caveats_approved(audience) {
        "A record that rests on, or is challenged by, material withheld from this audience \
         carries a fixed caveat saying so; the material is not named, counted or described."
    } else {
        "A record that rests on, or is challenged by, material withheld from this audience is \
         withheld as well, so nothing shown depends on what is not shown."
    }
}

/// One record the audience policy withheld. For the publisher only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exclusion {
    /// The record; its prefix names its kind.
    pub id: Id,
    /// Why it was withheld: `audience` (its effective classification is
    /// above the audience at the pinned snapshot), `audience_today` (only
    /// today's classification withholds it), `challenge_withheld` or
    /// `basis_withheld` (material bearing on it is withheld and this
    /// audience gets no caveat), or `basis_unverifiable` (its dependency
    /// closure could not be walked while something is withheld).
    pub reason: &'static str,
}

/// One record shown with caveats. For the publisher only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Caveated {
    pub id: Id,
    pub caveats: Vec<Caveat>,
}

/// Material bearing on one candidate record, computed on the full pinned
/// snapshot.
struct Bearing {
    challenges: BTreeSet<Id>,
    /// The dependency closure; `None` when it could not be walked.
    basis: Option<BTreeSet<Id>>,
}

/// The inclusion and caveat decisions of one export.
pub struct Inclusion<'a> {
    policy: AudiencePolicy,
    pinned: Classifier<'a>,
    /// Today's state when the export pins an earlier snapshot.
    today: Option<Classifier<'a>>,
    /// The pinned and today's states, for byte availability.
    states: (&'a State, Option<&'a State>),
    /// Every withheld record of the pinned state with its reason.
    withheld: BTreeMap<Id, &'static str>,
    /// Caveats of shown records that have any.
    caveats: BTreeMap<Id, Vec<Caveat>>,
}

impl<'a> Inclusion<'a> {
    /// Decide inclusion and caveats for every record of `pinned`.
    pub fn new(pinned: &'a State, today: Option<&'a State>, audience: Audience) -> Self {
        let level = match audience {
            Audience::Public => classification::Audience::Public,
            Audience::Internal => classification::Audience::Internal,
            Audience::Restricted => classification::Audience::Restricted,
        };
        let mut inclusion = Self {
            policy: AudiencePolicy::strict(level),
            pinned: Classifier::new(pinned),
            today: today.map(Classifier::new),
            states: (pinned, today),
            withheld: BTreeMap::new(),
            caveats: BTreeMap::new(),
        };
        for record in pinned.records() {
            if !inclusion.classified(record, Field::Summary) {
                let reason = if inclusion
                    .pinned
                    .may_show(record, Field::Summary, &inclusion.policy)
                {
                    "audience_today"
                } else {
                    "audience"
                };
                inclusion.withheld.insert(record.header.id, reason);
            }
        }
        if !inclusion.withheld.is_empty() {
            inclusion.decide_dependents(pinned, caveats_approved(audience));
        }
        inclusion
    }

    /// Whether bytes may no longer be read through Artifact `id`: its
    /// availability was withdrawn at the pinned snapshot or since. An export
    /// copies no bytes through such a reference and reports its retention as
    /// `intentionally_removed`.
    pub fn availability_withdrawn(&self, id: Id) -> bool {
        investigation_core::retention_ops::bytes_withdrawn(self.states.0, self.states.1, id)
    }

    /// The dependent-record layer, iterated until no further record is
    /// withheld, then the caveats of what remains.
    fn decide_dependents(&mut self, state: &State, approved: bool) {
        let challenged_by = challenges_by_subject(state);
        let bearings: BTreeMap<Id, Bearing> = state
            .records()
            .filter(|record| !self.withheld.contains_key(&record.header.id))
            .map(|record| (record.header.id, bearing(state, record, &challenged_by)))
            .collect();
        loop {
            let mut changed = false;
            for (id, bearing) in &bearings {
                if self.withheld.contains_key(id) {
                    continue;
                }
                let reason = if bearing.basis.is_none() {
                    Some("basis_unverifiable")
                } else if approved {
                    None
                } else {
                    self.caveats_of(bearing)
                        .first()
                        .map(|caveat| caveat.as_str())
                };
                if let Some(reason) = reason {
                    self.withheld.insert(*id, reason);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (id, bearing) in &bearings {
            if self.withheld.contains_key(id) {
                continue;
            }
            let caveats = self.caveats_of(bearing);
            if !caveats.is_empty() {
                self.caveats.insert(*id, caveats);
            }
        }
    }

    fn caveats_of(&self, bearing: &Bearing) -> Vec<Caveat> {
        let withheld = |ids: &BTreeSet<Id>| ids.iter().any(|id| self.withheld.contains_key(id));
        let mut caveats = Vec::new();
        if withheld(&bearing.challenges) {
            caveats.push(Caveat::ChallengeWithheld);
        }
        if bearing.basis.as_ref().is_some_and(withheld) {
            caveats.push(Caveat::BasisWithheld);
        }
        caveats
    }

    /// The classification layer alone for one channel.
    fn classified(&self, record: &Record, field: Field) -> bool {
        self.pinned.may_show(record, field, &self.policy)
            && self.today.as_ref().is_none_or(|today| {
                today
                    .state()
                    .current(record.header.id)
                    .is_some_and(|current| today.may_show(current, field, &self.policy))
            })
    }

    /// Whether the record's title, summary and content may appear. A shown
    /// record may still need its [`Self::caveats`] beside it.
    pub fn includes(&self, record: &Record) -> bool {
        self.may_show(record, Field::Summary)
    }

    /// Whether one channel of the record may appear.
    pub fn may_show(&self, record: &Record, field: Field) -> bool {
        !self.withheld.contains_key(&record.header.id) && self.classified(record, field)
    }

    /// The caveats every channel that shows `record` must show beside it,
    /// in a fixed order; empty when it needs none or is not included.
    pub fn caveats(&self, record: &Record) -> &[Caveat] {
        if self.includes(record) {
            self.caveats
                .get(&record.header.id)
                .map_or(&[], Vec::as_slice)
        } else {
            &[]
        }
    }

    /// Every withheld record of the pinned snapshot, in ID order.
    pub fn exclusions(&self) -> Vec<Exclusion> {
        self.withheld
            .iter()
            .map(|(id, reason)| Exclusion { id: *id, reason })
            .collect()
    }

    /// Every shown record that carries caveats, in ID order.
    pub fn caveated(&self) -> Vec<Caveated> {
        self.caveats
            .iter()
            .map(|(id, caveats)| Caveated {
                id: *id,
                caveats: caveats.clone(),
            })
            .collect()
    }
}

/// Statements: records whose standing rests on a dependency closure.
fn statement(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Conclusion | Kind::Decision | Kind::Assessment | Kind::Comparison
    )
}

fn subject_id(body: &Value) -> Option<Id> {
    serde_json::from_value::<RevisionRef>(body.get("subject_ref")?.clone())
        .ok()
        .map(|reference| reference.id)
}

/// Challenges by the record they name: open objections, and reviews and
/// assessments of it. `.1` holds the open objections alone.
type Challenges = (BTreeMap<Id, Vec<Id>>, BTreeMap<Id, Vec<Id>>);

fn challenges_by_subject(state: &State) -> Challenges {
    let mut all: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
    let mut objections: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
    for record in state.records() {
        let kind = record.header.id.kind();
        let open = kind == Kind::Objection
            && record.body.get("status").and_then(Value::as_str) == Some("open");
        if !(open || matches!(kind, Kind::Review | Kind::Assessment)) {
            continue;
        }
        let Some(subject) = subject_id(&record.body) else {
            continue;
        };
        all.entry(subject).or_default().push(record.header.id);
        if open {
            objections
                .entry(subject)
                .or_default()
                .push(record.header.id);
        }
    }
    (all, objections)
}

fn bearing(state: &State, record: &Record, (all, objections): &Challenges) -> Bearing {
    let id = record.header.id;
    let mut challenges: BTreeSet<Id> = all.get(&id).into_iter().flatten().copied().collect();
    if !statement(id.kind()) {
        return Bearing {
            challenges,
            basis: Some(BTreeSet::new()),
        };
    }
    // A closure member that does not resolve here cannot be judged, so the
    // closure counts as unverifiable rather than as clean.
    let basis = review_ops::required_reads(state, record)
        .ok()
        .map(|reads| {
            reads
                .into_iter()
                .map(|reference| reference.id)
                .filter(|member| *member != id)
                .collect::<BTreeSet<Id>>()
        })
        .filter(|members| {
            members
                .iter()
                .all(|member| state.current(*member).is_some())
        });
    for member in basis.iter().flatten() {
        challenges.extend(objections.get(member).into_iter().flatten().copied());
    }
    challenges.remove(&id);
    Bearing { challenges, basis }
}
