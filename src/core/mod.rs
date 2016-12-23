mod recycler;

use self::recycler::Recycler;
use error::{Result,Error};
use mio::{Poll,Events,Token,Ready};
use std::num::Wrapping;

pub struct TimeoutId(u64);
pub struct WakeupId(u64);

#[derive(Debug,Clone)]
pub struct Handle {
    id: usize,
    generation: u64,
}

pub trait LoopIface<Context, Ev> {
    fn insert(&mut self, event: Ev) -> Result<Handle>;
}

pub trait Scope<Context> {
    fn with_context<F>(&mut self, f: F) where F: FnOnce(&mut Context);
    fn stop(&mut self);
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()>;
}

pub type Response = Result<bool>;

pub trait Event<Context> {
    fn init<S: Scope<Context>>(&mut self, scope: &mut S) -> Response;
    fn io<S: Scope<Context>>(&mut self, scope: &mut S, token: &Token, ready: &Ready) -> Response { Err(Error::DefaultImpl) }
    fn timeout<S: Scope<Context>>(&mut self, scope: &mut S, id: &TimeoutId) -> Response { Err(Error::DefaultImpl) }
    fn signal<S: Scope<Context>>(&mut self, scope: &mut S, signal: i8) -> Response { Err(Error::DefaultImpl) }
    fn idle<S: Scope<Context>>(&mut self, scope: &mut S) -> Response { Err(Error::DefaultImpl) }
    // Any better interface? A way to directly send data to it? A separate trait?
    fn wakeup<S: Scope<Context>>(&mut self, scope: &mut S, id: &WakeupId) -> Response { Err(Error::DefaultImpl) }
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

impl<Context, Ev: Event<Context>> Loop<Context, Ev> {
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
    /// Kill an event at given index.
    fn event_kill(&mut self, idx: usize) {
        // TODO: Some other handling, like killing its IOs, timers, etc
        self.events.release(idx);
    }
    /// Access the stored context
    pub fn with_context<F>(&mut self, f: F) where F: FnOnce(&mut Context) {
        f(&mut self.context)
    }
    pub fn event_alive(&self, handle: &Handle) -> bool {
        self.events.valid(handle.id) && self.events[handle.id].generation == handle.generation
    }
    pub fn run_one(&mut self) -> Result<()> {
        unimplemented!();
    }
    pub fn run_until_complete(&mut self, handle: &Handle) -> Result<()> {
        let mut checked = false;
        while self.event_alive(handle) {
            /*
             * We want to deteckt a deadlock when an event that is curretly running is recursively
             * waited on. We do that check on the first iteration only, as it can't change later
             * on. We know the event is alive, so we don't have to check for the validity of
             * the id or if it got reused.
             */
            if !checked && self.events[handle.id].event.is_none() {
                return Err(Error::DeadLock)
            }
            checked = true;
            self.run_one()?
        }
        Ok(())
    }
    pub fn run(&mut self) -> Result<()> {
        self.active = true;
        while self.active {
            self.run_one()?
        }
        Ok(())
    }
    pub fn stop(&mut self) {
        self.active = false;
    }
}

impl<Context, Ev: Event<Context>> LoopIface<Context, Ev> for Loop<Context, Ev> {
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
            let mut scope: LoopScope<Self> = LoopScope {
                event_loop: self,
                handle: handle.clone(),
            };
            event.init(&mut scope)
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

pub struct LoopScope<'a, Loop: 'a> {
    event_loop: &'a mut Loop,
    handle: Handle,
}

impl<'a, Context, Ev: Event<Context>> Scope<Context> for LoopScope<'a, Loop<Context, Ev>> {
    fn with_context<F>(&mut self, f: F) where F: FnOnce(&mut Context) {
        self.event_loop.with_context(f)
    }
    fn stop(&mut self) {
        self.event_loop.stop()
    }
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()> {
        self.event_loop.run_until_complete(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::cell::Cell;

    struct InitAndContextEvent(Rc<Cell<bool>>);

    impl Event<bool> for InitAndContextEvent {
        fn init<S: Scope<bool>>(&mut self, scope: &mut S) -> Response {
            scope.with_context(|c| *c = true);
            Ok(false)
        }
    }

    impl Drop for InitAndContextEvent {
        fn drop(&mut self) {
            let InitAndContextEvent(ref flag) = *self;
            flag.set(true);
        }
    }

    #[test]
    /**
     * Test we can access the context from within an event and its init gets called.
     * Also test the thing gets destroyed.
     */
    fn init_and_context() {
        let destroyed = Rc::new(Cell::new(false));
        let mut l = Loop::new(false).unwrap();
        let handle = l.insert(InitAndContextEvent(destroyed.clone())).unwrap();
        // Init got called (the context gets set to true there
        l.with_context(|c| assert!(*c));
        /*
         * As the event returns false from its init, the destructor should have gotten called
         * by now (even before the loop itself)
         */
        assert!(destroyed.get());
        // And it is not alive (obviously)
        assert!(!l.event_alive(&handle));
        // As it is not alive, run_until_complete finishes right away
        l.run_until_complete(&handle);
    }
}
