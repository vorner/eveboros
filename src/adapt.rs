//! Useful utilities to combine events together in various ways.
//!
//! # Side by side composition
//!
//! The [Loop](../struct.Loop.html) allows only one kind of [Event](../trait.Event.html) to be
//! inserted. In the real world, multiple different kinds are usually needed. We therefore provide
//! ways to compose multiple event types into one that can be inserted.
//!
//! Using the [combined](../macro.combined.html) macro (due to the macro limitations, it lives in
//! the root module of the crate), you can create an statically-dispatched event. It is basically an
//! enum with one variant per the real event. It uses `match` to decide which function gets called.
//!
//! There are also the [SelfDynEvent](struct.SelfDynEvent.html) and
//! [AnyDynEvent](struct.AnyDynEvent.html). These use dynamic dispatch through trait objects.
//!
//! The static approach provides better performance, since the compiler knows what methods get
//! called and can inline them. On the other hand, dynamic dispatch allows one piece of code be
//! oblivious of all the types other pieces of code might want to use. Also, if there's one large
//! variant in the enum, all the composite events will be this large even though it might be a waste
//! for the rest of the events.
//!
//! You can also combine the approaches. The top-level composition would be the static one and it
//! would combine the performance sensitive and known event types. In addition, it would contain one
//! dynamic event option, where all the unknown, performance insensitive or very large events would
//! go.

/// Some symbols re-exported from other crates, for macro use.
pub mod reexports {
    pub use mio::Ready;
    pub use libc::pid_t;
    pub use nix::sys::signal::Signal;
}

