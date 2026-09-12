//! Ordered parallel sources, lazy mapping and terminal operations.
//! Reductions require associative operations; fold seeds must be identities.
//! Floating-point reductions may differ from sequential accumulation.
/// Lazy iterator transformations.
pub mod adaptator;
/// Element-aligned slice chunks.
pub mod chunk_aligned;
/// Exclusive usize range sources.
pub mod range;
/// Borrowing slice sources and extension traits.
pub mod slice;
pub use adaptator::Map;
pub use chunk_aligned::ChunksAligned;
pub use range::RangeIter;
pub use slice::{ParallelSlice, ParallelSliceMut, SliceIter, SliceIterMut};

const MIN_CHUNK_SIZE: usize = 512;

/// A terminal operation that can divide its output at an exact item index.
pub trait Consumer<T>: Send + Sized {
    /// Partial or final output.
    type Result: Send;
    /// Process one item.
    fn consume(&mut self, item: T);
    /// Divide before either half is consumed.
    fn split_at(self, index: usize) -> (Self, Self);
    /// Merge left and right outputs in source order.
    fn combine(left: Self::Result, right: Self::Result) -> Self::Result;
    /// Finalize a leaf.
    fn finish(self) -> Self::Result;
}
/// A source that can execute a divisible consumer.
pub trait ParallelIterator: Sized + Send {
    /// Produced element.
    type Item: Send;
    /// Execute a consumer; advanced extension point for custom sources.
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result;
    /// Execute an action for each element. Side-effect order is unspecified.
    fn for_each<F: Fn(Self::Item) + Sync>(self, f: F) {
        self.drive_to(adaptator::ForEachConsumer { f: &f });
    }
    /// Lazily transform each element.
    fn map<R: Send, F: Fn(Self::Item) -> R + Send + Sync>(self, f: F) -> Map<Self, F> {
        Map {
            base: self,
            f: std::sync::Arc::new(f),
        }
    }
    /// Fold leaves using a neutral seed, then combine their outputs.
    /// The seed is cloned once per split, not once per element.
    /// Combine must be associative and agree with the leaf operation.
    fn fold<A: Send + Clone, F: Fn(A, Self::Item) -> A + Sync, G: Fn(A, A) -> A + Sync>(
        self,
        identity: A,
        operation: F,
        combine: G,
    ) -> A {
        self.drive_to(adaptator::FoldConsumer {
            acc: Some(identity),
            operation: &operation,
            combine: &combine,
        })
        .acc
    }
    /// Combine elements with an associative operation; empty input yields None.
    fn reduce<F: Fn(Self::Item, Self::Item) -> Self::Item + Sync>(
        self,
        operation: F,
    ) -> Option<Self::Item> {
        self.drive_to(adaptator::ReduceConsumer {
            acc: None,
            operation: &operation,
        })
        .acc
    }
    /// Collect elements in source order.
    fn collect(self) -> Vec<Self::Item> {
        self.drive_to(adaptator::CollectConsumer(Vec::new()))
    }
    /// Fill an exactly sized destination in source order, without an intermediate buffer.
    /// Panics before evaluation if lengths differ. A task panic may leave partial writes.
    fn fill(self, output: &mut [Self::Item])
    where
        Self: IndexedParallelIterator,
    {
        assert_eq!(self.len(), output.len(), "fill requires matching lengths");
        self.drive_to(adaptator::FillConsumer { output, index: 0 });
    }
}
/// A parallel iterator with an exact length and order-preserving splitting.
pub trait IndexedParallelIterator: ParallelIterator {
    /// Exact number of elements.
    fn len(&self) -> usize;
    /// Whether the source is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Split at an element index; panics when index exceeds length.
    fn split_at(self, index: usize) -> (Self, Self);
    /// Consume this source sequentially; used for small leaves.
    fn into_sequential(self) -> impl Iterator<Item = Self::Item>;
}
/// Convert a source into a parallel iterator.
pub trait IntoParallelIterator {
    /// Element type.
    type Item: Send;
    /// Resulting iterator.
    type Iter: ParallelIterator<Item = Self::Item>;
    /// Create a parallel iterator from the source without executing it.
    fn parallelize(self) -> Self::Iter;
}
pub(crate) fn drive<I: IndexedParallelIterator, C: Consumer<I::Item>>(
    source: I,
    mut consumer: C,
) -> C::Result {
    if source.len() > MIN_CHUNK_SIZE
        && let Some(worker_context) = crate::pool::current_worker()
    {
        let mid = source.len() / 2;
        let (left, right) = source.split_at(mid);
        let (lc, rc) = consumer.split_at(mid);
        let (left, right) =
            crate::pool::join_on(&worker_context.shared, move || drive(left, lc), move || drive(right, rc));
        return C::combine(left, right);
    }
    for item in source.into_sequential() {
        consumer.consume(item);
    }
    consumer.finish()
}
