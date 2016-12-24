mod recycler;

// TODO: Some logging

use self::recycler::Recycler;
use error::{Result,Error};
use mio::{Poll,Events,Token,Ready};
use std::num::Wrapping;
use std::any::Any;
use std::collections::VecDeque;
use std::marker::Sized;

#[derive(Debug,Clone,PartialEq,Eq,Hash)]
pub struct TimeoutId(u64);

#[derive(Debug,Clone,PartialEq,Eq,Hash)]
pub struct Handle {
    id: usize,
    generation: u64,
}

pub trait LoopIface<Context> {
    fn with_context<F>(&mut self, f: F) where F: FnOnce(&mut Context);
    fn run_one(&mut self) -> Result<()>;
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()>;
    fn run(&mut self) -> Result<()>;
    fn stop(&mut self);
    fn event_alive(&self, handle: &Handle) -> bool;
    fn event_count(&self) -> usize;
}

pub trait Scope<Context>: LoopIface<Context> {
    fn handle(&self) -> &Handle;
}

pub trait EvAccess<Context, Ev: Event<Context>> {
    fn insert(&mut self, event: Ev) -> Result<Handle>;
    fn post<Data>(&mut self, handle: &Handle, data: Data) -> Result<()> where Ev: Postable<Context, Data>;
}

pub type Response = Result<bool>;

pub trait Event<Context> where Self: Sized {
    fn init<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, scope: &mut S) -> Response;
    fn io<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S, _token: &Token, _ready: &Ready) -> Response { Err(Error::DefaultImpl) }
    fn timeout<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S, _id: &TimeoutId) -> Response { Err(Error::DefaultImpl) }
    fn signal<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S, _signal: i8) -> Response { Err(Error::DefaultImpl) }
    fn idle<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S) -> Response { Err(Error::DefaultImpl) }
    // Any better interface? A way to directly send data to it? A separate trait?
    fn wakeup<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S, _data: Option<Box<Any>>) -> Response { Err(Error::DefaultImpl) }
}

pub trait Postable<Context, Data>: Event<Context> {
    fn deliver<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S, _data: Data) -> Response;
}

struct EvHolder<Event> {
    event: Option<Event>,
    generation: u64,
    // Some other accounting data
}

#[derive(Debug)]
enum EventParam {
    Io(Token, Ready),
    Timeout(TimeoutId),
    Signal(i8),
    Wakeup(Option<Box<Any>>),
}

#[derive(Debug)]
struct BasicEvent {
    recipipent: Handle,
    param: EventParam,
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
    // Preparsed events we received from mio and other sources, ready to be dispatched one by one
    scheduled: VecDeque<BasicEvent>,
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
            scheduled: VecDeque::new(),
        })
    }
    /// Kill an event at given index.
    fn event_kill(&mut self, idx: usize) {
        // TODO: Some other handling, like killing its IOs, timers, etc
        self.events.release(idx);
    }
    // Run function on an event, with the scope and result checking
    fn event_call<F: FnOnce(&mut Ev, &mut LoopScope<Self>) -> Response>(&mut self, handle: &Handle, f: F) -> Result<()> {
        // First check both the index and generation
        if !self.events.valid(handle.id) {
            return Err(Error::Missing)
        }
        if self.events[handle.id].generation != handle.generation {
            return Err(Error::Missing)
        }
        // Try to extract the event out (because of the borrow checker)
        if let Some(mut event) = self.events[handle.id].event.take() {
            // Perform the call
            let result = {
                let mut scope: LoopScope<Self> = LoopScope {
                    event_loop: self,
                    handle: handle.clone(),
                };
                f(&mut event, &mut scope)
            };
            // Return the event we took out
            self.events[handle.id].event = Some(event);
            match result {
                Ok(true) => (), // Keep the event alive
                /*
                 * Kill it either because it failed or because it said it doesn't want to continue
                 * any more.
                 */
                _ => self.event_kill(handle.id)
            }
            // Get rid of the Ok content, but leave error intact
            result.map(|_| ())
        } else {
            // Not there, but the holder is ‒ it must sit on some outer stack frame, being called
            Err(Error::Busy)
        }
    }
}

