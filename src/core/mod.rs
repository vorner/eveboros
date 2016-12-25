mod recycler;

// TODO: Some logging

// TODO: How do we propagate errors? Kill the loop? Error handler?

use self::recycler::Recycler;
use error::{Result,Error};
use mio::{Poll,Events,Token,Ready,Evented,PollOpt};
use std::num::Wrapping;
use std::any::Any;
use std::collections::{VecDeque,BinaryHeap,HashSet};
use std::marker::Sized;
use std::time::{Instant,Duration};
use std::cmp::Ordering;
use std::mem::replace;

#[derive(Debug,Clone,PartialEq,Eq,Hash,Ord,PartialOrd)]
pub struct IoId {
    token: Token,
}

#[derive(Debug,Clone,PartialEq,Eq,Hash,Ord,PartialOrd)]
pub struct TimeoutId {
    id: u64
}

#[derive(Debug,Clone,PartialEq,Eq,Hash,Ord,PartialOrd)]
pub struct Handle {
    id: usize,
    generation: u64,
}

pub trait LoopIface<Context> {
    fn with_context<F: FnOnce(&mut Context) -> Result<()>>(&mut self, f: F) -> Result<()>;
    fn run_one(&mut self) -> Result<()>;
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()>;
    fn run(&mut self) -> Result<()>;
    fn stop(&mut self);
    fn event_alive(&self, handle: &Handle) -> bool;
    fn event_count(&self) -> usize;
    // This one may be cached and little bit behind in case of long CPU computations
    fn now(&self) -> &Instant;
}

pub trait Scope<Context>: LoopIface<Context> {
    fn handle(&self) -> &Handle;
    fn timeout_at(&mut self, when: Instant) -> TimeoutId;
    fn timeout_after(&mut self, after: &Duration) -> TimeoutId {
        let at = *self.now() + *after;
        self.timeout_at(at)
    }
    fn io_register<E: Evented + 'static>(&mut self, io: E, interest: Ready, opts: PollOpt) -> Result<IoId>;
    fn io_update(&mut self, id: &IoId, interest: Ready, opts: PollOpt) -> Result<()>;
    fn io_remove(&mut self, id: &IoId) -> Result<()>;
    fn with_io<E: Evented + 'static, R, F: FnOnce(&mut E) -> Result<R>>(&mut self, id: &IoId, f: F) -> Result<R>;
}

pub trait EvAccess<Context, Ev: Event<Context>> {
    fn insert(&mut self, event: Ev) -> Result<Handle>;
    fn post<Data>(&mut self, handle: &Handle, data: Data) -> Result<()> where Ev: Postable<Context, Data>;
}

pub type Response = Result<bool>;

pub trait Event<Context> where Self: Sized {
    fn init<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, scope: &mut S) -> Response;
    fn io<S: Scope<Context> + EvAccess<Context, Self>>(&mut self, _scope: &mut S, _id: &IoId, _ready: Ready) -> Response { Err(Error::DefaultImpl) }
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
    // How many timeouts does it have?
    timeouts: usize,
    // The actual Ids of this event holder (we need to drop them when the event dies)
    ios: HashSet<IoId>,
    // Some other accounting data
}

#[derive(Debug)]
enum TaskParam {
    Io(IoId, Ready),
    Timeout(TimeoutId),
    Signal(i8),
    Wakeup(Option<Box<Any>>),
}

#[derive(Debug)]
struct Task {
    recipient: Handle,
    param: TaskParam,
}

#[derive(Debug,Eq,PartialEq)]
struct TimeoutHolder {
    id: TimeoutId,
    recipient: Handle,
    when: Instant,
}

impl Ord for TimeoutHolder {
    fn cmp(&self, other: &Self) -> Ordering {
        // Flip around (we want the smaller ones first)
        other.when.cmp(&self.when)
    }
}

impl PartialOrd for TimeoutHolder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

pub type ErrorHandler<Ev> = Box<FnMut(Ev, Error) -> Result<()>>;

struct IoHolder<E: Evented> {
    recipient: Handle,
    io: E,
}

