/// Scheduling preference for queued jobs, without preemption.
/// Continuous high-priority work may starve lower priorities.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Background work.
    Low,
    /// Default preference.
    #[default]
    Normal,
    /// Preferred over normal and low work.
    High,
}
impl Priority {
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

#[derive(Default)]
struct Completion {
    cleanup: Option<Box<dyn Send>>,
    finished: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}
impl Drop for Completion {
    fn drop(&mut self) {
        // This runs OUTSIDE the erased FnOnce call frame, including on unwind.
        // Its argument's borrow protectors must end before a scope can return
        // or a handle can hand borrowed results back to the caller.
        drop(self.cleanup.take());
        if let Some(finished) = &self.finished {
            finished.store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

/// An owned detached task with scheduling metadata.
/// Its closure moves into a queue and then into exactly one executor.
/// Running consumes the job; the scheduler neither clones nor retries it.
pub struct Job {
    // Field order also applies to rejection: captures drop before completion.
    task: Box<dyn FnOnce() + Send + 'static>,
    completion: Completion,
    priority: Priority,
    label: Option<&'static str>,
}
impl Job {
    /// Construct a normal-priority task.
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self {
            task: Box::new(f),
            completion: Completion::default(),
            priority: Priority::Normal,
            label: None,
        }
    }
    /// Erases a borrowed task's lifetime solely for storage in scheduler queues.
    ///
    /// # Safety
    /// The caller must wait until the entire erased FnOnce invocation has
    /// returned/unwound and its captures have been destroyed before their
    /// lifetime ends. Signalling inside that closure is insufficient.
    /// Enqueue must either accept without further unwind or drop on rejection.
    /// Leaking a result handle must not bypass the wait.
    pub(crate) unsafe fn from_borrowed<'a>(
        task: Box<dyn FnOnce() + Send + 'a>,
        priority: Priority,
    ) -> Self {
        // SAFETY: caller arranges structured completion outside the call frame;
        // only trait-object storage lifetime is erased, not a reference.
        let task = unsafe {
            std::mem::transmute::<Box<dyn FnOnce() + Send + 'a>, Box<dyn FnOnce() + Send + 'static>>(
                task,
            )
        };
        Self {
            task,
            completion: Completion::default(),
            priority,
            label: None,
        }
    }
    pub(crate) fn with_cleanup(mut self, cleanup: impl Send + 'static) -> Self {
        self.completion.cleanup = Some(Box::new(cleanup));
        self
    }
    pub(crate) fn with_finish_signal(
        mut self,
        finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.completion.finished = Some(finished);
        self
    }
    /// Set priority.
    pub fn set_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }
    /// Attach a descriptive label.
    pub fn set_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
    /// Execute immediately on the calling thread.
    pub fn run(self) {
        let Self {
            task, completion, ..
        } = self;
        task();
        // On panic the same cleanup runs while unwinding this outer frame.
        drop(completion);
    }
    /// Read priority.
    pub fn priority(&self) -> Priority {
        self.priority
    }
    /// Read label.
    pub fn label(&self) -> Option<&'static str> {
        self.label
    }
}
/// Conversion into a detached task.
pub trait IntoJob {
    /// Convert the value.
    fn into_job(self) -> Job;
}
impl<F: FnOnce() + Send + 'static> IntoJob for F {
    fn into_job(self) -> Job {
        Job::new(self)
    }
}
impl IntoJob for Job {
    fn into_job(self) -> Job {
        self
    }
}
