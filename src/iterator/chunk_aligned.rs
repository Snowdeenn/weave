use super::*;
/// Slice chunks split only at chunk boundaries. The final chunk may be short.
/// This does not promise hardware SIMD alignment.
pub struct ChunksAligned<'a, T> {
    slice: &'a [T],
    chunk_size: usize,
}
impl<'a, T> ChunksAligned<'a, T> {
    /// Create chunks; panics if chunk_size is zero.
    pub fn new(slice: &'a [T], chunk_size: usize) -> Self {
        assert!(chunk_size > 0, "chunk size must be nonzero");
        Self { slice, chunk_size }
    }
}
impl<'a, T: Sync> ParallelIterator for ChunksAligned<'a, T> {
    type Item = &'a [T];
    fn drive_to<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        drive(self, consumer)
    }
}
impl<T: Sync> IndexedParallelIterator for ChunksAligned<'_, T> {
    fn len(&self) -> usize {
        self.slice.len().div_ceil(self.chunk_size)
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        assert!(index <= self.len(), "split index exceeds chunk count");
        let offset = index.saturating_mul(self.chunk_size).min(self.slice.len());
        let (left, right) = self.slice.split_at(offset);
        (
            Self::new(left, self.chunk_size),
            Self::new(right, self.chunk_size),
        )
    }
    fn into_sequential(self) -> impl Iterator<Item = Self::Item> {
        self.slice.chunks(self.chunk_size)
    }
}
