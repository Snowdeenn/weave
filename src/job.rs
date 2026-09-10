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
/// An owned detached task with scheduling metadata.
pub struct Job {
    task: Box<dyn FnOnce() + Send + 'static>,
    priority: Priority,
    label: Option<&'static str>,
}
impl Job {
    /// Construct a normal-priority task.
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self {
            task: Box::new(f),
            priority: Priority::Normal,
            label: None,
        }
    }
    pub(crate) fn from_raw(task: Box<dyn FnOnce() + Send + 'static>, priority: Priority) -> Self {
        Self {
            task,
            priority,
            label: None,
        }
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
        (self.task)()
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
