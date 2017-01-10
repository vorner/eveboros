// TODO: Some logging

use std::num::Wrapping;
use std::any::Any;
use std::collections::{VecDeque, BinaryHeap, HashSet, HashMap};
use std::marker::{Sized, PhantomData};
use std::time::{Instant, Duration};
use std::cmp::Ordering;
use std::mem::replace;
use std::convert::From;
use std::any::TypeId;
use std::sync::mpsc::TryRecvError;
use std::os::unix::io::AsRawFd;

use mio::{Poll, Events, Token, Ready, Evented, PollOpt};
use mio::channel::{Receiver, Sender, channel, SendError};
use mio::unix::EventedFd;
use linked_hash_map::LinkedHashMap;
use threadpool::ThreadPool;
use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::{SignalFd, SFD_CLOEXEC, SFD_NONBLOCK};
use nix::sys::wait::{WaitStatus, waitpid, WNOHANG};
use nix::Errno;
use nix;
use libc::pid_t;

use super::{IoId, TimeoutId, Handle};
use recycler::Recycler;
use error::{Result, Error};
use stolen_cell::StolenCell;

/// A thread-safe channel to notify events.
///
/// It is possible to create a channel that allows sending data to specific event
/// in the event loop. This channel can be sent to another thread. If you don't need thread safety,
/// you can use [Loop::send](struct.Loop.html#method.send) or
/// [Loop::post](struct.Loop.html#method.post).
///
/// When the channel is created (by [Loop::channel](struct.Loop.html#method.channel)), it is checked
/// that the event is still alive. However, there's no notification through the channel when the
/// event dies ‒ further messages will simply disappear.  However, when the originating event loop
/// is destroyed, the channel returns [Error::LoopGone](error/enum.Error.html) when attempting to
/// send a message. Also, it is not checked the recipient can accept this type of message. If it
/// doesn't, the message is simply dropped.
///
/// # Examples
///
/// ```
/// use eveboros::*;
/// use std::thread;
/// use std::any::TypeId;
///
/// struct Recipient;
///
/// impl<AnyEv: From<Recipient>> Event<(), AnyEv> for Recipient {
///     fn init<S: Scope<(), AnyEv>>(&mut self, scope: &mut S) -> Response {
///         scope.expect_message(TypeId::of::<()>());
///         Ok(true)
///     }
///     fn message<S: Scope<(), AnyEv>>(&mut self, scope: &mut S, _message: Message) -> Response {
///         scope.stop();
///         Ok(false)
///     }
/// }
///
/// fn main() {
///     let mut l: Loop<(), Recipient> = Loop::new(()).unwrap();
///     let handle = l.insert(Recipient).unwrap();
///     let mut channel = l.channel(handle).unwrap();
///     thread::spawn(move || {
///         channel.send(()).unwrap();
///     });
///     l.run().unwrap();
/// }
/// ```
#[derive(Clone)]
pub struct Channel {
    sender: Sender<RemoteMessage>,
    handle: Handle,
}

impl Channel {
    /// Send a message to the corresponding event.
    pub fn send<T: Any + 'static + Send>(&mut self, data: T) -> Result<()> {
        self.sender
            .send(RemoteMessage {
                recipient: self.handle,
                data: Box::new(data),
                real_type: TypeId::of::<T>(),
                mode: DeliveryMode::Remote,
            })
            .map_err(|e| match e {
                SendError::Io(io) => Error::Io(io),
                SendError::Disconnected(_) => Error::LoopGone,
            })
    }
}

/// Message to an event.
///
/// This is what an event gets when someone sends (or posts) it some data. It can be examined and
/// the corresponding data type extracted.
///
/// # Examples
///
/// ```
/// use eveboros::*;
/// use std::any::TypeId;
///
/// struct Recipient;
///
/// impl<AnyEv: From<Recipient>> Event<(), AnyEv> for Recipient {
///     fn init<S: Scope<(), AnyEv>>(&mut self, scope: &mut S) -> Response {
///         scope.expect_message(TypeId::of::<i32>());
///         Ok(true)
///     }
///     fn message<S: Scope<(), AnyEv>>(&mut self, _scope: &mut S, message: Message) -> Response {
///         assert_eq!(*message.real_type(), TypeId::of::<i32>());
///         assert_eq!(*message.mode(), DeliveryMode::Post(None));
///         assert_eq!(message.get::<i32>().unwrap(), 42);
///         Ok(true)
///     }
/// }
///
/// fn main() {
///     let mut l: Loop<(), Recipient> = Loop::new(()).unwrap();
///     let handle = l.insert(Recipient).unwrap();
///     l.post(handle, 42).unwrap();
/// }
/// ```
#[derive(Debug)]
pub struct Message {
    data: Box<Any + 'static>,
    real_type: TypeId,
    mode: DeliveryMode,
}

impl Message {
    /// Extract the held data.
    ///
    /// Extracts the data inside, consuming the message in the process. If the type does not match,
    /// [Error::MsgType](error/enum.Error.html) is returned.
    ///
    /// If you are not sure about the type upfront, you may check the
    /// [real_type()](#method.real_type) method.
    pub fn get<T: Any + 'static>(self) -> Result<T> {
        if let DeliveryMode::Background(_) = self.mode {
            // The background task already contains result. We want to squash that, instead of
            // returning Result<Result<T>>.
            //
            let proto: Result<Result<T>> = self.data.downcast().map_err(|_| Error::MsgType).map(|b| *b);
            match proto {
                Ok(rt) => rt,
                Err(e) => Err(e),
            }
        } else {
            self.data.downcast().map_err(|_| Error::MsgType).map(|b| *b)
        }
    }
    /// Get a type ID of the thing stored inside.
    pub fn real_type(&self) -> &TypeId {
        &self.real_type
    }
    /// The mode of the message.
    ///
    /// This specifies how the message originated and optionally from whom.
    pub fn mode(&self) -> &DeliveryMode {
        &self.mode
    }
}

/// For sending messages between threads.
///
/// Public to make the compiler happy, mostly. Not to be interacted with directly.
#[derive(Debug)]
pub struct RemoteMessage {
    data: Box<Any + 'static + Send>,
    real_type: TypeId,
    mode: DeliveryMode,
    recipient: Handle,
}

/// This is the object safe part of [LoopIface](trait.LoopIface.html).
///
/// The split into object-safe part and the rest allows wrapping the scope into an Box or something.
pub trait LoopIfaceObjSafe<Context, Ev> {
    /// Insert the concrete, converted, event.
    ///
    /// The user should use [trait.LoopIface.html#tymethod.insert) instead. This is simply used to
    /// provide means of implementing it around object safe interface.
    fn insert_exact(&mut self, event: Ev) -> Result<Handle>;
    /// Get a context accessor
    fn context(&mut self) -> &mut Context;
    /// Run one iteration of the event loop.
    ///
    /// This fires at most one callback to an event. You usually don't need to call this directly,
    /// but it allows checking for termination conditions.
    ///
    /// # Notes
    ///
    /// It is technically possible to call this from within an event callback. But when the loop
    /// tries to call the same event again, it currently generates
    /// [Error::Busy](error/enum.Error.html) error. There are plans to postpone the further calls to
    /// the same event, but it is not implemented yet.
    fn run_one(&mut self) -> Result<()>;
    /// Run the event loop until the given event completes.
    ///
    /// This runs the event loop until the event identified by the handle completes.
    ///
    /// # Notes
    ///
    /// Similar to the [run_one](#tymethod.run_one) method, if this gets called from within an event
    /// callback and any event callback of that event would have to be called again,
    /// [Error::Busy](error/enum.Error.html) error is generated. Furthermore, if anything
    /// tries to wait for completion of an event that is currently inside a callback, the
    /// [Error::DeadLock](error/enum.Error.html) error is returned.
    fn run_until_complete(&mut self, handle: Handle) -> Result<()>;
    /// Run the event loop.
    ///
    /// Run the event loop until someone calls [stop](#tymethod.stop). If all the events inside the
    /// loop terminate, the [Error::Empty](error/enum.Error.html) error is returned.
    fn run(&mut self) -> Result<()>;
    /// Run the event loop until it is empty of events or stopped.
    ///
    /// Run the event loop and stop when it gets empty or when stop is called explicitly.
    fn run_until_empty(&mut self) -> Result<()> {
        match self.run() {
            Err(Error::Empty) => Ok(()), // We actually want that
            r => r,
        }
    }
    /// Stop the event loop.
    ///
    /// If the event loop was started by [run](#tymethod.run), stop it (once the current callback
    /// terminates). If there are multiple nested `run()`s, this terminates the intermost one.
    fn stop(&mut self);
    /// Check if the event is still alive.
    ///
    /// Check if the event still lives and waits for callbacks.
    ///
    /// The result is undefined if you mix handles from foreign loops. However, you do get a result.
    fn event_alive(&self, handle: Handle) -> bool;
    /// How many active events are there in the loop?
    fn event_count(&self) -> usize;
    /// Return the current time.
    ///
    /// Return the time when the loop last gathered events. This allows for tracking time, but in a
    /// cheaper way than running `Instant::now()` every time, as this gets cached for possibly
    /// multiple event callbacks.
    ///
    /// On the other hand, the time may be a bit stale, if the handling of previous callbacks took a
    /// long time.
    fn now(&self) -> &Instant;
    /// Low-level implementation of send
    fn send_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()>;
    /// Low-level implementation of post
    fn post_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()>;
    /// Create a communication channel.
    ///
    /// This allows sending of messages from a foreign thread. See documentation for the
    /// [channel](struct.Channel.html) for more details.
    ///
    /// If you want to communicate from the same thread, using [post](#tymethod.post) and
    /// [send](#tymethod.send) may be more convenient.
    fn channel(&mut self, handle: Handle) -> Result<Channel>;
}

