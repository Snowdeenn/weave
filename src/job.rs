#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Low,
    Normal,
    High,
}

pub struct Id<Tag> {
    pub index: usize,
    pub generation: u32,
    _phantom: std::marker::PhantomData<Tag>,
}
pub struct JobTag;
pub type JobId = Id<JobTag>;

pub struct Job {
    priority: Priority,
    task: Box<dyn FnOnce() + Send + 'static>,
    label: Option<&'static str>,
}

impl Job {
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self {
            priority: Priority::Normal,
            task: Box::new(f),
            label: None,
        }
    }
    pub(crate) fn from_raw(task: Box<dyn FnOnce() + Send + 'static>) -> Self {
    Self {
        priority: Priority::Normal,
        task,
        label: None,
    }
}
    pub fn set_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }
    pub fn set_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
    pub fn run(self) {
        (self.task)()
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }
    pub fn label(&self) -> Option<&'static str> {
        self.label
    }
}

pub trait IntoJob {
    fn into_job(self) -> Job;
}

impl<F> IntoJob for F
where
    F: FnOnce() + Send + 'static,
{
    #[inline]
    fn into_job(self) -> Job {
        Job::new(self)
    }
}

impl IntoJob for Job {
    fn into_job(self) -> Job {
        self
    }
}
