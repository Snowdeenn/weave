use crate::{cache_padded::CachePadded, pool::current_worker_idx};
use std::sync::Mutex;

pub struct WorkerLocal<T> {
    inner: Vec<CachePadded<Mutex<T>>>,
}

impl<T> WorkerLocal<T> {
    pub fn new<F>(num_thread: usize, f: F) -> Self
    where
        F: Fn() -> T,
    {
        let mut inner = Vec::<CachePadded<Mutex<T>>>::with_capacity(num_thread);
        for _ in 0..num_thread {
            inner.push(CachePadded::new(Mutex::new(f())));
        }
        Self { inner }
    }
    pub fn with<F>(&self, f: F)
    where
        F: FnOnce(&mut T),
    {
        // SAFETY : à appeler uniquement depuis un worker du pool.
        // Depuis le thread principal, current_worker_idx() retourne 0 par défaut.
        let current_thread = current_worker_idx();
        let mut scratchpad = self.inner[current_thread].0.lock().unwrap();
        f(&mut *scratchpad);
    }
}
