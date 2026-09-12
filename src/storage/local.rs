use crate::{
    ThreadPool,
    cache_padded::CachePadded,
    pool::{SharedPoolData, current},
};
use std::sync::{Arc, Mutex, TryLockError, Weak};

/// Invalid access to worker-local storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerLocalError {
    /// The caller is not a worker of the storage's owning pool.
    WrongPool,
    /// This worker's value is already borrowed (including cooperative reentry).
    AlreadyBorrowed,
    /// A previous callback panicked while mutating this value.
    Poisoned,
}
impl std::fmt::Display for WorkerLocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WorkerLocalError {}

/// One independently initialized value per worker, bound to a specific pool.
pub struct WorkerLocal<T> {
    owner: Weak<SharedPoolData>,
    inner: Vec<CachePadded<Mutex<T>>>,
}
impl<T> WorkerLocal<T> {
    /// Initialize one value for each worker, on the calling thread.
    pub fn new(pool: &ThreadPool, f: impl Fn() -> T) -> Self {
        Self::with_index(pool, |_| f())
    }
    /// Initialize values using the worker index.
    pub fn with_index(pool: &ThreadPool, f: impl Fn(usize) -> T) -> Self {
        Self {
            owner: Arc::downgrade(&pool.shared),
            inner: (0..pool.num_threads())
                .map(|i| CachePadded::new(Mutex::new(f(i))))
                .collect(),
        }
    }
    /// Access this worker's value. Panics on invalid or recursive access.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.try_with(f).expect("invalid WorkerLocal access")
    }
    /// Access this worker's value without waiting on a recursive borrow.
    pub fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Result<R, WorkerLocalError> {
        let worker_context = current().ok_or(WorkerLocalError::WrongPool)?;
        if !Weak::ptr_eq(&self.owner, &Arc::downgrade(&worker_context.shared)) {
            return Err(WorkerLocalError::WrongPool);
        }
        let mut value = self.inner[worker_context.index].0.try_lock().map_err(|e| match e {
            TryLockError::WouldBlock => WorkerLocalError::AlreadyBorrowed,
            TryLockError::Poisoned(_) => WorkerLocalError::Poisoned,
        })?;
        Ok(f(&mut value))
    }
    /// Consume the storage and recover every value, even if a callback panicked.
    pub fn into_inner(self) -> Vec<T> {
        self.inner
            .into_iter()
            .map(|value| value.0.into_inner().unwrap_or_else(|e| e.into_inner()))
            .collect()
    }
}