/// An object that can control an event loop.
///
/// There are certain operations that can be done on the message loop [Loop](struct.Loop.html).
/// However, sometimes it is accessed through a proxy object. All these objects (including the loop)
/// implement this trait.
///
/// [Scopes](trait.Scope.html) passed to message callbacks also implement the loop interface.
///
/// It is not expected users of this library to *implement* this trait, only use it.
pub trait LoopIface<Context, Ev>: LoopIfaceObjSafe<Context, Ev> {
    /// Insert another event into the event loop.
    ///
    /// It returns a handle representing the event inside the loop.
    ///
    /// The reason why it takes events convertible to the one the loop operates on is composition.
    /// This way, if all the events properly accept scopes with generic events the current one is
    /// convertible to, it is possible to create an event wrapping multiple such ones without
    /// modifying the underlying events.
    ///
    /// # Examples
    ///
    /// This is the good way:
    ///
    /// ```
    /// # use eveboros::*;
    ///
    /// struct SomeEvent;
    ///
    /// // This can be put in many different Loops
    /// impl<Context, AnyEv: From<SomeEvent>> Event<Context, AnyEv> for SomeEvent {
    ///     fn init<S: Scope<Context, AnyEv>>(&mut self, scope: &mut S) -> Response {
    ///         // Note that this'll recursively insert infinite number of events
    ///         scope.insert(SomeEvent)?; // Insert another event of the same type as us
    ///         Ok(true)
    ///     }
    /// }
    ///
    /// # fn main() {}
    /// ```
    ///
    /// But this way prevents the event from being reused inside some wrapper:
    ///
    /// ```
    /// # use eveboros::*;
    ///
    /// struct SomeEvent;
    ///
    /// // This way it can be put only into Loop<(), SomeEvent>
    /// impl Event<(), SomeEvent> for SomeEvent {
    ///     fn init<S: Scope<(), SomeEvent>>(&mut self, scope: &mut S) -> Response {
    ///         scope.insert(SomeEvent)?;
    ///         Ok(true)
    ///     }
    /// }
    ///
    /// # fn main() {}
    /// ```
    fn insert<EvAny>(&mut self, event: EvAny) -> Result<Handle>
        where Ev: From<EvAny>
    {
        self.insert_exact(event.into())
    }
    /// Access the context inside the loop.
    ///
    /// Run a closure that gets the (mutable) global context inside the event loop. The result of
    /// the closure is propagated as the result of this method.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eveboros::*;
    /// struct UselessEvent;
    /// impl<AnyEv: From<UselessEvent>> Event<u32, AnyEv> for UselessEvent {
    ///     fn init<S: Scope<u32, AnyEv>>(&mut self, _scope: &mut S) -> Response {
    ///         Ok(false)
    ///     }
    /// }
    ///
    /// fn main() {
    ///     let mut l: Loop<u32, UselessEvent> = Loop::new(42).unwrap();
    ///     l.with_context(|context| {
    ///         assert_eq!(42, *context);
    ///         *context = 0;
    ///         Ok(())
    ///     }).unwrap();
    /// }
    /// ```
    fn with_context<R, F: FnOnce(&mut Context) -> Result<R>>(&mut self, f: F) -> Result<R> {
        f(self.context())
    }
    /// Send some data to the event asynchronously.
    ///
    /// This sends data asynchronously. The event will get its
    /// [message](trait.Event.html#tymethod.message) callback called with the passed data, once it is
    /// its turn.
    ///
    /// It does check the event is allive at the time of sending. However, there's no guarantee the
    /// event will not terminate sooner than it receives the message. In such case the message is
    /// simply lost.
    ///
    /// # Errors
    ///
    /// It can return following [errors](error/enum.Error.html):
    ///
    /// * `Error::Missing` if the event is no longer alive.
    /// * `Error::MsgUnexpected` if the event has't declared it is ready to receive this type of
    ///   data.
    fn send<T: Any + 'static>(&mut self, handle: Handle, data: T) -> Result<()> {
        self.send_impl(handle, Box::new(data), TypeId::of::<T>())
    }
    /// Send some data to the event synchronously.
    ///
    /// This sends data synchronously ‒ it calls the [message](trait.Event.html#tymethod.message)
    /// callback directly from this method. Errors from the message get propagated.
    ///
    /// # Errors
    ///
    /// It can return following [errors](error/enum.Error.html):
    ///
    /// * `Error::Missing` if the event is no longer alive.
    /// * `Error::MsgUnexpected` if the event has't declared it is ready to receive this type of
    ///   data.
    /// * `Error::Busy` if the target event is currently in the middle of a callback.
    fn post<T: Any + 'static>(&mut self, handle: Handle, data: T) -> Result<()> {
        self.post_impl(handle, Box::new(data), TypeId::of::<T>())
    }
}

// Implement it for trait objects
impl<Context, ScopeEvent> LoopIface<Context, ScopeEvent> for LoopIfaceObjSafe<Context, ScopeEvent> {}
// Implement it for concrete objects
impl<Context, ScopeEvent, Obj: LoopIfaceObjSafe<Context, ScopeEvent>> LoopIface<Context, ScopeEvent> for Obj {}

/// An object-safe subset of the [Scope](trait.Scope.html) trait.
pub trait ScopeObjSafe<Context, Ev>: LoopIfaceObjSafe<Context, Ev> {
    /// The handle of the current event.
    fn handle(&self) -> Handle;
    /// Schedule a timeout at a given time.
    ///
    /// Once the given instant passes, the event's [timeout](trait.Event.html#timeout) callback gets
    /// called, with the returned id.
    ///
    /// Single event may have arbitrary number of timeouts registered.
    ///
    /// The timeout is single-shot (happens just once), but you can register a new one from the
    /// callback.
    fn timeout_at(&mut self, when: Instant) -> TimeoutId;
    /// Schedule a timeout after a given time.
    ///
    /// This is almost the same as [timeout_at](#tymethod.timeout_at), with the only difference that
    /// this is at a time interval from now, not at a given instant.
    ///
    /// ```
    /// use eveboros::*;
    /// use std::time::Duration;
    ///
    /// struct RepeatedTimer(Duration, u32);
    ///
    /// impl<Context, AnyEv: From<RepeatedTimer>> Event<Context, AnyEv> for RepeatedTimer {
    ///     fn init<S: Scope<Context, AnyEv>>(&mut self, scope: &mut S) -> Response {
    ///         // Schedule a timeout
    ///         scope.timeout_after(&self.0);
    ///         Ok(true)
    ///     }
    ///     fn timeout<S: Scope<Context, AnyEv>>(&mut self, scope: &mut S, _id: TimeoutId) -> Response {
    ///         self.1 += 1;
    ///         println!("Tick {}!", self.1);
    ///         // Schedule another one
    ///         scope.timeout_after(&self.0);
    ///         if self.1 >= 10 {
    ///             scope.stop();
    ///             Ok(false)
    ///         } else {
    ///             Ok(true)
    ///         }
    ///     }
    /// }
    ///
    /// fn main() {
    ///     let mut l: Loop<(), RepeatedTimer> = Loop::new(()).unwrap();
    ///     l.insert(RepeatedTimer(Duration::new(0, 10), 0)).unwrap();
    ///     l.run().unwrap();
    /// }
    /// ```
    fn timeout_after(&mut self, after: &Duration) -> TimeoutId {
        let at = *self.now() + *after;
        self.timeout_at(at)
    }
    /// Show interest in receiving these types of signals.
    ///
    /// This also automatically enables the loop's handling of this signal.
    fn signal(&mut self, signal: Signal) -> Result<()>;
    /// Run code when the loop is idle.
    ///
    /// Run the [idle](trait.Event.html#tymethod.idle) callback once the event loop has nothing
    /// better than you. It is possible to register only one idle callback at a time, but it is
    /// possible to register a new one from the `idle` callback itself.
    fn idle(&mut self);
    /// Allow receiving of messages of the given type.
    ///
    /// Note that you don't have to do this to receive results from background tasks, they get
    /// allowed automatically.
    ///
    /// As the type can't be deduced from the parameters, you have to call it as
    /// `scope.expect_message::<i32>()` (or with any other type you want).
    fn expect_message(&mut self, tp: TypeId);
    /// Register any evented holder
    fn io_register_any(&mut self, io: Box<IoHolderAny>, interest: Ready, opts: PollOpt) -> Result<IoId>;
    /// Borrow the IO
    fn io_mut(&mut self, id: IoId) -> Result<&mut Box<IoHolderAny>>;
    /// Modify the events watched on the given IO.
    fn io_update(&mut self, id: IoId, interest: Ready, opts: PollOpt) -> Result<()>;
    /// Stop watching the given IO.
    ///
    /// The IO is destroyed. It is planned this will return the ID, but this is not yet implemented.
    fn io_remove(&mut self, id: IoId) -> Result<()>;
    /// Submit a task to a background thread
    fn background_submit(&mut self, task: Box<TaskWrapper>) -> Result<BackgroundId>;
    /// Track a child process.
    ///
    /// It is not allowed to track the same child by multiple events. Also, this registers the
    /// SIGCHLD signal into the loop.
    ///
    /// It's not possible to track children not started by this process.
    ///
    /// # Note
    ///
    /// You might want to enable SIGCHLD on the event loop before forking/starting the child to
    /// avoid race conditions (the child terminating sooner than the SIGCHLD is enabled, leading to
    /// missed signal).
    ///
    /// # Panics
    ///
    /// It panics if you try to register the same `pid` multiple times (by either the same or
    /// different events) into the same loop.
    fn child(&mut self, pid: pid_t) -> Result<()>;
}

