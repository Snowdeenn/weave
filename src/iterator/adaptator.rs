use super::*;
use std::sync::Arc;

pub(crate) struct CollectConsumer<T>(pub(crate) Vec<T>);
impl<T: Send> Consumer<T> for CollectConsumer<T> {
    type Result = Vec<T>;
    fn consume(&mut self, item: T) {
        self.0.push(item);
    }
    fn split_at(self, _: usize) -> (Self, Self) {
        (self, Self(Vec::new()))
    }
    fn combine(mut left: Vec<T>, right: Vec<T>) -> Vec<T> {
        left.extend(right);
        left
    }
    fn finish(self) -> Vec<T> {
        self.0
    }
}

/// Lazy, order-preserving transformation of another iterator.
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
pub(crate) struct ForEachConsumer<'a, F> {
    pub(crate) f: &'a F,
}
impl<T, F: Fn(T) + Sync> Consumer<T> for ForEachConsumer<'_, F> {
    type Result = ();
    fn consume(&mut self, item: T) {
        (self.f)(item);
    }
    fn split_at(self, _: usize) -> (Self, Self) {
        (Self { f: self.f }, self)
    }
    fn combine(_: (), _: ()) {}
    fn finish(self) {}
}
pub(crate) struct FillConsumer<'a, T> {
    pub(crate) output: &'a mut [T],
    pub(crate) index: usize,
}
impl<T: Send> Consumer<T> for FillConsumer<'_, T> {
    type Result = ();
    fn consume(&mut self, item: T) {
        self.output[self.index] = item;
        self.index += 1;
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.output.split_at_mut(index);
        (
            Self {
                output: left,
                index: 0,
            },
            Self {
                output: right,
                index: 0,
            },
        )
    }
    fn combine(_: (), _: ()) {}
    fn finish(self) {}
}
pub(crate) struct FoldConsumer<'a, A, F, G> {
    pub(crate) acc: Option<A>,
    pub(crate) operation: &'a F,
    pub(crate) combine: &'a G,
}
pub(crate) struct FoldResult<'a, A, G> {
    pub(crate) acc: A,
    combine: &'a G,
}
impl<'a, T, A: Clone + Send, F: Fn(A, T) -> A + Sync, G: Fn(A, A) -> A + Sync> Consumer<T>
    for FoldConsumer<'a, A, F, G>
{
    type Result = FoldResult<'a, A, G>;
    fn consume(&mut self, item: T) {
        let old = self.acc.take().expect("fold accumulator present");
        self.acc = Some((self.operation)(old, item));
    }
    fn split_at(self, _: usize) -> (Self, Self) {
        (
            Self {
                acc: self.acc.clone(),
                operation: self.operation,
                combine: self.combine,
            },
            self,
        )
    }
    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        FoldResult {
            acc: (left.combine)(left.acc, right.acc),
            combine: left.combine,
        }
    }
    fn finish(self) -> Self::Result {
        FoldResult {
            acc: self.acc.expect("fold accumulator present"),
            combine: self.combine,
        }
    }
}
pub(crate) struct ReduceConsumer<'a, T, F> {
    pub(crate) acc: Option<T>,
    pub(crate) operation: &'a F,
}
pub(crate) struct ReduceResult<'a, T, F> {
    pub(crate) acc: Option<T>,
    operation: &'a F,
}
impl<'a, T: Send, F: Fn(T, T) -> T + Sync> Consumer<T> for ReduceConsumer<'a, T, F> {
    type Result = ReduceResult<'a, T, F>;
    fn consume(&mut self, item: T) {
        self.acc = Some(match self.acc.take() {
            Some(acc) => (self.operation)(acc, item),
            None => item,
        });
    }
    fn split_at(self, _: usize) -> (Self, Self) {
        (
            Self {
                acc: None,
                operation: self.operation,
            },
            self,
        )
    }
    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        let acc = match (left.acc, right.acc) {
            (Some(a), Some(b)) => Some((left.operation)(a, b)),
            (a, None) => a,
            (None, b) => b,
        };
        ReduceResult {
            acc,
            operation: left.operation,
        }
    }
    fn finish(self) -> Self::Result {
        ReduceResult {
            acc: self.acc,
            operation: self.operation,
        }
    }
}
