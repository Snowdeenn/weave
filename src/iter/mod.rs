pub mod adaptator;
pub mod slice;
const MIN_CHUNK_SIZE: usize = 512;

pub trait Consumer<T>: Send + Sized {
    type Split: Consumer<T>;
    fn consume(&mut self, item: T);
    fn split(self) -> (Self::Split, Self::Split);
    fn finish(self);
}

pub trait ParallelIterator: Sized + Send {
    type Item: Send;
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C);

    fn for_each<F>(self, f: F)
    where
        F: Fn(Self::Item) + Sync,
    {
        self.drive_to(adaptator::ForEachConsumer { f: &f });
    }
    
    fn fill<T>(self, slice: &mut [Self::Item]) {
        self.drive_to(adaptator::FillConsumer { slice, index: 0 });
    }
}

pub trait IndexedParallelIterator: ParallelIterator {
    fn len(&self) -> usize;
    fn split_at(self, index: usize) -> (Self, Self);
}
