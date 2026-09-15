use super::*;

/// A parallel iterator that yields each element alongside its index.
pub struct Enumerate<I> {
    pub(crate) base: I,
}

impl<I: IndexedParallelIterator> ParallelIterator for Enumerate<I> {
    type Item = (usize, I::Item);
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        self.base.drive_to(EnumerateConsumer { consumer, index: 0 })
    }
}

struct EnumerateConsumer<C> {
    consumer: C,
    index: usize,
}

impl<T: Send, C> Consumer<T> for EnumerateConsumer<C>
where
    C: Consumer<(usize, T)>,
{
    type Result = C::Result;
    fn consume(&mut self, item: T) {
        let index = self.index;
        self.index += 1;
        self.consumer.consume((index, item));
    }

    fn finish(self) -> Self::Result {
        self.consumer.finish()
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let right_index = self.index + index;
        let (left, right) = self.consumer.split_at(index);
        (
            EnumerateConsumer {
                consumer: left,
                index: self.index,
            },
            EnumerateConsumer {
                consumer: right,
                index: right_index,
            },
        )
    }

    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        C::combine(left, right)
    }
}
