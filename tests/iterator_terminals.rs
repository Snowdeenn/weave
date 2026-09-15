use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicUsize, Ordering},
};
use weave::{ThreadPoolBuilder, iter::*};

#[test]
fn aggregates_match_sequential_at_split_boundaries() {
    for workers in [1, 2, 4] {
        let pool = ThreadPoolBuilder::new().num_threads(workers).build();
        pool.install(|| {
            for len in [0, 1, 511, 512, 513, 1025, 4097] {
                let source = || {
                    (0..len)
                        .parallelize()
                        .filter_map(|n| (n % 3 == 0).then_some(n))
                };
                let expected: Vec<_> = (0..len).filter(|n| n % 3 == 0).collect();
                assert_eq!(source().count(), expected.len());
                assert_eq!(source().sum::<usize>(), expected.iter().sum::<usize>());
                assert_eq!(source().min(), expected.iter().copied().min());
                assert_eq!(source().max(), expected.iter().copied().max());
            }
            let values = [1i64, -2, 8, -4];
            assert_eq!(values.iter_parallel().sum::<i64>(), 3);
            assert_eq!(values.iter_parallel().min(), Some(&-4));
            assert_eq!(values.iter_parallel().max(), Some(&8));
            assert_eq!([1.25, 2.5, 0.25].iter_parallel().sum::<f64>(), 4.0);
        });
    }
}

#[test]
fn count_evaluates_and_drops_owned_items() {
    struct Item<'a>(&'a AtomicUsize);
    impl Drop for Item<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let drops = AtomicUsize::new(0);
    let pool = ThreadPoolBuilder::new().num_threads(4).build();
    assert_eq!(
        pool.install(|| (0..4097).parallelize().map(|_| Item(&drops)).count()),
        4097
    );
    assert_eq!(drops.load(Ordering::Relaxed), 4097);
}

#[test]
fn sum_supports_a_distinct_non_clone_accumulator() {
    struct Total(usize);
    impl std::iter::Sum<usize> for Total {
        fn sum<I: Iterator<Item = usize>>(items: I) -> Self {
            Self(items.sum())
        }
    }
    impl std::iter::Sum for Total {
        fn sum<I: Iterator<Item = Self>>(items: I) -> Self {
            Self(items.map(|x| x.0).sum())
        }
    }
    let pool = ThreadPoolBuilder::new().num_threads(4).build();
    assert_eq!(
        pool.install(|| (0..4097).parallelize().sum::<Total>()).0,
        (0..4097).sum()
    );
    assert_eq!((0..0).parallelize().sum::<Total>().0, 0);
}

#[test]
fn extrema_preserve_tie_semantics_without_clone() {
    #[derive(Debug)]
    struct Item {
        key: usize,
        position: usize,
    }
    impl PartialEq for Item {
        fn eq(&self, other: &Self) -> bool {
            self.key == other.key
        }
    }
    impl Eq for Item {}
    impl PartialOrd for Item {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Item {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            self.key.cmp(&other.key)
        }
    }
    let pool = ThreadPoolBuilder::new().num_threads(4).build();
    pool.install(|| {
        let source = || {
            (0..4097).parallelize().map(|position| Item {
                key: position % 7,
                position,
            })
        };
        assert_eq!(source().min().unwrap().position, 0);
        assert_eq!(source().max().unwrap().position, 4094);
    });
}

#[test]
fn searches_handle_filtered_order_empty_and_missing_matches() {
    for workers in [1, 2, 4] {
        let pool = ThreadPoolBuilder::new().num_threads(workers).build();
        pool.install(|| {
            for len in [0, 1, 511, 512, 513, 1025, 4097] {
                for start in [0, 1, 511, 512, 513, 2049, 4096, 5000] {
                    let source = || {
                        (0..len)
                            .parallelize()
                            .filter(|n| n % 2 == 0)
                            .filter_map(|n| (n % 3 != 1).then_some(n))
                            .map(|n| n + 1)
                    };
                    let expected = (0..len)
                        .filter(|n| n % 2 == 0 && n % 3 != 1)
                        .map(|n| n + 1)
                        .find(|n| *n >= start);
                    assert_eq!(source().find_first(|n| *n >= start), expected);
                    assert_eq!(source().find(|n| *n >= start), expected);
                    let any = source().find_any(|n| *n >= start);
                    assert_eq!(any.is_some(), expected.is_some());
                    if let Some(n) = any {
                        assert!(n >= start && n <= len && (n - 1) % 2 == 0 && (n - 1) % 3 != 1);
                    }
                }
            }
        });
    }
}

