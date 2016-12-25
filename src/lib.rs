/*!
 * EveBoros is a high-level event loop for Rust.
 *
 * # Motivation
 *
 * Why another event loop library? What is wrong with the others?
 *
 * Well, [mio](https://crates.io/crates/mio) is rather very low level. It's simply a wrapper around
 * epoll or whatever the equivalent is on your system. It's more of a building block for an event
 * loop than being one.
 *
 * The others offer sophisticated models which can be used for network server development. These
 * models turn to a hindrance once trying to write something a bit different. For example, none of
 * the others support easy handling of executed subprocesses (which doesn't mean only waiting for
 * them to happen, but interacting with their stdios).
 *
 * EveBoros goes with the good old callback style. You register for an event and once that event
 * fires, your code gets called. The aim is more on versatility and ease of use than on following
 * strict paradigms. Also, the performance goal is second to ergonomics.
 *
 * However, the other event loops provide some good protocol implementations. Therefore, EveBoros
 * tries making reuse of these protocol implementations possible. It tries to put its power into
 * the loop, not the protocols.
 *
 * # The others
 *
 * There are two main asynchronous projects in Rust going on. The first one is
 * [rotor](https://crates.io/crates/rotor). Its state machines are quite cool. However, the need to
 * use just one kind of a state machine in a given event loop is limiting if you do something more
 * complex than answering one kind of protocol. Splitting the different functionality into
 * different modules is hard, as the machines need to be composed together.
 *
 * EveBoros allows plugging distinct types of rotor's machines into the loop. It does so at the
 * cost of working with trait objects instead of concrete types.
 *
 * [Tokio](https://github.com/tokio-rs/tokio) works with futures. The downside of futures is they
 * form a tree with some global goal at the root. However, some workloads would require a DAG ‒
 * when an event influences multiple end goals. Also, it's somewhat hard to follow what's going on
 * inside the futures magic. It, however, seems that many of the tokio's protocols and utilities
 * can work on top of any leaf IO futures, not just their's. And it is possible for EveBoros
 * register events and execute tasks.
 *
 * # Design goals
 *
 * * Allow a library to plug another protocol inside the loop without explicit cooperation from the
 *   main application.
 * * Provide various interfaces (at least event based, state machine based and futures based ones).
 * * Work with other things besides IO, namely:
 *   - Subprocesses
 *   - Thread pools
 *   - Process pools
 *   - Inter-thread message queues
 *   - Unix signals
 *
 * # Interface
 *
 * There's one central object, `Loop`. You register for events, each time providing a `Callback`
 * object. The callback object gets its method called whenever the event happens.
 *
 * Upon registration, the ownership of the callback is transfered into a new `Handle` object. The
 * handle object can be cloned (and all of the handles manipulate the same `Callback` and event).
 * The handle allows access to the original callback and allows manipulation with the registration.
 * If you drop the handle (or all of them, if you cloned), then the event is unregistered. If you
 * destroy the loop first, the callbacks are preserved, but won't get called any more.
 *
 * Different kinds of events have their `Callback`s and `Handle`s generic over different parameter
 * types. As the `Callback` is a trait, the library may provide some pre-made kinds (like the
 * futures).
 *
 * Furthermore, the loop is recursive. It is allowed to call methods waiting for completion of some
 * events even from callbacks of other events.
 *
 * # Thread safety
 *
 * The loop and the handles are not thread `Send`. This means you need to have a separate event
 * loop per thread, but you are allowed to do whatever you like inside the callbacks.
 *
 * # Status
 *
 * The library is currently in its early development status.
 *
 * Also, the way we'll connect to the other code (like creating the scopes for the machines) is a
 * bit in flux.
 */

extern crate mio;
extern crate linked_hash_map;

pub mod error;
mod core;

pub use core::Loop;
