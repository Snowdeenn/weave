/// Pool construction failure.
#[derive(Debug)]
pub enum BuildError {
    /// At least one worker is required.
    ZeroThreads,
    /// Thread names cannot contain a NUL byte.
    InvalidThreadName,
    /// The OS could not create a worker.
    Spawn(std::io::Error),
    /// The startup channel closed before every worker confirmed initialization.
    StartupDisconnected(std::sync::mpsc::RecvError),
    /// A worker could not apply its requested CPU placement during startup.
    Affinity {
        /// Index of the worker whose initialization failed.
        worker_index: usize,
        /// Logical CPU requested for that worker.
        cpu: crate::topology::CpuId,
        /// Underlying affinity failure.
        source: crate::AffinityError,
    },
}
impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroThreads => write!(f, "a pool needs at least one worker"),
            Self::InvalidThreadName => write!(f, "thread name contains a NUL byte"),
            Self::Spawn(e) => write!(f, "cannot start worker: {e}"),
            Self::StartupDisconnected(e) => {
                write!(
                    f,
                    "worker startup ended before all confirmations were received: {e}"
                )
            }
            Self::Affinity {
                worker_index,
                cpu,
                source,
            } => write!(
                f,
                "cannot pin worker {worker_index} to CPU {}: {source}",
                cpu.get()
            ),
        }
    }
}
impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(e) => Some(e),
            Self::StartupDisconnected(e) => Some(e),
            Self::Affinity { source, .. } => Some(source),
            _ => None,
        }
    }
}
