use crate::{
    iter::{IndexedParallelIterator, ParallelIterator},
    pool::current_pool,
};

pub struct SliceIter<'a, T> {
    slice: &'a [T],
}
impl<'a, T: Send + Sync> ParallelIterator for SliceIter<'a, T> {
    type Item = &'a T;

    fn drive_to<C: super::Consumer<Self::Item>>(self, mut consumer: C) {
        let slice_len = self.slice.len();
        if slice_len <= super::MIN_CHUNK_SIZE {
            for item in self.slice {
                consumer.consume(item);
            }
            consumer.finish();
        } else {
            let mid = self.slice.len() / 2;
            let pool = current_pool().unwrap(); // TODO: Mieux gerer l'erreur
            let (left, right) = self.split_at(mid);
            let (lc, rc) = consumer.split();
            // SAFETY : drive_to attend que les deux moitiés soient finies via pool.join()
            // donc les données référencées par C sont garanties vivantes
            let left_job: Box<dyn FnOnce() + Send + 'static> = unsafe {
                std::mem::transmute(
                    Box::new(move || left.drive_to(lc)) as Box<dyn FnOnce() + Send + '_>
                )
            };
            pool.join(left_job, move || right.drive_to(rc));
        }
    }
}
impl<'a, T: Send + Sync> IndexedParallelIterator for SliceIter<'a, T> {
    fn len(&self) -> usize {
        self.slice.len()
    }
    fn split_at(self, index: usize) -> (Self, Self) {
        let (s1, s2) = self.slice.split_at(index);
        (Self { slice: s1 }, Self { slice: s2 })
    }
}
