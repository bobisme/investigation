//! Snapshot-bound paging over the deterministic gap inventory.
//!
//! A cursor binds the case, the exact verified snapshot, the gap rule version,
//! the filter and the ordering it was issued for, and the last gap it returned.
//! Later pages are read from that snapshot, so writes committed between page
//! reads cannot add, drop or repeat entries in the original sequence. Any
//! mismatch is an explicit, repairable rejection, never silent drift.
//!
//! The cursor is opaque to callers but not secret: it carries only identifiers
//! that the page it came from already disclosed. Its checksum detects damage;
//! it is not an authorization token.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    gap_rules::{GAP_RULE_VERSION, Gap, GapInventory, GapStatus},
    identity::{Id, Kind},
    wire::{ErrorCode, Failure, Snapshot},
};

pub const GAP_PAGE_VERSION: &str = "inv.gap_page.v1";
pub const DEFAULT_GAP_PAGE_LIMIT: usize = 50;
pub const MAX_GAP_PAGE_LIMIT: usize = 500;

const CURSOR_VERSION: &str = "gpc1";
const MAX_CURSOR_BYTES: usize = 16 * 1024;
const CHECKSUM_HEX: usize = 16;
const RESTART: &str = "Repeat the request without a cursor to start again at the current snapshot, or resend this cursor with the case, filter and ordering it was issued for.";

/// The total order a page sequence walks. Only one stable order exists today;
/// the cursor still records it so a later order cannot reuse an old position.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapOrdering {
    /// Ascending stable `gap_id`, the inventory's canonical order.
    #[default]
    GapId,
}

/// Conjunctive filter. An empty set places no constraint on that field; a
/// non-empty set matches any of its members.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapFilter {
    #[serde(default)]
    pub statuses: BTreeSet<GapStatus>,
    /// Record kind prefixes such as `hyp` or `obs`; matches when any subject has the kind.
    #[serde(default)]
    pub subject_kinds: BTreeSet<String>,
    #[serde(default)]
    pub rule_ids: BTreeSet<String>,
    /// Record IDs; matches when the gap's read set names any of them. Omitted
    /// when empty so cursors issued before this field existed still decode
    /// and compare equal.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub read_ids: BTreeSet<Id>,
}

impl GapFilter {
    pub fn validate(&self) -> Result<(), Failure> {
        if let Some(kind) = self.subject_kinds.iter().find(|kind| {
            !Kind::ALL
                .iter()
                .any(|known| known.prefix() == kind.as_str())
        }) {
            let mut failure = Failure::new(
                ErrorCode::InvalidInput,
                format!("Unknown subject kind {kind:?}; use a record kind prefix."),
            )
            .at("/filter/subject_kinds");
            failure.error.expected = Some(json!(
                Kind::ALL
                    .iter()
                    .map(|kind| kind.prefix())
                    .collect::<Vec<_>>()
            ));
            return Err(failure);
        }
        if self.rule_ids.iter().any(|rule| rule.trim().is_empty()) {
            return Err(
                Failure::new(ErrorCode::InvalidInput, "A rule ID filter cannot be empty.")
                    .at("/filter/rule_ids"),
            );
        }
        Ok(())
    }

