use super::*;
use std::ops::Range;
/// Parallel source over an exclusive usize range.
pub struct RangeIter {
    range: Range<usize>,
}
impl RangeIter {
    /// Create a range source.
    pub fn new(range: Range<usize>) -> Self {
        Self { range }
    }
}
impl ParallelIterator for RangeIter {
    type Item = usize;
    fn drive_to<C: Consumer<usize>>(self, consumer: C) -> C::Result {
        drive(self, consumer)
    }
}
impl IndexedParallelIterator for RangeIter {
    fn len(&self) -> usize {
        self.range.end.saturating_sub(self.range.start)
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        assert!(index <= self.len(), "split index exceeds range length");
        let end = self.range.end.max(self.range.start);
        let mid = self.range.start + index;
        (Self::new(self.range.start..mid), Self::new(mid..end))
    }
    fn into_sequential(self) -> impl Iterator<Item = usize> {
        self.range
    }
}
impl IntoParallelIterator for Range<usize> {
    type Item = usize;
    type Iter = RangeIter;
    fn parallelize(self) -> RangeIter {
        RangeIter::new(self)
    }
}
impl IntoParallelIterator for RangeIter {
    type Item = usize;
    type Iter = Self;
    fn parallelize(self) -> Self {
        self
    }
}
