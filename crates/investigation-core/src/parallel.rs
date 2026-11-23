//! Contiguous chunks of a slice mapped on a few scoped threads, results in
//! order. Used where one read maps hundreds of thousands of independent
//! items, such as the cells of a large option-by-criterion matrix.

/// At most this many threads, the calling thread included.
const MAX_THREADS: usize = 8;

/// Fewer elementary steps than this per thread do not pay for a thread.
const MIN_WORK_PER_THREAD: usize = 16_384;

/// `f` over contiguous chunks of `items`, concatenated in item order.
///
/// `work` is the caller's estimate of the elementary steps `f(items)` takes;
/// below `2 * MIN_WORK_PER_THREAD`, or on one CPU, `f(items)` runs on the
/// calling thread. When `f` maps each item on its own, so that
/// `f(a ++ b) == f(a) ++ f(b)`, the result equals `f(items)` whatever the
/// number of threads. A thread that cannot start leaves its chunk to the
/// calling thread; a panic in a chunk resumes on the calling thread. The
/// results may borrow from `items`.
pub(crate) fn map_chunks<'s, T, R, F>(items: &'s [T], work: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&'s [T]) -> Vec<R> + Sync,
{
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_THREADS)
        .min(work / MIN_WORK_PER_THREAD)
        .min(items.len());
    if threads <= 1 {
        return f(items);
    }
    let size = items.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|scope| {
        let mut chunks = items.chunks(size);
        let first = chunks.next().unwrap_or_default();
        let helpers: Vec<_> = chunks
            .map(|chunk| {
                std::thread::Builder::new()
                    .spawn_scoped(scope, move || f(chunk))
                    .map_err(|_| chunk)
            })
            .collect();
        let mut results = f(first);
        for helper in helpers {
            match helper {
                Ok(handle) => results.extend(
                    handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                ),
                Err(chunk) => results.extend(f(chunk)),
            }
        }
        results
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_concatenate_in_item_order_whatever_the_work_estimate() {
        let items: Vec<u32> = (0..100_000).collect();
        let double = |chunk: &[u32]| chunk.iter().map(|item| item * 2).collect::<Vec<_>>();
        let expected = double(&items);
        for work in [0, MIN_WORK_PER_THREAD * 2, usize::MAX] {
            assert_eq!(map_chunks(&items, work, double), expected);
        }
        assert!(map_chunks(&[] as &[u32], usize::MAX, double).is_empty());
        assert_eq!(map_chunks(&[7u32], usize::MAX, double), vec![14]);
    }

    #[test]
    #[should_panic(expected = "chunk failed")]
    fn a_panic_in_a_chunk_reaches_the_caller() {
        let items: Vec<u32> = (0..100_000).collect();
        map_chunks(&items, usize::MAX, |chunk: &[u32]| -> Vec<u32> {
            assert!(!chunk.contains(&99_999), "chunk failed");
            chunk.to_vec()
        });
    }
}
