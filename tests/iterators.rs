use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicUsize, Ordering},
};
use weave::{ThreadPoolBuilder, iter::*};

#[test]
fn operations_match_sequential_results_at_split_boundaries() {
    for workers in [1, 2, 4] {
        let pool = ThreadPoolBuilder::new().num_threads(workers).build();
        pool.install(|| {
            for len in [0, 1, 511, 512, 513, 1023, 1024, 1025, 4001, 65537] {
                let expected: Vec<_> = (0..len).map(|n| n * 2 + 1).collect();
                assert_eq!(
                    (0..len).parallelize().map(|n| n * 2 + 1).collect(),
                    expected
                );
                let mut output = vec![0; len];
                (0..len).parallelize().map(|n| n * 2 + 1).fill(&mut output);
                assert_eq!(output, expected);
                let sum = (0..len).parallelize().fold(0, |a, b| a + b, |a, b| a + b);
                assert_eq!(sum, (0..len).sum::<usize>());
                let reduced = (0..len).parallelize().reduce(|a, b| a + b);
                assert_eq!(reduced, if len == 0 { None } else { Some(sum) });
                let count = AtomicUsize::new(0);
                output.iter_parallel().for_each(|_| {
                    count.fetch_add(1, Ordering::Relaxed);
                });
                assert_eq!(count.load(Ordering::Relaxed), len);
            }
        });
    }
}

#[test]
fn slices_borrow_mutably_and_preserve_order() {
    let pool = ThreadPoolBuilder::new().num_threads(3).build();
    let mut values = vec![1; 2001];
    pool.install(|| values.iter_parallel_mut().for_each(|v| *v += 3));
    assert_eq!(values, vec![4; 2001]);
    assert_eq!(
        values.iter_parallel().map(|v| v + 1).collect(),
        vec![5; 2001]
    );
    let source = SliceIter::new(&values).parallelize();
    assert_eq!(source.len(), 2001);
    let (a, b) = source.split_at(1000);
    assert_eq!((a.len(), b.len()), (1000, 1001));
    let array = [1, 2, 3];
    assert_eq!(
        array.iter_parallel().map(|v| v * 2).collect(),
        vec![2, 4, 6]
    );
}

#[test]
fn chunks_keep_the_tail_and_split_in_chunk_units() {
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    let values: Vec<_> = (0..5003).collect();
    pool.install(|| {
        for size in [1, 2, 3, 7, 512, 6000, usize::MAX] {
            let chunks = values.chunks_parallel(size);
            assert_eq!(chunks.len(), values.chunks(size).len());
            assert_eq!(
                chunks.map(|c| c.to_vec()).collect(),
                values.chunks(size).map(|c| c.to_vec()).collect::<Vec<_>>()
            );
        }
        let (left, right) = values.chunks_parallel(3).split_at(2);
        assert_eq!(left.collect(), vec![&values[0..3], &values[3..6]]);
        assert_eq!(right.collect()[0], &values[6..9]);
    });
    assert!(catch_unwind(|| values.chunks_parallel(0)).is_err());
}

#[test]
fn empty_reversed_and_large_offset_ranges() {
    let start = 10;
    assert!((start..3).parallelize().is_empty());
    let (left, right) = (start..3).parallelize().split_at(0);
    assert!(left.is_empty() && right.is_empty());
    assert_eq!(
        (usize::MAX - 3..usize::MAX).parallelize().collect(),
        vec![usize::MAX - 3, usize::MAX - 2, usize::MAX - 1]
    );
    assert!(catch_unwind(|| (0..3).parallelize().split_at(4)).is_err());
}

#[test]
fn map_is_lazy_and_supports_non_clone_items() {
    struct Value(usize);
    let calls = AtomicUsize::new(0);
    let mapped = (0..1000).parallelize().map(|n| {
        calls.fetch_add(1, Ordering::Relaxed);
        Value(n)
    });
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    let result = pool
        .install(|| mapped.reduce(|a, b| Value(a.0 + b.0)))
        .unwrap();
    assert_eq!(result.0, (0..1000).sum());
    assert_eq!(calls.load(Ordering::Relaxed), 1000);
    assert_eq!((0..10).parallelize().map(Value).collect().len(), 10);
}

#[test]
fn fill_rejects_mismatch_before_running_mapper() {
    let calls = AtomicUsize::new(0);
    let mut output = [99; 2];
    assert!(
        catch_unwind(AssertUnwindSafe(|| (0..3)
            .parallelize()
            .map(|n| {
                calls.fetch_add(1, Ordering::Relaxed);
                n
            })
            .fill(&mut output)))
        .is_err()
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(output, [99; 2]);
}

#[test]
fn iterator_panics_propagate_and_pool_remains_usable() {
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    assert!(
        catch_unwind(AssertUnwindSafe(|| pool.install(|| {
            (0..4000).parallelize().for_each(|n| {
                assert_ne!(n, 7);
            });
        })))
        .is_err()
    );
    assert_eq!(
        pool.install(|| (0..1000).parallelize().reduce(|a, b| a + b)),
        Some(499500)
    );
}

#[test]
fn fold_clones_only_at_splits_and_ordered_collection_is_stable() {
    struct Acc<'a> {
        total: usize,
        clones: &'a AtomicUsize,
    }
    impl Clone for Acc<'_> {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                total: self.total,
                clones: self.clones,
            }
        }
    }
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    let clones = AtomicUsize::new(0);
    let result = pool.install(|| {
        (0..4096).parallelize().fold(
            Acc {
                total: 0,
                clones: &clones,
            },
            |mut a, n| {
                a.total += n;
                a
            },
            |mut a, b| {
                a.total += b.total;
                a
            },
        )
    });
    assert_eq!(result.total, (0..4096).sum());
    assert!(clones.load(Ordering::Relaxed) < 16);
    assert_eq!(
        pool.install(|| (0..2000).parallelize().map(|n| n.to_string()).collect()),
        (0..2000).map(|n| n.to_string()).collect::<Vec<_>>()
    );
}
