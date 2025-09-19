use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// Determines the offset of the len field within a Vec.
#[inline]
const fn vec_len_offset<T>() -> usize {
    // SAFETY:
    // - Vec<T> requires that ptr be non-null and aligned and what not, but Vec::len doesn't touch ptr.
    // - Vec<T> requires that cap >= len, but Vec::len doesn't touch cap.
    // - Vec<T> and [usize; 3] have the same layout and alignment.
    unsafe { std::mem::transmute::<&[usize; 3], &Vec<T>>(&[0, 1, 2]).len() }
}

/// Safety: changing returned .2 (&mut usize) is considered the same as calling `.set_len(_)`.
///
/// This method provides unique access to all vec parts at once.
#[inline]
const unsafe fn vec_split_at_spare_mut_with_len<T>(
    vec: &mut Vec<T>,
) -> (&mut [T], &mut [MaybeUninit<T>], &mut usize) {
    let ptr = vec.as_mut_ptr();
    let len = vec.len();
    let cap = vec.capacity();

    // SAFETY:
    // - `ptr` is guaranteed to be valid for `self.len` elements
    // - but the allocation extends out to `self.buf.capacity()` elements, possibly
    // uninitialized
    let spare_ptr = unsafe { ptr.add(len) }.cast::<MaybeUninit<T>>();
    let spare_len = cap - len;

    // SAFETY:
    // - The offset returned by vec_len_offset is guaranteed to point to the len field within a Vec<T>.
    let len_mut = unsafe {
        NonNull::new(vec as *mut Vec<T>)
            .unwrap()
            .cast::<usize>()
            .add(vec_len_offset::<T>())
            .as_mut()
    };

    // SAFETY:
    // - `ptr` is guaranteed to be valid for `self.len` elements
    // - `spare_ptr` is pointing one element past the buffer, so it doesn't overlap with `initialized`
    // - `len_mut` doesn't overlap with either
    unsafe {
        (
            std::slice::from_raw_parts_mut(ptr, len),
            std::slice::from_raw_parts_mut(spare_ptr, spare_len),
            len_mut,
        )
    }
}

/// A copy of `alloc::vec::set_len_on_drop`.
mod set_len_on_drop {
    // Set the length of the vec when the `SetLenOnDrop` value goes out of scope.
    //
    // The idea is: The length field in SetLenOnDrop is a local variable
    // that the optimizer will see does not alias with any stores through the Vec's data
    // pointer. This is a workaround for alias analysis issue #32155
    pub(super) struct SetLenOnDrop<'a> {
        len: &'a mut usize,
        local_len: usize,
    }

    impl<'a> SetLenOnDrop<'a> {
        #[inline]
        pub(super) fn new(len: &'a mut usize) -> Self {
            SetLenOnDrop {
                local_len: *len,
                len,
            }
        }

        #[inline]
        pub(super) fn increment_len(&mut self, increment: usize) {
            self.local_len += increment;
        }
    }

    impl Drop for SetLenOnDrop<'_> {
        #[inline]
        fn drop(&mut self) {
            *self.len = self.local_len;
        }
    }
}

use set_len_on_drop::SetLenOnDrop;

#[cold]
fn panic_exceeded_capacity() -> ! {
    panic!("Insufficient capacity reserved to write all elements!")
}

pub struct Spare<'a, T> {
    len_mut: &'a mut usize,
    slots: std::slice::IterMut<'a, MaybeUninit<T>>,
}

impl<'a, T> Spare<'a, T> {
    pub fn push(&mut self, item: T) {
        let Some(slot) = self.slots.next() else {
            panic_exceeded_capacity();
        };
        slot.write(item);
        *self.len_mut += 1;
    }
}

impl<T> std::iter::Extend<T> for Spare<'_, T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let mut len = SetLenOnDrop::new(self.len_mut);
        for item in iter.into_iter() {
            let Some(slot) = self.slots.next() else {
                panic_exceeded_capacity();
            };
            slot.write(item);
            len.increment_len(1);
        }
    }
}

impl<T> crate::SplitSpare<T> for Vec<T> {
    type Spare<'a>
        = Spare<'a, T>
    where
        Self: 'a;

    fn split_spare<'s>(&'s mut self) -> (&'s mut [T], Self::Spare<'s>) {
        let (initialized, spare, len_mut) = unsafe { vec_split_at_spare_mut_with_len(self) };
        let spare = Spare {
            len_mut,
            slots: spare.iter_mut(),
        };
        (initialized, spare)
    }

    fn reserve_split_spare<'s>(&'s mut self, additional: usize) -> (&'s mut [T], Self::Spare<'s>) {
        self.reserve(additional);
        self.split_spare()
    }
}

#[cfg(test)]
mod tests {
    use crate::SplitSpare;

    #[test]
    fn reserve_split_spare_works() {
        let mut vec = vec![1, 2, 3];

        let (init, mut spare) = vec.reserve_split_spare(3);

        assert_eq!(init, &[1, 2, 3]);

        spare.extend(init.iter().copied().map(|i| i + init.len()));

        assert_eq!(vec, &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    #[should_panic(expected = "Insufficient capacity reserved to write all elements!")]
    fn exceed() {
        let mut vec: Vec<i32> = Vec::new();

        let (init, mut spare) = vec.split_spare();

        assert_eq!(init, &[]);

        spare.extend([1, 2, 3].iter().copied());
    }
}
