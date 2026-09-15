use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct SumConsumer<S>(pub(crate) S);

impl<T, S: Send + std::iter::Sum<T> + std::iter::Sum<S>> Consumer<T> for SumConsumer<S> {
    type Result = S;
    fn consume(&mut self, item: T) {
        let previous = std::mem::replace(
            &mut self.0,
            <S as std::iter::Sum<T>>::sum(std::iter::empty()),
        );
        let item = <S as std::iter::Sum<T>>::sum(std::iter::once(item));
        self.0 = <S as std::iter::Sum<S>>::sum([previous, item].into_iter());
    }
    fn split_at(self, _: usize) -> (Self, Self) {
        (
            self,
            Self(<S as std::iter::Sum<T>>::sum(std::iter::empty())),
        )
    }
    fn combine(left: S, right: S) -> S {
        <S as std::iter::Sum<S>>::sum([left, right].into_iter())
    }
    fn finish(self) -> S {
        self.0
    }
}

// A match in a left subtree cancels only its right sibling and descendants.
// Nodes describe source partitions, so this also works after filtering.
struct SearchNode {
    left_found: AtomicBool,
    parent: Option<(Arc<SearchNode>, bool)>,
}

enum SearchState {
    Any(Arc<AtomicBool>),
    First(Arc<SearchNode>),
}

impl SearchState {
    fn split(self) -> (Self, Self) {
        match self {
            Self::Any(found) => (Self::Any(found.clone()), Self::Any(found)),
            Self::First(parent) => {
                let child = |right| {
                    Self::First(Arc::new(SearchNode {
                        left_found: AtomicBool::new(false),
                        parent: Some((parent.clone(), right)),
                    }))
                };
                (child(false), child(true))
            }
        }
    }

    fn cancelled(&self) -> bool {
        match self {
            Self::Any(found) => found.load(Ordering::Relaxed),
            Self::First(node) => {
                let mut node = node.as_ref();
                while let Some((parent, right)) = &node.parent {
                    if *right && parent.left_found.load(Ordering::Relaxed) {
                        return true;
                    }
                    node = parent;
                }
                false
            }
        }
    }

    fn claim(&self) -> bool {
        match self {
            Self::Any(found) => !found.swap(true, Ordering::Relaxed),
            Self::First(node) => {
                let mut node = node.as_ref();
                while let Some((parent, right)) = &node.parent {
                    if !right {
                        parent.left_found.store(true, Ordering::Relaxed);
                    }
                    node = parent;
                }
                true
            }
        }
    }
}

pub(crate) struct FindConsumer<'a, T, P> {
    predicate: &'a P,
    result: Option<T>,
    state: SearchState,
}

impl<'a, T, P> FindConsumer<'a, T, P> {
    pub(crate) fn any(predicate: &'a P) -> Self {
        Self {
            predicate,
            result: None,
            state: SearchState::Any(Arc::new(AtomicBool::new(false))),
        }
    }
    pub(crate) fn first(predicate: &'a P) -> Self {
        Self {
            predicate,
            result: None,
            state: SearchState::First(Arc::new(SearchNode {
                left_found: AtomicBool::new(false),
                parent: None,
            })),
        }
    }
}

impl<T: Send, P: Fn(&T) -> bool + Sync> Consumer<T> for FindConsumer<'_, T, P> {
    type Result = Option<T>;
    fn consume(&mut self, item: T) {
        if !self.is_full() && (self.predicate)(&item) && self.state.claim() {
            self.result = Some(item);
        }
    }
    fn is_full(&self) -> bool {
        self.result.is_some() || self.state.cancelled()
    }
    fn split_at(self, _: usize) -> (Self, Self) {
        let (left, right) = self.state.split();
        (
            Self {
                predicate: self.predicate,
                result: self.result,
                state: left,
            },
            Self {
                predicate: self.predicate,
                result: None,
                state: right,
            },
        )
    }
    fn combine(left: Self::Result, right: Self::Result) -> Self::Result {
        left.or(right)
    }
    fn finish(self) -> Self::Result {
        self.result
    }
}

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
