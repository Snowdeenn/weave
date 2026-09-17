use super::*;

/// Lazy, order-preserving filtering and mapping adapter.
pub struct FilterMap<I, F> {
    /// Source iterator whose items are inspected and optionally transformed.
    pub base: I,
    /// Operation returning `Some` for an output item or `None` to discard it.
    pub op: F,
}

impl<R, I, F> ParallelIterator for FilterMap<I, F>
where
    I: ParallelIterator,
    F: Fn(I::Item) -> Option<R> + Send + Sync,
    R: Send + Sync,
{
    type Item = R;

    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        self.base.drive_to(FilterMapConsumer {
            consumer,
            op: &self.op,
        })
    }
}

struct FilterMapConsumer<'f, C, F> {
    consumer: C,
    op: &'f F,
}

impl<'f, U, T, C, F> Consumer<T> for FilterMapConsumer<'f, C, F>
where
    C: Consumer<U>,
    F: Fn(T) -> Option<U> + Sync + 'f,
{
    type Result = C::Result;

    fn consume(&mut self, item: T) {
        if let Some(result) = (self.op)(item) {
            self.consumer.consume(result);
        }
    }
    fn is_full(&self) -> bool {
        self.consumer.is_full()
    }

    fn finish(self) -> Self::Result {
        self.consumer.finish()
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.consumer.split_at(index);
        (
            FilterMapConsumer {
                consumer: left,
                op: self.op,
            },
            FilterMapConsumer {
                consumer: right,
                op: self.op,
            },
        )
    }

    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        C::combine(left, right)
    }
}
