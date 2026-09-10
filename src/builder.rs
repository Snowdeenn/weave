use crate::{BuildError, ThreadPool};
/// Independent configuration options for a pool.
#[derive(Default)]
pub struct ThreadPoolBuilder {
    num_threads: Option<usize>,
    thread_name: Option<String>,
}
impl ThreadPoolBuilder {
    /// Defaults to available CPU parallelism and the name "weave".
    pub fn new() -> Self {
        Self::default()
    }
    /// Set worker count; zero is rejected.
    pub fn num_threads(mut self, n: usize) -> Self {
        self.num_threads = Some(n);
        self
    }
    /// Compatibility spelling of [Self::num_threads].
    pub fn num_thread(self, n: usize) -> Self {
        self.num_threads(n)
    }
    /// Set worker name prefix.
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = Some(name.into());
        self
    }
    /// Build, panicking on construction failure.
    pub fn build(self) -> ThreadPool {
        self.try_build().expect("cannot build weave pool")
    }
    /// Build with explicit error handling.
    pub fn try_build(self) -> Result<ThreadPool, BuildError> {
        let n = self
            .num_threads
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from));
        ThreadPool::try_new(n, self.thread_name.unwrap_or_else(|| "weave".into()))
    }
}
