//! Snapshot-bound gap paging against a real journal with interleaved writers.

use std::collections::BTreeSet;

use investigation_core::{
    command::RevisionRef,
    gap_page::{GapCursor, GapFilter, GapPage, GapPageRequest},
    gap_rules::enumerate_gaps,
    identity::Id,
    wire::{ErrorCode, Failure, Receipt, Snapshot},
};
use investigation_store::{Store, Workspace, pinned::read_gap_page};
use serde_json::{Value, json};

const RULE: &str = "hypothesis.no_assessment";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: std::path::PathBuf,
    keys: usize,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        Workspace::init(&root, false).unwrap();
        Self {
            _temporary: temporary,
            root,
            keys: 0,
        }
    }

    /// Each call opens its own handle, as an independent writer process would.
    fn store(&self) -> Store {
        Store::open(Workspace::open(&self.root).unwrap())
    }

    fn submit(&mut self, case_id: Option<Id>, op: &str, payload: Value) -> Receipt {
        self.keys += 1;
        let input = json!({
            "schema_version":"inv.command.v1",
            "command_id":format!("command-{}", self.keys),
            "case_id":case_id,
            "actor":"agent:paging",
            "preconditions":[],
            "operations":[{"op":op,"payload":payload}],
        });
        self.store()
            .record(&serde_json::to_vec(&input).unwrap())
            .unwrap_or_else(|failure| panic!("{op}: {failure:?}"))
    }

    fn case(&mut self, title: &str) -> Id {
        self.submit(None, "case.create", json!({"title":title,"mode":"record"}))
            .created[0]
            .id
    }

    fn question(&mut self, case_id: Id) -> RevisionRef {
        let receipt = self.submit(
            Some(case_id),
            "question.create",
            json!({"wording":"What explains the recorded difference?","importance":"normal"}),
        );
        created(&receipt)
    }

    fn hypothesis(&mut self, case_id: Id, question: RevisionRef, index: usize) -> RevisionRef {
        let receipt = self.submit(
            Some(case_id),
            "hypothesis.create",
            json!({
                "question_ref":question,"statement":format!("Candidate account {index}."),
                "possible_falsifiers":[],"rival_refs":[],"assumption_refs":[],
            }),
        );
        created(&receipt)
    }

    fn retire(&mut self, case_id: Id, hypothesis: RevisionRef) {
        self.submit(
            Some(case_id),
            "hypothesis.disposition",
            json!({
                "hypothesis_ref":hypothesis,"lifecycle":"retired",
                "rationale":"The recorded scope excludes this account.","evidence_refs":[],
            }),
        );
    }

    fn page(&self, case_id: Id, request: &GapPageRequest) -> Result<GapPage, Failure> {
        read_gap_page(&mut self.store(), case_id, request)
    }
}

fn created(receipt: &Receipt) -> RevisionRef {
    RevisionRef {
        id: receipt.created[0].id,
        revision: receipt.created[0].revision,
    }
}

fn hypothesis_filter() -> GapFilter {
    GapFilter {
        rule_ids: [RULE.to_owned()].into(),
        ..GapFilter::default()
    }
}

fn request(filter: GapFilter, limit: usize, cursor: Option<String>) -> GapPageRequest {
    GapPageRequest {
        filter,
        limit,
        cursor,
        ..GapPageRequest::default()
    }
}

fn ids(page: &GapPage) -> Vec<String> {
    page.gaps.iter().map(|gap| gap.gap_id.clone()).collect()
}

fn subject_gap(hypothesis: RevisionRef) -> String {
    format!("gap:{RULE}:{}", hypothesis.id)
}

#[test]
fn empty_and_single_page_sequences_need_no_cursor() {
    let mut fixture = Fixture::new();
    let case_id = fixture.case("Paging fixture");
    let empty = fixture
        .page(case_id, &request(hypothesis_filter(), 5, None))
        .unwrap();
    assert!(empty.gaps.is_empty());
    assert_eq!((empty.matching_total, empty.remaining), (0, 0));
    assert!(empty.next_cursor.is_none());
    assert!(empty.newer_head.is_none());

    let question = fixture.question(case_id);
    for index in 0..3 {
        fixture.hypothesis(case_id, question, index);
    }
    let single = fixture
        .page(case_id, &request(hypothesis_filter(), 3, None))
        .unwrap();
    assert_eq!(single.gaps.len(), 3);
    assert_eq!((single.position, single.remaining), (0, 0));
    assert!(single.next_cursor.is_none());

    // The unfiltered first page agrees with the complete inventory behind `next`.
    let replay = fixture.store().case(case_id).unwrap();
    let inventory = enumerate_gaps(&replay.state, &replay.snapshot, case_id).unwrap();
    let all = fixture
        .page(case_id, &request(GapFilter::default(), 500, None))
        .unwrap();
    assert_eq!(all.gaps, inventory.gaps);
    assert_eq!(all.snapshot, replay.snapshot);
}

