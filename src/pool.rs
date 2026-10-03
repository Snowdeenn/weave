//! Scheduler invariants (see docs/SCHEDULER_INVARIANTS.md for details and tests).
//!
//! A successfully enqueued Job has exactly one owner: one queue or one executor.
//! Queue access and shutdown/pending transitions use the same scheduler mutex;
//! mutex release/acquisition publishes captures, while the condvar only wakes.
//! At transition boundaries, pending counts queued jobs plus popped jobs whose
//! execute call has not yet accounted for completion. Taking a job does not
//! decrement it. Both worker loops and cooperative helpers consume jobs once,
//! outside the lock, then account for return or recoverable unwind exactly once.
//!
//! Shutdown drains rather than cancels. Running parents remain counted while
//! publishing descendants. Exit requires shutdown and pending == 0 under the
//! lock. Internal callers must not enqueue after all workers have exited.
//! Eventual completion assumes terminating callbacks/destructors, worker
//! progress, no indefinite starvation, usable locks and nonoverflowing counts.
//! Strict priorities do not guarantee fairness; blocking user code and process
//! aborts are outside this guarantee. Result readiness precedes final scheduler
//! accounting and must not be used as a substitute for pending == 0.

use crate::{
    BuildError, IntoJob, Job, JoinHandle, Priority, ThreadPoolBuilder, WorkerLayout,
    affinity::pin_current_thread, handle::JobState, steal::StealPlan,
};
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
    // Queued + popped but not yet accounted for, including suspended parents.
    pending: usize,
    shutdown: bool,
    steals: crate::StealStats,
    steal_plan: StealPlan,
}

/// Diagnostic scheduler-lock counters. Available only with scheduler-metrics.
/// This instrumentation changes lock acquisition and adds clocks/atomics.
/// Snapshots are approximate across concurrent updates, not timing baselines.
#[cfg(feature = "scheduler-metrics")]
#[derive(Clone, Copy, Debug, Default)]
pub struct SchedulerMetrics {
    /// Attempts to acquire the central scheduler mutex.
    pub lock_attempts: u64,
    /// Attempts whose initial try_lock found another owner.
    pub contended: u64,
    /// Sum of elapsed nanoseconds waiting after such a failed try_lock.
    pub wait_ns: u64,
}
#[cfg(feature = "scheduler-metrics")]
#[derive(Default)]
struct LockMetrics {
    attempts: std::sync::atomic::AtomicU64,
    contended: std::sync::atomic::AtomicU64,
    wait_ns: std::sync::atomic::AtomicU64,
}

pub(crate) struct SharedPoolData {
    #[cfg(feature = "scheduler-metrics")]
    metrics: LockMetrics,
    scheduler: Mutex<Scheduler>,
    wake: Condvar,
}

