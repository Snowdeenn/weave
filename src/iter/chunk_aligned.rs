use crate::pool::current_pool;

use super::*;

pub struct ChunksAligned<'a, T> {
    slice: &'a [T],
    chunk_size: usize,
}

impl<'a, T> ParallelIterator for ChunksAligned<'a, T>
where
    T: Send + Sync,
{
    type Item = &'a [T];
    fn drive_to<C: Consumer<Self::Item>>(self, mut consumer: C) -> C::Result {
        let slice_len = self.slice.len();
        if slice_len <= MIN_CHUNK_SIZE {
            for chunk in self.slice.chunks(self.chunk_size) {
                consumer.consume(chunk);
            }
            consumer.finish()
        } else {
            let mid = slice_len / 2;
            let pool = current_pool().unwrap();
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

impl<'a, T> IndexedParallelIterator for ChunksAligned<'a, T>
where
    T: Send + Sync,
{
    fn len(&self) -> usize {
        self.slice.len() / self.chunk_size
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.slice.split_at(index * self.chunk_size);
        (
            ChunksAligned {
                slice: left,
                chunk_size: self.chunk_size,
            },
            ChunksAligned {
                slice: right,
                chunk_size: self.chunk_size,
            },
        )
    }
}
