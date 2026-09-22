use crate::{BuildError, ThreadPool, WorkerLayout};
/// Automatic worker-count selection.
#[derive(Default)]
pub struct Automatic;

/// Explicit worker-count selection.
pub struct FixedCount {
    count: usize,
}

/// Hardware-placement selection applied when the pool starts.
pub struct Planned {
    layout: WorkerLayout,
}

/// Pool configuration whose type records the selected worker policy.
///
/// Explicit counts and layouts are mutually exclusive. Naming remains available
/// in every state. Repeating the same policy replaces its value.
///
/// ```compile_fail
/// use weave::{ThreadPoolBuilder, WorkerLayout};
/// fn conflicting(layout: WorkerLayout) {
///     ThreadPoolBuilder::new().num_threads(4).worker_layout(layout);
/// }
/// ```
/// ```compile_fail
/// use weave::{ThreadPoolBuilder, WorkerLayout};
/// fn conflicting(layout: WorkerLayout) {
///     ThreadPoolBuilder::new().worker_layout(layout).num_threads(4);
/// }
/// ```
/// ```compile_fail
/// use weave::{ThreadPoolBuilder, WorkerLayout};
/// fn conflicting(layout: WorkerLayout) {
///     ThreadPoolBuilder::new().worker_layout(layout).num_thread(4);
/// }
/// ```
/// Building from a layout creates one worker per placement and binds each
/// worker to its selected logical CPU before returning the pool. Construction
/// fails if any worker cannot apply its affinity.
pub struct ThreadPoolBuilder<State = Automatic> {
    workers: State,
    thread_name: Option<String>,
}

impl Default for ThreadPoolBuilder<Automatic> {
    fn default() -> Self {
        Self {
            workers: Automatic,
            thread_name: None,
        }
    }
}

impl ThreadPoolBuilder<Automatic> {
    /// Defaults to available CPU parallelism and the name "weave".
    pub fn new() -> Self {
        Self::default()
    }
    /// Set worker count; zero is rejected.
    pub fn num_threads(self, n: usize) -> ThreadPoolBuilder<FixedCount> {
        ThreadPoolBuilder {
            workers: FixedCount { count: n },
            thread_name: self.thread_name,
        }
    }
    /// Compatibility spelling of [Self::num_threads].
    pub fn num_thread(self, n: usize) -> ThreadPoolBuilder<FixedCount> {
        self.num_threads(n)
    }
    /// Select a placement plan, making explicit counts unavailable.
    pub fn worker_layout(self, layout: WorkerLayout) -> ThreadPoolBuilder<Planned> {
        ThreadPoolBuilder {
            workers: Planned { layout },
            thread_name: self.thread_name,
        }
    }
    /// Build, panicking on construction failure.
    pub fn build(self) -> ThreadPool {
        self.try_build().expect("cannot build weave pool")
    }
    /// Build using available parallelism with explicit error handling.
    pub fn try_build(self) -> Result<ThreadPool, BuildError> {
        let n = std::thread::available_parallelism().map_or(1, usize::from);
        ThreadPool::try_new(n, self.thread_name.unwrap_or_else(|| "weave".into()))
    }
}

impl<State> ThreadPoolBuilder<State> {
    /// Set worker name prefix.
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = Some(name.into());
        self
    }
}

impl ThreadPoolBuilder<FixedCount> {
    /// Replace the worker count; zero is rejected at construction time.
    pub fn num_threads(mut self, n: usize) -> Self {
        self.workers.count = n;
        self
    }
    /// Compatibility spelling of [Self::num_threads].
    pub fn num_thread(self, n: usize) -> Self {
        self.num_threads(n)
    }
    /// Build, panicking on construction failure.
    pub fn build(self) -> ThreadPool {
        self.try_build().expect("cannot build weave pool")
    }
    /// Build with explicit error handling.
    pub fn try_build(self) -> Result<ThreadPool, BuildError> {
        let n = self.workers.count;
        ThreadPool::try_new(n, self.thread_name.unwrap_or_else(|| "weave".into()))
    }
}

impl ThreadPoolBuilder<Planned> {
    /// Replace the selected placement plan.
    pub fn worker_layout(mut self, layout: WorkerLayout) -> Self {
        self.workers.layout = layout;
        self
    }

    /// Builds and pins the workers, panicking if construction fails.
    pub fn build(self) -> ThreadPool {
        self.try_build().expect("cannot build weave pool")
    }

    /// Builds and pins the workers with explicit error handling.
    pub fn try_build(self) -> Result<ThreadPool, BuildError> {
        ThreadPool::try_with_layout(
            self.workers.layout,
            self.thread_name.unwrap_or_else(|| "weave".into()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{CoreId, CpuId, NumaNodeId, PackageId, topology_fixture};

    #[test]
    fn count_transitions_preserve_names_and_allow_replacement() {
        let builder: ThreadPoolBuilder<FixedCount> = ThreadPoolBuilder::default()
            .thread_name("before")
            .num_thread(2)
            .num_threads(3);
        assert_eq!(builder.thread_name.as_deref(), Some("before"));
        assert_eq!(builder.workers.count, 3);
        let builder = builder.thread_name("after").num_thread(4);
        assert_eq!(builder.thread_name.as_deref(), Some("after"));
        assert_eq!(builder.workers.count, 4);
    }

    #[test]
    fn planned_transitions_preserve_names_and_allow_replacement() {
        let core = CoreId::new(PackageId::new(0), 0);
        let topology = topology_fixture(&[
            (CpuId::new(0), core, NumaNodeId::new(0)),
            (CpuId::new(1), core, NumaNodeId::new(0)),
        ]);
        let logical = WorkerLayout::one_per_logical_cpu(&topology);
        let physical = WorkerLayout::one_per_physical_core(&topology);
        let builder: ThreadPoolBuilder<Planned> = ThreadPoolBuilder::new()
            .thread_name("before")
            .worker_layout(logical.clone());
        assert_eq!(builder.workers.layout, logical);
        assert_eq!(builder.thread_name.as_deref(), Some("before"));
        let builder = builder.thread_name("after").worker_layout(physical.clone());
        assert_eq!(builder.workers.layout, physical);
        assert_eq!(builder.thread_name.as_deref(), Some("after"));
    }
}