use self::reexports::*;
use mio::PollOpt;
use std::any::{Any, TypeId};
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
            fn io<S: Scope<$context, $name>>(&mut self, scope: &mut S, id: IoId, ready: $crate::adapt::reexports::Ready) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.io(scope, id, ready), )+
                }
            }
            fn timeout<S: Scope<$context, $name>>(&mut self, scope: &mut S, id: TimeoutId) -> Response {
                match self {
                    $( &mut $name :: $subname ( ref mut val ) => val.timeout(scope, id), )+
                }
            }
            fn signal<S: Scope<$context, $name>>(&mut self, scope: &mut S, signal: $crate::adapt::reexports::Signal) -> Response {
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
            fn child<S: Scope<$context, $name>>(&mut self, scope: &mut S, pid: $crate::adapt::reexports::pid_t, exit: ChildExit) -> Response {
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


/// This allows combining events statically side by side.
///
/// This macro creates an enum event that is a choice between several other sub-events. The dispatch
/// is static (using a `match` statement). For now, it can generate only a concrete final event (it
/// can't be type-parametrized for now).
///
/// If you don't know what events you want to compose in advance, you may use
/// [SelfDynEvent](adapt/struct.SelfDynEvent.html) or [AnyDynEvent](adapt/struct.AnyDynEvent.html).
///
/// # Examples
///
/// ```
/// #[macro_use]
/// extern crate eveboros;
///
/// use eveboros::*;
/// use eveboros::adapt::*;
/// use eveboros::error::*;
/// use std::time::Duration;
///
/// struct Timeout;
/// struct Idle;
///
/// #[derive(Debug,Eq,PartialEq)]
/// enum EvType {
///     Timeout,
///     Idle,
/// }
///
/// type Ctx = Vec<EvType>;
///
/// impl<Ev: From<Timeout>> Event<Ctx, Ev> for Timeout {
///     fn init<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S) -> Response {
///         scope.timeout_after(&Duration::new(0, 0));
///         Ok(true)
///     }
///     fn timeout<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S, _timeout: TimeoutId) -> Response {
///         scope.with_context(|ctx| {
///             ctx.push(EvType::Timeout);
///             Ok(())
///         }).unwrap();
///         Ok(false)
///     }
/// }
///
/// impl<Ev: From<Idle>> Event<Ctx, Ev> for Idle {
///     fn init<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S) -> Response {
///         scope.idle();
///         Ok(true)
///     }
///     fn idle<S: Scope<Ctx, Ev>>(&mut self, scope: &mut S) -> Response {
///         scope.with_context(|ctx| {
///             ctx.push(EvType::Idle);
///             Ok(())
///         }).unwrap();
///         scope.stop();
///         Ok(false)
///     }
/// }
///
/// combined!(IdleTimeout, Ctx,
///     T(Timeout),
///     I(Idle),
/// );
///
/// fn main() {
///     let mut l: Loop<Ctx, IdleTimeout> = Loop::new(Vec::new()).unwrap();
///     l.insert(Timeout).unwrap();
///     l.insert(Idle).unwrap();
///     l.run().unwrap();
///     l.with_context(|ctx| {
///         assert_eq!(vec![EvType::Timeout, EvType::Idle], *ctx);
///         Ok(())
///     }).unwrap();
/// }
/// ```
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

// We want to be able to create trait objects from events. Therefore we wrap them in the
// EventWrapper which implements the WrappedEvent trait, which is object safe.
//
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

impl<Context, Ev, ScopeEvent> WrappedEvent<Context, ScopeEvent> for EventWrapper<Ev>
    where Ev: Event<Context, ScopeEvent>
{
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

/// A dynamic dispatch event wrapper.
///
/// This allows to wrap any event into the same kind of object. It allows to have a
/// [Loop](../struct.Loop.html) and plug events of potentionally unknown types into it.
/// Obviously, at the cost of dynamic dispatch when calling the callbacks.
///
/// It is basically the same as [AnyDynEvent](struct.AnyDynEvent.html), except this one is for the
/// situation it is the main event (it would be impossible to create the correct type because of
/// cyclic types).
///
/// # Examples
///
/// ```
/// use eveboros::*;
/// use eveboros::adapt::*;
///
/// struct UselessEvent;
/// impl<Context, Ev> Event<Context, Ev> for UselessEvent {
///     fn init<S: Scope<Context, Ev>>(&mut self, _scope: &mut S) -> Response {
///         Ok(true)
///     }
/// }
///
/// let mut l: Loop<(), SelfDynEvent<()>> = Loop::new(()).unwrap();
/// l.insert(SelfDynEvent::new(UselessEvent)).unwrap();
/// ```
pub struct SelfDynEvent<Context>(Box<WrappedEvent<Context, SelfDynEvent<Context>>>);

impl<Context> SelfDynEvent<Context> {
    /// Wrap another event into this one.
    pub fn new<Ev: Event<Context, SelfDynEvent<Context>> + 'static>(ev: Ev) -> Self {
        SelfDynEvent(Box::new(EventWrapper(ev)))
    }
}

// Reuse the same implementation for SelfDynEvent and AnyDynEvent
macro_rules! dyn_event {
    ( $tp: ty, $ev: ty, $( $param: ident ),+ ) => {
        impl<$( $param ),+> Event<Context, $ev> for $tp {
            fn init<S: Scope<Context, $ev>>(&mut self, scope: &mut S) -> Response {
                self.0.init(scope)
            }
            fn io<S: Scope<Context, $ev>>(&mut self, scope: &mut S, id: IoId, ready: Ready) -> Response {
                self.0.io(scope, id, ready)
            }
            fn timeout<S: Scope<Context, $ev>>(&mut self, scope: &mut S, id: TimeoutId) -> Response {
                self.0.timeout(scope, id)
            }
            fn signal<S: Scope<Context, $ev>>(&mut self, scope: &mut S, signal: Signal) -> Response {
                self.0.signal(scope, signal)
            }
            fn idle<S: Scope<Context, $ev>>(&mut self, scope: &mut S) -> Response {
                self.0.idle(scope)
            }
            fn message<S: Scope<Context, $ev>>(&mut self, scope: &mut S, msg: Message) -> Response {
                self.0.message(scope, msg)
            }
            fn child<S: Scope<Context, $ev>>(&mut self, scope: &mut S, pid: pid_t, exit: ChildExit) -> Response {
                self.0.child(scope, pid, exit)
            }
        }
    }
}

dyn_event!(SelfDynEvent<Context>, SelfDynEvent<Context>, Context );

/// A dynamic dispatch event wrapper.
///
/// It is basically the same as [SelfDynEvent](struct.SelfDynEvent.html), but this one allows the
/// loop's event to be different. It may be useful if you want to have some events dispatched
/// statically (through the [compose](../macro.compose.html) macro) for performance, but allow for
/// the flexibility of dynamically dispatched events.
pub struct AnyDynEvent<Context, Ev>(Box<WrappedEvent<Context, Ev>>);

impl<Context, OuterEvent> AnyDynEvent<Context, OuterEvent> {
    /// Wrap another event into this one.
    pub fn new<InnerEvent: Event<Context, OuterEvent> + 'static>(ev: InnerEvent) -> Self {
        AnyDynEvent(Box::new(EventWrapper(ev)))
    }
}

dyn_event!(AnyDynEvent<Context, Ev>, Ev, Context, Ev );

struct DynScope<'a, Context: 'a, ScopeEv: 'a>(&'a mut ScopeObjSafe<Context, ScopeEv>);