/// Necessary information passed to every callback.
///
/// An object implementing this trait is passed to each callback of an event. It allows manipulating
/// both the underlying event loop (through the inherited [LoopIface](trait.LoopIface.html) trait)
/// and the state of the event (like registering timeouts and other things).
///
/// The scope is different for each event (because it knows which event it is for).
///
/// Also, the concrete data types implementing this are not public, on purpose. Various proxy or
/// abstracting layers implement different scopes and it is not desirable to have events not
/// working with them.
///
/// It is not expected users of this library to *implement* this trait, only use it.
pub trait Scope<Context, Ev>: LoopIface<Context, Ev> + ScopeObjSafe<Context, Ev> {
    /// Watch a MIO Evented.
    ///
    /// Watch something that MIO can watch. It is usually a socket or something like that. Set what
    /// events you want to get notified about.
    ///
    /// Later on you cat remove it by [io_remove](#tymethod.io_remove) or modify the watched events by
    /// [io_update](#tymethod.io_update).
    ///
    /// The IO object's ownership is passed to the event loop and it can be accessed only through
    /// the returned ID.
    ///
    /// # Examples
    ///
    /// ```
    /// # extern crate mio;
    /// # extern crate eveboros;
    /// use eveboros::*;
    /// use mio::tcp::TcpStream;
    /// use std::io::Write;
    ///
    /// struct Communicator;
    ///
    /// impl<Context, AnyEv: From<Communicator>> Event<Context, AnyEv> for Communicator {
    ///     fn init<S: Scope<Context, AnyEv>>(&mut self, scope: &mut S) -> Response {
    ///         let t = TcpStream::connect(&"127.0.0.1:21".parse().unwrap()).unwrap();
    ///         scope.io_register(t, Ready::writable(), PollOpt::empty()).unwrap();
    ///         Ok(true)
    ///     }
    ///     fn io<S: Scope<Context, AnyEv>>(&mut self, scope: &mut S, io: IoId, _ready: Ready) -> Response {
    ///         // Try sending password.
    ///         scope.with_io(io, |stream: &mut TcpStream| {
    ///             write!(stream, "root\nroot\n").unwrap();
    ///             Ok(())
    ///         }).unwrap();
    ///         scope.io_remove(io).unwrap();
    ///         Ok(false)
    ///     }
    /// }
    ///
    /// # fn main() {}
    /// ```
    fn io_register<E: Evented + 'static>(&mut self, io: E, interest: Ready, opts: PollOpt) -> Result<IoId> {
        let handle = self.handle();
        self.io_register_any(Box::new(IoHolder {
                                 recipient: handle,
                                 io: io,
                             }),
                             interest,
                             opts)
    }
    /// Access the watched IO.
    ///
    /// Get a mutable access to the IO passed to the loop to be watched.
    fn with_io<E: Evented + 'static, R, F: FnOnce(&mut E) -> Result<R>>(&mut self, id: IoId, f: F) -> Result<R> {
        // Get the boxed any-holder instance, cast it to Box<Any> and try to cast it.
        let io: &mut IoHolder<E> = self.io_mut(id)?.as_any_mut().downcast_mut().ok_or(Error::IoType)?;
        f(&mut io.io)
    }
    /// Run a task in the background, in another thread.
    ///
    /// There is no guarantee when it gets run, or if the results come in order of submission. The
    /// result will get send through the `message` callback, with a `DeliveryMode` `Background`. You
    /// don't have to register the return type to receive it (and submitting a task with some return
    /// type doesn't register it for the ordinary messages).
    ///
    /// # Note
    ///
    /// While the `f` function returns `Result<R>`, once the result is delivered, you should
    /// `message.get::<R>()` only, as that already returns `Result<R>` (the errors of the function
    /// are squashed together, as a convenience, with the errors from `get`tting the result.
    fn background<R: Any + 'static + Send, F: 'static + Send + FnOnce() -> Result<R>>(&mut self, f: F) -> Result<BackgroundId> {
        let handle = self.handle();
        self.background_submit(Box::new(WrappedTask {
            f: Some(move |id, sender| {
                let mut wrapper: BackgroundWrapper<R> = BackgroundWrapper {
                    requestor: handle,
                    id: id,
                    complete: false,
                    sender: sender,
                    _final: PhantomData,
                };
                let result = f();
                let _ = wrapper.sender.send(RemoteMessage {
                    recipient: handle,
                    data: Box::new(result),
                    real_type: TypeId::of::<R>(),
                    mode: DeliveryMode::Background(id),
                });
                wrapper.complete = true;
            }),
        }))
    }
    /// This is similar to `background`, but produces multiple results instead of one.
    ///
    /// It acts in a similar way to `background`. However, once the task is run, it produces an
    /// iterator. The background thread sends one `R` for each item received from the iterator (as a
    /// separate message). It then terminates the sequence by Err(Error::IterEnd).
    ///
    /// The results are buffered in a channel. So if there's a large number of results, the use of
    /// iterator instead of one object holding all is better if generating them is slower than using
    /// them.
    fn background_iter<R: Any + 'static + Send, I: 'static + Iterator<Item = R>, F: 'static + Send + FnOnce() -> Result<I>>(&mut self, f: F) -> Result<BackgroundId> {
        let handle = self.handle();
        self.background_submit(Box::new(WrappedTask {
            f: Some(move |id: BackgroundId, sender: Sender<RemoteMessage>| {
                let result = f();
                {
                    // Block, so send gets destroyed soon enough
                    let send = |value: Result<R>| {
                        let _ = sender.send(RemoteMessage {
                            recipient: handle,
                            data: Box::new(value),
                            real_type: TypeId::of::<R>(),
                            mode: DeliveryMode::Background(id),
                        });
                    };
                    match result {
                        Ok(iter) => {
                            for val in iter {
                                send(Ok(val));
                            }
                            send(Err(Error::IterEnd));
                        },
                        Err(err) => send(Err(err)),
                    }
                }
            }),
        }))
    }
}

// Implement it for concrete types
impl<Context, ScopeEvent, Obj: ScopeObjSafe<Context, ScopeEvent>> Scope<Context, ScopeEvent> for Obj {}

/// Common response of the Event.
pub type Response = Result<bool>;

/// An opaque id of a background job
#[derive(Debug,Clone,Copy,Eq,PartialEq,Ord,PartialOrd,Hash)]
pub struct BackgroundId(u64);

/// The cause of an arriving message.
///
/// This specifies what the reason for the current [Message](struct.Message.html) is, or who sent
/// it.
#[derive(Debug,Clone,Eq,PartialEq,Ord,PartialOrd,Hash)]
pub enum DeliveryMode {
    /// The message was generated by a [post](trait.LoopIface.html#tymethod.post).
    ///
    /// The parameter specifies what event sent it, if any.
    Post(Option<Handle>),
    /// The message was generated by a [send](trait.LoopIface.html#tymethod.send).
    ///
    /// The parameter specifies what event sent it, if any.
    Send(Option<Handle>),
    /// This wasn't sent directly, but is a result of a
    /// [background](trait.Scope.html#tymethod.background) method. The ID of the job is the
    /// parameter.
    Background(BackgroundId),
    /// Sent through a channel, possibly from another thread.
    Remote,
}

/// How did the child exit?
#[derive(Debug,Clone,Eq,PartialEq)]
pub enum ChildExit {
    /// A normal exit, with the given exit code
    Exited(i8),
    /// Dead caused by a signal
    Signaled(Signal),
}

