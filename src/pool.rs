use crate::SendPtr;
use crate::builder::ThreadPoolBuidler;
use crate::handle::JobState;
use crate::job::{IntoJob, Job};
use crate::scope::Scope;
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

thread_local! {
    pub(crate) static WORKER_IDX: Cell<usize> = const {
       Cell::new(0)
    };
    pub(crate) static CURRENT_POOL: Cell<*const ThreadPool> = const {
       Cell::new(std::ptr::null())
    };
}
pub(crate) struct Shared {
    global_queue: Mutex<VecDeque<Job>>,
    condvar: Condvar,
    shutdown: Mutex<bool>,
}

pub struct Worker {
    id: usize,
    queue: Arc<Mutex<VecDeque<Job>>>,
}

pub struct ThreadPool {
    shared: Arc<Shared>,
    locals_queues: Vec<Arc<Mutex<VecDeque<Job>>>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    _pin: std::marker::PhantomPinned,
}

impl Default for ThreadPool {
    fn default() -> Self {
        ThreadPoolBuidler::new().build()
    }
}

impl ThreadPool {
    pub fn new(num_threads: usize, thread_name: String) -> Self {
        let shared = Arc::new(Shared {
            global_queue: Mutex::new(VecDeque::new()),
            condvar: Condvar::new(),
            shutdown: Mutex::new(false),
        });
        let mut locals_queues: Vec<Arc<Mutex<VecDeque<Job>>>> = Vec::with_capacity(num_threads);
        // Créer le pool sur le heap pour avoir une adresse stable
        let mut pool = Box::new(ThreadPool {
            shared: Arc::clone(&shared),
            locals_queues: locals_queues.clone(),
            threads: Vec::with_capacity(num_threads),
            _pin: std::marker::PhantomPinned,
        });

        let pool_ptr = SendPtr(&*pool);
        for i in 0..num_threads {
            locals_queues.push(Arc::new(Mutex::new(VecDeque::new())));
            let shared = Arc::clone(&shared);
            let local_queue = Arc::clone(&locals_queues[i]);
            let worker = Worker {
                id: i,
                queue: local_queue,
            };
            let handle = std::thread::Builder::new()
                .name(format!("{thread_name} - {i}"))
                .spawn(move || worker_loop(worker, shared, pool_ptr))
                .unwrap();
            pool.threads.push(handle);
        }

        *pool
    }

    pub fn spawn_job(&self, job: impl IntoJob) {
        self.shared
            .global_queue
            .lock()
            .unwrap()
            .push_back(job.into_job());
        self.shared.condvar.notify_one();
    }

    pub fn join<'a, T, U, F1, F2>(&self, f1: F1, f2: F2) -> (T, U)
    where
        F1: FnOnce() -> T + Send,
        F2: FnOnce() -> U,
        T: Send + 'a,
    {
        let (state, handle) = JobState::<T>::channel();

        // SAFETY : join() est bloquant — on attend que f1 soit finie avant de retourner.
        // Les données capturées par f1 sont garanties vivantes pendant toute l'exécution.
        let f1: Box<dyn FnOnce() -> T + Send + 'static> =
            unsafe { std::mem::transmute(Box::new(f1) as Box<dyn FnOnce() -> T + Send + '_>) };

        let job_fn = move || {
            state.complete(f1());
        };

        // SAFETY : join() est bloquant — les données capturées sont garanties vivantes
        let job_raw: Box<dyn FnOnce() + Send + 'static> =
            unsafe { std::mem::transmute(Box::new(job_fn) as Box<dyn FnOnce() + Send + '_>) };
            
        self.shared.global_queue.lock().unwrap().push_back(Job::from_raw(job_raw));
        self.shared.condvar.notify_one();
        let result_f2 = f2();
        let result_f1 = handle.join();

        (result_f1, result_f2)
    }

    pub fn scope<'scope, F>(&'scope self, f: F)
    where
        F: FnOnce(&Scope<'scope>),
    {
        let scope = Scope::new(self);
        f(&scope);
        scope.wait_all();
    }
}

fn worker_loop(worker: Worker, shared: Arc<Shared>, pool: SendPtr<ThreadPool>) {
    WORKER_IDX.with(|cell| cell.set(worker.id));
    CURRENT_POOL.with(|cell| cell.set(pool.get()));
    loop {
        let job = {
            worker
                .queue
                .lock()
                .unwrap()
                .pop_back()
                .or_else(|| shared.global_queue.lock().unwrap().pop_front())
        };
        if let Some(job) = job {
            job.run();
            continue;
        }

        let mut shutdown = shared.shutdown.lock().unwrap();
        if *shutdown {
            break;
        }

        while !*shutdown {
            shutdown = shared.condvar.wait(shutdown).unwrap();
        }

        if *shutdown {
            break;
        }
        continue;
    }
}

pub(crate) fn current_worker_idx() -> usize {
    WORKER_IDX.with(|cell| cell.get())
}
pub(crate) fn current_pool() -> Option<&'static ThreadPool> {
    CURRENT_POOL.with(|cell| {
        let ptr = cell.get();
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    })
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        let mut shutdown = self.shared.shutdown.lock().unwrap();
        *shutdown = true;
        self.shared.condvar.notify_all();

        for handle in self.threads.drain(..) {
            handle.join().unwrap();
        }
    }
}
