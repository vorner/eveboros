//! EveBoros is a high-level and flexible event loop for Rust.
//!
//! # Motivation
//!
//! Why another event loop library? What is wrong with the others?
//!
//! Well, [mio](https://crates.io/crates/mio) is rather very low level. It's simply a wrapper around
//! epoll or whatever the equivalent is on your system. It's more of a building block for an event
//! loop than being one.
//!
//! The others offer sophisticated models which can be used for network server development. These
//! models turn to a hindrance once trying to write something a bit different. For example, none of
//! the others support easy handling of executed subprocesses (which doesn't mean only waiting for
//! them to terminate, but interacting with their stdios as well).
//!
//! EveBoros goes with the good old callback style. You register for an event and once that event
//! fires, your code gets called. The aim is more on versatility and ease of use than on following
//! strict paradigms. Also, the performance goal is second to ergonomics.
//!
//! However, the other event loops provide some good protocol implementations. Therefore, EveBoros
//! tries making reuse of these protocol implementations possible. It tries to put its power into
//! the loop, not the protocols.
//!
//! # The others
//!
//! There are two main asynchronous projects in Rust going on. The first one is
//! [rotor](https://crates.io/crates/rotor). Its state machines are quite cool. However, the need to
//! use just one kind of a state machine in a given event loop is limiting if you do something more
//! complex than answering one kind of protocol. Splitting the different functionality into
//! different modules is hard, as the machines need to be composed together. Also, it is hard to
//! watch multiple event sources from the same state machine.
//!
//! EveBoros allows plugging distinct types of rotor's machines into the loop. It does so at the
//! cost of working with trait objects instead of concrete types.
//!
//! [Tokio](https://github.com/tokio-rs/tokio) works with futures. The downside of futures is they
//! form a tree with some global goal at the root. However, some workloads would require a DAG ‒
//! when an event influences multiple end goals. Also, it's somewhat hard to follow what's going on
//! inside the futures magic. It, however, seems that many of the tokio's protocols and utilities
//! can work on top of any leaf IO futures, not just their's. And it is possible for EveBoros
//! register events and execute tasks.
//!
//! # Design goals
//!
//! * Allow a library to plug another protocol inside the loop without explicit cooperation from the
//!   main application.
//! * Provide various interfaces (at least event based, state machine based and futures based ones).
//! * Work with other things besides IO, namely:
//!   - Subprocesses
//!   - Thread pools
//!   - Process pools
//!   - Inter-thread message queues
//!   - Unix signals
//!
//! Some of these are implemented as abstraction layers or events plugged into the core loop.
//!
//! # Interface
//!
//! There's one central object, [Loop](struct.Event.html). You register for events, each time
//! providing an [Event](trait.Event.html) object. The event registers interest for some things and
//! gets its callbacks executed when the thing happens.
//!
//! Upon registration, the ownership of the event is transfered into the loop, returning a
//! [Handle](struct.Handle.html) object. The handle object can be cloned
//! The handle allows accessing to the original event and allows manipulation with the registration.
//!
//! Furthermore, the loop is recursive. It is allowed to call methods waiting for completion of some
//! events even from callbacks of other events (though there are some rough edges around that, some
//! things need to be implemented).
//!
//! # Thread safety
//!
//! The loop is not thread safe, you need a different loop in each thread. Furthermore, due to POSIX
//! limitatitons, it is possible to handle each signal in only one thread. As handling of child
//! processes containst receiving a SIGCHLD, only one thread can handle child processes.
//!
//! # Status
//!
//! The library is currently in its early development status.
//!
//! Also, the way we'll connect to the other code (like creating the scopes for the machines) is a
//! bit in flux.
//!
//! # Examples
//!
//! ```
//! extern crate eveboros;
//! extern crate mio;
//!
//! use eveboros::*;
//! use mio::{Ready,PollOpt};
//! use mio::tcp::{TcpStream,TcpListener};
//! use std::io::Write;
//! use std::time::Duration;
//! use std::net::{IpAddr,Ipv4Addr,SocketAddr};
//!
//! struct WriterWithTimeout;
//!
//! impl<AnyEv: From<WriterWithTimeout>> Event<SocketAddr, AnyEv> for WriterWithTimeout {
//!     fn init<S: Scope<SocketAddr, AnyEv>>(&mut self, scope: &mut S) -> Response {
//!         let mut t: Option<TcpStream> = None;
//!         scope.with_context(|context| {
//!             t = Some(TcpStream::connect(context)?);
//!             Ok(())
//!         }).unwrap();
//!         scope.io_register(t.unwrap(), Ready::writable(), PollOpt::empty())?;
//!         scope.timeout_after(&Duration::new(10, 0));
//!         Ok(true)
//!     }
//!     fn io<S: Scope<SocketAddr, AnyEv>>(&mut self, scope: &mut S, io: IoId, _ready: Ready) -> Response {
//!         // Try sending password.
//!         scope.with_io(io, |stream: &mut TcpStream| {
//!             write!(stream, "root\nroot\n").unwrap();
//!             Ok(())
//!         }).unwrap();
//!         // Not really needed, we terminate the event just after this.
//!         scope.io_remove(io).unwrap();
//!         // After sending the password, terminate the event.
//!         Ok(false)
//!     }
//!     fn timeout<S: Scope<SocketAddr, AnyEv>>(&mut self, _scope: &mut S, _timeout: TimeoutId) -> Response {
//!         // Give up after 10 seconds
//!         Ok(false)
//!     }
//! }
//!
//! fn main() {
//!     let listener = TcpListener::bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0)).unwrap();
//!     let mut ev_loop: Loop<SocketAddr, WriterWithTimeout> = Loop::new(listener.local_addr().unwrap()).unwrap();
//!     let handle = ev_loop.insert(WriterWithTimeout).unwrap();
//!     ev_loop.run_until_complete(handle).unwrap();
//! }
//!
//! ```
//!

extern crate mio;
extern crate linked_hash_map;
extern crate threadpool;
extern crate nix;
extern crate libc;

pub mod error;
pub mod adapt;
mod core;
mod recycler;
mod stolen_cell;

pub use core::{IoId, TimeoutId, Handle, Channel, Message, BackgroundId, Loop, LoopIface, LoopIfaceObjSafe, Scope, ScopeObjSafe, Event, Response, DeliveryMode, ChildExit, IoHolderAny, TaskWrapper};