/// An event registered in the event loop.
///
/// This is the main trait of the library. A user shall define a type implementing this trait and
/// register it within the [loop](struct.Loop.html).
///
/// The event may register multiple things of interest that may happen in the future and one of the
/// callbacks gets called whenever the thing of interest happens.
///
/// Each of these callbacks return a [Response](type.Response.html). If the response is `Ok(true)`,
/// the event continues on living. If it is `Ok(false)`, the event is destroyed. On error, the event
/// is also destroyed. However, the error is passed to the [error
/// handler](struct.Loop.html#method.error_handler_set) and any error returned (propagated) from the
/// handler terminates the current loop invocation. The loop can be called again after that,
/// however.
///
/// The Context can serve as a global storage accessible from all the events.
///
/// The ScopeEvent is usually this Event. However, it may be some type this is convertible to and
/// allows the loop to contain multiple „base“ events at once. If you implement the corresponding
/// `From` trait, it can be inserted directly.
///
/// Except for the [init](#tymethod.init) method, all these callbacks have a default (error-raising)
/// implementation. The reason is, most events don't need all of them. If you don't register for
/// something, you don't have to implement the corresponding callback.
pub trait Event<Context, ScopeEvent>
    where Self: Sized
{
    /// Called during insertion.
    ///
    /// This callback is called when the event is inserted into the loop. It is a good place to
    /// register things of interest.
    ///
    /// As this is called from the [insert](trait.LoopIface.html#tymethod.insert), any possible
    /// error is returned as the result of insert.
    fn init<S: Scope<Context, ScopeEvent>>(&mut self, scope: &mut S) -> Response;
    /// A watched IO is ready.
    ///
    /// A watched IO is ready to be read, written or in an error state (or in multiple at once), as
    /// requested by the [io_register](trait.Scope.html#tymethod.io_register) method. The
    /// registration is multi-shot, eg firing it doesn't prevent it from firing further (when it is
    /// still/again ready).
    fn io<S: Scope<Context, ScopeEvent>>(&mut self, _scope: &mut S, _id: IoId, _ready: Ready) -> Response {
        Err(Error::DefaultImpl)
    }
    /// A timeout happened.
    fn timeout<S: Scope<Context, ScopeEvent>>(&mut self, _scope: &mut S, _id: TimeoutId) -> Response {
        Err(Error::DefaultImpl)
    }
    /// A registered signal got received.
    fn signal<S: Scope<Context, ScopeEvent>>(&mut self, _scope: &mut S, _signal: Signal) -> Response {
        Err(Error::DefaultImpl)
    }
    /// This is a good time to run some tasks.
    ///
    /// The event registered to be notified when the loop is idle. This happens now, which means it
    /// is a good time to run some postponed tasks.
    fn idle<S: Scope<Context, ScopeEvent>>(&mut self, _scope: &mut S) -> Response {
        Err(Error::DefaultImpl)
    }
    /// A message was received.
    ///
    /// Someone or something generated a message. This can be sent (or posted) by another event or
    /// owner of the loop, sent through a channel from another thread or can be a result of a
    /// [background](trait.Scope.html#tymethod.background) task. Except for the background tasks
    /// (where the results are allways allowed), receipt of each type of a message must be
    /// explicitly allowed through [expect_message](trait.ScopeObjSafe.html#tymethod.expect_message).
    fn message<S: Scope<Context, ScopeEvent>>(&mut self, _scope: &mut S, _msg: Message) -> Response {
        Err(Error::DefaultImpl)
    }
    /// A watched process child terminated.
    fn child<S: Scope<Context, ScopeEvent>>(&mut self, _scope: &mut S, _pid: pid_t, _exit: ChildExit) -> Response {
        Err(Error::DefaultImpl)
    }
}

struct EvHolder<Event> {
    // Becase we need to pass a mutable reference to both the loop and the event (which lives
    // inside the loop), we need to cheat the borrow checker a bit. The original solution of having
    // Option<Event> and taking it out and then returning it back worked, but was clumsy. This way
    // we pretend to take it out and back, but without the actual taking and returning.
    event: StolenCell<Event>,
    generation: u64,
    // How many timeouts does it have?
    timeouts: usize,
    // The IOs belonging to this event
    ios: HashMap<usize, Box<IoHolderAny>>,
    expected_messages: HashSet<TypeId>,
    // The children we want to be informed about
    children: HashSet<pid_t>,
}

#[derive(Debug)]
enum TaskParam {
    Io(IoId, Ready),
    Timeout(TimeoutId),
    Signal(Signal),
    Message(Message),
    Child(pid_t, ChildExit),
    Idle,
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
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An error handler.
///
/// It gets the event that caused the error (before it gets destroyed) and the error.
pub type ErrorHandler<Ev> = Box<FnMut(Ev, Error) -> Result<()>>;

struct IoHolder<E: Evented> {
    recipient: Handle,
    io: E,
}

/// This is public only formally O:-)
#[allow(missing_docs)]
pub trait IoHolderAny: Any {
    fn io(&self) -> &Evented;
    fn recipient(&self) -> Handle;
    fn as_any_mut(&mut self) -> &mut Any;
}

impl<E: Evented + 'static> IoHolderAny for IoHolder<E> {
    fn io(&self) -> &Evented {
        &self.io
    }
    fn recipient(&self) -> Handle {
        self.recipient
    }
    fn as_any_mut(&mut self) -> &mut Any {
        self
    }
}

const TOKEN_SHIFT: usize = 2;
const CHANNEL_TOK: Token = Token(0);
const SIGNAL_TOK: Token = Token(1);

/// Similar to FnOnce(), but one that can actually be passed where we want
pub trait TaskWrapper: Send + 'static {
    /// Call the task
    fn call(&mut self, id: BackgroundId, sender: Sender<RemoteMessage>);
}

struct WrappedTask<F: FnOnce(BackgroundId, Sender<RemoteMessage>) + Send + 'static> {
    f: Option<F>,
}

impl<F: FnOnce(BackgroundId, Sender<RemoteMessage>) + Send + 'static> TaskWrapper for WrappedTask<F> {
    fn call(&mut self, id: BackgroundId, sender: Sender<RemoteMessage>) {
        (self.f.take().unwrap())(id, sender)
    }
}

struct BackgroundWrapper<FinalR: Send + 'static> {
    requestor: Handle,
    id: BackgroundId,
    sender: Sender<RemoteMessage>,
    complete: bool,
    _final: PhantomData<FinalR>,
}

// A trick. This gets called even whn we panic. So, we send a message it panicked in case it isn't
// complete yet.
//
impl<FinalR: Send + 'static> Drop for BackgroundWrapper<FinalR> {
    fn drop(&mut self) {
        if !self.complete {
            // The only way this can fail is if the loop disappeared. We want to
            // ignore such errors, because there's nobody to tell about the failure.
            //
            let _ = self.sender.send(RemoteMessage {
                recipient: self.requestor,
                data: Box::new(Err(Error::BackgroundPanicked) as Result<FinalR>),
                real_type: TypeId::of::<Result<FinalR>>(),
                mode: DeliveryMode::Background(self.id),
            });
        }
    }
}

/// The event loop itself.
///
/// This structure is the core of the librariry. You create one loop with certain kind of context
/// (to hold a global state accessible to all the events) and an event type (which may be a concrete
/// type or a wrapper around multiple different event types) and fill it with
/// [events](trait.Event.html). Then you run the loop, which blocks the current thread and
/// dispatches the events. The events can terminate and create more events.
///
/// It is possible to have an active loop in more than one thread (distinct instance in each
/// thread). However, each unix signal can be handled by only one thread (and therefore event loop).
/// As a result, only one event loop may handle child processes.
pub struct Loop<Context, Ev> {
    poll: Poll,
    mio_events: Events,
    active: bool,
    context: Context,
    error_handler: ErrorHandler<Ev>,
    events: Recycler<EvHolder<Ev>>,
    // We try to detect referring to an event with Handle from event that had the same index as us
    // and the index got reused by adding generation to the event. It is very unlikely we would
    // hit the very same index and go through all 2^64 iterations to cause hitting a collision.
    generation: Wrapping<u64>,
    // Preparsed events we received from mio and other sources, ready to be dispatched one by one
    scheduled: VecDeque<Task>,
    // The timeouts we want to fire some time. We don't prune the ones from events that died,
    // but we ignore them when they die.
    //
    // We don't remove timeouts of killed events right away, we ignore them when they fire.
    // However, when there's more dead ones than live ones, we go through it whole and prune them.
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
    // Ownership of the IOs
    ios: Recycler<Handle>,
    ios_released: HashSet<usize>,
    want_idle: LinkedHashMap<Handle, ()>,
    // Allow sending tasks from other threads (generated by friendly objects)
    sender: Sender<RemoteMessage>,
    receiver: Receiver<RemoteMessage>,
    threadpool: Option<ThreadPool>,
    background_generation: Wrapping<u64>,
    // Signal handling
    signal_mask: SigSet,
    signal_fd: SignalFd,
    signal_recipients: HashMap<i32, HashSet<Handle>>,
    // Child tracking
    children: HashMap<pid_t, Handle>,
}