#[derive(Clone)]
pub(crate) struct WorkerContext {
    pub shared: Arc<SharedPoolData>,
    pub index: usize,
}
#[cfg(test)]
thread_local! {
    static REJECT_NEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
pub(crate) fn reject_next_submission() {
    REJECT_NEXT.with(|reject| reject.set(true));
}

thread_local! {
    static CURRENT_WORKER: RefCell<Option<WorkerContext>> = const { RefCell::new(None) };
}
pub(crate) fn current_worker() -> Option<WorkerContext> {
    CURRENT_WORKER.with(|c| c.borrow().clone())
}
pub(crate) fn help_current_worker() -> bool {
    current_worker().is_some_and(|ctx| ctx.shared.help(ctx.index))
}
impl Scheduler {
    // Caller holds the scheduler mutex. A pop transfers the sole Job owner;
    // pending is unchanged until execute accounts for completion.
    fn take(&mut self, index: usize) -> Option<Job> {
        for priority in (0..3).rev() {
            if let Some(job) = self.local[index][priority].pop_back() {
                return Some(job);
            }
            if let Some(job) = self.global[priority].pop_front() {
                return Some(job);
            }
            for &victim in self.steal_plan.victims_for(index) {
                if let Some(job) = self.local[victim][priority].pop_front() {
                    self.steal_plan
                        .record_steal(index, victim, &mut self.steals);
                    return Some(job);
                }
            }
        }
        None
    }
}
impl SharedPoolData {
    fn lock_scheduler(&self) -> std::sync::MutexGuard<'_, Scheduler> {
        #[cfg(not(feature = "scheduler-metrics"))]
        {
            self.scheduler.lock().unwrap()
        }
        #[cfg(feature = "scheduler-metrics")]
        {
            use std::sync::{TryLockError, atomic::Ordering};
            self.metrics.attempts.fetch_add(1, Ordering::Relaxed);
            match self.scheduler.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(error)) => panic!("scheduler poisoned: {error}"),
                Err(TryLockError::WouldBlock) => {
                    let start = std::time::Instant::now();
                    let guard = self.scheduler.lock().unwrap();
                    self.metrics.contended.fetch_add(1, Ordering::Relaxed);
                    self.metrics.wait_ns.fetch_add(
                        start.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                        Ordering::Relaxed,
                    );
                    guard
                }
            }
        }
    }
    pub(crate) fn enqueue(self: &Arc<Self>, job: Job) {
        #[cfg(test)]
        REJECT_NEXT.with(|reject| {
            assert!(!reject.replace(false), "injected publication failure");
        });
        let local_worker = current_worker()
            .filter(|ctx| Arc::ptr_eq(&ctx.shared, self))
            .map(|ctx| ctx.index);
        let mut scheduler = self.lock_scheduler();
        let priority = job.priority().index();
        let Some(next_pending) = scheduler.pending.checked_add(1) else {
            drop(scheduler);
            panic!("scheduler task count overflow");
        };
        let queue = match local_worker {
            Some(i) => &mut scheduler.local[i][priority],
            None => &mut scheduler.global[priority],
        };
        if let Err(error) = queue.try_reserve(1) {
            // Never unwind while holding the scheduler lock or after acceptance.
            // The caller still owns job; dropping it releases borrowed captures.
            drop(scheduler);
            panic!("cannot reserve scheduler queue: {error}");
        }
        queue.push_back(job);
        // After publication, no fallible operation may precede return.
        scheduler.pending = next_pending;
        self.wake.notify_one();
    }
    fn execute(&self, job: Job) {
        // No scheduler lock is held while user code runs.
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| job.run())) {
            discard(payload);
        }
        let mut scheduler = self.lock_scheduler();
        // The consumed job cannot be retried. Account only after run/unwind
        // and panic-payload cleanup, including for cooperative executions.
        scheduler.pending -= 1;
        self.wake.notify_all();
    }
    fn help(&self, index: usize) -> bool {
        let job = self.lock_scheduler().take(index);
        if let Some(job) = job {
            self.execute(job);
            true
        } else {
            false
        }
    }
    // Request draining; accepted parents may still enqueue descendants.
    fn stop(&self) {
        self.lock_scheduler().shutdown = true;
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
        let steal_plan = StealPlan::circular(num_threads);
        let mut pool = Self::prepare(num_threads, &thread_name, steal_plan)?;
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

    pub(crate) fn try_with_layout(
        layout: WorkerLayout,
        thread_name: String,
    ) -> Result<ThreadPool, BuildError> {
        let num_threads = layout.worker_count();
        let steal_plan = StealPlan::topology_aware(&layout);
        let mut pool = Self::prepare(num_threads, &thread_name, steal_plan)?;
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), BuildError>>();
        for worker in layout.workers() {
            let shared = pool.shared.clone();
            let worker_index = worker.worker_index();
            let worker_tx = tx.clone();
            let worker_cpu = worker.cpu();

            match std::thread::Builder::new()
                .name(format!("{thread_name}-{worker_index}"))
                .spawn(move || {
                    match pin_current_thread(worker_cpu) {
                        Ok(()) => {
                            if worker_tx.send(Ok(())).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = worker_tx.send(Err(BuildError::Affinity {
                                worker_index,
                                source: e,
                                cpu: worker_cpu,
                            }));
                            return;
                        }
                    }
                    drop(worker_tx);
                    worker_loop(shared, worker_index)
                }) {
                Ok(thread) => pool.threads.push(thread),
                Err(error) => return Err(BuildError::Spawn(error)),
            }
        }
        drop(tx);
        // On failure, dropping the local pool stops and joins its workers.
        wait_for_startup(&rx, num_threads)?;
        Ok(pool)
    }

    fn prepare(
        num_threads: usize,
        thread_name: &str,
        steal_plan: StealPlan,
    ) -> Result<Self, BuildError> {
        if num_threads == 0 {
            return Err(BuildError::ZeroThreads);
        }
        if thread_name.contains('\0') {
            return Err(BuildError::InvalidThreadName);
        }
        let shared = Arc::new(SharedPoolData {
            #[cfg(feature = "scheduler-metrics")]
            metrics: LockMetrics::default(),
            scheduler: Mutex::new(Scheduler {
                global: Default::default(),
                local: (0..num_threads).map(|_| Queues::default()).collect(),
                pending: 0,
                shutdown: false,
                steals: Default::default(),
                steal_plan,
            }),
            wake: Condvar::new(),
        });
        Ok(Self {
            shared,
            threads: Vec::new(),
            num_threads,
        })
    }
    /// Worker count.
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }
    /// Returns optional instrumentation counters; use only in diagnostic runs.
    #[cfg(feature = "scheduler-metrics")]
    pub fn scheduler_metrics(&self) -> SchedulerMetrics {
        use std::sync::atomic::Ordering;
        SchedulerMetrics {
            lock_attempts: self.shared.metrics.attempts.load(Ordering::Relaxed),
            contended: self.shared.metrics.contended.load(Ordering::Relaxed),
            wait_ns: self.shared.metrics.wait_ns.load(Ordering::Relaxed),
        }
    }

    /// Count of actual transfers from another worker's local deque.
    pub fn steal_count(&self) -> usize {
        self.steal_stats().total()
    }
    /// Returns a consistent snapshot of successful steals by placement proximity.
    pub fn steal_stats(&self) -> crate::StealStats {
        self.shared.lock_scheduler().steals
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
        let finished = state.finished.clone();
        let job = Job::new(move || state.complete(catch_unwind(AssertUnwindSafe(f))))
            .set_priority(priority)
            .with_finish_signal(finished);
        self.spawn(job);
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
        if current_worker().is_some_and(|ctx| Arc::ptr_eq(&ctx.shared, &self.shared)) {
            return f();
        }
        join_on(&self.shared, f, || ()).0
    }
}
fn wait_for_startup(
    rx: &std::sync::mpsc::Receiver<Result<(), BuildError>>,
    num_threads: usize,
) -> Result<(), BuildError> {
    for _ in 0..num_threads {
        rx.recv().map_err(BuildError::StartupDisconnected)??;
    }
    Ok(())
}

