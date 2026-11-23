//! Positive inclusion for exports. Pure.
//!
//! A record is included only when a rule includes it: its effective
//! classification at the pinned snapshot and at today's head must both permit
//! the audience under the strict policy, which keeps no ID-only placeholders.
//! Today's classification governs an earlier snapshot too, so a record raised
//! since then stays out of a re-export, and a record withheld at the pinned
//! snapshot is never shown because it was cleared later.

use investigation_core::{
    classification::{self, AudiencePolicy, Classifier, Field},
    identity::Id,
    state::{Record, State},
};
use serde::Serialize;

use crate::Audience;

/// One record the audience policy withheld. For the publisher only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exclusion {
    /// The record; its prefix names its kind.
    pub id: Id,
    /// Why it was withheld: `audience` (its effective classification is
    /// above the audience at the pinned snapshot) or `audience_today` (only
    /// today's classification withholds it).
    pub reason: &'static str,
}

/// The inclusion rule of one export.
pub struct Inclusion<'a> {
    policy: AudiencePolicy,
    pinned: Classifier<'a>,
    /// Today's state when the export pins an earlier snapshot.
    today: Option<Classifier<'a>>,
}

impl<'a> Inclusion<'a> {
    pub fn new(pinned: &'a State, today: Option<&'a State>, audience: Audience) -> Self {
        let audience = match audience {
            Audience::Public => classification::Audience::Public,
            Audience::Internal => classification::Audience::Internal,
            Audience::Restricted => classification::Audience::Restricted,
        };
        Self {
            policy: AudiencePolicy::strict(audience),
            pinned: Classifier::new(pinned),
            today: today.map(Classifier::new),
        }
    }

    /// Whether the record's title, summary and content may appear.
    pub fn includes(&self, record: &Record) -> bool {
        self.may_show(record, Field::Summary)
    }

    /// Whether one channel of the record may appear.
    pub fn may_show(&self, record: &Record, field: Field) -> bool {
        self.pinned.may_show(record, field, &self.policy) && self.today_allows(record, field)
    }

    fn today_allows(&self, record: &Record, field: Field) -> bool {
        self.today.as_ref().is_none_or(|today| {
            today
                .state()
                .current(record.header.id)
                .is_some_and(|current| today.may_show(current, field, &self.policy))
        })
    }

    /// Every record of `state` the policy withholds, in ID order.
    pub fn exclusions(&self, state: &State) -> Vec<Exclusion> {
        state
            .records()
            .filter(|record| !self.includes(record))
            .map(|record| Exclusion {
                id: record.header.id,
                reason: if self.pinned.may_show(record, Field::Summary, &self.policy) {
                    "audience_today"
                } else {
                    "audience"
                },
            })
            .collect()
    }
}
