mod recycler;

use self::recycler::Recycler;
use error::Result;
use mio::{Poll,Events,Token,Ready};
use std::num::Wrapping;

pub struct TimeoutId(usize);
pub struct WakeupId(usize);

#[derive(Clone)]
pub struct Handle {
    id: usize,
    generation: u64,
}

pub trait LoopIface<Context, Ev> {
    fn insert(&mut self, event: Ev) -> Result<Handle>;
}

pub struct Scope<'a, Loop: 'a> {
    event_loop: &'a mut Loop,
    handle: Handle,
}

pub type Response = Result<bool>;

// TODO: Default implementations
pub trait Event<Loop> {
    fn init<'a>(&'a mut self, scope: &Scope<'a, Loop>) -> Response;
    fn io<'a>(&'a mut self, scope: &Scope<'a, Loop>, token: &Token, ready: &Ready) -> Response;
    fn timeout<'a>(&'a mut self, scope: &Scope<'a, Loop>, id: &TimeoutId) -> Response;
    fn signal<'a>(&'a mut self, scope: &Scope<'a, Loop>, signal: i8) -> Response;
    // Any better interface? A way to directly send data to it? A separate trait?
    fn wakeup<'a>(&'a mut self, scope: &Scope<'a, Loop>, id: &WakeupId) -> Response;
}

struct EvHolder<Event> {
    event: Option<Event>,
    generation: u64,
    // Some other accounting data
}

pub struct Loop<Context, Ev> {
    poll: Poll,
    mio_events: Events,
    active: bool,
    context: Context,
    events: Recycler<EvHolder<Ev>>,
    /*
     * We try to detect referring to an event with Handle from event that had the same index as us
     * and the index got reused by adding generation to the event. It is very unlikely we would
     * hit the very same index and go through all 2^64 iterations to cause hitting a collision.
     */
    generation: Wrapping<u64>,
}

impl<Context, Ev: Event<Loop<Context, Ev>>> Loop<Context, Ev> {
    /**
     * Create a new Loop.
     *
     * The loop is empty, holds no events, but is otherwise ready.
     */
    pub fn new(context: Context) -> Result<Self> {
        Ok(Loop {
            poll: Poll::new()?,
            mio_events: Events::with_capacity(1024),
            active: false,
            context: context,
            events: Recycler::new(),
            generation: Wrapping(0),
        })
    }
    pub fn run_one(&mut self) -> Result<()> {
        unimplemented!();
    }
    pub fn run_until(&mut self, handle: &Handle) -> Result<()> {
        unimplemented!();
    }
    pub fn run(&mut self) -> Result<()> {
        self.active = true;
        while self.active {
            self.run_one()?
        }
        Ok(())
    }
    /// Kill an event at given index.
    fn event_kill(&mut self, idx: usize) {
        // TODO: Some other handling, like killing its IOs, timers, etc
    }
}

impl<Context, Ev: Event<Loop<Context, Ev>>> LoopIface<Context, Ev> for Loop<Context, Ev> {
    fn insert(&mut self, event: Ev) -> Result<Handle> {
        let mut event = event;
        // Assign a new generation
        let Wrapping(generation) = self.generation;
        self.generation += Wrapping(1);
        // Store the event in the storage
        let idx = self.events.store(EvHolder {
            event: None, // We shall put the event there later on
            generation: generation
        });
        // Generate a handle for it
        let handle = Handle {
            id: idx,
            generation: generation
        };
        // Run the init for the event
        let init_result = {
            let event = &mut event;
            let scope = Scope {
                event_loop: self,
                handle: handle.clone(),
            };
            event.init(&scope)
        };
        // Store the event where it belongs
        self.events[idx].event = Some(event);
        // Decide according to the result
        match init_result {
            Err(e) => {
                self.event_kill(idx);
                return Err(e)
            },
            Ok(false) => self.event_kill(idx),
            Ok(true) => (),
        }
        Ok(handle)
    }
}
