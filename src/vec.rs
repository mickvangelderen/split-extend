use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// Determines the offset of the len field within a Vec.
#[inline]
fn vec_len_offset_of_val<T>(vec: &mut Vec<T>) -> usize {
    // See https://users.rust-lang.org/t/134050 for why the implementation here is the way it is. We have to work around
    // unsound manipulations to Vec.
    if std::mem::size_of::<T>() == 0 {
        // ZST

        // Use ManuallyDrop to avoid dropping non-existing elements caused by `set_len`.
        let mut vec: Vec<std::mem::MaybeUninit<T>> =
            unsafe { std::mem::transmute(Vec::<T>::new()) };

        // Should be a no-op, but just in case Vec's internals require this.
        vec.reserve(1);

        let before: [usize; 3] = unsafe { std::mem::transmute_copy(&vec) };

        // SAFETY:
        // - T is zero-sized and the Vec's drop won't be called, so we can set the len to anything
        unsafe {
            vec.set_len(1);
        }

        let after: [usize; 3] = unsafe { std::mem::transmute_copy(&vec) };

        match std::array::from_fn(|i| after[i] ^ before[i]) {
            [_, 0, 0] => 0,
            [0, _, 0] => 1,
            [0, 0, _] => 2,
            _ => unreachable!(),
        }
    } else {
        // Non-ZST

        // Ensure the capacity is non-zero.
        vec.reserve(1);

        // Get the ptr/cap/len triple with the length set to 0.
        let parts = unsafe {
            let orig_len = vec.len();
            vec.set_len(0);
            let parts: [usize; 3] = std::mem::transmute_copy(&*vec);
            vec.set_len(orig_len);
            parts
        };

        // As per the layout guarantees, *only* the length should be 0.
        match parts {
            [0, a, b] if a != 0 && b != 0 => 0,
            [a, 0, b] if a != 0 && b != 0 => 1,
            [a, b, 0] if a != 0 && b != 0 => 2,
            _ => unreachable!(),
        }
    }
}

/// Safety: changing returned .2 (&mut usize) is considered the same as calling `.set_len(_)`.
///
/// This method provides unique access to all vec parts at once.
#[inline]
unsafe fn vec_split_at_spare_mut_with_len<T>(
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
            .add(vec_len_offset_of_val(vec))
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
