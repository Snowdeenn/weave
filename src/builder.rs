use crate::pool::ThreadPool;

pub struct ThreadPoolBuidler {
    num_thread: Option<usize>,
    thread_name: Option<String>,
}

impl ThreadPoolBuidler {
    pub fn new() -> Self{
        Self { num_thread: None, thread_name: None }
    }
    pub fn num_thread(mut self, n: usize) -> Self {
        self.num_thread = Some(n);
        self
    }
    pub fn thread_name(mut self, name: impl Into<String>) -> Self {
        self.thread_name = Some(name.into());
        self
    }
    pub fn build(self) -> ThreadPool {

        if let (Some(num), Some(name))= (self.num_thread, self.thread_name) {
            ThreadPool::new(num, name)
        } else {
            ThreadPool::new(num_cpus::get(), String::new())
        }
    }
}