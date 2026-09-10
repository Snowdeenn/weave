use super::*;
/// Parallel immutable slice source.
pub struct SliceIter<'a, T> {
    slice: &'a [T],
}
impl<'a, T> SliceIter<'a, T> {
    /// Create a source without copying elements.
    pub fn new(slice: &'a [T]) -> Self {
        Self { slice }
    }
}
impl<'a, T: Sync> ParallelIterator for SliceIter<'a, T> {
    type Item = &'a T;
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        drive(self, consumer)
    }
}
impl<T: Sync> IndexedParallelIterator for SliceIter<'_, T> {
    fn len(&self) -> usize {
        self.slice.len()
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.slice.split_at(index);
        (Self::new(left), Self::new(right))
    }
    fn into_sequential(self) -> impl Iterator<Item = Self::Item> {
        self.slice.iter()
    }
}
impl<'a, T: Sync> IntoParallelIterator for SliceIter<'a, T> {
    type Item = &'a T;
    type Iter = Self;
    fn parallelize(self) -> Self {
        self
    }
}
impl<'a, T: Sync> IntoParallelIterator for &'a [T] {
    type Item = &'a T;
    type Iter = SliceIter<'a, T>;
    fn parallelize(self) -> Self::Iter {
        SliceIter::new(self)
    }
}
impl<'a, T: Sync> IntoParallelIterator for &'a Vec<T> {
    type Item = &'a T;
    type Iter = SliceIter<'a, T>;
    fn parallelize(self) -> Self::Iter {
        SliceIter::new(self)
    }
}
/// Parallel mutable slice source with disjoint leaves.
pub struct SliceIterMut<'a, T> {
    slice: &'a mut [T],
}
impl<'a, T> SliceIterMut<'a, T> {
    /// Create a source borrowing all elements exclusively.
    pub fn new(slice: &'a mut [T]) -> Self {
        Self { slice }
    }
}
impl<'a, T: Send> ParallelIterator for SliceIterMut<'a, T> {
    type Item = &'a mut T;
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        drive(self, consumer)
    }
}
impl<T: Send> IndexedParallelIterator for SliceIterMut<'_, T> {
    fn len(&self) -> usize {
        self.slice.len()
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.slice.split_at_mut(index);
        (Self::new(left), Self::new(right))
    }
    fn into_sequential(self) -> impl Iterator<Item = Self::Item> {
        self.slice.iter_mut()
    }
}
impl<'a, T: Send> IntoParallelIterator for &'a mut [T] {
    type Item = &'a mut T;
    type Iter = SliceIterMut<'a, T>;
    fn parallelize(self) -> Self::Iter {
        SliceIterMut::new(self)
    }
}
impl<'a, T: Send> IntoParallelIterator for &'a mut Vec<T> {
    type Item = &'a mut T;
    type Iter = SliceIterMut<'a, T>;
    fn parallelize(self) -> Self::Iter {
        SliceIterMut::new(self)
    }
}
/// Convenient parallel operations on slices (also available through Vec/array deref).
pub trait ParallelSlice<T: Sync> {
    /// Iterate by shared reference.
    fn iter_parallel(&self) -> SliceIter<'_, T>;
    /// Iterate over chunks, including a final short chunk.
    /// Chunk boundaries are aligned to element counts, not SIMD memory addresses.
    fn chunks_parallel(&self, chunk_size: usize) -> ChunksAligned<'_, T>;
    /// Compatibility name for element-aligned chunks.
    fn chunk_aligned(&self, chunk_size: usize) -> ChunksAligned<'_, T> {
        self.chunks_parallel(chunk_size)
    }
}
impl<T: Sync> ParallelSlice<T> for [T] {
    fn iter_parallel(&self) -> SliceIter<'_, T> {
        SliceIter::new(self)
    }
    fn chunks_parallel(&self, chunk_size: usize) -> ChunksAligned<'_, T> {
        ChunksAligned::new(self, chunk_size)
    }
}
/// Parallel mutable access to disjoint slice elements.
pub trait ParallelSliceMut<T: Send> {
    /// Iterate by exclusive reference.
    fn iter_parallel_mut(&mut self) -> SliceIterMut<'_, T>;
}
impl<T: Send> ParallelSliceMut<T> for [T] {
    fn iter_parallel_mut(&mut self) -> SliceIterMut<'_, T> {
        SliceIterMut::new(self)
    }
}
