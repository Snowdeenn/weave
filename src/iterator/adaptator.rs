use super::*;

pub struct ForEachConsumer<'f, F> {
    pub f: &'f F,
}

impl<'f, F, T> Consumer<T> for ForEachConsumer<'f, F>
where
    F: Fn(T) + Sync,
{
    type Split = Self;
    type Result = ();

    fn consume(&mut self, item: T) {
        (self.f)(item);
    }

    fn finish(self) -> Self::Result {}

    fn split(self) -> (Self::Split, Self::Split) {
        (ForEachConsumer { f: self.f }, ForEachConsumer { f: self.f })
    }
    fn combine(_left: Self::Result, _right: Self::Result) -> Self::Result {}
}

pub struct FillConsumer<'a, T> {
    pub slice: &'a mut [T],
    pub index: usize, // pour savoir où écrire le prochain item
}

impl<'a, T: Send> Consumer<T> for FillConsumer<'a, T> {
    type Split = Self;
    type Result = ();
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
    fn combine(_left: Self::Result, _right: Self::Result) -> Self::Result {}
}
unsafe impl<'a, T> Send for FillConsumer<'a, T> where T: Send {}

pub struct FoldConsumer<'f, Acc, F, C> {
    pub accumulator: Acc,
    pub operation: &'f F,
    pub combine_op: &'f C,
}

pub struct FoldResult<'f, Acc, C> {
    pub acc: Acc,
    pub combine_op: &'f C,
}

impl<'f, Acc, F, C, T> Consumer<T> for FoldConsumer<'f, Acc, F, C>
where
    F: Fn(Acc, T) -> Acc + Sync,
    C: Fn(Acc, Acc) -> Acc + Sync,
    Acc: Clone + Send,
{
    type Split = Self;
    type Result = FoldResult<'f, Acc, C>;
    fn consume(&mut self, item: T) {
        self.accumulator = (self.operation)(self.accumulator.clone(), item);
    }

    fn finish(self) -> Self::Result {
        FoldResult {
            acc: self.accumulator,
            combine_op: self.combine_op,
        }
    }

    fn split(self) -> (Self::Split, Self::Split) {
        (
            FoldConsumer {
                accumulator: self.accumulator.clone(),
                operation: self.operation,
                combine_op: self.combine_op,
            },
            FoldConsumer {
                accumulator: self.accumulator,
                operation: self.operation,
                combine_op: self.combine_op,
            },
        )
    }

    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        FoldResult {
            acc: (left.combine_op)(left.acc, right.acc),
            combine_op: left.combine_op,
        }
    }
}

pub struct ReduceConsumer<'f, F, T> {
    pub accumulator: Option<T>,
    pub operation: &'f F,
}

pub struct ReduceResult<'f, F, T> {
    pub value: Option<T>,
    pub operation: &'f F,
}

impl<'f, F, T> Consumer<T> for ReduceConsumer<'f, F, T>
where
    F: Fn(T, T) -> T + Sync,
    T: Send + Clone,
{
    type Split = Self;
    type Result = ReduceResult<'f, F, T>;

    fn consume(&mut self, item: T) {
        self.accumulator = match self.accumulator.take() {
            Some(acc) => Some((self.operation)(acc, item)),
            None => Some(item),
        };
    }

    fn split(self) -> (Self::Split, Self::Split) {
        (
            ReduceConsumer {
                accumulator: self.accumulator,
                operation: self.operation,
            },
            ReduceConsumer {
                accumulator: None,
                operation: self.operation,
            },
        )
    }

    fn finish(self) -> Self::Result {
        ReduceResult {
            value: self.accumulator,
            operation: self.operation,
        }
    }

    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        let combined_value = match (left.value, right.value) {
            (Some(l), Some(r)) => Some((left.operation)(l, r)),
            (Some(l), None) => Some(l),
            (None, Some(r)) => Some(r),
            (None, None) => None,
        };

        ReduceResult {
            value: combined_value,
            operation: left.operation,
        }
    }
}