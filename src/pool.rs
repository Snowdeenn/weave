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
    pending: usize,
    shutdown: bool,
    steals: crate::StealStats,
    steal_plan: StealPlan,
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
pub(crate) fn current_worker() -> Option<WorkerContext> {
    CURRENT_WORKER.with(|c| c.borrow().clone())
}
pub(crate) fn help_current_worker() -> bool {
    current_worker().is_some_and(|ctx| ctx.shared.help(ctx.index))
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
    pub(crate) fn enqueue(self: &Arc<Self>, job: Job) {
        let local_worker = current_worker()
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
    /// Count of actual transfers from another worker's local deque.
    pub fn steal_count(&self) -> usize {
        self.steal_stats().total()
    }
    /// Returns a consistent snapshot of successful steals by placement proximity.
    pub fn steal_stats(&self) -> crate::StealStats {
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
    CURRENT_WORKER.with(|c| {
        *c.borrow_mut() = Some(WorkerContext {
            shared: shared.clone(),
            index,
        })
    });
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