impl<Context, Ev: Event<Context>> LoopIface<Context> for Loop<Context, Ev> {
    /// Access the stored context
    fn with_context<F>(&mut self, f: F) where F: FnOnce(&mut Context) {
        f(&mut self.context)
    }
    fn run_one(&mut self) -> Result<()> {
        unimplemented!();
    }
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()> {
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
    fn run(&mut self) -> Result<()> {
        self.active = true;
        while self.active {
            self.run_one()?
        }
        Ok(())
    }
    fn stop(&mut self) {
        self.active = false;
    }
    fn event_alive(&self, handle: &Handle) -> bool {
        self.events.valid(handle.id) && self.events[handle.id].generation == handle.generation
    }
    fn event_count(&self) -> usize {
        self.events.len()
    }
}

impl<Context, Ev: Event<Context>> EvAccess<Context, Ev> for Loop<Context, Ev> {
    fn insert(&mut self, event: Ev) -> Result<Handle> {
        // Assign a new generation
        let Wrapping(generation) = self.generation;
        self.generation += Wrapping(1);
        // Store the event in the storage
        let idx = self.events.store(EvHolder {
            event: Some(event),
            generation: generation
        });
        // Generate a handle for it
        let handle = Handle {
            id: idx,
            generation: generation
        };
        // Run the init for the event
        self.event_call(&handle, |event, context| event.init(context))?;
        Ok(handle)
    }
    fn post<Data>(&mut self, handle: &Handle, data: Data) -> Result<()> where Ev: Postable<Context, Data> {
        self.event_call(handle, |event, scope| event.deliver(scope, data))
    }
}

pub struct LoopScope<'a, Loop: 'a> {
    event_loop: &'a mut Loop,
    handle: Handle,
}

impl<'a, Context, Ev: Event<Context>> LoopIface<Context> for LoopScope<'a, Loop<Context, Ev>> {
    fn with_context<F>(&mut self, f: F) where F: FnOnce(&mut Context) { self.event_loop.with_context(f) }
    fn stop(&mut self) { self.event_loop.stop() }
    fn run_one(&mut self) -> Result<()> { self.event_loop.run_one() }
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()> { self.event_loop.run_until_complete(handle) }
    fn run(&mut self) -> Result<()> { self.event_loop.run() }
    fn event_alive(&self, handle: &Handle) -> bool { self.event_loop.event_alive(handle) }
    fn event_count(&self) -> usize { self.event_loop.event_count() }
}

impl<'a, Context, Ev: Event<Context>> Scope<Context> for LoopScope<'a, Loop<Context, Ev>> {
    fn handle(&self) -> &Handle { &self.handle }
}

impl<'a, Context, Ev: Event<Context>> EvAccess<Context, Ev> for LoopScope<'a, Loop<Context, Ev>> {
    fn insert(&mut self, event: Ev) -> Result<Handle> { self.event_loop.insert(event) }
    fn post<Data>(&mut self, handle: &Handle, data: Data) -> Result<()> where Ev: Postable<Context, Data> { self.event_loop.post(handle, data) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::error::*;
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

    /**
     * Test we can access the context from within an event and its init gets called.
     * Also test the thing gets destroyed.
     */
    #[test]
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
        l.run_until_complete(&handle).unwrap();
        assert_eq!(0, l.event_count());
    }

    struct Recipient;

    impl<C> Event<C> for Recipient {
        fn init<S>(&mut self, _scope: &mut S) -> Response {
            // Stay alive for now
            Ok(true)
        }
    }

    // Terminate when getting ()
    impl<C> Postable<C, ()> for Recipient {
        fn deliver<S>(&mut self, _scope: &mut S, _data: ()) -> Response {
            // Give up after receiving anything
            Ok(false)
        }
    }

    fn err<T>(result: Result<T>, error: Error) {
        assert!(match result {
            Err(error) => true,
            _ => false
        })
    }

    /**
     * Test we can post to an event.
     */
    #[test]
    fn post() {
        // Create an event that stays there, at least for a while
        let mut l = Loop::new(()).unwrap();
        let handle = l.insert(Recipient).unwrap();
        assert!(l.event_alive(&handle));
        // Post something to it (successfully)
        l.post(&handle, ()).unwrap();
        // But it received it and went away
        assert!(!l.event_alive(&handle));
        // And if we try to send again, it fails with Missing
        err(l.post(&handle, ()), Error::Missing);
    }

    struct Recurse;
    // Recurse to self
    impl<C> Postable<C, Recurse> for Recipient {
        fn deliver<S: Scope<C> + EvAccess<C, Self>>(&mut self, scope: &mut S, data: Recurse) -> Response {
            let handle = scope.handle().clone();
            scope.post(&handle, data)?;
            Ok(true)
        }
    }

    /**
     * Test we detect busy/recursive post access to an event.
     */
    #[test]
    fn busy_post() {
        // Create an event that stays there
        let mut l = Loop::new(()).unwrap();
        let handle = l.insert(Recipient).unwrap();
        assert!(l.event_alive(&handle));
        // Post a Recurse to it, which would create an infinite recursion of posting to self
        err(l.post(&handle, Recurse), Error::Busy);
        /*
         * As it returned error, it got killed (it returned error because it called itself, not
         * because it was called while busy).
         */
        assert!(!l.event_alive(&handle));
    }
}
