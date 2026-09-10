use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use weave::{BuildError, Job, Priority, ThreadPoolBuilder, WorkerLocal, WorkerLocalError};

// A stalled pool fails its test instead of hanging the whole test process.
fn bounded(f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
    });
    match rx
        .recv_timeout(Duration::from_secs(15))
        .expect("pool operation timed out")
    {
        Ok(()) => (),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[test]
fn builder_options_are_independent_and_zero_is_rejected() {
    bounded(|| {
        assert!(matches!(
            ThreadPoolBuilder::new().num_threads(0).try_build(),
            Err(BuildError::ZeroThreads)
        ));
        assert!(matches!(
            ThreadPoolBuilder::new()
                .thread_name("bad\0name")
                .try_build(),
            Err(BuildError::InvalidThreadName)
        ));
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        assert_eq!(pool.num_threads(), 1);
        assert_eq!(
            pool.submit(|| std::thread::current().name().unwrap().to_owned())
                .join()
                .unwrap(),
            "weave-0"
        );
        let named = ThreadPoolBuilder::new().thread_name("custom").build();
        assert!(
            named
                .submit(|| std::thread::current()
                    .name()
                    .unwrap()
                    .starts_with("custom-"))
                .join()
                .unwrap()
        );
    });
}

#[test]
fn idle_workers_wake_and_drop_drains_all_jobs() {
    bounded(|| {
        let completed = Arc::new(AtomicUsize::new(0));
        let pool = ThreadPoolBuilder::new().num_threads(4).build();
        std::thread::sleep(Duration::from_millis(20));
        for _ in 0..2000 {
            let completed = completed.clone();
            pool.spawn(move || {
                completed.fetch_add(1, Ordering::Relaxed);
            });
        }
        drop(pool);
        assert_eq!(completed.load(Ordering::Relaxed), 2000);
    });
}

#[test]
fn handles_report_results_panics_and_detach() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let (tx, rx) = mpsc::channel();
        let handle = pool.submit(move || {
            rx.recv().unwrap();
            42
        });
        assert!(!handle.is_done());
        tx.send(()).unwrap();
        assert_eq!(handle.join().unwrap(), 42);
        let failure = pool
            .submit(|| panic!("original payload"))
            .join()
            .unwrap_err();
        assert_eq!(failure.downcast_ref::<&str>(), Some(&"original payload"));
        pool.spawn(|| panic!("detached failure"));
        assert_eq!(pool.submit(|| 7).join().unwrap(), 7);
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        drop(pool.submit(move || c.fetch_add(1, Ordering::Relaxed)));
        drop(pool);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    });
}

#[test]
fn nested_submit_join_and_install_work_on_one_worker() {
    bounded(|| {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(1).build());
        let p = pool.clone();
        let result = pool
            .submit(move || {
                let a = p.submit(|| 20).join().unwrap();
                let (b, c) = p.join(|| 10, || p.install(|| 12));
                a + b + c
            })
            .join()
            .unwrap();
        assert_eq!(result, 42);
    });
}

#[test]
fn join_waits_for_borrowed_branch_when_other_panics() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(2).build();
        let mut value = 0;
        let result = catch_unwind(AssertUnwindSafe(|| {
            pool.join(
                || {
                    std::thread::sleep(Duration::from_millis(10));
                    value = 42;
                },
                || panic!("right"),
            );
        }));
        assert!(result.is_err());
        assert_eq!(value, 42);
        let result = catch_unwind(AssertUnwindSafe(|| {
            pool.join(|| panic!("left"), || panic!("right"))
        }));
        assert_eq!(result.unwrap_err().downcast_ref::<&str>(), Some(&"left"));
        assert_eq!(pool.submit(|| 5).join().unwrap(), 5);
    });
}

#[test]
fn scope_borrows_returns_values_and_waits_for_descendants() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let mut values = vec![0; 24];
        let count = AtomicUsize::new(0);
        let result = pool.scope(|s| {
            for (i, value) in values.iter_mut().enumerate() {
                s.spawn(move || *value = i);
            }
            s.spawn(|| {
                s.spawn(|| {
                    count.fetch_add(1, Ordering::Relaxed);
                })
            });
            s.submit(|| 42).join().unwrap()
        });
        assert_eq!(result, 42);
        assert_eq!(values, (0..24).collect::<Vec<_>>());
        assert_eq!(count.load(Ordering::Relaxed), 1);
        pool.install(|| {
            pool.scope(|s| {
                s.spawn(|| s.spawn(|| {}));
            })
        });
    });
}

#[test]
fn scope_waits_on_body_panic_and_propagates_task_panic() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(2).build();
        let mut value = 0;
        let error = catch_unwind(AssertUnwindSafe(|| {
            pool.scope(|s| {
                s.spawn(|| {
                    std::thread::sleep(Duration::from_millis(10));
                    value = 1;
                });
                panic!("body");
            })
        }))
        .unwrap_err();
        assert_eq!(value, 1);
        assert_eq!(error.downcast_ref::<&str>(), Some(&"body"));
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.scope(|s| {
                s.spawn(|| panic!("scoped"));
            })))
            .is_err()
        );
        assert_eq!(pool.submit(|| 42).join().unwrap(), 42);
    });
}

#[test]
fn scoped_submission_panics_are_observed_or_propagated() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        pool.scope(|s| {
            assert!(s.submit(|| panic!("handled")).join().is_err());
        });
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.scope(|s| {
                drop(s.submit(|| panic!("unhandled")));
            })))
            .is_err()
        );
        // Forgetting a handle must not let borrowing work escape the scope.
        let mut value = 0;
        pool.scope(|s| {
            std::mem::forget(s.submit(|| {
                value = 9;
            }))
        });
        assert_eq!(value, 9);
    });
}

