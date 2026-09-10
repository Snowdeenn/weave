//! A work-stealing pool with scoped tasks and parallel iterators.
//! Use [ThreadPool::install] to select a pool; iterators otherwise run sequentially.
mod builder;
mod cache_padded;
mod error;
mod handle;
mod iterator;
mod job;
mod pool;
mod scope;
mod storage;

pub use builder::ThreadPoolBuilder;
pub use error::BuildError;
pub use handle::JoinHandle;
pub use job::{IntoJob, Job, Priority};
pub use pool::ThreadPool;
pub use scope::Scope;
pub use storage::local::{WorkerLocal, WorkerLocalError};
/// Parallel iteration traits and extensions.
pub mod iter {
    pub use crate::iterator::*;
}
