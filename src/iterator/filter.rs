use std::sync::Arc;

use super::*;

/// Lazy, order-preserving filter over a parallel iterator.
///
/// Filtering may change the number of produced elements, so this adapter does
/// not implement [`IndexedParallelIterator`].
pub struct Filter<I, P> {
    pub(crate) base: I,
    pub(crate) predicate: Arc<P>,
}

impl<I, P> ParallelIterator for Filter<I, P>
where
    I: ParallelIterator,
    P: Fn(&I::Item) -> bool + Send + Sync,
{
    type Item = I::Item;
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        self.base.drive_to(FilterConsumer {
            consumer,
            predicate: &*self.predicate,
        })
    }
}

struct FilterConsumer<'p, C, P> {
    consumer: C,
    predicate: &'p P,
}

impl<'p, T, C, P: 'p> Consumer<T> for FilterConsumer<'p, C, P>
where
    C: Consumer<T>,
    P: Fn(&T) -> bool + Sync,
{
    type Result = C::Result;
    fn consume(&mut self, item: T) {
        if (self.predicate)(&item) {
            self.consumer.consume(item);
        }
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.consumer.split_at(index);
        (
            FilterConsumer {
                consumer: left,
                predicate: self.predicate,
            },
            FilterConsumer {
                consumer: right,
                predicate: self.predicate,
            },
        )
    }
    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        C::combine(left, right)
    }
    fn finish(self) -> Self::Result {
        self.consumer.finish()
    }
}