#[test]
fn searches_stop_upstream_callbacks_on_a_sequential_source() {
    for any in [false, true] {
        let calls = AtomicUsize::new(0);
        let source = (0..10_000)
            .parallelize()
            .map(|n| {
                calls.fetch_add(1, Ordering::Relaxed);
                n
            })
            .filter(|_| true)
            .filter_map(Some);
        let result = if any {
            source.find_any(|n| *n == 3)
        } else {
            source.find_first(|n| *n == 3)
        };
        assert_eq!(result, Some(3));
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }
}

#[test]
fn searches_move_non_sync_values_and_return_borrowed_items() {
    let pool = ThreadPoolBuilder::new().num_threads(4).build();
    pool.install(|| {
        assert_eq!(
            (0..4097)
                .parallelize()
                .map(Cell::new)
                .find_first(|n| n.get() >= 513)
                .unwrap()
                .get(),
            513
        );
        assert_eq!(
            (0..4097)
                .parallelize()
                .map(Cell::new)
                .find_any(|n| n.get() == 513)
                .unwrap()
                .get(),
            513
        );
        let mut values = vec![0; 4097];
        values[1000] = 7;
        *values.iter_parallel_mut().find(|n| **n == 7).unwrap() = 9;
        assert_eq!(values[1000], 9);
        assert_eq!(values.iter_parallel().find(|n| **n == 9), Some(&9));
    });
}

#[test]
fn find_first_keeps_searching_left_when_right_finishes_first() {
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    let (sender, receiver) = std::sync::mpsc::channel();
    let receiver = std::sync::Mutex::new(receiver);
    let result = pool.install(|| {
        (0..4096).parallelize().find_first(|n| match *n {
            0 => {
                receiver
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("the right partition must run concurrently");
                true
            }
            2048 => {
                sender.send(()).unwrap();
                true
            }
            _ => false,
        })
    });
    assert_eq!(result, Some(0));
}

#[test]
fn searches_cancel_remaining_work_inside_a_pool() {
    let pool = ThreadPoolBuilder::new().num_threads(1).build();
    for any in [false, true] {
        let calls = AtomicUsize::new(0);
        let result = pool.install(|| {
            let source = (0..16_384)
                .parallelize()
                .map(|n| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    n
                })
                .filter(|_| true)
                .filter_map(Some);
            if any {
                source.find_any(|_| true)
            } else {
                source.find_first(|_| true)
            }
        });
        assert!(result.is_some());
        if !any {
            assert_eq!(result, Some(0));
        }
        assert!(calls.load(Ordering::Relaxed) < 512);
    }
}

#[test]
fn extend_appends_owned_values_in_order_and_preserves_empty_destination() {
    let pool = ThreadPoolBuilder::new().num_threads(4).build();
    let mut output = vec![String::from("prefix")];
    pool.install(|| {
        (0..4097)
            .parallelize()
            .filter(|n| n % 3 == 0)
            .map(|n| n.to_string())
            .extend(&mut output)
    });
    let expected: Vec<_> = std::iter::once(String::from("prefix"))
        .chain((0..4097).filter(|n| n % 3 == 0).map(|n| n.to_string()))
        .collect();
    assert_eq!(output, expected);
    (0..0)
        .parallelize()
        .map(|n| n.to_string())
        .extend(&mut output);
    assert_eq!(output, expected);
    let mut set = std::collections::BTreeSet::new();
    pool.install(|| (0..1025).parallelize().extend(&mut set));
    assert_eq!(set, (0..1025).collect());
}

#[test]
fn terminal_panics_propagate_and_pool_recovers() {
    let pool = ThreadPoolBuilder::new().num_threads(4).build();
    for any in [false, true] {
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.install(|| {
                let predicate = |_: &usize| -> bool { panic!("search panic") };
                if any {
                    (0..4097).parallelize().find_any(predicate)
                } else {
                    (0..4097).parallelize().find_first(predicate)
                }
            })))
            .is_err()
        );
    }
    let mut output = vec![42];
    assert!(
        catch_unwind(AssertUnwindSafe(|| pool.install(|| {
            (0..4097)
                .parallelize()
                .map(|n| {
                    assert_ne!(n, 513);
                    n
                })
                .extend(&mut output);
        })))
        .is_err()
    );
    assert_eq!(output, [42]);
    assert_eq!(pool.install(|| (0..10).parallelize().sum::<usize>()), 45);
}