    pub fn matches(&self, gap: &Gap) -> bool {
        (self.statuses.is_empty() || self.statuses.contains(&gap.status))
            && (self.rule_ids.is_empty() || self.rule_ids.contains(&gap.rule_id))
            && (self.subject_kinds.is_empty()
                || gap
                    .subject_refs
                    .iter()
                    .any(|subject| self.subject_kinds.contains(subject.id.kind().prefix())))
            && (self.read_ids.is_empty()
                || gap
                    .read_set
                    .iter()
                    .any(|read| self.read_ids.contains(&read.id)))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GapPageRequest {
    pub filter: GapFilter,
    pub ordering: GapOrdering,
    pub limit: usize,
    /// An encoded cursor from a previous page's `next_cursor`.
    pub cursor: Option<String>,
}

impl Default for GapPageRequest {
    fn default() -> Self {
        Self {
            filter: GapFilter::default(),
            ordering: GapOrdering::default(),
            limit: DEFAULT_GAP_PAGE_LIMIT,
            cursor: None,
        }
    }
}

/// Everything a continuation is bound to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapCursor {
    pub case_id: Id,
    pub snapshot: Snapshot,
    pub rule_version: String,
    pub ordering: GapOrdering,
    pub filter: GapFilter,
    /// The last gap returned; the next page starts strictly after it.
    pub after_gap_id: String,
}

impl GapCursor {
    /// A cursor positioned before the first matching gap at `snapshot`. It lets
    /// another read (such as `next`) hand out a drill-down whose first page is
    /// bound to the snapshot that read used, not to whatever head is current.
    pub fn start(
        case_id: Id,
        snapshot: Snapshot,
        filter: GapFilter,
        ordering: GapOrdering,
    ) -> Self {
        Self {
            case_id,
            snapshot,
            rule_version: GAP_RULE_VERSION.into(),
            ordering,
            filter,
            after_gap_id: String::new(),
        }
    }

    pub fn encode(&self) -> String {
        let payload = serde_json::to_vec(self).expect("gap cursor serializes");
        let body = format!("{CURSOR_VERSION}.{}", base64url_encode(&payload));
        let checksum = checksum(&body);
        format!("{body}.{checksum}")
    }

    pub fn decode(text: &str) -> Result<Self, Failure> {
        let malformed = || {
            cursor_failure(
                "The cursor is malformed or was altered; it cannot continue a page sequence.",
                None,
                None,
            )
        };
        if text.len() > MAX_CURSOR_BYTES {
            return Err(malformed());
        }
        let (body, sum) = text.rsplit_once('.').ok_or_else(malformed)?;
        let (version, payload) = body.split_once('.').ok_or_else(malformed)?;
        if version != CURSOR_VERSION {
            return Err(cursor_failure(
                "The cursor uses an unsupported version.",
                Some(json!(CURSOR_VERSION)),
                Some(json!(version)),
            ));
        }
        if sum != checksum(body) {
            return Err(malformed());
        }
        let bytes = base64url_decode(payload).ok_or_else(malformed)?;
        serde_json::from_slice(&bytes).map_err(|_| malformed())
    }

