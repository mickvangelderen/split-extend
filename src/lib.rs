pub mod vec;

pub trait SplitSpare<T> {
    type Spare<'s>: ::std::iter::Extend<T>
    where
        Self: 's;

    fn split_spare<'s>(&'s mut self) -> (&'s mut [T], Self::Spare<'s>);

    /// Convenience function to reserve and then `split_spare`.
    fn reserve_split_spare<'s>(&'s mut self, additional: usize) -> (&'s mut [T], Self::Spare<'s>);
}
