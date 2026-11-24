//! What `brief` selects from one case snapshot, and in which order. Pure.
//!
//! `brief`, the vault's case Overview and the `handoff` export each tell the
//! bounded story of a case. They share these selections so that, for the
//! same snapshot, they lead with the same outcome and list the same
//! questions, evidence and inquiries in the same priority order. Each reader
//! applies its own visibility and budget afterwards; nothing here filters by
//! audience.

use crate::{
    identity::Kind,
    state::{Record, State},
};

/// Whether a record is still in force: neither its lifecycle nor its status
/// is `superseded`.
pub fn is_live(record: &Record) -> bool {
    record.body["lifecycle"] != "superseded" && record.body["status"] != "superseded"
}

/// Whether a record is a Conclusion or a Decision.
pub fn is_outcome(record: &Record) -> bool {
    matches!(record.header.id.kind(), Kind::Conclusion | Kind::Decision)
}

/// Every current record in acceptance order, ties by ID.
pub fn in_acceptance_order(state: &State) -> Vec<&Record> {
    let mut records: Vec<&Record> = state.records().collect();
    records.sort_by_key(|record| (record.header.acceptance_sequence, record.header.id));
    records
}

/// Conclusions and Decisions in brief priority order: live ones newest
/// first, so the leading outcome is the last to be cut, then superseded ones
/// newest first.
pub fn outcomes(state: &State) -> Vec<&Record> {
    let newest_first = in_acceptance_order(state)
        .into_iter()
        .rev()
        .filter(|record| is_outcome(record));
    let (mut live, past): (Vec<_>, Vec<_>) = newest_first.partition(|record| is_live(record));
    live.extend(past);
    live
}

/// [`outcomes`] with each live outcome that `flagged` marks moved up to
/// just behind the leading one, keeping their order. A brief flags an
/// outcome whose effective freshness is not current, so a budget cuts
/// current outcomes before one a change has made stale (bn-1wre).
pub fn outcomes_flagged_first(state: &State, flagged: impl Fn(&Record) -> bool) -> Vec<&Record> {
    flagged_first(
        outcomes(state),
        |record| is_live(record),
        |record| flagged(record),
    )
}

/// `ordered` with its first item kept first, then every later item that is
/// both `live` and `flagged`, then the rest, each group in its order.
fn flagged_first<T>(
    ordered: Vec<T>,
    live: impl Fn(&T) -> bool,
    flagged: impl Fn(&T) -> bool,
) -> Vec<T> {
    let mut items = ordered.into_iter();
    let Some(first) = items.next() else {
        return Vec::new();
    };
    let (raised, rest): (Vec<T>, Vec<T>) = items.partition(|item| live(item) && flagged(item));
    std::iter::once(first).chain(raised).chain(rest).collect()
}

/// The outcome whose answer, caveat and contradiction lead the case: the
/// newest Conclusion or Decision that is not superseded.
pub fn primary_outcome(state: &State) -> Option<&Record> {
    state
        .records()
        .filter(|record| is_outcome(record) && is_live(record))
        .max_by_key(|record| (record.header.acceptance_sequence, record.header.id))
}

/// Questions whose disposition is `open` or `deferred`, in acceptance order.
pub fn unresolved_questions(state: &State) -> Vec<&Record> {
    in_acceptance_order(state)
        .into_iter()
        .filter(|record| {
            record.header.id.kind() == Kind::Question
                && matches!(
                    record
                        .body
                        .get("disposition")
                        .and_then(serde_json::Value::as_str),
                    Some("open" | "deferred")
                )
        })
        .collect()
}

/// Observations and Comparisons, in acceptance order.
pub fn evidence(state: &State) -> Vec<&Record> {
    in_acceptance_order(state)
        .into_iter()
        .filter(|record| {
            matches!(
                record.header.id.kind(),
                Kind::Observation | Kind::Comparison
            )
        })
        .collect()
}

/// Inquiry plans that are not retired, in acceptance order.
pub fn active_inquiries(state: &State) -> Vec<&Record> {
    in_acceptance_order(state)
        .into_iter()
        .filter(|record| {
            record.header.id.kind() == Kind::Inquiry && record.body["lifecycle"] != "retired"
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::flagged_first;

    #[test]
    fn flagged_live_items_follow_the_first_and_keep_their_order() {
        // 0 leads; odd items are flagged; items from 6 on are past.
        let ordered: Vec<u32> = (0..9).collect();
        let ranked = flagged_first(ordered, |item| *item < 6, |item| item % 2 == 1);
        assert_eq!(ranked, vec![0, 1, 3, 5, 2, 4, 6, 7, 8]);
        // The leading item stays first even when it is not flagged, and a
        // flagged leading item is not repeated.
        assert_eq!(
            flagged_first(vec![1, 2, 3], |_| true, |item| item % 2 == 1),
            [1, 3, 2]
        );
        assert_eq!(
            flagged_first(Vec::<u32>::new(), |_| true, |_| true),
            Vec::<u32>::new()
        );
    }
}