trait IoHolderAny: Any {
    fn io(&self) -> &Evented;
    fn recipient(&self) -> &Handle;
    fn as_any_mut(&mut self) -> &mut Any;
}

impl<E: Evented + 'static> IoHolderAny for IoHolder<E> {
    fn io(&self) -> &Evented { &self.io }
    fn recipient(&self) -> &Handle { &self.recipient }
    fn as_any_mut(&mut self) -> &mut Any { self }
}

const TOKEN_SHIFT: usize = 2;

pub struct Loop<Context, Ev> {
    poll: Poll,
    mio_events: Events,
    active: bool,
    context: Context,
    error_handler: ErrorHandler<Ev>,
    events: Recycler<EvHolder<Ev>>,
    /*
     * We try to detect referring to an event with Handle from event that had the same index as us
     * and the index got reused by adding generation to the event. It is very unlikely we would
     * hit the very same index and go through all 2^64 iterations to cause hitting a collision.
     */
    generation: Wrapping<u64>,
    // Preparsed events we received from mio and other sources, ready to be dispatched one by one
    scheduled: VecDeque<Task>,
    /*
     * The timeouts we want to fire some time. We don't prune the ones from events that died,
     * but we ignore them when they die.
     *
     * We don't remove timeouts of killed events right away, we ignore them when they fire.
     * However, when there's more dead ones than live ones, we go through it whole and prune them.
     */
    timeouts: BinaryHeap<TimeoutHolder>,
    // How many were dropped because of dying event?
    timeouts_dead: usize,
    // How many did fire?
    timeouts_fired: usize,
    // How many were created?
    timeouts_inserted: usize,
    timeout_generation: Wrapping<u64>,
    // Cached from the last tick of the loop
    now: Instant,
    ios: Recycler<Option<Box<IoHolderAny>>>,
    ios_released: HashSet<usize>,
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
            error_handler: Box::new(|_, error| Err(error)),
            events: Recycler::new(),
            generation: Wrapping(0),
            scheduled: VecDeque::new(),
            timeouts: BinaryHeap::new(),
            timeouts_dead: 0,
            timeouts_fired: 0,
            timeouts_inserted: 0,
            timeout_generation: Wrapping(0),
            now: Instant::now(),
            ios: Recycler::new(),
            ios_released: HashSet::new(),
        })
    }
    pub fn error_handler_set(&mut self, handler: ErrorHandler<Ev>) { self.error_handler = handler }
    /// Kill an event at given index.
    fn event_kill(&mut self, idx: usize) -> Option<Ev> {
        // TODO: Some other handling, like killing its IOs, timers, etc
        let event = self.events.release(idx);
        // These timeouts are dead now
        self.timeouts_dead += event.timeouts;
        event.event
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
                Ok(false) => { self.event_kill(handle.id); }, // Kill it as asked for
                Err(err) => {
                    // We can unwrap, we just returned the event there a moment ago
                    let event = self.event_kill(handle.id).unwrap();
                    /*
                     * Call the error handler with the broken event and propagate any error it
                     * returns
                     */
                    (self.error_handler)(event, err)?;
                }
            }
            Ok(())
        } else {
            // Not there, but the holder is ‒ it must sit on some outer stack frame, being called
            Err(Error::Busy)
        }
    }
    fn timeout_at(&mut self, handle: &Handle, when: Instant) -> TimeoutId {
        // Generate an ID for the timeout
        let Wrapping(id) = self.timeout_generation;
        let id = TimeoutId { id: id };
        self.timeout_generation += Wrapping(1);
        // This gets called only through the event's context, so it's safe
        self.events[handle.id].timeouts += 1;
        self.timeouts_inserted += 1;

        self.timeouts.push(TimeoutHolder {
            id: id.clone(),
            recipient: handle.clone(),
            when: when
        });

        id
    }
    /**
     * Compute when to wake up latest (we can wake up sooner). This takes the scheduled timeouts
     * into consideration, but also things like existence of Idle tasks.
     */
    fn timeout_min(&mut self) -> Option<Duration> {
        /*
         * First, get rid of all timeouts to dead events (since we don't want to wake up because of
         * them).
         */
        while self.timeouts.peek().map_or(false, |t| !self.event_alive(&t.recipient)) {
            self.timeouts.pop();
        }
        // The computations may have taken some time, so get a fresh now.
        let now = Instant::now();
        self.timeouts.peek().map(|t| {
            if t.when < now {
                Duration::new(0, 0)
            } else {
                t.when.duration_since(now)
            }
        })
    }
    /**
     * Gather some tasks. They get added in priority (eg. if there are some IO tasks, no timeouts
     * are added to the queue):
     *
     * * IO tasks, signals, Wakeups
     * * Timeouts
     * * Idle tasks
     *
     * It is possible this produces no tasks.
     */
    fn tasks_gather(&mut self) -> Result<()> {
        assert!(self.scheduled.is_empty());
        // Release the IO ids we held, there are no more things in queue which could trigger them
        for id in self.ios_released.drain() {
            assert!(self.ios.release(id).is_none());
        }
        if self.events.is_empty() {
            // We can't gather any tasks if there are no events, we would just block forever
            return Err(Error::Empty);
        }
        let wakeup = self.timeout_min();
        self.poll.poll(&mut self.mio_events, wakeup)?;
        // We slept a while, update the now cache
        self.now = Instant::now();

        if self.mio_events.is_empty() {
            /*
             * If there are too many dead timeouts, clean them up.
             * Have some limit for low amount of events, where we don't bother.
             */
            let handled = self.timeouts_inserted - self.timeouts.len();
            let handled_dead = handled - self.timeouts_fired;
            let still_dead = self.timeouts_dead - handled_dead;
            let still_alive = self.timeouts.len() - still_dead;
            // Reset the stats
            self.timeouts_dead = still_dead;
            self.timeouts_fired = 0;
            self.timeouts_inserted = self.timeouts.len();
            // Check for too many dead ones in the storage
            if still_dead > 5 && still_alive * 3 < still_dead {
                let mut old = replace(&mut self.timeouts, BinaryHeap::new());
                let pruned: BinaryHeap<TimeoutHolder> = old.drain().filter(|ref t| self.event_alive(&t.recipient)).collect();
                self.timeouts = pruned;
                self.timeouts_dead = 0;
                self.timeouts_inserted = self.timeouts.len();
            }

            while self.timeouts.peek().map_or(false, |t| t.when <= self.now) {
                // This must be Some(...), because of the condition above
                let timeout = self.timeouts.pop().unwrap();
                self.scheduled.push_back(Task {
                    recipient: timeout.recipient,
                    param: TaskParam::Timeout(timeout.id),
                })
            }
            Ok(())
            // TODO: If none added, look for idle tasks
        } else {
            for ev in self.mio_events.iter() {
                let Token(mut idx) = ev.token();
                if idx >= TOKEN_SHIFT {
                    idx -= TOKEN_SHIFT;
                    /*
                     * We should not get any invalid events or IOs now, we didn't fire any events
                     * yet
                     */
                    self.scheduled.push_back(Task {
                        recipient: self.ios[idx].as_ref().unwrap().recipient().clone(),
                        param: TaskParam::Io(IoId { token: ev.token() }, ev.kind()),
                    });
                } else {
                    // For now, we don't have the special FDs yet
                    unreachable!();
                }
            }
            Ok(())
        }
    }
}

