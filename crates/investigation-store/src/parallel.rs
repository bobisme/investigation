//! Order-preserving parallel map for independent, read-only work: replaying
//! the cases of a verified load, and planning the cases of a vault render.

use std::{
    num::NonZero,
    sync::atomic::{AtomicUsize, Ordering},
};

/// How many threads to use for at most `limit` independent items: the
/// available parallelism, capped at `limit`, and at least 1.
pub fn threads(limit: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(1, NonZero::get)
        .min(limit)
        .max(1)
}

/// `work` applied to every item, on up to `threads` threads, with the
/// results in the order of `items`. Which thread runs an item never changes
/// its result as long as `work` reads only its argument and shared, immutable
/// data. A panic in `work` is resumed on the calling thread.
pub fn in_parallel<'i, T: Sync, R: Send>(
    items: &'i [T],
    threads: usize,
    work: impl Fn(&'i T) -> R + Sync,
) -> Vec<R> {
    let threads = threads.min(items.len());
    if threads <= 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break done;
                        };
                        done.push((index, work(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| match worker.join() {
                Ok(done) => done,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    });
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_order_of_the_items_on_any_number_of_threads() {
        let items: Vec<u64> = (0..1_000).collect();
        let expected: Vec<u64> = items.iter().map(|item| item * item).collect();
        for threads in [0, 1, 2, 7, 64] {
            assert_eq!(in_parallel(&items, threads, |item| item * item), expected);
        }
        assert!(in_parallel(&[] as &[u64], 8, |item| *item).is_empty());
    }

    #[test]
    #[should_panic(expected = "item 500")]
    fn a_panic_in_the_work_reaches_the_caller() {
        let items: Vec<u64> = (0..1_000).collect();
        in_parallel(&items, 4, |item| assert_ne!(*item, 500, "item 500"));
    }
}
