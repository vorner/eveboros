/**
 * Useful utilities to combine events together in various ways.
 */

pub use mio::Ready;
pub use libc::pid_t;
pub use nix::sys::signal::Signal;

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

#[cfg(test)]
mod tests {
}