#[test]
fn a_cursor_sequence_stays_on_its_snapshot_across_concurrent_writes() {
    let mut fixture = Fixture::new();
    let case_id = fixture.case("Paging fixture");
    let question = fixture.question(case_id);
    let hypotheses: Vec<RevisionRef> = (0..7)
        .map(|index| fixture.hypothesis(case_id, question, index))
        .collect();

    let original = fixture
        .page(case_id, &request(hypothesis_filter(), 500, None))
        .unwrap();
    let expected = ids(&original);
    assert_eq!(expected.len(), 7);

    let mut page = fixture
        .page(case_id, &request(hypothesis_filter(), 2, None))
        .unwrap();
    assert_eq!(page.snapshot, original.snapshot);
    let mut walked = ids(&page);
    let mut pages = 1;
    let mut added = vec![];
    while let Some(cursor) = page.next_cursor.clone() {
        // Another writer commits between every page: one new gap, and the
        // retirement of a gap the sequence has not returned yet.
        added.push(fixture.hypothesis(case_id, question, 100 + pages));
        let unreturned = hypotheses
            .iter()
            .copied()
            .find(|hypothesis| !walked.contains(&subject_gap(*hypothesis)));
        if let Some(hypothesis) = unreturned
            && pages == 1
        {
            fixture.retire(case_id, hypothesis);
        }
        page = fixture
            .page(case_id, &request(hypothesis_filter(), 2, Some(cursor)))
            .unwrap();
        pages += 1;
        assert_eq!(page.snapshot, original.snapshot);
        let head = page.newer_head.clone().expect("writes advanced the head");
        assert!(head.sequence > original.snapshot.sequence);
        assert_eq!(page.position, walked.len());
        walked.extend(ids(&page));
    }
    assert_eq!(pages, 4);
    assert_eq!(walked, expected, "no missing, repeated or foreign entries");
    let unique: BTreeSet<_> = walked.iter().collect();
    assert_eq!(unique.len(), walked.len());

    // Restarting without a cursor reads the new head.
    let fresh = fixture
        .page(case_id, &request(hypothesis_filter(), 500, None))
        .unwrap();
    assert!(fresh.newer_head.is_none());
    assert!(fresh.snapshot.sequence > original.snapshot.sequence);
    let fresh_ids = ids(&fresh);
    assert_eq!(fresh_ids.len(), 7 - 1 + added.len());
    for hypothesis in &added {
        assert!(fresh_ids.contains(&subject_gap(*hypothesis)));
        assert!(!expected.contains(&subject_gap(*hypothesis)));
    }
}

#[test]
fn mismatched_cursors_are_rejected_with_a_repairable_error() {
    let mut fixture = Fixture::new();
    let case_id = fixture.case("Paging fixture");
    let other_case = fixture.case("Other fixture");
    let question = fixture.question(case_id);
    for index in 0..4 {
        fixture.hypothesis(case_id, question, index);
    }
    let first = fixture
        .page(case_id, &request(hypothesis_filter(), 1, None))
        .unwrap();
    let encoded = first.next_cursor.clone().unwrap();
    fixture.hypothesis(case_id, question, 9);

    let assert_repairable = |failure: Failure| {
        assert_eq!(failure.error.code, ErrorCode::InvalidInput);
        assert_eq!(failure.error.path.as_deref(), Some("/cursor"));
        assert!(
            failure.error.message.contains("without a cursor"),
            "{}",
            failure.error.message
        );
        failure
    };

    let other_filter = assert_repairable(
        fixture
            .page(
                case_id,
                &request(GapFilter::default(), 1, Some(encoded.clone())),
            )
            .unwrap_err(),
    );
    assert!(other_filter.error.expected.is_some() && other_filter.error.actual.is_some());

    assert_repairable(
        fixture
            .page(
                other_case,
                &request(hypothesis_filter(), 1, Some(encoded.clone())),
            )
            .unwrap_err(),
    );

    let decoded = GapCursor::decode(&encoded).unwrap();
    let forged = |snapshot: Snapshot| {
        GapCursor {
            snapshot,
            ..decoded.clone()
        }
        .encode()
    };
    let wrong_hash = forged(Snapshot {
        sequence: decoded.snapshot.sequence,
        head_hash: format!("sha256:{}", "0".repeat(64)).parse().unwrap(),
    });
    let beyond_head = forged(Snapshot {
        sequence: 10_000,
        head_hash: decoded.snapshot.head_hash.clone(),
    });
    for cursor in [wrong_hash, beyond_head] {
        let failure = assert_repairable(
            fixture
                .page(case_id, &request(hypothesis_filter(), 1, Some(cursor)))
                .unwrap_err(),
        );
        assert!(failure.error.message.contains("not a verified head"));
    }

    let damaged = format!("{}0", &encoded[..encoded.len() - 1]);
    let damaged = if damaged == encoded {
        format!("{}1", &encoded[..encoded.len() - 1])
    } else {
        damaged
    };
    assert_repairable(
        fixture
            .page(case_id, &request(hypothesis_filter(), 1, Some(damaged)))
            .unwrap_err(),
    );

    // The original cursor still continues on its own snapshot after the rejections.
    let second = fixture
        .page(case_id, &request(hypothesis_filter(), 1, Some(encoded)))
        .unwrap();
    assert_eq!(second.snapshot, first.snapshot);
    assert_eq!(second.position, 1);
}

#[test]
fn a_pinned_replay_matches_the_head_replay_it_was_once() {
    let mut fixture = Fixture::new();
    let case_id = fixture.case("Paging fixture");
    let question = fixture.question(case_id);
    fixture.hypothesis(case_id, question, 0);
    let then = fixture.store().case(case_id).unwrap();
    let hypothesis = fixture.hypothesis(case_id, question, 1);
    fixture.retire(case_id, hypothesis);
    let now = fixture.store().case(case_id).unwrap();
    assert_eq!(now.at_snapshot(&then.snapshot).unwrap(), then);
    assert_eq!(now.at_snapshot(&now.snapshot).unwrap(), now);
}
