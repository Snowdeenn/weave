use crate::{
    Job, JoinHandle, Priority, ThreadPool,
    handle::JobState,
    pool::{SharedPoolData, discard, help_current_worker},
};
use std::{
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
type Panic = Box<dyn std::any::Any + Send + 'static>;
struct GroupState {
    pending: usize,
    panic: Option<Panic>,
    submissions: Vec<Arc<AtomicBool>>,
}
struct Group {
    state: Mutex<GroupState>,
    wake: Condvar,
}

// Register before publication and settle on both execution and rejection.
// Job stores the ticket outside its erased FnOnce invocation.
struct GroupTicket {
    group: Arc<Group>,
}
impl GroupTicket {
    fn register(group: Arc<Group>) -> Self {
        let mut state = group.state.lock().unwrap();
        let Some(next) = state.pending.checked_add(1) else {
            drop(state);
            panic!("scope task count overflow");
        };
        state.pending = next;
        drop(state);
        Self { group }
    }
}
impl Drop for GroupTicket {
    fn drop(&mut self) {
        let mut state = self.group.state.lock().unwrap();
        state.pending -= 1;
        self.group.wake.notify_all();
    }
}
/// A region in which tasks may borrow external data.
/// All tasks finish before the region exits, even if its body panics.
/// Obtain a scope through [ThreadPool::scope].
///
/// Borrows created inside the body cannot outlive that body:
/// ```compile_fail
/// let pool = weave::ThreadPool::default();
/// pool.scope(|s| {
///     let temporary = String::from("too short");
///     s.spawn(|| println!("{temporary}"));
/// });
/// ```
///
/// A handle cannot carry a borrow of a body-local value out of the scope:
/// ```compile_fail
/// let pool = weave::ThreadPool::default();
/// let handle = pool.scope(|s| {
///     let temporary = String::from("too short");
///     s.submit(|| temporary.as_str())
/// });
/// println!("{}", handle.join().unwrap());
/// ```
///
/// Descendant tasks cannot borrow their parent's temporary stack values:
/// ```compile_fail
/// let pool = weave::ThreadPool::default();
/// pool.scope(|s| {
///     s.spawn(|| {
///         let temporary = String::from("too short");
///         s.spawn(|| println!("{temporary}"));
///     });
/// });
/// ```
pub struct Scope<'scope, 'env: 'scope> {
    shared: Arc<SharedPoolData>,
    group: Arc<Group>,
    scope: PhantomData<&'scope mut &'scope ()>,
    env: PhantomData<&'env mut &'env ()>,
}
impl ThreadPool {
    /// Execute a region of borrowing tasks, waiting for all descendants.
    /// An unjoined failed submission causes a scope panic. A joined failure
    /// belongs to its caller. A panic in the body takes precedence.
    pub fn scope<'env, F, R>(&self, f: F) -> R
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> R,
    {
        let scope = Scope {
            shared: self.shared.clone(),
            group: Arc::new(Group {
                state: Mutex::new(GroupState {
                    pending: 0,
                    panic: None,
                    submissions: Vec::new(),
                }),
                wake: Condvar::new(),
            }),
            scope: PhantomData,
            env: PhantomData,
        };
        let body = catch_unwind(AssertUnwindSafe(|| f(&scope)));
        scope.wait();
        let (panic, unjoined) = {
            let mut state = scope.group.state.lock().unwrap();
            (
                state.panic.take(),
                state
                    .submissions
                    .iter()
                    .any(|failed| failed.load(Ordering::Acquire)),
            )
        };
        match body {
            Err(payload) => {
                discard(panic);
                resume_unwind(payload)
            }
            Ok(value) => {
                if let Some(payload) = panic {
                    discard(value);
                    resume_unwind(payload);
                }
                if unjoined {
                    discard(value);
                    panic!("an unjoined scoped task panicked");
                }
                value
            }
        }
    }
}
impl<'scope, 'env> Scope<'scope, 'env> {
    /// Schedule a borrowing task.
    pub fn spawn(&'scope self, f: impl FnOnce() + Send + 'scope) {
        self.spawn_with_priority(Priority::Normal, f);
    }
    /// Schedule a borrowing task at a chosen priority.
    pub fn spawn_with_priority(&'scope self, priority: Priority, f: impl FnOnce() + Send + 'scope) {
        self.spawn_task(priority, f, None);
    }
    fn spawn_task(
        &'scope self,
        priority: Priority,
        f: impl FnOnce() + Send + 'scope,
        finished: Option<Arc<AtomicBool>>,
    ) {
        let ticket = GroupTicket::register(self.group.clone());
        let group = self.group.clone();
        let task: Box<dyn FnOnce() + Send + 'scope> = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(f));
            let mut state = group.state.lock().unwrap();
            let extra = match result {
                Err(payload) if state.panic.is_none() => {
                    state.panic = Some(payload);
                    None
                }
                Err(payload) => Some(payload),
                Ok(()) => None,
            };
            drop(state);
            discard(extra);
        });
        // SAFETY: completion is OUTSIDE this FnOnce frame. Job::run drops its
        // cleanup only after task() returns/unwinds. On rejection, Job's field
        // order drops task and all captures before cleanup. scope waits for
        // every ticket, including descendants, even when its body panics.
        let mut job = unsafe { Job::from_borrowed(task, priority) }.with_cleanup(ticket);
        if let Some(finished) = finished {
            job = job.with_finish_signal(finished);
        }
        self.shared.enqueue(job);
    }

    /// Submit a borrowing task and receive its result.
    pub fn submit<T: Send + 'scope>(
        &'scope self,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> JoinHandle<T> {
        self.submit_with_priority(Priority::Normal, f)
    }
    /// Submit borrowing work at a chosen priority.
    pub fn submit_with_priority<T: Send + 'scope>(
        &'scope self,
        priority: Priority,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> JoinHandle<T> {
        let (state, mut handle) = JobState::channel();
        let failed = Arc::new(AtomicBool::new(false));
        handle.scoped_failure = Some(failed.clone());
        self.group
            .state
            .lock()
            .unwrap()
            .submissions
            .push(failed.clone());
        let finished = state.finished.clone();
        self.spawn_task(
            priority,
            move || {
                let result = catch_unwind(AssertUnwindSafe(f));
                failed.store(result.is_err(), Ordering::Release);
                state.complete(result);
                drop(state);
            },
            Some(finished),
        );
        handle
    }
    fn wait(&self) {
        loop {
            if self.group.state.lock().unwrap().pending == 0 {
                return;
            }
            if help_current_worker() {
                continue;
            }
            let state = self.group.state.lock().unwrap();
            if state.pending == 0 {
                return;
            }
            drop(
                self.group
                    .wake
                    .wait_timeout(state, std::time::Duration::from_millis(1))
                    .unwrap(),
            );
        }
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::{ThreadPoolBuilder, pool::reject_next_submission};
    use std::sync::atomic::AtomicUsize;

    struct Capture<'a>(&'a AtomicUsize);
    impl Drop for Capture<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn rejected_scoped_submission_settles_ticket_and_drains_accepted_borrows() {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let drops = AtomicUsize::new(0);
        let mut borrowed = 0;
        let result = catch_unwind(AssertUnwindSafe(|| {
            pool.scope(|s| {
                s.spawn(|| borrowed = 42);
                let capture = Capture(&drops);
                reject_next_submission();
                drop(s.submit(move || {
                    drop(capture);
                    7
                }));
            })
        }));
        assert!(result.is_err());
        assert_eq!(borrowed, 42);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(pool.submit(|| 9).join().unwrap(), 9);
    }

    #[test]
    fn caught_publication_failure_allows_more_scoped_work() {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let drops = AtomicUsize::new(0);
        let mut borrowed = 0;
        pool.scope(|s| {
            let capture = Capture(&drops);
            reject_next_submission();
            assert!(
                catch_unwind(AssertUnwindSafe(|| {
                    s.spawn(move || drop(capture));
                }))
                .is_err()
            );
            s.spawn(|| borrowed = 5);
        });
        assert_eq!(borrowed, 5);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn rejected_join_drops_borrowed_branch_without_running_right() {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let drops = AtomicUsize::new(0);
        let right_calls = AtomicUsize::new(0);
        let capture = Capture(&drops);
        reject_next_submission();
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.join(
                move || drop(capture),
                || {
                    right_calls.fetch_add(1, Ordering::SeqCst);
                },
            )))
            .is_err()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(right_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn panicking_capture_destructor_still_drains_scope() {
        struct BadCapture<'a>(&'a AtomicUsize);
        impl Drop for BadCapture<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("capture destructor");
            }
        }
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let drops = AtomicUsize::new(0);
        let mut borrowed = 0;
        assert!(
            catch_unwind(AssertUnwindSafe(|| pool.scope(|s| {
                let bad = BadCapture(&drops);
                s.spawn(move || {
                    let _capture = bad;
                });
                s.spawn(|| borrowed = 9);
            })))
            .is_err()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(borrowed, 9);
        assert_eq!(pool.submit(|| 7).join().unwrap(), 7);
    }
}
