use super::*;
use std::sync::Arc;

/// Lazy, order-preserving transformation over a parallel iterator.
pub struct Map<I, F> {
    pub(crate) base: I,
    pub(crate) f: Arc<F>,
}
impl<I: ParallelIterator, R: Send, F: Fn(I::Item) -> R + Send + Sync> ParallelIterator
    for Map<I, F>
{
    type Item = R;
    fn drive_to<C: Consumer<R>>(self, consumer: C) -> C::Result {
        self.base.drive_to(MapConsumer {
            consumer,
            f: &*self.f,
        })
    }
}
impl<I: IndexedParallelIterator, R: Send, F: Fn(I::Item) -> R + Send + Sync> IndexedParallelIterator
    for Map<I, F>
{
    fn len(&self) -> usize {
        self.base.len()
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.base.split_at(index);
        (
            Self {
                base: left,
                f: self.f.clone(),
            },
            Self {
                base: right,
                f: self.f,
            },
        )
    }
    fn into_sequential(self) -> impl Iterator<Item = R> {
        self.base.into_sequential().map(move |item| (self.f)(item))
    }
}
struct MapConsumer<'a, C, F> {
    consumer: C,
    f: &'a F,
}
impl<T, R, C: Consumer<R>, F: Fn(T) -> R + Sync> Consumer<T> for MapConsumer<'_, C, F> {
    type Result = C::Result;
    fn consume(&mut self, item: T) {
        self.consumer.consume((self.f)(item));
    }
    fn is_full(&self) -> bool {
        self.consumer.is_full()
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.consumer.split_at(index);
        (
            Self {
                consumer: left,
                f: self.f,
            },
            Self {
                consumer: right,
                f: self.f,
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
