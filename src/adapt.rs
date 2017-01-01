/**
 * Useful utilities to combine events together in various ways.
 */

pub use mio::{Ready,PollOpt};
pub use libc::pid_t;
pub use nix::sys::signal::Signal;
use std::any::{Any,TypeId};
use std::time::Instant;
use super::*;
use super::error::*;

/// Part of [combined](macro.combined.html) macro implementation
#[macro_export]
macro_rules! combined_impl {
    ( $name: ident, $context: ty, $( $subname: ident ( $sub: ty ), )+ ) => {
        impl Event<$context, $name> for $name {
            fn init<S: Scope<$context, $name>>(&mut self, scope: &mut S) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.init(scope), )+
                }
            }
            fn io<S: Scope<$context, $name>>(&mut self, scope: &mut S, id: IoId, ready: Ready) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.io(scope, id, ready), )+
                }
            }
            fn timeout<S: Scope<$context, $name>>(&mut self, scope: &mut S, id: TimeoutId) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.timeout(scope, id), )+
                }
            }
            fn signal<S: Scope<$context, $name>>(&mut self, scope: &mut S, signal: Signal) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.signal(scope, signal), )+
                }
            }
            fn idle<S: Scope<$context, $name>>(&mut self, scope: &mut S) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.idle(scope), )+
                }
            }
            fn message<S: Scope<$context, $name>>(&mut self, scope: &mut S, msg: Message) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.message(scope, msg), )+
                }
            }
            fn child<S: Scope<$context, $name>>(&mut self, scope: &mut S, pid: pid_t, exit: ChildExit) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.child(scope, pid, exit), )+
                }
            }
        }

        $(
        impl From<$sub> for $name {
            fn from(val: $sub) -> $name { $name :: $subname (val) }
        }
        )+
    };
}


/**
 * This allows combining events statically side by side.
 *
 * This macro creates an enum event that is a choice between several other sub-events. The dispatch
 * is static (using a `match` statement). For now, it can generate only a concrete final event (it
 * can't be type-parametrized for now).
 *
 * # Examples
 *
 * ```
 * #[macro_use]
 * extern crate eveboros;
 *
 * use eveboros::*;
 * use eveboros::adapt::*;
 * use eveboros::error::*;
 * use std::time::Duration;
 *
 * struct Timeout;
 * struct Idle;
 *
 * #[derive(Debug,Eq,PartialEq)]
 * enum EvType {
 *     Timeout,
 *     Idle,
 * }
 *
 * type Ctx = Vec<EvType>;
 *
 * impl<Ev: From<Timeout>> Event<Ctx, Ev> for Timeout {
 *     fn init<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S) -> Response {
 *         scope.timeout_after(&Duration::new(0, 0));
 *         Ok(true)
 *     }
 *     fn timeout<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S, _timeout: TimeoutId) -> Response {
 *         scope.with_context(|ctx| {
 *             ctx.push(EvType::Timeout);
 *             Ok(())
 *         }).unwrap();
 *         Ok(false)
 *     }
 * }
 *
 * impl<Ev: From<Idle>> Event<Ctx, Ev> for Idle {
 *     fn init<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S) -> Response {
 *         scope.idle();
 *         Ok(true)
 *     }
 *     fn idle<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S) -> Response {
 *         scope.with_context(|ctx| {
 *             ctx.push(EvType::Idle);
 *             Ok(())
 *         }).unwrap();
 *         scope.stop();
 *         Ok(false)
 *     }
 * }
 *
 * combined!(IdleTimeout, Ctx,
 *     T(Timeout),
 *     I(Idle),
 * );
 *
 * fn main() {
 *     let mut l: Loop<Ctx, IdleTimeout> = Loop::new(Vec::new()).unwrap();
 *     l.insert(Timeout).unwrap();
 *     l.insert(Idle).unwrap();
 *     l.run().unwrap();
 *     l.with_context(|ctx| {
 *         assert_eq!(vec![EvType::Timeout, EvType::Idle], *ctx);
 *         Ok(())
 *     }).unwrap();
 * }
 * ```
 */
#[macro_export]
macro_rules! combined {
    ( pub $name: ident, $context: ty, $( $subname: ident ( $sub: ty ), )+ ) => {
        pub enum $name {
            $( $subname($sub), )+
        }
        combined_impl!( $name, $context, $( $subname ($sub), )+);
    };
    ( $name: ident, $context: ty, $( $subname: ident ( $sub: ty ), )+ ) => {
        enum $name {
            $( $subname($sub), )+
        }
        combined_impl!( $name, $context, $( $subname ($sub), )+);
    };
}

/*
 * We want to be able to create trait objects from events. Therefore we wrap them in the
 * EventWrapper which implements the WrappedEvent trait, which is object safe.
 */
trait WrappedEvent<Context, ScopeEvent> {
    fn init(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>) -> Response;
    fn io(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, id: IoId, ready: Ready) -> Response;
    fn timeout(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, id: TimeoutId) -> Response;
    fn signal(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, signal: Signal) -> Response;
    fn idle(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>) -> Response;
    fn message(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, msg: Message) -> Response;
    fn child(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, pid: pid_t, exit: ChildExit) -> Response;
}

struct EventWrapper<Ev>(Ev);