impl<Context, Ev: Event<Context>> LoopIface<Context> for Loop<Context, Ev> {
    /// Access the stored context
    fn with_context<F: FnOnce(&mut Context) -> Result<()>>(&mut self, f: F) -> Result<()> {
        f(&mut self.context)
    }
    fn run_one(&mut self) -> Result<()> {
        // We loop until we find a task that is for a living event
        loop {
            while self.scheduled.is_empty() {
                // Make sure we have something to do
                self.tasks_gather()?
            }
            let task = self.scheduled.pop_front().unwrap();
            if self.event_alive(&task.recipient) {
                return match task.param {
                    TaskParam::Io(id, ready) => {
                        let Token(mut idx) = id.token;
                        idx -= TOKEN_SHIFT;
                        // Check that the IO is still valid
                        if self.ios.valid(idx) && self.ios[idx].is_some() {
                            self.event_call(&task.recipient, |event, context| event.io(context, &id, ready))
                        } else {
                            // It's OK to lose the IO in the meantime
                            Ok(())
                        }
                    },
                    TaskParam::Timeout(id) => {
                        self.timeouts_fired += 1;
                        self.events[task.recipient.id].timeouts -= 1;
                        self.event_call(&task.recipient, |event, context| event.timeout(context, &id))
                    },
                    TaskParam::Signal(_signal) => unimplemented!(),
                    TaskParam::Wakeup(_data) => unimplemented!(),
                }
            }
        }
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
    fn event_count(&self) -> usize { self.events.len() }
    fn now(&self) -> &Instant { &self.now }
}

impl<Context, Ev: Event<Context>> EvAccess<Context, Ev> for Loop<Context, Ev> {
    fn insert(&mut self, event: Ev) -> Result<Handle> {
        // Assign a new generation
        let Wrapping(generation) = self.generation;
        self.generation += Wrapping(1);
        // Store the event in the storage
        let idx = self.events.store(EvHolder {
            event: Some(event),
            generation: generation,
            timeouts: 0,
            ios: HashSet::new(),
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

impl<'a, Context, Ev: Event<Context>> LoopScope<'a, Loop<Context, Ev>> {
    fn io_idx(&mut self, id: &IoId) -> Result<usize> {
        let Token(mut idx) = id.token;
        idx -= TOKEN_SHIFT;
        if !self.event_loop.ios.valid(idx) || self.event_loop.ios[idx].as_ref().map_or(true, |io| *io.recipient() != self.handle) {
            return Err(Error::MissingIo);
        }
        Ok(idx)
    }
}

impl<'a, Context, Ev: Event<Context>> LoopIface<Context> for LoopScope<'a, Loop<Context, Ev>> {
    fn with_context<F: FnOnce(&mut Context) -> Result<()>>(&mut self, f: F) -> Result<()> { self.event_loop.with_context(f) }
    fn stop(&mut self) { self.event_loop.stop() }
    fn run_one(&mut self) -> Result<()> { self.event_loop.run_one() }
    fn run_until_complete(&mut self, handle: &Handle) -> Result<()> { self.event_loop.run_until_complete(handle) }
    fn run(&mut self) -> Result<()> { self.event_loop.run() }
    fn event_alive(&self, handle: &Handle) -> bool { self.event_loop.event_alive(handle) }
    fn event_count(&self) -> usize { self.event_loop.event_count() }
    fn now(&self) -> &Instant { self.event_loop.now() }
}

impl<'a, Context, Ev: Event<Context>> Scope<Context> for LoopScope<'a, Loop<Context, Ev>> {
    fn handle(&self) -> &Handle { &self.handle }
    fn timeout_at(&mut self, when: Instant) -> TimeoutId { self.event_loop.timeout_at(&self.handle, when) }
    fn io_register<E: Evented + 'static>(&mut self, io: E, interest: Ready, opts: PollOpt) -> Result<IoId> {
        let id = self.event_loop.ios.store(Some(Box::new(IoHolder {
            recipient: self.handle.clone(),
            io: io
        })));
        let token = Token(id + TOKEN_SHIFT);
        if let Err(err) = self.event_loop.poll.register(self.event_loop.ios[id].as_ref().unwrap().io(), token, interest, opts) {
            // If it fails, we want to get rid of the stored io first before returning the error
            self.event_loop.ios.release(id);
            return Err(Error::Io(err))
        }
        let id = IoId { token: token };
        self.event_loop.events[self.handle.id].ios.insert(id.clone());
        Ok(id)
    }
    fn io_update(&mut self, id: &IoId, interest: Ready, opts: PollOpt) -> Result<()> {
        let idx = self.io_idx(id)?;
        self.event_loop.poll.reregister(self.event_loop.ios[idx].as_ref().unwrap().io(), id.token, interest, opts).map_err(Error::Io)
    }
    fn io_remove(&mut self, id: &IoId) -> Result<()> {
        let idx = self.io_idx(id)?;
        // If the io is valid, remove it from both indexes
        self.event_loop.events[self.handle.id].ios.remove(id);
        /*
         * Remove the registration.
         *
         * Note that we still keep the ID held and will release it only before we gather new tasks.
         * This way we can prevent the ID being reused and get triggered by an old event.
         */
        self.event_loop.ios_released.insert(idx);
        self.event_loop.poll.deregister(self.event_loop.ios[idx].take().unwrap().io()).map_err(Error::Io)
        // TODO: Find a way to return the thing?
    }
    fn with_io<E: Evented + 'static, R, F: FnOnce(&mut E) -> Result<R>>(&mut self, id: &IoId, f: F) -> Result<R> {
        let idx = self.io_idx(id)?;
        // Madness to get the real type of the object
        let iob: Option<&mut Box<IoHolderAny>> = self.event_loop.ios[idx].as_mut();
        let io: &mut IoHolder<E> = iob.unwrap().as_any_mut().downcast_mut().ok_or(Error::IoType)?;
        f(&mut io.io)
    }
}

impl<'a, Context, Ev: Event<Context>> EvAccess<Context, Ev> for LoopScope<'a, Loop<Context, Ev>> {
    fn insert(&mut self, event: Ev) -> Result<Handle> { self.event_loop.insert(event) }
    fn post<Data>(&mut self, handle: &Handle, data: Data) -> Result<()> where Ev: Postable<Context, Data> { self.event_loop.post(handle, data) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::error::*;
    use mio::tcp::{TcpListener,TcpStream};
    use mio::{Ready,PollOpt};
    use std::rc::Rc;
    use std::cell::Cell;
    use std::time::Duration;
    use std::net::{IpAddr,Ipv4Addr,SocketAddr};
    use std::io::Write;

    struct InitAndContextEvent(Rc<Cell<bool>>);

    impl Event<bool> for InitAndContextEvent {
        fn init<S: Scope<bool>>(&mut self, scope: &mut S) -> Response {
            scope.with_context(|c| {
                *c = true;
                Ok(())
            }).map(|_| false)
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
        l.with_context(|c| {
            assert!(*c);
            Ok(())
        }).unwrap();
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

    macro_rules! err {
        ($result:expr, $err: pat) => (assert!(match $result { Err($err) => true, _ => false }))
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
        err!(l.post(&handle, ()), Error::Missing);
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
        err!(l.post(&handle, Recurse), Error::Busy);
        /*
         * As it returned error, it got killed (it returned error because it called itself, not
         * because it was called while busy).
         */
        assert!(!l.event_alive(&handle));
    }

    // An event that terminates after given amount of milliseconds
    struct Timeouter {
        milliseconds: u32,
        id: Option<TimeoutId>,
    }

    impl Event<()> for Timeouter {
        fn init<S: Scope<()>>(&mut self, scope: &mut S) -> Response {
            self.id = Some(scope.timeout_after(&Duration::new(0, self.milliseconds)));
            /*
             * Add another timeout that won't have the opportunity to fire, check it doesn't cause
             * problems
             */
            scope.timeout_after(&Duration::new(0, self.milliseconds + 1));
            Ok(true)
        }
        // Terminate on the first timeout we get
        fn timeout<S>(&mut self, _scope: &mut S, id: &TimeoutId) -> Response {
            assert_eq!(Some(id.clone()), self.id);
            Ok(false)
        }
    }

    #[test]
    fn timeout() {
        let mut l = Loop::new(()).unwrap();
        let handle = l.insert(Timeouter {
            milliseconds: 0,
            id: None,
        }).unwrap();
        // It is alive, because it needs the loop to turn to get the timeout
        assert!(l.event_alive(&handle));
        // We can wait for it to happen. Only one timeout gets called (checked in the event)
        l.run_until_complete(&handle).unwrap();
        // Add another one that needs to wait a while and check it actually waited
        assert!(!l.event_alive(&handle));
        // Add another event
        let handle_wait = l.insert(Timeouter {
            milliseconds: 500,
            id: None,
        }).unwrap();
        // The old one didn't get resurrected
        assert!(!l.event_alive(&handle));
        // The new one lives
        assert!(l.event_alive(&handle_wait));
        let wait_start = l.now().clone();
        // It should just fire the one event here
        l.run_one().unwrap();
        assert!(!l.event_alive(&handle_wait));
        assert!(l.now().duration_since(wait_start) >= Duration::new(0, 500));
        // Check dead-timeouts stats. They got adjusted before the run of the second event.
        assert_eq!(2, l.timeouts_inserted);
        assert_eq!(1, l.timeouts_fired);
        assert_eq!(1, l.timeouts_dead);
        /*
         * One dead is still left (the other one would have fired by now, so it's dropped).
         * The one left may be either in the scheduled list or in timouts heap.
         */
        assert_eq!(1, l.timeouts.len() + l.scheduled.len());
    }

    struct ErrorTimeout;

    impl Event<()> for ErrorTimeout {
        fn init<S: Scope<()>>(&mut self, scope: &mut S) -> Response {
            scope.timeout_after(&Duration::new(0, 0));
            Ok(true)
        }
        // The timeout is not implemented, so the default returns an error
    }

    #[test]
    fn error_handler() {
        let mut l = Loop::new(()).unwrap();
        let handle = l.insert(ErrorTimeout).unwrap();
        assert!(l.event_alive(&handle));
        // When we run it, it should terminate the loop with error, as it propagates.
        err!(l.run(), Error::DefaultImpl);
        assert!(!l.event_alive(&handle));
        // Return some other random error
        l.error_handler_set(Box::new(|_, _| Err(Error::DeadLock)));
        // When we run that failing thing through, it changes the error by the handler
        let handle = l.insert(ErrorTimeout).unwrap();
        err!(l.run(), Error::DeadLock);
        assert!(!l.event_alive(&handle));
        // When we ignore the error, it just lets the event die
        let handle = l.insert(ErrorTimeout).unwrap();
        l.error_handler_set(Box::new(|_, _| Ok(())));
        l.run_until_complete(&handle).unwrap();
        assert!(!l.event_alive(&handle));
        // But if we try to run the loop long-term, it complains it is empty
        l.insert(ErrorTimeout).unwrap();
        err!(l.run(), Error::Empty);
    }

    struct Listener;

    impl Event<Option<SocketAddr>> for Listener {
        fn init<S: Scope<Option<SocketAddr>>>(&mut self, scope: &mut S) -> Response {
            // The port 0 = OS, please choose for me.
            let listener = TcpListener::bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0))?;
            // And we need to know what the OS chose
            scope.with_context(|c| {
                *c = Some(listener.local_addr()?);
                Ok(())
            })?;
            scope.io_register(listener, Ready::readable(), PollOpt::empty())?;
            Ok(true)
        }
        fn io<S: Scope<Option<SocketAddr>>>(&mut self, scope: &mut S, id: &IoId, ready: Ready) -> Response {
            assert_eq!(Ready::readable(), ready);
            err!(scope.with_io(id, |_stream: &mut TcpStream| -> Result<()> {
                unreachable!();
            }), Error::IoType);
            scope.with_io(id, |listener: &mut TcpListener| {
                let (mut stream, _) = listener.accept()?;
                writeln!(stream, "hello").map_err(Error::Io)
            })?;
            scope.io_remove(id)?;
            err!(scope.io_update(id, Ready::writable(), PollOpt::empty()), Error::MissingIo);
            scope.stop();
            Ok(true)
        }
    }

    #[test]
    fn io() {
        let mut l = Loop::new(None).unwrap();
        l.insert(Listener).unwrap();
        assert_eq!(1, l.event_count());
        let mut addr: Option<SocketAddr> = None;
        l.with_context(|c| {
            addr = c.take();
            Ok(())
        }).unwrap();
        let _stream = TcpStream::connect(&addr.unwrap()).unwrap();
        l.run().unwrap();
    }
}
