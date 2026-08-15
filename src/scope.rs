use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use crate::{
    handle::{JobState, JoinHandle},
    job::Job,
    pool::ThreadPool,
};

pub(crate) struct ScopeWaker {
    pub mutex: Mutex<()>,
    pub condvar: Condvar,
}

pub struct Scope<'scope> {
    pool: &'scope ThreadPool,
    pending: Arc<AtomicUsize>,
    waker: Arc<ScopeWaker>,
}

impl<'scope> Scope<'scope> {
    pub fn new(pool: &'scope ThreadPool) -> Self {
        Self {
            pool: pool,
            pending: Arc::new(AtomicUsize::new(0)),
            waker: Arc::new(ScopeWaker {
                mutex: Mutex::new(()),
                condvar: Condvar::new(),
            }),
        }
    }
    pub fn spawn(&'scope self, f: impl FnOnce() + Send + 'scope) {
        self.pending.fetch_add(1, Ordering::Relaxed);

        let wrapped_job = move || {
            f();
            self.pending.fetch_sub(1, Ordering::Relaxed);
            self.waker.condvar.notify_all();
        };

        let job_fn: Box<dyn FnOnce() + Send + 'scope> = Box::new(wrapped_job);
        // Effacer le lifetime — SAFETY : safe uniquement parce que wait_all()
        // garantit que les jobs sont finis avant que 'scope expire
        let job_fn: Box<dyn FnOnce() + Send + 'static> = unsafe { std::mem::transmute(job_fn) };
        self.pool.spawn_job(job_fn);
    }
    pub fn submit<T: Send + 'scope>(
        &'scope self,
        f: impl FnOnce() -> T + Send + 'scope,
    ) -> JoinHandle<T> {
        self.pending.fetch_add(1, Ordering::Relaxed);
        let (state, handle) = JobState::<T>::channel();

        let wrapped_job = move || {
            let result = f();
            state.complete(result);
            self.pending.fetch_sub(1, Ordering::Release);
            self.waker.condvar.notify_all();
        };

        let job_fn: Box<dyn FnOnce() + Send + 'scope> = Box::new(wrapped_job);
        // Effacer le lifetime — SAFETY : safe uniquement parce que wait_all()
        // garantit que les jobs sont finis avant que 'scope expire
        let job_fn: Box<dyn FnOnce() + Send + 'static> = unsafe { std::mem::transmute(job_fn) };
        self.pool.spawn_job(job_fn);
        handle
    }
    pub(crate) fn wait_all(&self) {
        let mut guard = self.waker.mutex.lock().unwrap();
        while self.pending.load(Ordering::Acquire) > 0 {
            guard = self.waker.condvar.wait(guard).unwrap();
        }
    }
}