impl<Context, Ev, ScopeEvent> WrappedEvent<Context, ScopeEvent> for EventWrapper<Ev> where Ev: Event<Context, ScopeEvent> {
    fn init(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>) -> Response {
        self.0.init(&mut DynScope(scope))
    }
    fn io(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, id: IoId, ready: Ready) -> Response {
        self.0.io(&mut DynScope(scope), id, ready)
    }
    fn timeout(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, id: TimeoutId) -> Response {
        self.0.timeout(&mut DynScope(scope), id)
    }
    fn signal(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, signal: Signal) -> Response {
        self.0.signal(&mut DynScope(scope), signal)
    }
    fn idle(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>) -> Response {
        self.0.idle(&mut DynScope(scope))
    }
    fn message(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, msg: Message) -> Response {
        self.0.message(&mut DynScope(scope), msg)
    }
    fn child(&mut self, scope: &mut ScopeObjSafe<Context, ScopeEvent>, pid: pid_t, exit: ChildExit) -> Response {
        self.0.child(&mut DynScope(scope), pid, exit)
    }
}

pub struct SelfDynEvent<Context>(Box<WrappedEvent<Context, SelfDynEvent<Context>>>);

impl<Context> SelfDynEvent<Context> {
    pub fn new<Ev: Event<Context, SelfDynEvent<Context>> + 'static>(ev: Ev) -> Self {
        SelfDynEvent(Box::new(EventWrapper(ev)))
    }
}

impl<Context> Event<Context, SelfDynEvent<Context>> for SelfDynEvent<Context> {
    fn init<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S) -> Response {
        self.0.init(scope)
    }
    fn io<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S, id: IoId, ready: Ready) -> Response {
        self.0.io(scope, id, ready)
    }
    fn timeout<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S, id: TimeoutId) -> Response {
        self.0.timeout(scope, id)
    }
    fn signal<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S, signal: Signal) -> Response {
        self.0.signal(scope, signal)
    }
    fn idle<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S) -> Response {
        self.0.idle(scope)
    }
    fn message<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S, msg: Message) -> Response {
        self.0.message(scope, msg)
    }
    fn child<S: Scope<Context, SelfDynEvent<Context>>>(&mut self, scope: &mut S, pid: pid_t, exit: ChildExit) -> Response {
        self.0.child(scope, pid, exit)
    }
}

struct DynScope<'a, Context: 'a, ScopeEv: 'a>(&'a mut ScopeObjSafe<Context, ScopeEv>);

impl<'a, Context, ScopeEv> LoopIfaceObjSafe<Context, ScopeEv> for DynScope<'a, Context, ScopeEv> {
    fn insert_exact(&mut self, event: ScopeEv) -> Result<Handle> { self.0.insert_exact(event) }
    fn context(&mut self) -> &mut Context { self.0.context() }
    fn run_one(&mut self) -> Result<()> { self.0.run_one() }
    fn run_until_complete(&mut self, handle: Handle) -> Result<()> { self.0.run_until_complete(handle) }
    fn run(&mut self) -> Result<()> { self.0.run() }
    fn stop(&mut self) { self.0.stop() }
    fn event_alive(&self, handle: Handle) -> bool { self.0.event_alive(handle) }
    fn event_count(&self) -> usize { self.0.event_count() }
    fn now(&self) -> &Instant { self.0.now() }
    fn send_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> { self.0.send_impl(handle, data, tp) }
    fn post_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> { self.0.post_impl(handle, data, tp) }
    fn channel(&mut self, handle: Handle) -> Result<Channel> { self.0.channel(handle) }
}

impl<'a, Context, ScopeEv> ScopeObjSafe<Context, ScopeEv> for DynScope<'a, Context, ScopeEv> {
    fn handle(&self) -> Handle { self.0.handle() }
    fn timeout_at(&mut self, when: Instant) -> TimeoutId { self.0.timeout_at(when) }
    fn signal(&mut self, signal: Signal) -> Result<()> { self.0.signal(signal) }
    fn idle(&mut self) { self.0.idle() }
    fn expect_message(&mut self, tp: TypeId) { self.0.expect_message(tp) }
    fn io_register_any(&mut self, io: Box<IoHolderAny>, interest: Ready, opts: PollOpt) -> Result<IoId> { self.0.io_register_any(io, interest, opts) }
    fn io_mut(&mut self, id: IoId) -> Result<&mut Box<IoHolderAny>> { self.0.io_mut(id) }
    fn io_update(&mut self, id: IoId, interest: Ready, opts: PollOpt) -> Result<()> { self.0.io_update(id, interest, opts) }
    fn io_remove(&mut self, id: IoId) -> Result<()> { self.0.io_remove(id) }
    fn background_submit(&mut self, task: Box<TaskWrapper>) -> Result<BackgroundId> { self.0.background_submit(task) }
    fn child(&mut self, pid: pid_t) -> Result<()> { self.0.child(pid) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::*;
    use std::time::Duration;

    struct IdleEvent;

    impl<Ev> Event<bool, Ev> for IdleEvent {
        fn init<S: Scope<bool, Ev>>(&mut self, scope: &mut S) -> Response {
            scope.idle();
            Ok(true)
        }
        fn idle<S: Scope<bool, Ev>>(&mut self, scope: &mut S) -> Response {
            scope.stop();
            scope.with_context(|ctx| {
                *ctx = true;
                Ok(())
            }).map(|_| false)
        }
    }

    struct TimeoutEvent;

    impl<Ev> Event<bool, Ev> for TimeoutEvent {
        fn init<S: Scope<bool, Ev>>(&mut self, scope: &mut S) -> Response {
            scope.timeout_after(&Duration::new(0, 0));
            Ok(true)
        }
        fn timeout<S: Scope<bool, Ev>>(&mut self, _scope: &mut S, _id: TimeoutId) -> Response {
            Ok(false)
        }
    }

    /// Put two different events in the same loop.
    #[test]
    fn dyn_event() {
        let mut l: Loop<bool, SelfDynEvent<bool>> = Loop::new(false).unwrap();
        l.insert(SelfDynEvent::new(IdleEvent)).unwrap();
        l.insert(SelfDynEvent::new(TimeoutEvent)).unwrap();
        l.run().unwrap();
        assert_eq!(0, l.event_count());
    }
}