impl<'a, Context, ScopeEv> LoopIfaceObjSafe<Context, ScopeEv> for DynScope<'a, Context, ScopeEv> {
    fn insert_exact(&mut self, event: ScopeEv) -> Result<Handle> {
        self.0.insert_exact(event)
    }
    fn context(&mut self) -> &mut Context {
        self.0.context()
    }
    fn run_one(&mut self) -> Result<()> {
        self.0.run_one()
    }
    fn run_until_complete(&mut self, handle: Handle) -> Result<()> {
        self.0.run_until_complete(handle)
    }
    fn run(&mut self) -> Result<()> {
        self.0.run()
    }
    fn stop(&mut self) {
        self.0.stop()
    }
    fn event_alive(&self, handle: Handle) -> bool {
        self.0.event_alive(handle)
    }
    fn event_count(&self) -> usize {
        self.0.event_count()
    }
    fn now(&self) -> &Instant {
        self.0.now()
    }
    fn send_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.0.send_impl(handle, data, tp)
    }
    fn post_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.0.post_impl(handle, data, tp)
    }
    fn channel(&mut self, handle: Handle) -> Result<Channel> {
        self.0.channel(handle)
    }
}

impl<'a, Context, ScopeEv> ScopeObjSafe<Context, ScopeEv> for DynScope<'a, Context, ScopeEv> {
    fn handle(&self) -> Handle {
        self.0.handle()
    }
    fn timeout_at(&mut self, when: Instant) -> TimeoutId {
        self.0.timeout_at(when)
    }
    fn signal(&mut self, signal: Signal) -> Result<()> {
        self.0.signal(signal)
    }
    fn idle(&mut self) {
        self.0.idle()
    }
    fn expect_message(&mut self, tp: TypeId) {
        self.0.expect_message(tp)
    }
    fn io_register_any(&mut self, io: Box<IoHolderAny>, interest: Ready, opts: PollOpt) -> Result<IoId> {
        self.0.io_register_any(io, interest, opts)
    }
    fn io_mut(&mut self, id: IoId) -> Result<&mut Box<IoHolderAny>> {
        self.0.io_mut(id)
    }
    fn io_update(&mut self, id: IoId, interest: Ready, opts: PollOpt) -> Result<()> {
        self.0.io_update(id, interest, opts)
    }
    fn io_remove(&mut self, id: IoId) -> Result<()> {
        self.0.io_remove(id)
    }
    fn background_submit(&mut self, task: Box<TaskWrapper>) -> Result<BackgroundId> {
        self.0.background_submit(task)
    }
    fn child(&mut self, pid: pid_t) -> Result<()> {
        self.0.child(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::*;
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
                })
                .map(|_| false)
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

    combined!(IdleTimeout, bool,
        T(TimeoutEvent),
        I(IdleEvent),
    );

    /// Put two different events into the same loop,
    #[test]
    fn static_event() {
        let mut l: Loop<bool, IdleTimeout> = Loop::new(false).unwrap();
        l.insert(IdleEvent).unwrap();
        l.insert(TimeoutEvent).unwrap();
        l.run().unwrap();
        l.with_context(|ctx| {
                assert!(*ctx);
                Ok(())
            })
            .unwrap();
        assert_eq!(0, l.event_count());
    }

    /// Put two different events into the same loop, composed dynamically
    #[test]
    fn dyn_event() {
        let mut l: Loop<bool, SelfDynEvent<bool>> = Loop::new(false).unwrap();
        l.insert(SelfDynEvent::new(IdleEvent)).unwrap();
        l.insert(SelfDynEvent::new(TimeoutEvent)).unwrap();
        l.run().unwrap();
        l.with_context(|ctx| {
                assert!(*ctx);
                Ok(())
            })
            .unwrap();
        assert_eq!(0, l.event_count());
    }

    combined!(Both, bool,
        D(AnyDynEvent<bool, Both>),
    );

    /// Put both compositions into play (this may make sense if there was another
    /// statically-dispatched event, this version is of little use but for testing).
    #[test]
    fn both_events() {
        let mut l: Loop<bool, Both> = Loop::new(false).unwrap();
        l.insert(AnyDynEvent::new(IdleEvent)).unwrap();
        l.insert(AnyDynEvent::new(TimeoutEvent)).unwrap();
        l.run().unwrap();
        l.with_context(|ctx| {
                assert!(*ctx);
                Ok(())
            })
            .unwrap();
        assert_eq!(0, l.event_count());
    }

}
