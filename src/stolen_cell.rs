use std::cell::UnsafeCell;

// We need to pass both the event and the event loop as mutable references. But that is a problem,
// since one lives inside the other. The original solution was to have the event inside an Option
// and take it out (locking it), then return it back after the callback. That can be slow in case
// the event is large.
//
// This way we somewhat emulate the Option, but without the copying. We lock the thing from further
// accesses, but don't copy it. This is unsafe in theory, as we don't hold a borrow of the
// underlying data, it could go away and we would have a dangling pointer. This, however, doesn't
// happen in our case, since the event holder must be there for the length of the callback anyway.

pub struct StolenCell<T> {
    data: UnsafeCell<T>,
    stolen: bool,
}

impl<T> StolenCell<T> {
    pub fn new(data: T) -> StolenCell<T> {
        StolenCell {
            data: UnsafeCell::new(data),
            stolen: false,
        }
    }
    pub fn into_inner(self) -> T {
        assert!(!self.stolen);
        unsafe { self.data.into_inner() }
    }
    pub fn steal(&mut self) -> StolenData<T> {
        assert!(!self.stolen);
        self.stolen = true;
        StolenData(self.data.get())
    }
    pub fn unsteal(&mut self, data: StolenData<T>) {
        assert_eq!(data.0, self.data.get());
        self.stolen = false;
    }
    pub fn stolen(&self) -> bool {
        self.stolen
    }
}

pub struct StolenData<T>(*mut T);

impl<T> StolenData<T> {
    pub unsafe fn as_mut(&mut self) -> &mut T {
        self.0.as_mut().unwrap()
    }
}
