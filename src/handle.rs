use crate::pool::help_current_worker;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
pub(crate) struct JobState<T> {
    result: Mutex<Option<std::thread::Result<T>>>,
    ready: Condvar,
    // Published by Job's outer completion guard, after FnOnce has returned.
    pub(crate) finished: Arc<AtomicBool>,
}
impl<T> JobState<T> {
    pub(crate) fn channel() -> (Arc<Self>, JoinHandle<T>) {
        let state = Arc::new(Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
            finished: Arc::new(AtomicBool::new(false)),
        });
        (
            state.clone(),
            JoinHandle {
                state,
                scoped_failure: None,
            },
        )
    }
    // Publishing a result does not grant permission to reuse borrowed captures.
    // That requires the separate Release/Acquire completion handshake.
    pub(crate) fn complete(&self, result: std::thread::Result<T>) {
        *self.result.lock().unwrap() = Some(result);
        self.ready.notify_all();
    }
}
/// Result of a submitted task. Dropping the handle does not cancel the task.
pub struct JoinHandle<T> {
    state: Arc<JobState<T>>,
    pub(crate) scoped_failure: Option<Arc<AtomicBool>>,
}
impl<T> JoinHandle<T> {
    /// Whether the task has returned or panicked and released its captures.
    pub fn is_done(&self) -> bool {
        self.state.finished.load(Ordering::Acquire) && self.state.result.lock().unwrap().is_some()
    }
    /// Wait for a value or the original panic payload.
    /// Workers execute available work while waiting. Returning also requires
    /// the entire task wrapper to have released its captures.
    pub fn join(self) -> std::thread::Result<T> {
        loop {
            if self.state.finished.load(Ordering::Acquire)
                && let Some(value) = self.state.result.lock().unwrap().take()
            {
                if let Some(failed) = &self.scoped_failure {
                    failed.store(false, Ordering::Release);
                }
                return value;
            }
            if help_current_worker() {
                continue;
            }
            let result = self.state.result.lock().unwrap();
            if self.state.finished.load(Ordering::Acquire) && result.is_some() {
                continue;
            }
            // Result publication wakes ready, and the outer completion may
            // happen immediately afterwards. The bounded retry also observes
            // that second event without storing a borrowed result in the guard.
            drop(
                self.state
                    .ready
                    .wait_timeout(result, std::time::Duration::from_millis(1))
                    .unwrap(),
            );
        }
    }
}