    /// Check what the caller asked for against what the cursor was issued for.
    /// Run this before pinning so a mismatch costs no replay.
    pub fn check_request(
        &self,
        case_id: Id,
        filter: &GapFilter,
        ordering: GapOrdering,
    ) -> Result<(), Failure> {
        if self.case_id != case_id {
            return Err(cursor_failure(
                "The cursor was issued for a different case.",
                Some(json!(case_id)),
                Some(json!(self.case_id)),
            ));
        }
        if self.rule_version != GAP_RULE_VERSION {
            return Err(cursor_failure(
                "The cursor was issued by a different gap rule version; a changed rule is not new evidence, so the sequence cannot continue.",
                Some(json!(GAP_RULE_VERSION)),
                Some(json!(self.rule_version)),
            ));
        }
        if self.filter != *filter || self.ordering != ordering {
            return Err(cursor_failure(
                "The cursor was issued for a different filter or ordering.",
                Some(json!({"filter": filter, "ordering": ordering})),
                Some(json!({"filter": self.filter, "ordering": self.ordering})),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GapPage {
    pub schema_version: String,
    pub case_id: Id,
    /// The verified snapshot every page in this sequence reads.
    pub snapshot: Snapshot,
    /// Present when writes were committed after `snapshot`; restart without a
    /// cursor to read them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_head: Option<Snapshot>,
    pub rule_version: String,
    pub ordering: GapOrdering,
    pub filter: GapFilter,
    pub limit: usize,
    /// Matching gaps that precede this page in the sequence.
    pub position: usize,
    pub gaps: Vec<Gap>,
    pub matching_total: usize,
    /// Gaps in the unfiltered inventory at `snapshot`.
    pub inventory_total: usize,
    /// Matching gaps after this page.
    pub remaining: usize,
    pub next_cursor: Option<String>,
    pub limits: Vec<String>,
}

/// Return one page of `inventory`. With a cursor, the inventory must have been
/// enumerated at the cursor's snapshot; the caller pins it.
pub fn page_gaps(
    inventory: &GapInventory,
    filter: &GapFilter,
    ordering: GapOrdering,
    limit: usize,
    cursor: Option<&GapCursor>,
) -> Result<GapPage, Failure> {
    filter.validate()?;
    if !(1..=MAX_GAP_PAGE_LIMIT).contains(&limit) {
        let mut failure = Failure::new(
            ErrorCode::InvalidInput,
            format!("Use a page limit from 1 to {MAX_GAP_PAGE_LIMIT}."),
        )
        .at("/limit");
        failure.error.expected = Some(json!({"minimum": 1, "maximum": MAX_GAP_PAGE_LIMIT}));
        failure.error.actual = Some(json!(limit));
        return Err(failure);
    }
    let mut matching: Vec<&Gap> = inventory
        .gaps
        .iter()
        .filter(|gap| filter.matches(gap))
        .collect();
    match ordering {
        GapOrdering::GapId => matching.sort_by(|a, b| a.gap_id.cmp(&b.gap_id)),
    }
    let start = match cursor {
        None => 0,
        Some(cursor) => {
            cursor.check_request(inventory.case_id, filter, ordering)?;
            if cursor.snapshot != inventory.snapshot
                || cursor.rule_version != inventory.rule_version
            {
                return Err(cursor_failure(
                    "The page was not read from the cursor's pinned snapshot.",
                    Some(json!({"snapshot": cursor.snapshot, "rule_version": cursor.rule_version})),
                    Some(
                        json!({"snapshot": inventory.snapshot, "rule_version": inventory.rule_version}),
                    ),
                ));
            }
            // An empty position is a start cursor; no gap ID is empty.
            if cursor.after_gap_id.is_empty() {
                0
            } else {
                matching
                    .iter()
                    .position(|gap| gap.gap_id == cursor.after_gap_id)
                .map(|index| index + 1)
                .ok_or_else(|| {
                    cursor_failure(
                        "The cursor's last gap is absent from its pinned snapshot, so its position cannot be trusted.",
                        None,
                        Some(json!(cursor.after_gap_id)),
                    )
                })?
            }
        }
    };
    let end = start.saturating_add(limit).min(matching.len());
    let gaps: Vec<Gap> = matching[start..end]
        .iter()
        .map(|gap| (*gap).clone())
        .collect();
    let remaining = matching.len() - end;
    let next_cursor = gaps.last().filter(|_| remaining > 0).map(|last| {
        GapCursor {
            case_id: inventory.case_id,
            snapshot: inventory.snapshot.clone(),
            rule_version: inventory.rule_version.clone(),
            ordering,
            filter: filter.clone(),
            after_gap_id: last.gap_id.clone(),
        }
        .encode()
    });
    let mut limits = inventory.limits.clone();
    limits.push(
        "Every page of a cursor sequence reads the first page's snapshot; writes committed later appear only after restarting without a cursor."
            .into(),
    );
    Ok(GapPage {
        schema_version: GAP_PAGE_VERSION.into(),
        case_id: inventory.case_id,
        snapshot: inventory.snapshot.clone(),
        newer_head: None,
        rule_version: inventory.rule_version.clone(),
        ordering,
        filter: filter.clone(),
        limit,
        position: start,
        gaps,
        matching_total: matching.len(),
        inventory_total: inventory.gaps.len(),
        remaining,
        next_cursor,
        limits,
    })
}

/// A rejected continuation. The message says how to repair it; `expected` is
/// what this request needs and `actual` is what the cursor carries.
pub fn cursor_failure(message: &str, expected: Option<Value>, actual: Option<Value>) -> Failure {
    let mut failure =
        Failure::new(ErrorCode::InvalidInput, format!("{message} {RESTART}")).at("/cursor");
    failure.error.expected = expected;
    failure.error.actual = actual;
    failure
}

fn checksum(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    let mut hex = String::with_capacity(CHECKSUM_HEX);
    for byte in &digest[..CHECKSUM_HEX / 2] {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| {
            acc | (u32::from(*byte) << (16 - 8 * index))
        });
        for index in 0..=chunk.len() {
            out.push(char::from(
                ALPHABET[((value >> (18 - 6 * index)) & 63) as usize],
            ));
        }
    }
    out
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.as_bytes().chunks(4) {
        let mut value = 0u32;
        for (index, symbol) in chunk.iter().enumerate() {
            let digit = ALPHABET.iter().position(|known| known == symbol)?;
            value |= (digit as u32) << (18 - 6 * index);
        }
        let produced = chunk.len() - 1;
        for index in 0..produced {
            out.push((value >> (16 - 8 * index)) as u8);
        }
        // Reject non-canonical trailing bits so each cursor has one spelling.
        let used = 8 * produced;
        if value & ((1u32 << (24 - used)) - 1) != 0 {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        command::RevisionRef,
        gap_rules::{GapStatus, Prerequisite},
    };

    fn id(text: &str) -> Id {
        text.parse().unwrap()
    }

    fn snapshot(sequence: u64, fill: char) -> Snapshot {
        Snapshot {
            sequence,
            head_hash: format!("sha256:{}", fill.to_string().repeat(64))
                .parse()
                .unwrap(),
        }
    }

    fn gap(subject: Id, rule: &str, status: GapStatus) -> Gap {
        let subject_ref = RevisionRef {
            id: subject,
            revision: 1,
        };
        Gap {
            gap_id: format!("gap:{rule}:{subject}"),
            rule_id: rule.into(),
            rule_version: GAP_RULE_VERSION.into(),
            subject_refs: vec![subject_ref],
            read_set: vec![subject_ref],
            status,
            reason: "fixture".into(),
            prerequisites: Vec::<Prerequisite>::new(),
            suggested_operation: None,
            unresolved_fields: vec![],
            outcome_branches: vec![],
            decision_effect: "unknown".into(),
            time_sensitive: false,
            creation_sequence: 1,
            stall: None,
            advice: None,
        }
    }

    fn case_id() -> Id {
        id("case_019acd16-0000-7000-8000-000000000001")
    }

    fn inventory(count: usize) -> GapInventory {
        let mut gaps: Vec<Gap> = (0..count)
            .map(|index| {
                let subject = if index % 2 == 0 {
                    id(&format!("hyp_019acd16-0000-7000-8000-{:012}", index + 10))
                } else {
                    id(&format!("obs_019acd16-0000-7000-8000-{:012}", index + 10))
                };
                let (rule, status) = if index % 2 == 0 {
                    ("hypothesis.no_assessment", GapStatus::RequiresSource)
                } else {
                    (
                        "observation.validity_context_missing",
                        GapStatus::BlockedByQuality,
                    )
                };
                gap(subject, rule, status)
            })
            .collect();
        gaps.sort_by(|a, b| a.gap_id.cmp(&b.gap_id));
        GapInventory {
            schema_version: "inv.gaps.v1".into(),
            snapshot: snapshot(4, 'a'),
            rule_version: GAP_RULE_VERSION.into(),
            case_id: case_id(),
            gaps,
            limits: vec![],
        }
    }

    fn walk(inventory: &GapInventory, filter: &GapFilter, limit: usize) -> Vec<GapPage> {
        let mut pages =
            vec![page_gaps(inventory, filter, GapOrdering::GapId, limit, None).unwrap()];
        while let Some(next) = pages.last().unwrap().next_cursor.clone() {
            let cursor = GapCursor::decode(&next).unwrap();
            pages.push(
                page_gaps(inventory, filter, GapOrdering::GapId, limit, Some(&cursor)).unwrap(),
            );
        }
        pages
    }

    #[test]
    fn empty_single_and_multipage_sequences_cover_each_gap_exactly_once() {
        let empty = walk(&inventory(0), &GapFilter::default(), 3);
        assert_eq!(empty.len(), 1);
        assert!(empty[0].gaps.is_empty());
        assert_eq!((empty[0].matching_total, empty[0].remaining), (0, 0));
        assert!(empty[0].next_cursor.is_none());

        let single = walk(&inventory(3), &GapFilter::default(), 3);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].gaps.len(), 3);
        assert!(single[0].next_cursor.is_none());

        let full = inventory(7);
        let pages = walk(&full, &GapFilter::default(), 3);
        assert_eq!(
            pages.iter().map(|page| page.gaps.len()).collect::<Vec<_>>(),
            [3, 3, 1]
        );
        assert_eq!(
            pages.iter().map(|page| page.position).collect::<Vec<_>>(),
            [0, 3, 6]
        );
        let walked: Vec<_> = pages
            .iter()
            .flat_map(|page| page.gaps.iter().map(|gap| gap.gap_id.clone()))
            .collect();
        let expected: Vec<_> = full.gaps.iter().map(|gap| gap.gap_id.clone()).collect();
        assert_eq!(walked, expected);
        assert_eq!(pages.last().unwrap().remaining, 0);
    }

    #[test]
    fn filters_apply_before_paging_and_are_bound_into_the_cursor() {
        let full = inventory(9);
        let filter = GapFilter {
            statuses: [GapStatus::RequiresSource].into(),
            subject_kinds: ["hyp".to_owned()].into(),
            ..GapFilter::default()
        };
        let pages = walk(&full, &filter, 2);
        let walked: Vec<_> = pages.iter().flat_map(|page| &page.gaps).collect();
        assert_eq!(walked.len(), 5);
        assert!(
            walked
                .iter()
                .all(|gap| gap.status == GapStatus::RequiresSource)
        );
        assert_eq!(pages[0].inventory_total, 9);
        assert_eq!(pages[0].matching_total, 5);

        let cursor = GapCursor::decode(pages[0].next_cursor.as_deref().unwrap()).unwrap();
        let failure = page_gaps(
            &full,
            &GapFilter::default(),
            GapOrdering::GapId,
            2,
            Some(&cursor),
        )
        .unwrap_err();
        assert_eq!(failure.error.code, ErrorCode::InvalidInput);
        assert_eq!(failure.error.path.as_deref(), Some("/cursor"));
        assert!(failure.error.message.contains("without a cursor"));
        assert_eq!(
            failure.error.actual.as_ref().unwrap()["filter"],
            json!(filter)
        );
    }

    #[test]
    fn start_cursors_and_read_filters_page_from_the_first_matching_gap() {
        let full = inventory(7);
        let subject = full.gaps[2].subject_refs[0].id;
        let filter = GapFilter {
            read_ids: [subject].into(),
            ..GapFilter::default()
        };
        let start = GapCursor::start(
            full.case_id,
            full.snapshot.clone(),
            filter.clone(),
            GapOrdering::GapId,
        );
        let encoded = start.encode();
        assert_eq!(GapCursor::decode(&encoded).unwrap(), start);
        let page = page_gaps(&full, &filter, GapOrdering::GapId, 3, Some(&start)).unwrap();
        assert_eq!(page.position, 0);
        assert_eq!(page.matching_total, 1);
        assert_eq!(page.gaps[0].gap_id, full.gaps[2].gap_id);

        let every = GapCursor::start(
            full.case_id,
            full.snapshot.clone(),
            GapFilter::default(),
            GapOrdering::GapId,
        );
        let unfiltered = |cursor| {
            page_gaps(&full, &GapFilter::default(), GapOrdering::GapId, 3, cursor).unwrap()
        };
        assert_eq!(unfiltered(Some(&every)).gaps, unfiltered(None).gaps);

        // A start cursor is still bound to its snapshot.
        let mut moved = full.clone();
        moved.snapshot = snapshot(5, 'b');
        let failure = page_gaps(
            &moved,
            &GapFilter::default(),
            GapOrdering::GapId,
            3,
            Some(&every),
        )
        .unwrap_err();
        assert!(failure.error.message.contains("pinned snapshot"));

        // An empty read filter is omitted, so older cursors keep one spelling.
        assert!(
            serde_json::to_value(GapFilter::default())
                .unwrap()
                .get("read_ids")
                .is_none()
        );
    }

    #[test]
    fn mismatched_or_damaged_cursors_are_rejected_with_repair_guidance() {
        let full = inventory(5);
        let first = page_gaps(&full, &GapFilter::default(), GapOrdering::GapId, 2, None).unwrap();
        let encoded = first.next_cursor.unwrap();
        let cursor = GapCursor::decode(&encoded).unwrap();
        assert_eq!(cursor.encode(), encoded);

        let reject = |inventory: &GapInventory, cursor: &GapCursor| {
            page_gaps(
                inventory,
                &GapFilter::default(),
                GapOrdering::GapId,
                2,
                Some(cursor),
            )
            .unwrap_err()
        };
        let mut other_case = cursor.clone();
        other_case.case_id = id("case_019acd16-0000-7000-8000-000000000099");
        assert!(
            reject(&full, &other_case)
                .error
                .message
                .contains("different case")
        );

        let mut other_rules = cursor.clone();
        other_rules.rule_version = "slice-gaps.v0".into();
        assert!(
            reject(&full, &other_rules)
                .error
                .message
                .contains("rule version")
        );

        let mut moved = full.clone();
        moved.snapshot = snapshot(5, 'b');
        assert!(
            reject(&moved, &cursor)
                .error
                .message
                .contains("pinned snapshot")
        );

        let mut absent = cursor.clone();
        absent.after_gap_id = "gap:none:none".into();
        assert!(reject(&full, &absent).error.message.contains("absent"));

        for damaged in [
            encoded.replacen("gpc1", "gpc9", 1),
            format!("{}x", &encoded[..encoded.len() - 1]),
            encoded.replace('.', ""),
            "x".repeat(MAX_CURSOR_BYTES + 1),
            String::new(),
        ] {
            let failure = GapCursor::decode(&damaged).unwrap_err();
            assert_eq!(failure.error.code, ErrorCode::InvalidInput);
            assert_eq!(failure.error.path.as_deref(), Some("/cursor"));
        }
    }

    #[test]
    fn invalid_limits_and_filters_are_rejected() {
        let full = inventory(2);
        for limit in [0, MAX_GAP_PAGE_LIMIT + 1] {
            let failure = page_gaps(
                &full,
                &GapFilter::default(),
                GapOrdering::GapId,
                limit,
                None,
            )
            .unwrap_err();
            assert_eq!(failure.error.path.as_deref(), Some("/limit"));
        }
        let unknown = GapFilter {
            subject_kinds: ["widget".to_owned()].into(),
            ..GapFilter::default()
        };
        let failure = page_gaps(&full, &unknown, GapOrdering::GapId, 1, None).unwrap_err();
        assert_eq!(failure.error.path.as_deref(), Some("/filter/subject_kinds"));
    }

    #[test]
    fn base64url_round_trips_and_rejects_non_canonical_text() {
        for length in 0..20 {
            let bytes: Vec<u8> = (0..length).map(|index| (index * 37 + 11) as u8).collect();
            let text = base64url_encode(&bytes);
            assert_eq!(base64url_decode(&text).unwrap(), bytes);
        }
        assert_eq!(base64url_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url_encode(b"fo"), "Zm8");
        assert!(base64url_decode("Zm9").is_none());
        assert!(base64url_decode("Z").is_none());
        assert!(base64url_decode("Zm+v").is_none());
    }
}
