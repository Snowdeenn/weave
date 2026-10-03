use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use weave::{ThreadPoolBuilder, WorkerLocal, iter::*};
struct Capture<'a>(&'a AtomicUsize);
impl Drop for Capture<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn scoped_borrows_descendants_panics_and_detached_handle() {
    let pool = ThreadPoolBuilder::new().num_threads(1).build();
    let mut values = [0; 4];
    let drops = AtomicUsize::new(0);
    pool.scope(|s| {
        for v in &mut values {
            s.spawn(move || *v = 7);
        }
        let capture = Capture(&drops);
        s.spawn(move || {
            drop(capture);
            s.spawn(|| {});
        });
        drop(s.submit(|| 42));
    });
    assert_eq!(values, [7; 4]);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    let mut value = 0;
    assert!(
        catch_unwind(AssertUnwindSafe(|| pool.scope(|s| {
            s.spawn(|| value = 9);
            panic!("body");
        })))
        .is_err()
    );
    assert_eq!(value, 9);
}
#[test]
fn nested_cross_pool_and_worker_local_borrows() {
    let a = ThreadPoolBuilder::new().num_threads(1).build();
    let b = ThreadPoolBuilder::new().num_threads(1).build();
    let local = WorkerLocal::new(&a, || 0);
    let result = a.install(|| {
        b.install(|| {
            a.install(|| {
                let (a, b) = a.join(|| 20, || 22);
                local.with(|n| *n += 1);
                a + b
            })
        })
    });
    assert_eq!(result, 42);
    assert_eq!(local.into_inner(), vec![1]);
}
#[test]
fn parallel_mutable_slices_and_ordered_search() {
    let pool = ThreadPoolBuilder::new().num_threads(2).build();
    let mut values = vec![0; 513];
    pool.install(|| values.iter_parallel_mut().for_each(|n| *n = 3));
    assert_eq!(values, vec![3; 513]);
    assert_eq!(
        pool.install(|| (0..513)
            .parallelize()
            .enumerate()
            .find_first(|(_, n)| *n == 3)),
        Some((3, 3))
    );
}
#[test]
fn abandoned_handle_and_panic_release_owned_captures() {
    struct Owned(Arc<AtomicUsize>);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = ThreadPoolBuilder::new().num_threads(1).build();
    let value = Owned(drops.clone());
    drop(pool.submit(move || value));
    let value = Owned(drops.clone());
    assert!(
        pool.submit(move || {
            drop(value);
            panic!("task");
        })
        .join()
        .is_err()
    );
    drop(pool);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}
