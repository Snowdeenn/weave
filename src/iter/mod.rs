pub mod adaptator;
pub mod slice;
pub mod chunk_aligned;

const MIN_CHUNK_SIZE: usize = 512;

pub trait Consumer<T>: Send + Sized {
    type Split: Consumer<T, Result = Self::Result>;
    type Result: Send;
    fn consume(&mut self, item: T);
    fn split(self) -> (Self::Split, Self::Split);
    fn combine(left: Self::Result, right: Self::Result) -> Self::Result;
    fn finish(self) -> Self::Result;
}

pub trait ParallelIterator: Sized + Send {
    type Item: Send;
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result;

    fn for_each<F>(self, f: F)
    where
        F: Fn(Self::Item) + Sync,
    {
        self.drive_to(adaptator::ForEachConsumer { f: &f });
    }

    fn fill<T>(self, slice: &mut [Self::Item]) {
        self.drive_to(adaptator::FillConsumer { slice, index: 0 });
    }

    fn fold<Acc, F, G>(self, init: Acc, op: F, combine_op: G) -> Acc
    where
        F: Fn(Acc, Self::Item) -> Acc + Sync + Clone,
        G: Fn(Acc, Acc) -> Acc + Sync + Clone,
        Acc: Send + Clone,
    {
        self.drive_to(adaptator::FoldConsumer {
            accumulator: init,
            operation: &op,
            combine_op: &combine_op,
        })
        .acc
    }
    fn reduce<F>(self, f: F) -> Option<Self::Item>
    where
        F: Fn(Self::Item, Self::Item) -> Self::Item + Sync,
        Self::Item: Clone,
    {
        self.drive_to(adaptator::ReduceConsumer {
            accumulator: None,
            operation: &f,
        })
        .value
    }
}

pub trait IndexedParallelIterator: ParallelIterator {
    fn len(&self) -> usize;
    fn split_at(self, index: usize) -> (Self, Self);
}
