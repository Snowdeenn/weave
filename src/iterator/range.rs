use std::ops::Range;

use crate::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

pub struct RangeIter {
    range: Range<usize>,
}

impl ParallelIterator for RangeIter {
    type Item = usize;
    fn drive_to<C: super::Consumer<Self::Item>>(self, mut consumer: C) -> C::Result {
        let range_len = self.len();
        if range_len <= super::MIN_CHUNK_SIZE {
            for item in self.range {
                consumer.consume(item);
            }
            consumer.finish()
        } else {
            let mid = range_len / 2;
            let pool = crate::current_pool().unwrap(); // TODO: Mieux gerer l'erreur
            let (left, right) = self.split_at(mid);
            let (lc, rc) = consumer.split();

            // SAFETY : drive_to attend que les deux moitiés soient finies via pool.join()
            // donc les données référencées par C sont garanties vivantes
            let left_job: Box<dyn FnOnce() -> C::Result + Send + 'static> = unsafe {
                std::mem::transmute(Box::new(move || left.drive_to(lc))
                    as Box<dyn FnOnce() -> C::Result + Send + '_>)
            };

            let (left_res, right_res) = pool.join(left_job, move || right.drive_to(rc));
            C::combine(left_res, right_res)
        }
    }
}

impl IndexedParallelIterator for RangeIter {
    fn len(&self) -> usize {
        if self.range.start < self.range.end {
            self.range.end - self.range.start
        } else {
            0
        }
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let mid = self.range.start + index;
        (
            RangeIter {
                range: self.range.start..mid,
            },
            RangeIter {
                range: mid..self.range.end,
            },
        )
    }
}

impl IntoParallelIterator for RangeIter {
    type Item = usize;
    type Iter = RangeIter;
    fn parallelize(self) -> Self::Iter {
        RangeIter { range: self.range }
    }
}
