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
        self.group.state.lock().unwrap().pending += 1;
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
            // Drop extra panic payloads outside the lock and before declaring completion.
            drop(state);
            discard(extra);
            let mut state = group.state.lock().unwrap();
            state.pending -= 1;
            group.wake.notify_all();
        });
        // SAFETY: Scope is invariant in 'scope and cannot be constructed publicly.
        // ThreadPool::scope catches the body panic and waits for every task,
        // including descendants, before returning or unwinding. Captures are
        // consumed/dropped before pending is decremented.
        let task = unsafe {
            std::mem::transmute::<
                Box<dyn FnOnce() + Send + 'scope>,
                Box<dyn FnOnce() + Send + 'static>,
            >(task)
        };
        self.shared.enqueue(Job::from_raw(task, priority));
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
        self.spawn_with_priority(priority, move || {
            let result = catch_unwind(AssertUnwindSafe(f));
            failed.store(result.is_err(), Ordering::Release);
            state.complete(result);
            drop(state);
        });
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
