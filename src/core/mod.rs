mod recycler;

use error::Result;
use mio::{Poll,Events,Token,Ready};
use std::marker::PhantomData;

pub struct TimeoutId(usize);
pub struct WakeupId(usize);

pub struct Loop<Context, Ev> {
    poll: Poll,
    events: Events,
    active: bool,
    context: Context,
    _phantom: PhantomData<Ev>,  // So it compiles
    /*
     * TODO: We need some way to store the events. Even the ones that are not
     * implemented directly by the loop.
     */
}

pub trait LoopIface<Context, Ev> {
}

pub struct Scope<'a, Loop: 'a> {
    event_loop: &'a mut Loop,
}

pub type Response = Result<bool>;

pub trait Event<Loop> {
    fn io<'a>(&mut self, scope: &Scope<'a, Loop>, token: &Token, ready: &Ready) -> Response;
    fn timeout<'a>(&mut self, scope: &Scope<'a, Loop>, id: &TimeoutId) -> Response;
    fn signal<'a>(&mut self, scope: &Scope<'a, Loop>, signal: i8) -> Response;
    fn wakeup<'a>(&mut self, scope: &Scope<'a, Loop>, id: &WakeupId) -> Response;
}

pub struct Handle<Context, Ev> {
    _data: PhantomData<Context>,
    _event: PhantomData<Ev>,
}

impl<Context, Ev: Event<Context>> Loop<Context, Ev> {
    /**
     * Create a new Loop.
     *
     * The loop is empty, holds no events, but is otherwise ready.
     */
    pub fn new(context: Context) -> Result<Self> {
        Ok(Loop {
            poll: Poll::new()?,
            events: Events::with_capacity(1024),
            active: false,
            context: context,
            _phantom: PhantomData,
        })
    }
    pub fn run_one(&mut self) -> Result<()> {
        unimplemented!();
    }
    pub fn run_until(&mut self, handle: &Handle<Context, Ev>) -> Result<()> {
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
