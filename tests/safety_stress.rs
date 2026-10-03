use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use weave::{Job, Priority, ThreadPool, ThreadPoolBuilder, iter::*};

struct DropProbe(Arc<AtomicUsize>);
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

// A deadlock fails the test rather than parking the test process indefinitely.
fn bounded(f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
    });
    match rx
        .recv_timeout(Duration::from_secs(60))
        .expect("stress deadline exceeded")
    {
        Ok(()) => (),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[test]
fn closures_and_results_drop_once_on_success_panic_and_detach() {
    bounded(|| {
        let captures = Arc::new(AtomicUsize::new(0));
        let results = Arc::new(AtomicUsize::new(0));
        let probe = DropProbe(captures.clone());
        drop(Job::new(move || drop(probe))); // Never submitted.
        for workers in [1, 4] {
            let pool = ThreadPoolBuilder::new().num_threads(workers).build();
            let probe = DropProbe(captures.clone());
            pool.spawn(move || drop(probe));
            let probe = DropProbe(captures.clone());
            assert!(
                pool.submit(move || {
                    drop(probe);
                    panic!("expected");
                })
                .join()
                .is_err()
            );
            let probe = DropProbe(captures.clone());
            let result = DropProbe(results.clone());
            let (tx, rx) = mpsc::channel();
            let handle = pool.submit(move || {
                rx.recv().unwrap();
                drop(probe);
                result
            });
            drop(handle); // Result must be destroyed on completion.
            tx.send(()).unwrap();
            let result = pool
                .submit({
                    let results = results.clone();
                    move || DropProbe(results)
                })
                .join()
                .unwrap();
            drop(result); // Joined result is now caller-owned.
            drop(pool);
        }
        assert_eq!(captures.load(Ordering::SeqCst), 7);
        assert_eq!(results.load(Ordering::SeqCst), 4);
    });
}

#[test]
fn panicking_scope_drains_descendants_and_releases_captures() {
    bounded(|| {
        for workers in [1, 4] {
            let pool = ThreadPoolBuilder::new().num_threads(workers).build();
            let drops = Arc::new(AtomicUsize::new(0));
            let done = AtomicUsize::new(0);
            let mut borrowed = 0;
            assert!(
                catch_unwind(AssertUnwindSafe(|| pool.scope(|s| {
                    let probe = DropProbe(drops.clone());
                    let done = &done;
                    s.spawn(move || {
                        drop(probe);
                        s.spawn(move || {
                            done.fetch_add(1, Ordering::SeqCst);
                        });
                    });
                    s.spawn(|| borrowed = 42);
                    let probe = DropProbe(drops.clone());
                    s.spawn(move || {
                        drop(probe);
                        panic!("child panic");
                    });
                    panic!("body wins");
                })))
                .is_err()
            );
            assert_eq!(borrowed, 42);
            assert_eq!(done.load(Ordering::SeqCst), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 2);
            assert_eq!(pool.submit(|| 42).join().unwrap(), 42);
        }
    });
}

#[test]
fn worker_owned_shutdown_drains_children_and_releases_captures() {
    bounded(|| {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(1).build());
        let owner = pool.clone();
        let drops = Arc::new(AtomicUsize::new(0));
        let counts = drops.clone();
        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let handle = pool.submit(move || {
            start_rx.recv().unwrap();
            for _ in 0..32 {
                let probe = DropProbe(counts.clone());
                let done = done_tx.clone();
                owner.spawn(move || {
                    drop(probe);
                    done.send(()).unwrap();
                });
            }
            drop(owner); // Last ThreadPool owner; it cannot join this worker.
            drop(done_tx);
        });
        drop(pool);
        start_tx.send(()).unwrap();
        handle.join().unwrap();
        for _ in 0..32 {
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(drops.load(Ordering::SeqCst), 32);
    });
}

#[test]
fn seeded_external_wake_idle_and_shutdown_stress() {
    bounded(|| {
        let rounds: usize = std::env::var("WEAVE_STRESS_ROUNDS")
            .ok()
            .map(|v| v.parse().unwrap())
            .unwrap_or(24);
        let seed: u64 = std::env::var("WEAVE_STRESS_SEED")
            .ok()
            .map(|v| v.parse().unwrap())
            .unwrap_or(0x5eed);
        for workers in [1, 2, 4] {
            for round in 0..rounds {
                let pool = ThreadPoolBuilder::new().num_threads(workers).build();
                let counts = Arc::new((0..128).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
                let drops = Arc::new(AtomicUsize::new(0));
                // Include idle parking, wake waves and empty drains.
                std::thread::sleep(Duration::from_micros(100));
                let gate = Barrier::new(4);
                std::thread::scope(|s| {
                    for producer in 0..4 {
                        let (pool, gate, counts, drops) = (&pool, &gate, &counts, &drops);
                        s.spawn(move || {
                            gate.wait();
                            let mut rng =
                                seed ^ (round as u64).wrapping_mul(7919) ^ producer as u64;
                            for offset in 0..32 {
                                rng ^= rng << 13;
                                rng ^= rng >> 7;
                                rng ^= rng << 17;
                                let index = producer * 32 + offset;
                                let counts = counts.clone();
                                let probe = DropProbe(drops.clone());
                                let fail = rng.is_multiple_of(17);
                                pool.spawn(move || {
                                    counts[index].fetch_add(1, Ordering::SeqCst);
                                    drop(probe);
                                    if fail {
                                        panic!("seeded task panic");
                                    }
                                });
                                if rng.is_multiple_of(5) {
                                    std::thread::yield_now();
                                }
                            }
                        });
                    }
                });
                drop(pool);
                assert!(counts.iter().all(|n| n.load(Ordering::SeqCst) == 1));
                assert_eq!(drops.load(Ordering::SeqCst), 128);
                drop(ThreadPoolBuilder::new().num_threads(workers).build());
            }
        }
    });
}

fn recursive(pool: &ThreadPool, depth: usize) -> usize {
    if depth == 0 {
        return 1;
    }
    let (a, b) = pool.join(|| recursive(pool, depth - 1), || recursive(pool, depth - 1));
    a + b
}

#[test]
fn external_producers_nest_across_single_worker_pools() {
    bounded(|| {
        let a = ThreadPoolBuilder::new()
            .num_threads(1)
            .thread_name("a")
            .build();
        let b = ThreadPoolBuilder::new()
            .num_threads(1)
            .thread_name("b")
            .build();
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..16 {
                        assert_eq!(
                            a.install(|| b.install(|| a.install(|| recursive(&a, 5)))),
                            32
                        );
                        a.install(|| assert_eq!(std::thread::current().name(), Some("a-0")));
                    }
                });
            }
        });
    });
}