pub(crate) fn join_on<A: Send, B>(
    shared: &Arc<SharedPoolData>,
    left: impl FnOnce() -> A + Send,
    right: impl FnOnce() -> B,
) -> (A, B) {
    let (state, handle) = JobState::channel();
    let finished = state.finished.clone();
    let task: Box<dyn FnOnce() + Send + '_> =
        Box::new(move || state.complete(catch_unwind(AssertUnwindSafe(left))));
    // SAFETY: enqueue rejects before publication and drops the borrowed task
    // on failure. Once accepted it cannot unwind; both branches are caught and
    // the handle waits for completion outside the erased FnOnce call frame
    // before returning/resuming panic, not merely for result publication.
    let job = unsafe { Job::from_borrowed(task, Priority::Normal) }.with_finish_signal(finished);
    shared.enqueue(job);
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
    CURRENT_WORKER.with(|c| {
        *c.borrow_mut() = Some(WorkerContext {
            shared: shared.clone(),
            index,
        })
    });
    loop {
        let job = {
            let mut scheduler = shared.lock_scheduler();
            loop {
                if let Some(job) = scheduler.take(index) {
                    break Some(job);
                }
                // Empty queues alone are insufficient: popped parents may
                // still run and publish children. Check accounting under lock.
                if scheduler.shutdown && scheduler.pending == 0 {
                    break None;
                }
                // Atomically unlock and wait, then recheck both predicates;
                // enqueue/stop/execute change them under this same mutex.
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
        if current_worker().is_some_and(|ctx| Arc::ptr_eq(&ctx.shared, &self.shared)) {
            return;
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;
    use crate::topology::{CoreId, CpuId, NumaNodeId, PackageId, topology_fixture};

    fn scheduler() -> Scheduler {
        // CPU order deliberately differs from proximity order for worker 0.
        let entries = [(0, 0, 0), (1, 0, 1), (0, 1, 0), (0, 0, 0)];
        let entries = entries
            .into_iter()
            .enumerate()
            .map(|(cpu, (package, core, node))| {
                (
                    CpuId::new(cpu),
                    CoreId::new(PackageId::new(package), core),
                    NumaNodeId::new(node),
                )
            })
            .collect::<Vec<_>>();
        let layout = WorkerLayout::one_per_logical_cpu(&topology_fixture(&entries));
        Scheduler {
            global: Default::default(),
            local: (0..4).map(|_| Queues::default()).collect(),
            pending: 0,
            shutdown: false,
            steals: Default::default(),
            steal_plan: StealPlan::topology_aware(&layout),
        }
    }

    fn job(label: &'static str) -> Job {
        Job::new(|| {}).set_label(label)
    }

    fn take_label(scheduler: &mut Scheduler, worker: usize) -> &'static str {
        scheduler
            .take(worker)
            .expect("expected a queued job")
            .label()
            .unwrap()
    }

    #[test]
    fn takes_sibling_then_local_node_then_remote_and_counts_only_transfers() {
        let mut scheduler = scheduler();
        let priority = Priority::Normal.index();
        scheduler.local[1][priority].push_back(job("remote"));
        scheduler.local[2][priority].push_back(job("same node"));
        scheduler.local[3][priority].push_back(job("sibling oldest"));
        scheduler.local[3][priority].push_back(job("sibling newest"));
        assert_eq!(take_label(&mut scheduler, 0), "sibling oldest");
        assert_eq!(take_label(&mut scheduler, 0), "sibling newest");
        assert_eq!(take_label(&mut scheduler, 0), "same node");
        assert_eq!(take_label(&mut scheduler, 0), "remote");
        assert!(scheduler.take(0).is_none());
        assert_eq!(
            scheduler.steals,
            crate::StealStats {
                same_core: 2,
                same_numa: 1,
                remote_numa: 1,
                unknown: 0,
            }
        );
        assert_eq!(scheduler.steals.total(), 4);
    }

    #[test]
    fn local_lifo_and_global_fifo_precede_stealing_at_equal_priority() {
        let mut scheduler = scheduler();
        let priority = Priority::Normal.index();
        scheduler.local[0][priority].push_back(job("local oldest"));
        scheduler.local[0][priority].push_back(job("local newest"));
        scheduler.global[priority].push_back(job("global oldest"));
        scheduler.global[priority].push_back(job("global newest"));
        scheduler.local[3][priority].push_back(job("stolen"));
        for expected in [
            "local newest",
            "local oldest",
            "global oldest",
            "global newest",
        ] {
            assert_eq!(take_label(&mut scheduler, 0), expected);
            assert_eq!(scheduler.steals.total(), 0);
        }
        assert_eq!(take_label(&mut scheduler, 0), "stolen");
        assert_eq!(scheduler.steals.same_core, 1);
    }

    #[test]
    fn priority_precedes_queue_locality_and_victim_proximity() {
        let mut scheduler = scheduler();
        scheduler.local[0][Priority::Low.index()].push_back(job("local low"));
        scheduler.global[Priority::Normal.index()].push_back(job("global normal"));
        scheduler.local[3][Priority::Normal.index()].push_back(job("sibling normal"));
        scheduler.local[1][Priority::High.index()].push_back(job("remote high"));
        for expected in [
            "remote high",
            "global normal",
            "sibling normal",
            "local low",
        ] {
            assert_eq!(take_label(&mut scheduler, 0), expected);
        }
        assert_eq!(scheduler.steals.total(), 2);
    }

    #[test]
    fn circular_plan_wraps_and_records_unknown_topology() {
        let mut scheduler = scheduler();
        scheduler.steal_plan = StealPlan::circular(4);
        for victim in [0, 1, 3] {
            scheduler.local[victim][Priority::Normal.index()].push_back(job(match victim {
                0 => "zero",
                1 => "one",
                _ => "three",
            }));
        }
        for expected in ["three", "zero", "one"] {
            assert_eq!(take_label(&mut scheduler, 2), expected);
        }
        assert!(scheduler.take(2).is_none());
        assert_eq!(
            scheduler.steals,
            crate::StealStats {
                unknown: 3,
                ..Default::default()
            }
        );
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use crate::{affinity::AffinityError, topology::CpuId};
    use std::sync::mpsc;

    #[test]
    fn accepts_all_confirmations_even_after_senders_are_dropped() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(())).unwrap();
        tx.send(Ok(())).unwrap();
        drop(tx);
        assert!(wait_for_startup(&rx, 2).is_ok());
    }

    #[test]
    fn propagates_affinity_failure_after_an_earlier_success() {
        let (tx, rx) = mpsc::channel();
        let cpu = CpuId::new(1234);
        tx.send(Ok(())).unwrap();
        tx.send(Err(BuildError::Affinity {
            worker_index: 1,
            cpu,
            source: AffinityError::CpuOutOfRange(cpu),
        }))
        .unwrap();
        drop(tx);
        assert!(matches!(wait_for_startup(&rx, 2),
            Err(BuildError::Affinity { worker_index: 1, cpu: actual,
                source: AffinityError::CpuOutOfRange(source_cpu) })
                if actual == cpu && source_cpu == cpu));
    }

    #[test]
    fn rejects_disconnection_when_a_confirmation_is_missing() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(())).unwrap();
        drop(tx);
        let error = wait_for_startup(&rx, 2).unwrap_err();
        assert!(matches!(error, BuildError::StartupDisconnected(_)));
        assert!(std::error::Error::source(&error).is_some());
        assert!(error.to_string().contains("before all confirmations"));
    }
}

#[cfg(all(test, feature = "scheduler-metrics"))]
mod metrics_tests {
    use crate::ThreadPoolBuilder;
    #[test]
    fn diagnostic_counts_include_submission_and_execution() {
        let pool = ThreadPoolBuilder::new().num_threads(1).build();
        let before = pool.scheduler_metrics();
        assert_eq!(pool.submit(|| 42).join().unwrap(), 42);
        let after = pool.scheduler_metrics();
        assert!(after.lock_attempts >= before.lock_attempts + 2);
        assert!(after.contended <= after.lock_attempts);
        assert!(after.wait_ns >= before.wait_ns);
    }
}
