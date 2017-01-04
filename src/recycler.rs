//! Something like the slab allocator, but can grow when there's not enough space.
//!
use std::ops::{Index, IndexMut};

/// An allocator for one type of object. Or, well, allocator… you store things into it, so it's not
/// strictly an allocator, more like a storage.
///
/// Each item has an index that stays the same for the whole life of the item. It may get reused
/// once the item is released.
///
/// It asserts that `release()`d items are not accessed or released again.
///
/// # Examples
///
/// [//]: # Not-compiled example, as rustdoc has problems with examples for private pieces of code.
///
/// ```rust,ignore
/// let mut r: Recycler<u32> = Recycler::new();
/// let idx1: usize = s.store(1);
/// let idx2: usize = s.store(2);
///
/// assert_eq!(1, s[idx1]);
/// assert_eq!(2, s[idx2]);
/// assert_eq!(2, s.len());
///
/// s[idx1] = 3;
///
/// assert_eq!(3, s[idx3]);
///
/// s.release(idx1);
///
/// assert_eq!(1, s.len());
///
/// // s[idx1]; -- this is no longer valid (and is checked inside by an assert)
///
/// let idx3: usize = s.store(4); // May reuse the same index as idx1
/// ```
pub struct Recycler<T> {
    /// The actual storage where items live.
    ///
    /// The live slots have Some(T), while the currently unused ones have None. This is to
    /// sanity-check indexing and releasing of the items by user.
    data: Vec<Option<T>>,
    /// Indexes of currently unused slots, used as a stack (we don't care about the order and it's
    /// the fastest operation on Vec).
    free: Vec<usize>,
}

impl<T> Recycler<T> {
    /// Create a new, empty storage
    pub fn new() -> Self {
        Recycler {
            data: Vec::new(),
            free: Vec::new(),
        }
    }

    /// Put another item into the storage. Return the item's index
    pub fn store(&mut self, item: T) -> usize {
        match self.free.pop() {
            Some(idx) => {
                let ptr = &mut self.data[idx];
                assert!(ptr.is_none());
                *ptr = Some(item);
                idx
            },
            None => {
                self.data.push(Some(item));
                self.data.len() - 1
            },
        }
    }

    /// Return the item at the given index. The index is not valid until the storage retuns it for
    /// another newly stored item.
    ///
    /// The old content is returned.
    pub fn release(&mut self, idx: usize) -> T {
        let ptr = &mut self.data[idx];
        assert!(ptr.is_some());
        self.free.push(idx);
        ptr.take().unwrap()
    }

    /// Check if the given index is valid (stores some value)
    pub fn valid(&self, idx: usize) -> bool {
        idx < self.data.len() && self.data[idx].is_some()
    }

    /// How many items can be held without reallocation. Note that the reallocation happens
    /// automatically.
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.data.capacity()
    }

    /// How many items are there stored?
    pub fn len(&self) -> usize {
        self.data.len() - self.free.len()
    }

    /// Is it empty of any content?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Index<usize> for Recycler<T> {
    type Output = T;

    fn index(&self, idx: usize) -> &T {
        let ptr = &self.data[idx];
        assert!(ptr.is_some());
        ptr.as_ref().unwrap()
    }
}

impl<T> IndexMut<usize> for Recycler<T> {
    fn index_mut(&mut self, idx: usize) -> &mut T {
        let ptr = &mut self.data[idx];
        assert!(ptr.is_some());
        ptr.as_mut().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage() {
        let mut r: Recycler<u32> = Recycler::new();
        // When we create a new one, it is empty
        assert_eq!(0, r.data.len());
        assert_eq!(0, r.free.len());
        assert_eq!(0, r.len());
        assert!(r.is_empty());

        // Store something and observe it is there
        assert_eq!(0, r.store(1));
        assert_eq!(1, r.store(2));

        assert_eq!(0, r.free.len());
        assert_eq!(2, r.data.len());
        assert_eq!(vec![Some(1), Some(2)], r.data);
        assert_eq!(2, r.len());
        assert!(!r.is_empty());

        // Indexing checks
        assert_eq!(1, r[0]);
        assert_eq!(2, r[1]);

        // Mutable indexing
        r[0] = 3;
        assert_eq!(3, r[0]);
        assert_eq!(2, r.len());
        assert!(r.len() <= r.capacity());
        let cap = r.capacity();

        // Return one of them
        assert_eq!(2, r.store(4));
        assert_eq!(2, r.release(1));
        assert_eq!(2, r.len());
        assert_eq!(cap, r.capacity()); // The capacity doesn't change by releasing stuff

        // Check the other is still there
        assert_eq!(3, r[0]);

        // Internals
        assert_eq!(vec![Some(3), None, Some(4)], r.data);
        assert_eq!(vec![1], r.free);

        // Store some other now, the space gets reused
        assert_eq!(1, r.store(5));
        assert_eq!(vec![Some(3), Some(5), Some(4)], r.data);
        assert_eq!(0, r.free.len());
        // We don't reallocate when we just recycle some index
        assert_eq!(3, r.len());
        assert_eq!(cap, r.capacity());
    }

    #[test]
    #[should_panic]
    fn index_invalid() {
        let mut r: Recycler<u32> = Recycler::new();
        assert_eq!(0, r.store(0));
        r.release(0);

        assert_eq!(vec![None], r.data);
        assert_eq!(vec![0], r.free);

        r[0] = 3; // This one panics, because we released 0 previously (it's invalid index)
    }

    #[test]
    #[should_panic]
    fn return_twice() {
        let mut r: Recycler<u32> = Recycler::new();
        assert_eq!(0, r.store(1));
        r.release(0);

        assert_eq!(vec![None], r.data);
        assert_eq!(vec![0], r.free);

        r.release(0); // Trying to return this one for a second time
    }

    #[test]
    #[should_panic]
    fn oob() {
        let mut r: Recycler<u32> = Recycler::new();
        r[1] = 2; // Out of bounds, we never got such index (slightly different situation that an index we returned)
    }

    #[test]
    #[should_panic]
    fn oob_return() {
        let mut r: Recycler<u32> = Recycler::new();
        r.release(1); // Out of bounds, we release something we never got (slightly different than returning something twice)
    }
}