impl<Context, Ev: Event<Context, Ev>> Loop<Context, Ev> {
    /// Create a new Loop.
    ///
    /// The loop is empty, holds no events, but is otherwise ready.
    pub fn new(context: Context) -> Result<Self> {
        let (sender, receiver) = channel();
        let poll = Poll::new()?;
        poll.register(&receiver, CHANNEL_TOK, Ready::readable(), PollOpt::empty())?;
        let signal_mask = SigSet::empty();
        let signal_fd = SignalFd::with_flags(&signal_mask, SFD_CLOEXEC | SFD_NONBLOCK)?;
        poll.register(&EventedFd(&signal_fd.as_raw_fd()), SIGNAL_TOK, Ready::readable(), PollOpt::empty())?;
        Ok(Loop {
            poll: poll,
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
            want_idle: LinkedHashMap::new(),
            sender: sender,
            receiver: receiver,
            threadpool: None,
            background_generation: Wrapping(0),
            signal_mask: signal_mask,
            signal_fd: signal_fd,
            signal_recipients: HashMap::new(),
            children: HashMap::new(),
        })
    }
    /// Set an error handler.
    ///
    /// The event handler is invoked whenever an event returns an error. It may arbitrarily decide
    /// what should be done next with the error and return either an error (the same or another),
    /// which terminates the loop, or Ok() to let the loop continue.
    ///
    /// The default implementation simply passes the error through.
    pub fn error_handler_set(&mut self, handler: ErrorHandler<Ev>) {
        self.error_handler = handler
    }
    /// Kill an event at given index.
    fn event_kill(&mut self, idx: usize) -> Ev {
        let event = self.events.release(idx);
        // These timeouts are dead now
        self.timeouts_dead += event.timeouts;
        // Deregister the IOs (the IDs are deregistered in a delayed way)
        for (k, v) in event.ios.into_iter() {
            self.poll.deregister(v.io()).unwrap(); // Must not fail, this one is valid
            self.ios_released.insert(k);
        }
        let handle = Handle {
            id: idx,
            generation: event.generation,
        };
        self.want_idle.remove(&handle);
        // There are only few signals, go through them all instead of building some fancy
        // back-linking
        //
        for sig_recpt in self.signal_recipients.values_mut() {
            sig_recpt.remove(&handle);
        }
        for child in event.children.into_iter() {
            self.children.remove(&child);
        }
        // Return the internal event
        event.event.into_inner()
    }
    /// Run function on an event, with the scope and result checking
    fn event_call<F: FnOnce(&mut Ev, &mut LoopScope<Self>) -> Response>(&mut self, handle: Handle, f: F) -> Result<()> {
        // First check both the index and generation
        if !self.events.valid(handle.id) {
            return Err(Error::Missing);
        }
        if self.events[handle.id].generation != handle.generation {
            return Err(Error::Missing);
        }
        // Try to extract the event out (because of the borrow checker)
        if !self.events[handle.id].event.stolen() {
            let mut event = self.events[handle.id].event.steal();
            // Perform the call
            let result = {
                let mut scope: LoopScope<Self> = LoopScope {
                    event_loop: self,
                    handle: handle,
                };
                // Technically safe. The only bad thing that could happen is the
                // self.events[handle.id].event going away while we access it, but that just
                // doesn't happen here.
                //
                f(unsafe { event.as_mut() }, &mut scope)
            };
            // Return the event we took out
            self.events[handle.id].event.unsteal(event);
            match result {
                Ok(true) => (), // Keep the event alive
                Ok(false) => {
                    self.event_kill(handle.id);
                }, // Kill it as asked for
                Err(err) => {
                    // We can unwrap, we just returned the event there a moment ago
                    let event = self.event_kill(handle.id);
                    // Call the error handler with the broken event and propagate any error it
                    // returns
                    //
                    (self.error_handler)(event, err)?;
                },
            }
            Ok(())
        } else {
            // Not there, but the holder is ‒ it must sit on some outer stack frame, being called
            Err(Error::Busy)
        }
    }
    /// Schedule a timeout for the given event
    fn timeout_at(&mut self, handle: Handle, when: Instant) -> TimeoutId {
        // Generate an ID for the timeout
        let id = TimeoutId(self.timeout_generation.0);
        self.timeout_generation += Wrapping(1);
        // This gets called only through the event's context, so it's safe
        self.events[handle.id].timeouts += 1;
        self.timeouts_inserted += 1;

        self.timeouts.push(TimeoutHolder {
            id: id,
            recipient: handle,
            when: when,
        });

        id
    }
    /// Compute when to wake up latest (we can wake up sooner). This takes the scheduled timeouts
    /// into consideration, but also things like existence of Idle tasks.
    fn timeout_min(&mut self) -> Option<Duration> {
        // First, get rid of all timeouts to dead events (since we don't want to wake up because of
        // them).
        //
        while self.timeouts.peek().map_or(false, |t| !self.event_alive(t.recipient)) {
            self.timeouts.pop();
        }

        if !self.want_idle.is_empty() {
            // We have some idle tasks, so we run them whenever possible ‒ don't block
            return Some(Duration::new(0, 0));
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
    /// Gather the dead child processes
    fn children_gather(scheduled: &mut VecDeque<Task>, children: &mut HashMap<pid_t, Handle>, events: &mut Recycler<EvHolder<Ev>>) -> Result<()> {
        let mut push = |pid, exit| {
            if let Some(handle) = children.remove(&pid) {
                // The event must be alive, otherwise the handle would not be stored there, we
                // remove them right away.
                //
                assert!(events[handle.id].children.remove(&pid));
                scheduled.push_back(Task {
                    recipient: handle,
                    param: TaskParam::Child(pid, exit),
                });
            }
            // No event waiting for it → throw it away
        };
        loop {
            // Wait for any PID whatsoever, but don't block.
            match waitpid(-1, Some(WNOHANG)) {
                Err(nix::Error::Sys(Errno::EAGAIN)) => return Ok(()), // No more children terminated, done
                Ok(WaitStatus::StillAlive) => return Ok(()), // No more children terminated, done
                Err(nix::Error::Sys(Errno::ECHILD)) => return Ok(()), // There are no more children at all
                Err(err) => return Err(Error::Nix(err)), // Propagate other errors
                Ok(WaitStatus::Exited(pid, code)) => push(pid, ChildExit::Exited(code)),
                Ok(WaitStatus::Signaled(pid, signal, _)) => push(pid, ChildExit::Signaled(signal)),
                Ok(_) => (), // Other status changes are not interesting to us
            }
        }
    }
    /// Gather the IO-based tasks (IOs and others we got notified by IO thing)
    fn tasks_gather_io(&mut self) -> Result<()> {
        for ev in self.mio_events.iter() {
            match ev.token() {
                CHANNEL_TOK => {
                    loop {
                        match self.receiver.try_recv() {
                            Err(TryRecvError::Empty) => break, // We have read everything, return next time
                            Err(TryRecvError::Disconnected) => unreachable!(), // We hold one copy of sender ourselves
                            Ok(remote_message) => {
                                // We need to check the allowed types (because remote channels
                                // don't do it)
                                //
                                let passed = match remote_message.mode {
                                    DeliveryMode::Background(_) => true,
                                    _ => self.msg_type_check(remote_message.recipient, &remote_message.real_type).is_ok(),
                                };
                                if passed {
                                    self.scheduled.push_back(Task {
                                        recipient: remote_message.recipient,
                                        param: TaskParam::Message(Message {
                                            data: remote_message.data,
                                            real_type: remote_message.real_type,
                                            mode: remote_message.mode,
                                        }),
                                    })
                                }
                            },
                        }
                    }
                },
                SIGNAL_TOK => {
                    loop {
                        match self.signal_fd.read_signal() {
                            Ok(None) => break, // No more signals for now, return later
                            Ok(Some(siginfo)) => {
                                // Convert there and back ‒ it is not really guaranteed the
                                // from_c_int doesn't do any fancy conversion.
                                //
                                let signal = Signal::from_c_int(siginfo.ssi_signo as i32)?;
                                let signum = signal as i32;
                                if signal == Signal::SIGCHLD {
                                    Self::children_gather(&mut self.scheduled, &mut self.children, &mut self.events)?;
                                }
                                if let Some(ref recipients) = self.signal_recipients.get(&signum) {
                                    self.scheduled.extend(recipients.iter().map(|handle| {
                                        Task {
                                            recipient: handle.clone(),
                                            param: TaskParam::Signal(signal),
                                        }
                                    }));
                                }
                            },
                            Err(e) => return Err(Error::Nix(e)),
                        }
                    }
                },
                Token(mut idx) => {
                    assert!(idx >= TOKEN_SHIFT);
                    idx -= TOKEN_SHIFT;
                    // We should not get any invalid events or IOs now, we didn't fire any events
                    // yet
                    //
                    self.scheduled.push_back(Task {
                        recipient: self.ios[idx],
                        param: TaskParam::Io(IoId(ev.token()), ev.kind()),
                    });
                },
            }
        }
        Ok(())
    }
    /// Kill the timers for dead events if there are too many of them
    fn timers_cleanup(&mut self) {
        // If there are too many dead timeouts, clean them up.
        // Have some limit for low amount of events, where we don't bother.
        //
        // Compute more useful stats than we have
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
            let old = replace(&mut self.timeouts, BinaryHeap::new());
            let pruned: BinaryHeap<TimeoutHolder> = old.into_iter().filter(|ref t| self.event_alive(t.recipient)).collect();
            self.timeouts = pruned;
            self.timeouts_dead = 0;
            self.timeouts_inserted = self.timeouts.len();
        }
    }
    /// Take the timers that are due and put them to the scheduled queue
    fn tasks_gather_timers(&mut self) -> Result<()> {
        while self.timeouts.peek().map_or(false, |t| t.when <= self.now) {
            // This must be Some(...), because of the condition above
            let timeout = self.timeouts.pop().unwrap();
            self.scheduled.push_back(Task {
                recipient: timeout.recipient,
                param: TaskParam::Timeout(timeout.id),
            })
        }
        Ok(())
    }
    /// Gather some tasks (expect there are none scheduled now).
    ///
    /// It is possible this produces no tasks.
    fn tasks_gather(&mut self) -> Result<()> {
        assert!(self.scheduled.is_empty());
        // Release the IO ids we held, there are no more things in queue which could trigger them
        for id in self.ios_released.drain() {
            self.ios.release(id);
        }
        if self.events.is_empty() {
            // We can't gather any tasks if there are no events, we would just block forever
            return Err(Error::Empty);
        }
        let wakeup = self.timeout_min();
        self.poll.poll(&mut self.mio_events, wakeup)?;
        // We slept a while, update the now cache
        self.now = Instant::now();

        // The IO tasks and tasks brought in by special IO things
        self.tasks_gather_io()?;

        // Clean up the timer storage, if needed
        self.timers_cleanup();
        // Go and grab our timers
        self.tasks_gather_timers()?;

        // If there were no timeouts, add one idle task (we don't want to put too many there,
        // so we don't block the loop for too long.
        //
        if self.scheduled.is_empty() {
            if let Some((handle, _)) = self.want_idle.pop_front() {
                self.scheduled.push_back(Task {
                    recipient: handle,
                    param: TaskParam::Idle,
                });
            }
        }

        Ok(())
    }
    /// Check the given event exists and is willing to accept this kind of message
    fn msg_type_check(&self, handle: Handle, tid: &TypeId) -> Result<()> {
        if !self.event_alive(handle) {
            return Err(Error::Missing);
        }
        if !self.events[handle.id].expected_messages.contains(tid) {
            Err(Error::MsgUnexpected)
        } else {
            Ok(())
        }
    }
    /// An implementaion of the send method
    fn send_impl_sender(&mut self, from: Option<Handle>, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.msg_type_check(handle, &tp)?;
        self.scheduled.push_back(Task {
            recipient: handle,
            param: TaskParam::Message(Message {
                data: data,
                real_type: tp,
                mode: DeliveryMode::Send(from),
            }),
        });
        Ok(())
    }
    /// An implementation of the post method.
    fn post_impl_sender(&mut self, from: Option<Handle>, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.msg_type_check(handle, &tp)?;
        self.event_call(handle, |event, scope| {
            event.message(scope,
                          Message {
                              data: data,
                              real_type: tp,
                              mode: DeliveryMode::Post(from),
                          })
        })
    }
    /// Set the number of threads available for the
    /// [background](trait.Scope.html#tymethod.background) tasks.
    ///
    /// Set the number of threads used by the background tasks. At the beginning, no threads are
    /// started. If a background task is submitted before the number of threads is set, single one
    /// is started (the number can be changed later on).
    ///
    /// Raising the number is immediate. When lowering, threads stop only once they finish a task,
    /// so it can take some time.
    pub fn pool_thread_count_set(&mut self, cnt: usize) {
        match self.threadpool {
            None => self.threadpool = Some(ThreadPool::new(cnt)),
            Some(ref mut pool) => pool.set_num_threads(cnt),
        }
    }
    /// Let the loop handle the given signal (in addition to any others it already handles).
    ///
    /// This will let the event loop take care of the given signal. The signal is masked from the
    /// normal signal handlers in this thread and it is registered within the loop. It can then be
    /// handled by events registering for the signal.
    ///
    /// If more than one event registers for the signal, it is broadcasted between them. Similarly,
    /// if no event wants it, the signal is lost.
    ///
    /// Calling this when the signal is already enabled in this event loop is a no-op. Enabling it
    /// in multiple loops for process-level signals may have undesirable effects (the first one to
    /// ask for it gets the signal).
    ///
    /// The first event that asks for the signal automatically triggers enabling of the signal.
    ///
    /// # Notes
    ///
    /// Signals get merged when multiple same ones arrive before they get handled. Therefore,
    /// receiving the signal means that *at least one* was sent, but there may have been more.
    ///
    /// For the signal handling to work, the given signal must be masked in all threads for
    /// process-level signals (most of the useful ones are process-level ones). As the mask is
    /// inherited from creating thread, creating the loop first before starting any threads and
    /// calling this or registering events that use signals is one way to accomplish that. Note that
    /// this also includes the background threads in the loop's thread-pool, which are created once
    /// you either set the number of threads or the first background job is submitted.
    ///
    /// SIGCHLD is handled specially by the loop. But you can enable it this way for the above
    /// reasons.
    ///
    /// The original mask is *not* restored on the loop destruction.
    pub fn signal_enable(&mut self, signal: Signal) -> Result<()> {
        if self.signal_mask.contains(signal) {
            // Already registered, do nothing
            return Ok(());
        }
        self.signal_mask.add(signal);
        self.signal_mask.thread_block()?;
        self.signal_fd.set_mask(&self.signal_mask)?;
        Ok(())
    }
    /// Disable receiving of the given signal and unmask it from this process.
    ///
    /// This does not unregister the events that registered for the signal. They just don't get the
    /// signal notification any more.
    ///
    /// # Notes
    ///
    /// Note that any event may register for the signal later on, which would re-enable the signal
    /// handling.
    ///
    /// Other threads may not get the signals re-enabled automatically (but newly started threads
    /// inherit the current mask).
    pub fn signal_disable(&mut self, signal: Signal) -> Result<()> {
        if !self.signal_mask.contains(signal) {
            // Not there, nothing to disable
            return Ok(());
        }
        self.signal_mask.remove(signal);
        self.signal_fd.set_mask(&self.signal_mask)?;
        let mut unmask = SigSet::empty();
        unmask.add(signal);
        unmask.thread_unblock()?;
        Ok(())
    }
}

impl<Context, Ev: Event<Context, Ev>> LoopIfaceObjSafe<Context, Ev> for Loop<Context, Ev> {
    fn insert_exact(&mut self, event: Ev) -> Result<Handle> {
        // Assign a new generation
        let Wrapping(generation) = self.generation;
        self.generation += Wrapping(1);
        // Store the event in the storage
        let idx = self.events.store(EvHolder {
            event: StolenCell::new(event),
            generation: generation,
            timeouts: 0,
            ios: HashMap::new(),
            expected_messages: HashSet::new(),
            children: HashSet::new(),
        });
        // Generate a handle for it
        let handle = Handle {
            id: idx,
            generation: generation,
        };
        // Run the init for the event
        self.event_call(handle, |event, scope| event.init(scope))?;
        Ok(handle)
    }
    fn context(&mut self) -> &mut Context {
        &mut self.context
    }
    fn run_one(&mut self) -> Result<()> {
        // FIXME: We need to postpone events in case they are currently active.
        // We loop until we find a task that is for a living event
        loop {
            while self.scheduled.is_empty() {
                // Make sure we have something to do
                self.tasks_gather()?
            }
            let task = self.scheduled.pop_front().unwrap();
            if self.event_alive(task.recipient) {
                return match task.param {
                    TaskParam::Io(id, ready) => {
                        let IoId(Token(mut idx)) = id;
                        idx -= TOKEN_SHIFT;
                        // Check that the IO is still valid
                        if self.events[task.recipient.id].ios.contains_key(&idx) {
                            self.event_call(task.recipient, |event, scope| event.io(scope, id, ready))
                        } else {
                            // It's OK to lose the IO in the meantime
                            Ok(())
                        }
                    },
                    TaskParam::Timeout(id) => {
                        self.timeouts_fired += 1;
                        self.events[task.recipient.id].timeouts -= 1;
                        self.event_call(task.recipient, |event, scope| event.timeout(scope, id))
                    },
                    TaskParam::Signal(signal) => self.event_call(task.recipient, |event, scope| event.signal(scope, signal)),
                    TaskParam::Idle => self.event_call(task.recipient, |event, scope| event.idle(scope)),
                    TaskParam::Message(message) => self.event_call(task.recipient, |event, scope| event.message(scope, message)),
                    TaskParam::Child(pid, exit) => self.event_call(task.recipient, |event, scope| event.child(scope, pid, exit)),
                };
            }
        }
    }
    fn run_until_complete(&mut self, handle: Handle) -> Result<()> {
        let mut checked = false;
        while self.event_alive(handle) {
            // We want to deteckt a deadlock when an event that is curretly running is recursively
            // waited on. We do that check on the first iteration only, as it can't change later
            // on. We know the event is alive, so we don't have to check for the validity of
            // the id or if it got reused.
            //
            if !checked && self.events[handle.id].event.stolen() {
                return Err(Error::DeadLock);
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
    fn event_alive(&self, handle: Handle) -> bool {
        self.events.valid(handle.id) && self.events[handle.id].generation == handle.generation
    }
    fn event_count(&self) -> usize {
        self.events.len()
    }
    fn now(&self) -> &Instant {
        &self.now
    }
    fn send_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.send_impl_sender(None, handle, data, tp)
    }
    fn post_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.post_impl_sender(None, handle, data, tp)
    }
    fn channel(&mut self, handle: Handle) -> Result<Channel> {
        Ok(Channel {
            sender: self.sender.clone(),
            handle: handle,
        })
    }
}

struct LoopScope<'a, Loop: 'a> {
    event_loop: &'a mut Loop,
    handle: Handle,
}

impl<'a, Context, Ev: Event<Context, Ev>> LoopScope<'a, Loop<Context, Ev>> {
    fn io_idx(&mut self, id: IoId) -> Result<usize> {
        let IoId(Token(mut idx)) = id;
        idx -= TOKEN_SHIFT;
        if self.event_loop.events[self.handle.id].ios.contains_key(&idx) {
            Ok(idx)
        } else {
            Err(Error::MissingIo)
        }
    }
}

impl<'a, Context, Ev: Event<Context, Ev>> LoopIfaceObjSafe<Context, Ev> for LoopScope<'a, Loop<Context, Ev>> {
    fn insert_exact(&mut self, event: Ev) -> Result<Handle> {
        self.event_loop.insert_exact(event)
    }
    fn context(&mut self) -> &mut Context {
        &mut self.event_loop.context
    }
    fn stop(&mut self) {
        self.event_loop.stop()
    }
    fn run_one(&mut self) -> Result<()> {
        self.event_loop.run_one()
    }
    fn run_until_complete(&mut self, handle: Handle) -> Result<()> {
        self.event_loop.run_until_complete(handle)
    }
    fn run(&mut self) -> Result<()> {
        self.event_loop.run()
    }
    fn event_alive(&self, handle: Handle) -> bool {
        self.event_loop.event_alive(handle)
    }
    fn event_count(&self) -> usize {
        self.event_loop.event_count()
    }
    fn now(&self) -> &Instant {
        self.event_loop.now()
    }
    fn send_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.event_loop.send_impl_sender(Some(self.handle), handle, data, tp)
    }
    fn post_impl(&mut self, handle: Handle, data: Box<Any + 'static>, tp: TypeId) -> Result<()> {
        self.event_loop.post_impl_sender(Some(self.handle), handle, data, tp)
    }
    fn channel(&mut self, handle: Handle) -> Result<Channel> {
        self.event_loop.channel(handle)
    }
}

impl<'a, Context, Ev: Event<Context, Ev>> ScopeObjSafe<Context, Ev> for LoopScope<'a, Loop<Context, Ev>> {
    fn handle(&self) -> Handle {
        self.handle
    }
    fn timeout_at(&mut self, when: Instant) -> TimeoutId {
        self.event_loop.timeout_at(self.handle, when)
    }
    fn idle(&mut self) {
        self.event_loop.want_idle.insert(self.handle, ());
    }
    fn signal(&mut self, signal: Signal) -> Result<()> {
        self.event_loop.signal_enable(signal)?;
        let signum = signal as i32;
        self.event_loop.signal_recipients.entry(signum).or_insert(HashSet::new()).insert(self.handle);
        Ok(())
    }
    fn expect_message(&mut self, tp: TypeId) {
        self.event_loop.events[self.handle.id].expected_messages.insert(tp);
    }
    fn io_register_any(&mut self, io: Box<IoHolderAny>, interest: Ready, opts: PollOpt) -> Result<IoId> {
        let id = self.event_loop.ios.store(self.handle);
        let token = Token(id + TOKEN_SHIFT);
        if let Err(err) = self.event_loop.poll.register(io.io(), token, interest, opts) {
            // If it fails, we want to get rid of the stored io first before returning the error
            self.event_loop.ios.release(id);
            return Err(Error::Io(err));
        }
        self.event_loop.events[self.handle.id].ios.insert(id, io);
        Ok(IoId(token))
    }
    fn io_update(&mut self, id: IoId, interest: Ready, opts: PollOpt) -> Result<()> {
        let idx = self.io_idx(id)?;
        self.event_loop
            .poll
            .reregister(self.event_loop.events[self.handle.id].ios[&idx].io(), id.0, interest, opts)
            .map_err(Error::Io)
    }
    fn io_remove(&mut self, id: IoId) -> Result<()> {
        let idx = self.io_idx(id)?;
        // If the io is valid, remove it from both indexes
        let io = self.event_loop.events[self.handle.id].ios.remove(&idx).unwrap();
        self.event_loop.ios.release(idx);
        // Remove the registration.
        //
        // Note that we still keep the ID held and will release it only before we gather new tasks.
        // This way we can prevent the ID being reused and get triggered by an old event.
        //
        self.event_loop.ios_released.insert(idx);
        self.event_loop.poll.deregister(io.io()).map_err(Error::Io)
        // TODO: Find a way to return the thing?
    }
    fn io_mut(&mut self, id: IoId) -> Result<&mut Box<IoHolderAny>> {
        let idx = self.io_idx(id)?;
        Ok(self.event_loop.events[self.handle.id].ios.get_mut(&idx).unwrap())
    }
    fn background_submit(&mut self, task: Box<TaskWrapper>) -> Result<BackgroundId> {
        if self.event_loop.threadpool.is_none() {
            self.event_loop.pool_thread_count_set(1);
        }
        let id = BackgroundId(self.event_loop.background_generation.0);
        self.event_loop.background_generation += Wrapping(1);
        let sender = self.event_loop.sender.clone();
        let mut task: Box<_> = task;
        self.event_loop.threadpool.as_mut().unwrap().execute(move || {
            task.call(id, sender);
        });
        Ok(id)
    }
    fn child(&mut self, pid: pid_t) -> Result<()> {
        self.event_loop.signal_enable(Signal::SIGCHLD)?;
        assert!(!self.event_loop.children.contains_key(&pid));
        self.event_loop.children.insert(pid, self.handle);
        self.event_loop.events[self.handle.id].children.insert(pid);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::error::*;
    use ::{TimeoutId, IoId};
    use mio::tcp::{TcpListener, TcpStream};
    use mio::{Ready, PollOpt};
    use std::rc::Rc;
    use std::cell::Cell;
    use std::time::Duration;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::io::Write;
    use std::any::TypeId;
    use std::sync::mpsc::{Sender, channel};
    use std::thread::spawn;

    struct InitAndContextEvent(Rc<Cell<bool>>);

    impl Event<bool, InitAndContextEvent> for InitAndContextEvent {
        fn init<S: Scope<bool, InitAndContextEvent>>(&mut self, scope: &mut S) -> Response {
            scope.with_context(|c| {
                    *c = true;
                    Ok(())
                })
                .map(|_| false)
        }
    }

    impl Drop for InitAndContextEvent {
        fn drop(&mut self) {
            let InitAndContextEvent(ref flag) = *self;
            flag.set(true);
        }
    }

    /// Test we can access the context from within an event and its init gets called.
    /// Also test the thing gets destroyed.
    #[test]
    fn init_and_context() {
        let destroyed = Rc::new(Cell::new(false));
        let mut l: Loop<bool, InitAndContextEvent> = Loop::new(false).unwrap();
        let handle = l.insert(InitAndContextEvent(destroyed.clone())).unwrap();
        // Init got called (the context gets set to true there
        l.with_context(|c| {
                assert!(*c);
                Ok(())
            })
            .unwrap();
        // As the event returns false from its init, the destructor should have gotten called
        // by now (even before the loop itself)
        //
        assert!(destroyed.get());
        // And it is not alive (obviously)
        assert!(!l.event_alive(handle));
        // As it is not alive, run_until_complete finishes right away
        l.run_until_complete(handle).unwrap();
        assert_eq!(0, l.event_count());
    }

    macro_rules! err {
        ($result:expr, $err: pat) => (assert!(match $result { Err($err) => true, _ => false }))
    }

    struct Recurse;
    struct Recipient;
    struct SendB(bool);

    impl<C> Event<C, Recipient> for Recipient {
        fn init<S: Scope<C, Recipient>>(&mut self, scope: &mut S) -> Response {
            scope.expect_message(TypeId::of::<()>());
            scope.expect_message(TypeId::of::<Rc<Recurse>>());
            scope.expect_message(TypeId::of::<SendB>());
            // Stay alive for now
            Ok(true)
        }

        fn message<S: Scope<C, Recipient>>(&mut self, scope: &mut S, msg: Message) -> Response {
            let handle = scope.handle();
            if *msg.real_type() == TypeId::of::<()>() {
                Ok(false)
            } else if *msg.real_type() == TypeId::of::<Rc<Recurse>>() {
                scope.post(handle, msg.get::<Rc<Recurse>>()?)?;
                Ok(true)
            } else if *msg.real_type() == TypeId::of::<SendB>() {
                let from = match *msg.mode() {
                    DeliveryMode::Send(from) => from,
                    _ => unreachable!(),
                };
                let SendB(value) = msg.get::<SendB>()?;
                if value {
                    // Sending works even recursively
                    scope.send(handle, SendB(false))?;
                    assert!(from.is_none());
                    Ok(true)
                } else {
                    assert_eq!(handle, from.unwrap());
                    Ok(false)
                }
            } else {
                unreachable!();
            }
        }
    }

    /// Test sending.
    #[test]
    fn send() {
        let mut l: Loop<(), Recipient> = Loop::new(()).unwrap();
        let handle = l.insert(Recipient).unwrap();
        err!(l.send(handle, 42), Error::MsgUnexpected);
        l.send(handle, SendB(true)).unwrap();
        // It'll die, but sending works only when running ‒ nothing delivered yet
        assert!(l.event_alive(handle));
        l.run_until_complete(handle).unwrap();
        // It ended by now (because it got the message from itself, sent after receiving ours)
        assert!(!l.event_alive(handle));
        // Can't send any more
        err!(l.send(handle, ()), Error::Missing);
        // Errors from sending are not propagated to the send() call, but to the loop
        let handle = l.insert(Recipient).unwrap();
        l.send(handle, Rc::new(Recurse)).unwrap();
        err!(l.run_until_complete(handle), Error::Busy);
    }

    /// Test we can post to an event.
    #[test]
    fn post() {
        // Create an event that stays there, at least for a while
        let mut l: Loop<(), Recipient> = Loop::new(()).unwrap();
        let handle = l.insert(Recipient).unwrap();
        assert!(l.event_alive(handle));
        err!(l.post(handle, 42), Error::MsgUnexpected);
        // Post something to it (successfully)
        l.post(handle, ()).unwrap();
        // But it received it and went away
        assert!(!l.event_alive(handle));
        // And if we try to send again, it fails with Missing
        err!(l.post(handle, ()), Error::Missing);
    }

    /// Test we detect busy/recursive post access to an event.
    #[test]
    fn busy_post() {
        // Create an event that stays there
        let mut l: Loop<(), Recipient> = Loop::new(()).unwrap();
        let handle = l.insert(Recipient).unwrap();
        assert!(l.event_alive(handle));
        // Post a Recurse to it, which would create an infinite recursion of posting to self
        err!(l.post(handle, Rc::new(Recurse)), Error::Busy);
        // As it returned error, it got killed (it returned error because it called itself, not
        // because it was called while busy).
        //
        assert!(!l.event_alive(handle));
    }

    struct RemoteRecipient;
    struct RemoteHello(Sender<()>);

    impl Event<(), RemoteRecipient> for RemoteRecipient {
        fn init<S: Scope<(), RemoteRecipient>>(&mut self, scope: &mut S) -> Response {
            scope.expect_message(TypeId::of::<RemoteHello>());
            Ok(true)
        }
        fn message<S: Scope<(), RemoteRecipient>>(&mut self, _scope: &mut S, message: Message) -> Response {
            let RemoteHello(sender) = message.get::<RemoteHello>()?;
            sender.send(()).unwrap();
            Ok(false)
        }
    }

    /// Test for sending hello between multiple threads
    #[test]
    fn channel_hello() {
        let mut l: Loop<(), RemoteRecipient> = Loop::new(()).unwrap();
        let handle = l.insert(RemoteRecipient).unwrap();
        let mut ch = l.channel(handle).unwrap();
        let thread = spawn(move || {
            let (sender, receiver) = channel::<()>();
            // Send a hello to the event in a loop in another thread and wait for an answer
            ch.send(RemoteHello(sender)).unwrap();
            receiver.recv().unwrap();
        });
        l.run_until_complete(handle).unwrap();
        thread.join().unwrap();
    }

    struct BackReceiver {
        answer: Option<BackgroundId>,
        broken: Option<BackgroundId>,
        iter: Option<BackgroundId>,
        received: usize,
    }

    impl Event<(), BackReceiver> for BackReceiver {
        fn init<S: Scope<(), BackReceiver>>(&mut self, scope: &mut S) -> Response {
            self.broken = Some(scope.background(move || -> Result<()> { panic!("Testing handling of panic") })?);
            self.answer = Some(scope.background(move || Ok(42u8))?);
            self.iter = Some(scope.background_iter(move || Ok(vec![(), (), ()].into_iter()))?);
            Ok(true)
        }
        fn message<S: Scope<(), BackReceiver>>(&mut self, _scope: &mut S, message: Message) -> Response {
            self.received += 1;
            let id = match message.mode() {
                &DeliveryMode::Background(id) => id,
                _ => unreachable!(),
            };
            if Some(id) == self.answer {
                assert_eq!(42u8, message.get::<u8>()?);
                self.answer.take(); // Make sure each one arrives only once
            } else if Some(id) == self.broken {
                err!(message.get::<()>(), Error::BackgroundPanicked);
                self.broken.take();
            } else if Some(id) == self.iter {
                match message.get::<()>() {
                    Err(Error::IterEnd) => {
                        self.iter.take();
                    },
                    Err(e) => return Err(e),
                    Ok(()) => (),
                };
            } else {
                unreachable!();
            }
            // Stop after receiving all background tasks (the iter is triggered multiple times)
            Ok(self.received < 6)
        }
    }

    /// Test running tasks in background threads
    #[test]
    fn background() {
        let mut l: Loop<(), BackReceiver> = Loop::new(()).unwrap();
        l.pool_thread_count_set(2);
        // Run multiple events at once
        for _ in 0..3 {
            l.insert(BackReceiver {
                    answer: None,
                    broken: None,
                    iter: None,
                    received: 0,
                })
                .unwrap();
        }
        // Wait for all the events to finish
        err!(l.run(), Error::Empty);
    }

    // An event that terminates after given amount of milliseconds
    struct Timeouter {
        milliseconds: u32,
        id: Option<TimeoutId>,
    }

    impl Event<(), Timeouter> for Timeouter {
        fn init<S: Scope<(), Timeouter>>(&mut self, scope: &mut S) -> Response {
            self.id = Some(scope.timeout_after(&Duration::new(0, self.milliseconds)));
            // Add another timeout that won't have the opportunity to fire, check it doesn't cause
            // problems
            //
            scope.timeout_after(&Duration::new(0, self.milliseconds + 1));
            Ok(true)
        }
        // Terminate on the first timeout we get
        fn timeout<S>(&mut self, _scope: &mut S, id: TimeoutId) -> Response {
            assert_eq!(Some(id), self.id);
            Ok(false)
        }
    }

    /// Test receiving timeouts
    #[test]
    fn timeout() {
        let mut l: Loop<(), Timeouter> = Loop::new(()).unwrap();
        let handle = l.insert(Timeouter {
                milliseconds: 0,
                id: None,
            })
            .unwrap();
        // It is alive, because it needs the loop to turn to get the timeout
        assert!(l.event_alive(handle));
        // We can wait for it to happen. Only one timeout gets called (checked in the event)
        l.run_until_complete(handle).unwrap();
        // Add another one that needs to wait a while and check it actually waited
        assert!(!l.event_alive(handle));
        // Add another event
        let handle_wait = l.insert(Timeouter {
                milliseconds: 500,
                id: None,
            })
            .unwrap();
        // The old one didn't get resurrected
        assert!(!l.event_alive(handle));
        // The new one lives
        assert!(l.event_alive(handle_wait));
        let wait_start = l.now().clone();
        // It should just fire the one event here
        l.run_one().unwrap();
        assert!(!l.event_alive(handle_wait));
        assert!(l.now().duration_since(wait_start) >= Duration::new(0, 500));
        // Check dead-timeouts stats. They got adjusted before the run of the second event.
        assert_eq!(2, l.timeouts_inserted);
        assert_eq!(1, l.timeouts_fired);
        assert_eq!(1, l.timeouts_dead);
        // One dead is still left (the other one would have fired by now, so it's dropped).
        // The one left may be either in the scheduled list or in timouts heap.
        //
        assert_eq!(1, l.timeouts.len() + l.scheduled.len());
    }

    struct ErrorTimeout;

    impl Event<(), ErrorTimeout> for ErrorTimeout {
        fn init<S: Scope<(), ErrorTimeout>>(&mut self, scope: &mut S) -> Response {
            scope.timeout_after(&Duration::new(0, 0));
            Ok(true)
        }
        // The timeout is not implemented, so the default returns an error
    }

    /// Test explicit error handling in the loop.
    #[test]
    fn error_handler() {
        let mut l: Loop<(), ErrorTimeout> = Loop::new(()).unwrap();
        let handle = l.insert(ErrorTimeout).unwrap();
        assert!(l.event_alive(handle));
        // When we run it, it should terminate the loop with error, as it propagates.
        err!(l.run(), Error::DefaultImpl);
        assert!(!l.event_alive(handle));
        // Return some other random error
        l.error_handler_set(Box::new(|_, _| Err(Error::DeadLock)));
        // When we run that failing thing through, it changes the error by the handler
        let handle = l.insert(ErrorTimeout).unwrap();
        err!(l.run(), Error::DeadLock);
        assert!(!l.event_alive(handle));
        // When we ignore the error, it just lets the event die
        let handle = l.insert(ErrorTimeout).unwrap();
        l.error_handler_set(Box::new(|_, _| Ok(())));
        l.run_until_complete(handle).unwrap();
        assert!(!l.event_alive(handle));
        // But if we try to run the loop long-term, it complains it is empty
        l.insert(ErrorTimeout).unwrap();
        l.run_until_empty().unwrap();
    }

    struct Listener;

    impl Event<Option<SocketAddr>, Listener> for Listener {
        fn init<S: Scope<Option<SocketAddr>, Listener>>(&mut self, scope: &mut S) -> Response {
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
        fn io<S: Scope<Option<SocketAddr>, Listener>>(&mut self, scope: &mut S, id: IoId, ready: Ready) -> Response {
            assert_eq!(Ready::readable(), ready);
            err!(scope.with_io(id, |_stream: &mut TcpStream| -> Result<()> {
                     unreachable!();
                 }),
                 Error::IoType);
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

    /// Test notification about IO readiness
    #[test]
    fn io() {
        let mut l: Loop<Option<SocketAddr>, Listener> = Loop::new(None).unwrap();
        l.insert(Listener).unwrap();
        assert_eq!(1, l.event_count());
        let mut addr: Option<SocketAddr> = None;
        l.with_context(|c| {
                addr = c.take();
                Ok(())
            })
            .unwrap();
        let _stream = TcpStream::connect(&addr.unwrap()).unwrap();
        l.run().unwrap();
    }

    struct Idle;

    impl Event<(), Idle> for Idle {
        fn init<S: Scope<(), Idle>>(&mut self, scope: &mut S) -> Response {
            scope.timeout_after(&Duration::new(10, 0));
            scope.idle();
            Ok(true)
        }

        fn idle<S: Scope<(), Idle>>(&mut self, scope: &mut S) -> Response {
            scope.stop();
            Ok(false)
        }
        // The timeout is not implemented and would fail
    }

    /// Test running tasks when the loop is idle
    #[test]
    fn idle() {
        // If the idle doesn't fire, this would return error due to the timeout not being
        // implemented
        //
        let mut l: Loop<(), Idle> = Loop::new(()).unwrap();
        l.insert(Idle).unwrap();
        l.run().unwrap();
    }

    // Note that signal handling is tested in a separate integration test without any harness. We
    // need to block the signals in all threads, but running tests in parallel and the harness
    // itself living in a thread we never see simply prevents that.
    //
    // It would be great if POSIX offered better way to handle signals when there are threads, but,
    // well, we have to live with what we have.
    //
}
