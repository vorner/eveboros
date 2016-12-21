
use error::Result;
use mio::{Poll,Events};
use std::marker::PhantomData;
use std::time::Duration;

pub struct Loop {
    poll: Poll,
    events: Events,
    active: bool,
    /*
     * TODO: We need some way to store the events. Even the ones that are not
     * implemented directly by the loop.
     */
}

pub trait Event<Value> {
    fn call(&mut self, event_loop: &mut Loop, handle: &Handle<Value>, value: Value);
}

#[derive(Clone,Copy,Debug)]
pub struct Handle<Value> {
    _data: PhantomData<Value>,
}

pub trait LoopRegistrar<Event, Handle, Parameter> {
    fn register(&mut self, event: Event, param: &Parameter) -> Handle;
    // May fail if the event doesn't exist
    fn reregister(&mut self, handle: Handle, param: &Parameter) -> Result<()>;
    // May fail if the event doesn't exist
    fn deregister(&mut self, handle: Handle) -> Result<Event>;
    // May fail if the event doesn't exist
    fn borrow<F, R>(&self, func: F) -> Result<R> where F: FnOnce(&Event) -> R;
    // May fail if the event doesn't exist
    fn borrow_mut<F, R>(&mut self, func: F) -> Result<R> where F: FnOnce(&mut Event) -> R;
}

impl Loop {
    /**
     * Create a new Loop.
     *
     * The loop is empty, holds no events, but is otherwise ready.
     */
    pub fn new() -> Result<Self> {
        Ok(Loop {
            poll: Poll::new()?,
            events: Events::with_capacity(1024),
            active: false,
        })
    }
    pub fn run_one(&mut self) -> Result<()> {
        unimplemented!();
    }
    pub fn run_until<Value>(&mut self, handle: Handle<Value>) -> Result<()> {
        unimplemented!();
    }
    pub fn run(&mut self) -> Result<()> {
        self.active = true;
        while self.active {
            self.run_one()?
        }
        Ok(())
    }
}

trait TimerEvent: Event<Duration> {}
impl<E: Event<Duration>> TimerEvent for E {}
type TimerHandler = Handle<Duration>;