fn high_chain(pool: Arc<ThreadPool>, left: usize, order: Arc<Mutex<Vec<Priority>>>) {
    let target = pool.clone();
    pool.spawn_with_priority(Priority::High, move || {
        order.lock().unwrap().push(Priority::High);
        if left > 1 {
            high_chain(target, left - 1, order);
        }
    });
}

#[test]
fn sustained_high_priority_starves_lower_queues_until_stream_stops() {
    bounded(|| {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(1).build());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.spawn(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        for priority in [Priority::Low, Priority::Normal] {
            let order = order.clone();
            pool.spawn_with_priority(priority, move || order.lock().unwrap().push(priority));
        }
        high_chain(pool.clone(), 64, order.clone());
        release_tx.send(()).unwrap();
        // The final high closure releases the last extra owner before draining.
        // Wait for low through a marker, since Arc::drop alone need not drain.
        let (tx, rx) = mpsc::channel();
        pool.spawn_with_priority(Priority::Low, move || tx.send(()).unwrap());
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(pool);
        let order = order.lock().unwrap();
        assert_eq!(&order[..64], vec![Priority::High; 64]);
        assert_eq!(&order[64..], &[Priority::Normal, Priority::Low]);
    });
}

#[test]
fn ordered_results_and_search_do_not_promise_callback_order() {
    let pool = ThreadPoolBuilder::new().num_threads(1).build();
    let visited = Mutex::new(Vec::new());
    let output = pool.install(|| {
        (0..1024)
            .parallelize()
            .map(|n| {
                visited.lock().unwrap().push(n);
                n
            })
            .collect()
    });
    assert_eq!(output, (0..1024).collect::<Vec<_>>());
    assert_eq!(visited.lock().unwrap()[0], 512); // Right leaf runs locally first.
    assert_eq!(
        pool.install(|| (0..1024)
            .parallelize()
            .find_first(|n| *n == 10 || *n == 900)),
        Some(10)
    );
    assert!(matches!(
        pool.install(|| (0..1024).parallelize().find_any(|n| *n == 10 || *n == 900)),
        Some(10 | 900)
    ));
}

#[test]
fn floating_reduction_can_differ_from_sequential_grouping() {
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    let mut values = vec![1.0_f64; 1024];
    values[0] = 1e16;
    values[512] = -1e16;
    let sequential = values.iter().copied().reduce(|a, b| a + b).unwrap();
    let parallel = pool
        .install(|| values.iter_parallel().map(|n| *n).reduce(|a, b| a + b))
        .unwrap();
    assert_eq!(sequential, 511.0);
    assert_eq!(parallel, 0.0);
}
