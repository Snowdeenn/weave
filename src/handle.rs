use std::sync::{Arc, Condvar, Mutex};

struct Inner<T> {
    result: Option<T>,
    is_done: bool,
}

pub struct JobState<T> {
    inner: Mutex<Inner<T>>,
    condvar: Condvar,
}

impl<T> JobState<T> {
    pub fn channel() -> (Arc<Self>, JoinHandle<T>) {
        let state = Arc::new(Self {
            inner: Mutex::new(Inner {
                result: None,
                is_done: false,
            }),
            condvar: Condvar::new(),
        });
        let handle = JoinHandle {
            state: Arc::clone(&state),
        };
        (state, handle)
    }

    pub fn complete(&self, value: T) {
        // TODO: gestion panic
        let mut inner = self.inner.lock().unwrap();
        inner.result = Some(value);
        inner.is_done = true;
        
        self.condvar.notify_one();
    }
}

pub struct JoinHandle<T> {
    state: Arc<JobState<T>>,
}

impl<T> JoinHandle<T> {
    pub fn is_done(&self) -> bool {
        let inner = self.state.inner.lock().unwrap();
        inner.is_done
    }

    pub fn join(self) -> T {
        let mut inner = self.state.inner.lock().unwrap();

        while !inner.is_done {
            inner = self.state.condvar.wait(inner).unwrap();
        }
        inner
            .result
            .take()
            .expect("Le résultat doit être présent lorsque is_done vaut true")
    }
}