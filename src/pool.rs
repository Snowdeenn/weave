use crate::{BuildError, IntoJob, Job, JoinHandle, Priority, ThreadPoolBuilder, handle::JobState};
use std::{
    cell::RefCell,
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{Arc, Condvar, Mutex},
};
type Queues = [VecDeque<Job>; 3];
struct Scheduler {
    global: Queues,
    local: Vec<Queues>,
    pending: usize,
    shutdown: bool,
    steals: usize,
}
pub(crate) struct SharedPoolData {
    scheduler: Mutex<Scheduler>,
    wake: Condvar,
}

#[derive(Clone)]
pub(crate) struct WorkerContext {
    pub shared: Arc<SharedPoolData>,
    pub index: usize,
}
thread_local! {
    static CURRENT_WORKER: RefCell<Option<WorkerContext>> = const { RefCell::new(None) };
}
pub(crate) fn current() -> Option<WorkerContext> {
    CURRENT_WORKER.with(|c| c.borrow().clone())
}
pub(crate) fn help_current() -> bool {
    current().is_some_and(|ctx| ctx.shared.help(ctx.index))
}
impl Scheduler {
    fn take(&mut self, index: usize) -> Option<Job> {
        for priority in (0..3).rev() {
            if let Some(job) = self.local[index][priority].pop_back() {
                return Some(job);
            }
            if let Some(job) = self.global[priority].pop_front() {
                return Some(job);
            }
            for offset in 1..self.local.len() {
                let victim = (index + offset) % self.local.len();
                if let Some(job) = self.local[victim][priority].pop_front() {
                    self.steals += 1;
                    return Some(job);
                }
            }
        }
        None
    }
}
impl SharedPoolData {
    pub(crate) fn enqueue(self: &Arc<Self>, job: Job) {
        let local_worker = current()
            .filter(|ctx| Arc::ptr_eq(&ctx.shared, self))
            .map(|ctx| ctx.index);
        let mut scheduler = self.scheduler.lock().unwrap();
        let priority = job.priority().index();
        match local_worker {
            Some(i) => scheduler.local[i][priority].push_back(job),
            None => scheduler.global[priority].push_back(job),
        }
        scheduler.pending += 1;
        self.wake.notify_one();
    }
    fn execute(&self, job: Job) {
        // No scheduler lock is held while user code runs.
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| job.run())) {
            discard(payload);
        }
        let mut scheduler = self.scheduler.lock().unwrap();
        scheduler.pending -= 1;
        self.wake.notify_all();
    }
    fn help(&self, index: usize) -> bool {
        let job = self.scheduler.lock().unwrap().take(index);
        if let Some(job) = job {
            self.execute(job);
            true
        } else {
            false
        }
    }
    fn stop(&self) {
        self.scheduler.lock().unwrap().shutdown = true;
        self.wake.notify_all();
    }
}
/// Fixed-size worker pool with local deques and stealing.
/// External drop drains accepted work. Drop on an owned worker requests
/// shutdown and lets workers finish asynchronously to avoid self-joining.
pub struct ThreadPool {
    pub(crate) shared: Arc<SharedPoolData>,
    threads: Vec<std::thread::JoinHandle<()>>,
    num_threads: usize,
}
impl Default for ThreadPool {
    fn default() -> Self {
        ThreadPoolBuilder::new().build()
    }
}
impl ThreadPool {
    /// Construct a pool; panics if construction fails.
    pub fn new(num_threads: usize, thread_name: String) -> Self {
        Self::try_new(num_threads, thread_name).expect("cannot build weave pool")
    }
    pub(crate) fn try_new(num_threads: usize, thread_name: String) -> Result<Self, BuildError> {
        if num_threads == 0 {
            return Err(BuildError::ZeroThreads);
        }
        if thread_name.contains('\0') {
            return Err(BuildError::InvalidThreadName);
        }
        let shared = Arc::new(SharedPoolData {
            scheduler: Mutex::new(Scheduler {
                global: Default::default(),
                local: (0..num_threads).map(|_| Queues::default()).collect(),
                pending: 0,
                shutdown: false,
                steals: 0,
            }),
            wake: Condvar::new(),
        });
        let mut pool = Self {
            shared,
            threads: Vec::new(),
            num_threads,
        };
        for index in 0..num_threads {
            let shared = pool.shared.clone();
            match std::thread::Builder::new()
                .name(format!("{thread_name}-{index}"))
                .spawn(move || worker_loop(shared, index))
            {
                Ok(thread) => pool.threads.push(thread),
                Err(error) => return Err(BuildError::Spawn(error)),
            }
        }
        Ok(pool)
    }
    /// Worker count.
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }
    /// Count of actual transfers from another worker's local deque.
    pub fn steal_count(&self) -> usize {
        self.shared.scheduler.lock().unwrap().steals
    }
    /// Schedule detached work. The panic hook reports failures; workers survive.
    pub fn spawn(&self, job: impl IntoJob) {
        self.shared.enqueue(job.into_job());
    }
    /// Compatibility alias for [Self::spawn].
    pub fn spawn_job(&self, job: impl IntoJob) {
        self.spawn(job);
    }
    /// Schedule detached work with explicit priority.
    pub fn spawn_with_priority(&self, priority: Priority, f: impl FnOnce() + Send + 'static) {
        self.spawn(Job::new(f).set_priority(priority));
    }
    /// Submit work with a result handle.
    pub fn submit<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> JoinHandle<T> {
        self.submit_with_priority(Priority::Normal, f)
    }
    /// Submit work with explicit priority.
    pub fn submit_with_priority<T: Send + 'static>(
        &self,
        priority: Priority,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> JoinHandle<T> {
        let (state, handle) = JobState::channel();
        self.spawn_with_priority(priority, move || {
            state.complete(catch_unwind(AssertUnwindSafe(f)))
        });
        handle
    }
    /// Run two branches and wait for both, including on panic.
    /// If both panic, the left panic is propagated.
    pub fn join<A: Send, B>(
        &self,
        left: impl FnOnce() -> A + Send,
        right: impl FnOnce() -> B,
    ) -> (A, B) {
        join_on(&self.shared, left, right)
    }
    /// Run borrowed code on this pool, enabling parallel iterator execution.
    pub fn install<T: Send>(&self, f: impl FnOnce() -> T + Send) -> T {
        if current().is_some_and(|ctx| Arc::ptr_eq(&ctx.shared, &self.shared)) {
            return f();
        }
        join_on(&self.shared, f, || ()).0
    }
}
pub(crate) fn join_on<A: Send, B>(
    shared: &Arc<SharedPoolData>,
    left: impl FnOnce() -> A + Send,
    right: impl FnOnce() -> B,
) -> (A, B) {
    let (state, handle) = JobState::channel();
    let task: Box<dyn FnOnce() + Send + '_> =
        Box::new(move || state.complete(catch_unwind(AssertUnwindSafe(left))));
    // SAFETY: both branches are caught and the handle is always joined before
    // returning or resuming a panic. All captured borrows have finished use.
    let task = unsafe {
        std::mem::transmute::<Box<dyn FnOnce() + Send + '_>, Box<dyn FnOnce() + Send + 'static>>(
            task,
        )
    };
    shared.enqueue(Job::from_raw(task, Priority::Normal));
    let right = catch_unwind(AssertUnwindSafe(right));
    let left = handle.join();
    match (left, right) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(panic), other) => {
            discard(other);
            resume_unwind(panic)
        }
        (Ok(value), Err(panic)) => {
            discard(value);
            resume_unwind(panic)
        }
    }
}
pub(crate) fn discard<T>(value: T) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| drop(value))) {
        std::mem::forget(panic);
    }
}
fn worker_loop(shared: Arc<SharedPoolData>, index: usize) {
    CURRENT_WORKER.with(|c| *c.borrow_mut() = Some(WorkerContext { shared: shared.clone(), index }));
    loop {
        let job = {
            let mut scheduler = shared.scheduler.lock().unwrap();
            loop {
                if let Some(job) = scheduler.take(index) {
                    break Some(job);
                }
                if scheduler.shutdown && scheduler.pending == 0 {
                    break None;
                }
                scheduler = shared.wake.wait(scheduler).unwrap();
            }
        };
        match job {
            Some(job) => shared.execute(job),
            None => break,
        }
    }
    CURRENT_WORKER.with(|c| *c.borrow_mut() = None);
}
impl Drop for ThreadPool {
    fn drop(&mut self) {
        self.shared.stop();
        if current().is_some_and(|ctx| Arc::ptr_eq(&ctx.shared, &self.shared)) {
            return;
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
