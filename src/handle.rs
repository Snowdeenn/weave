use crate::pool::help_current;
use std::sync::{Arc, Condvar, Mutex};
pub(crate) struct JobState<T> {
    result: Mutex<Option<std::thread::Result<T>>>,
    ready: Condvar,
}
impl<T> JobState<T> {
    pub(crate) fn channel() -> (Arc<Self>, JoinHandle<T>) {
        let state = Arc::new(Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        });
        (
            state.clone(),
            JoinHandle {
                state,
                scoped_failure: None,
            },
        )
    }
    pub(crate) fn complete(&self, result: std::thread::Result<T>) {
        *self.result.lock().unwrap() = Some(result);
        self.ready.notify_all();
    }
}
/// Result of a submitted task. Dropping the handle does not cancel the task.
pub struct JoinHandle<T> {
    state: Arc<JobState<T>>,
    pub(crate) scoped_failure: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl<T> JoinHandle<T> {
    /// Whether the task has returned or panicked.
    pub fn is_done(&self) -> bool {
        self.state.result.lock().unwrap().is_some()
    }
    /// Wait for a value or the original panic payload.
    /// Workers execute available work while waiting.
    pub fn join(self) -> std::thread::Result<T> {
        loop {
            {
                let mut result = self.state.result.lock().unwrap();
                if let Some(value) = result.take() {
                    if let Some(failed) = &self.scoped_failure {
                        failed.store(false, std::sync::atomic::Ordering::Release);
                    }
                    return value;
                }
            }
            if help_current() {
                continue;
            }
            let result = self.state.result.lock().unwrap();
            if result.is_some() {
                continue;
            }
            drop(
                self.state
                    .ready
                    .wait_timeout(result, std::time::Duration::from_millis(1))
                    .unwrap(),
            );
        }
    }
}