#[test]
fn priorities_select_queued_work_without_preemption() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let gate = Arc::new(Barrier::new(2));
        let entered = Arc::new(Barrier::new(2));
        let (g, e) = (gate.clone(), entered.clone());
        pool.spawn(move || {
            e.wait();
            g.wait();
        });
        entered.wait();
        let order = Arc::new(Mutex::new(Vec::new()));
        for (priority, value) in [
            (Priority::Low, 1),
            (Priority::High, 2),
            (Priority::Normal, 3),
            (Priority::High, 4),
        ] {
            let order = order.clone();
            pool.spawn(
                Job::new(move || order.lock().unwrap().push(value))
                    .set_priority(priority)
                    .set_label("test"),
            );
        }
        gate.wait();
        drop(pool);
        assert_eq!(*order.lock().unwrap(), vec![2, 4, 3, 1]);
    });
}

#[test]
fn workers_really_steal_locally_spawned_jobs() {
    bounded(|| {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(2).build());
        let p = pool.clone();
        pool.submit(move || {
            let owner = std::thread::current().id();
            let (tx, rx) = mpsc::channel();
            p.spawn(move || {
                tx.send(std::thread::current().id()).unwrap();
            });
            // This worker does not help; only a thief can execute its child.
            let thief = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_ne!(owner, thief);
        })
        .join()
        .unwrap();
        assert!(pool.steal_count() >= 1);
    });
}

#[test]
fn worker_local_is_pool_bound_and_detects_recursive_access() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let other = ThreadPoolBuilder::new().num_threads(1).build();
        let local = WorkerLocal::new(&pool, || 0);
        assert_eq!(local.try_with(|_| ()), Err(WorkerLocalError::WrongPool));
        other.install(|| assert_eq!(local.try_with(|_| ()), Err(WorkerLocalError::WrongPool)));
        pool.install(|| {
            local.with(|value| {
                *value += 1;
                assert_eq!(
                    local.try_with(|_| ()),
                    Err(WorkerLocalError::AlreadyBorrowed)
                );
            });
        });
        assert_eq!(local.into_inner(), vec![1]);
    });
}

#[test]
fn worker_local_reports_poison_and_recovers_owned_values() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let local = WorkerLocal::new(&pool, || 0);
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.install(|| local.with(|v| {
                *v = 8;
                panic!("callback");
            }))))
            .is_err()
        );
        pool.install(|| assert_eq!(local.try_with(|_| ()), Err(WorkerLocalError::Poisoned)));
        assert_eq!(local.into_inner(), vec![8]);
    });
}

#[test]
fn pool_can_be_dropped_by_its_last_worker_owner() {
    bounded(|| {
        let pool = Arc::new(ThreadPoolBuilder::new().num_threads(2).build());
        let p = pool.clone();
        let (tx, rx) = mpsc::channel();
        let result = pool.submit(move || {
            rx.recv().unwrap();
            drop(p);
            42
        });
        drop(pool);
        tx.send(()).unwrap();
        assert_eq!(result.join().unwrap(), 42);
    });
}

#[test]
fn cross_pool_install_preserves_original_worker_context() {
    bounded(|| {
        let a = ThreadPoolBuilder::new()
            .num_threads(1)
            .thread_name("a")
            .build();
        let b = ThreadPoolBuilder::new()
            .num_threads(1)
            .thread_name("b")
            .build();
        a.install(|| {
            assert_eq!(std::thread::current().name(), Some("a-0"));
            b.install(|| {
                assert_eq!(std::thread::current().name(), Some("b-0"));
                assert_eq!(a.install(|| 42), 42);
            });
            assert_eq!(std::thread::current().name(), Some("a-0"));
        });
    });
}

#[test]
fn every_worker_has_a_distinct_local_slot() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(4).build();
        let values = WorkerLocal::with_index(&pool, |index| (index, 0));
        let gate = Barrier::new(4);
        pool.scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    values.with(|(_, count)| *count += 1);
                    gate.wait();
                });
            }
        });
        assert_eq!(values.into_inner(), vec![(0, 1), (1, 1), (2, 1), (3, 1)]);
    });
}

#[test]
fn concurrent_producers_execute_every_job_exactly_once() {
    bounded(|| {
        let pool = ThreadPoolBuilder::new().num_threads(4).build();
        let counts = Arc::new((0..4000).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
        std::thread::scope(|s| {
            for producer in 0..4 {
                let pool = &pool;
                let counts = &counts;
                s.spawn(move || {
                    for index in producer * 1000..(producer + 1) * 1000 {
                        let counts = counts.clone();
                        pool.spawn(move || {
                            counts[index].fetch_add(1, Ordering::Relaxed);
                        });
                    }
                });
            }
        });
        drop(pool);
        assert!(counts.iter().all(|n| n.load(Ordering::Relaxed) == 1));
    });
}

#[test]
fn scope_catches_panicking_result_destructors_without_hanging() {
    bounded(|| {
        struct BadDrop;
        impl Drop for BadDrop {
            fn drop(&mut self) {
                panic!("result destructor");
            }
        }
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let (tx, rx) = mpsc::channel();
        let result = catch_unwind(AssertUnwindSafe(|| {
            pool.scope(|s| {
                let handle = s.submit(move || {
                    rx.recv().unwrap();
                    BadDrop
                });
                drop(handle);
                tx.send(()).unwrap();
            })
        }));
        assert!(result.is_err());
        assert_eq!(pool.submit(|| 42).join().unwrap(), 42);
    });
}
