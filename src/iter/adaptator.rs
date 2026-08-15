use super::*;

pub struct ForEachConsumer<'f, F> {
    pub f: &'f F,
}

impl<'f, F, T> Consumer<T> for ForEachConsumer<'f, F>
where
    F: Fn(T) + Sync,
{
    type Split = Self;

    fn consume(&mut self, item: T) {
        (self.f)(item);
    }

    fn finish(self) {}

    fn split(self) -> (Self::Split, Self::Split) {
        (ForEachConsumer { f: self.f }, ForEachConsumer { f: self.f })
    }
}

pub struct FillConsumer<'a, T> {
    pub slice: &'a mut [T],
    pub index: usize, // pour savoir où écrire le prochain item
}

impl<'a, T: Send> Consumer<T> for FillConsumer<'a, T> {
    type Split = Self;
    fn consume(&mut self, item: T) {
        self.slice[self.index] = item;
        self.index += 1;
    }

    fn finish(self) {}

    fn split(self) -> (Self::Split, Self::Split) {
        let mid = self.slice.len() / 2;
        let (left, right) = self.slice.split_at_mut(mid);
        (
            FillConsumer {
                slice: left,
                index: 0,
            },
            FillConsumer {
                slice: right,
                index: 0,
            },
        )
    }
}
unsafe impl<'a, T> Send for FillConsumer<'a, T> where T: Send {}

pub struct FoldConsumer<'f, Acc, F> {
    pub accumulator: Acc,
    pub operation: &'f F,
}

impl<'f, Acc, F, T> Consumer<T> for FoldConsumer<'f, Acc, F>
where
    F: Fn(Acc, T) -> Acc + Sync,
    Acc: Clone + Send,
{
    type Split = Self;
    fn consume(&mut self, item: T) {
        self.accumulator = (self.operation)(self.accumulator.clone(), item);
    }

    fn finish(self) {}

    fn split(self) -> (Self::Split, Self::Split) {
        (
            FoldConsumer {
                accumulator: self.accumulator.clone(),
                operation: self.operation,
            },
            FoldConsumer {
                accumulator: self.accumulator.clone(),
                operation: self.operation,
            },
        )
    }
}
