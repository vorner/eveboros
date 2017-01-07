//! Simple TCP echo server
//!
//! The classical example for asynchronous library. It listens on a TCP, accepts any connections
//! and everything that comes sends back to where it came from.
//!
//! You specify the listening address on the command line.

#[macro_use(combined)]
extern crate eveboros;
extern crate mio;

use std::env::args;
use std::net::{SocketAddr, IpAddr, Ipv6Addr};
use std::io::{stderr, Write, Read, ErrorKind};

use mio::tcp::{TcpStream, TcpListener};

use eveboros::error::{Error, Result};
use eveboros::{Loop, LoopIface, LoopIfaceObjSafe, Event, Scope, Response, Signal, Ready, PollOpt, IoId};

// This will be the event responsible for listening for new connections
struct Listener(SocketAddr);

impl<Ev: From<Connection>> Event<(), Ev> for Listener {
    fn init<S: Scope<(), Ev>>(&mut self, scope: &mut S) -> Response {
        // Create the listening socket and hand it to the loop to be watched
        scope.io_register(TcpListener::bind(&self.0)?, Ready::readable(), PollOpt::empty())?;
        Ok(true)
    }
    fn io<S: Scope<(), Ev>>(&mut self, scope: &mut S, io: IoId, _ready: Ready) -> Response {
        // Accept the available connections, in a loop.
        // However, limit the number of accepted connections during one event loop iteration, to
        // prevent starvation of the other events there.
        for _ in 0..128 {
            // The mio docs say this returns Option<(TcpStream, SocketAddr)>, but in reality it
            // only returns the tuple.
            match scope.with_io(io, |listener: &mut TcpListener| listener.accept().map_err(|e| e.into())) {
                Err(Error::Io(ioerr)) => {
                    match ioerr.kind() {
                        ErrorKind::WouldBlock => return Ok(true), // We've run out of connections to accept
                        ErrorKind::Interrupted => (), // Ignore interrupts and try again
                        _ => return Err(Error::Io(ioerr)), // Something else bad happened
                    }
                },
                Err(err) => return Err(err), // Propagate other errors
                Ok((stream, _)) => {
                    scope.insert(Connection {
                            // Create a new connection event
                            stream: Some(stream),
                            buf: [0; 1024],
                            buf_pos: 0,
                            buf_size: 0,
                        })
                        .map(|_| ())?
                },
            }
        }
        Ok(true)
    }
}

// This one handles single connection. It'll be created by Listener
struct Connection {
    // This holds the stream during the creation
    stream: Option<TcpStream>,
    // A buffer we read into each time and write it back in the next iteration
    buf: [u8; 1024],
    buf_pos: usize,
    buf_size: usize,
}

impl<Ev> Event<(), Ev> for Connection {
    fn init<S: Scope<(), Ev>>(&mut self, scope: &mut S) -> Response {
        // We take the tcp stream out of self and pass it to the event loop.
        // We really do expect the stream to be there, so we do it without checking for None.
        scope.io_register(self.stream.take().unwrap(), Ready::readable(), PollOpt::empty())?;
        Ok(true)
    }
    fn io<S: Scope<(), Ev>>(&mut self, scope: &mut S, io: IoId, ready: Ready) -> Response {
        // We either want to read or write, not both at the same time
        if ready.is_readable() {
            // Nothing in the buffer.
            assert_eq!(self.buf_pos, self.buf_size);
            self.buf_pos = 0;
            // So we read some and set the size.
            self.buf_size = scope.with_io(io, |stream: &mut TcpStream| {
                    match stream.read(&mut self.buf[..]) {
                        // EOF. Produce an artificial error that'll kill the event in the event loop
                        // (but not the event loop, thanks to the error handler ‒ see run())
                        Ok(0) => Err(Error::User(None)),
                        Ok(size) => Ok(size), // Some data read
                        Err(ioerr) => {
                            match ioerr.kind() {
                                // Nothing read, but that's not that bad, try again later
                                ErrorKind::WouldBlock | ErrorKind::Interrupted => Ok(0),
                                _ => Err(Error::Io(ioerr)), // Some other bad error, propagate it
                            }
                        },
                    }
                })?;
            // In real life, we would try writing right now, because the socket is
            // very likely writable and it would save us some manipulation with the
            // ready-intents and another loop iteration. But in this example, we want
            // it to look simple, so we just read it and schedule writing once the
            // loop tells us.
        } else if ready.is_writable() {
            // Something in the buffer to be sent. Try sending it and adjust the position.
            assert!(self.buf_pos < self.buf_size);
            let size = scope.with_io(io, |stream: &mut TcpStream| {
                    match stream.write(&self.buf[self.buf_pos..self.buf_size]) {
                        Ok(size) => Ok(size),
                        Err(ioerr) => {
                            match ioerr.kind() {
                                // Try again next time
                                ErrorKind::WouldBlock | ErrorKind::Interrupted => Ok(0),
                                _ => Err(Error::Io(ioerr)), // Propagate other errors
                            }
                        },
                    }
                })?;
            self.buf_pos += size;
        } else {
            unreachable!();
        }
        // Adjust the readiness intent. It might be a NO-OP in some cases, but we don't really care
        // about that extra syscall here. Also, in real life, we might want to do some clever
        // things like if we have both some data to write and space to read to then register both
        // intents, but we don't do that here.
        if self.buf_pos == self.buf_size {
            // Nothing in the buffer ‒ we want to read
            scope.io_update(io, Ready::readable(), PollOpt::empty())?;
        } else {
            // We have data to write
            scope.io_update(io, Ready::writable(), PollOpt::empty())?;
        }
        Ok(true)
    }
}

// This one shuts the loop down gracefully on a signal. We could just let the default handler kill
// the program, but this demonstrates more.
struct Shutdown;

impl<Ev> Event<(), Ev> for Shutdown {
    fn init<S: Scope<(), Ev>>(&mut self, scope: &mut S) -> Response {
        // Register some signals when we would like to shut down
        scope.signal(Signal::SIGINT)?;
        scope.signal(Signal::SIGQUIT)?;
        scope.signal(Signal::SIGTERM)?;
        Ok(true)
    }
    fn signal<S: Scope<(), Ev>>(&mut self, scope: &mut S, _signal: Signal) -> Response {
        // Stop the loop
        scope.stop();
        // And we don't need to run any more
        Ok(false)
    }
}

// All the events combined together, so we can put them into the loop.
combined!(AllEvents, (),
    S(Shutdown),
    L(Listener),
    C(Connection),
);

fn run() -> Result<()> {
    // First, decide where we want to listen.
    // It would be cool to have actual address constants, but this is the wildcard [::].
    let listen_addr = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0));
    let port = args()
        .nth(1) // Take the port from the first parameter
        .or(Some("6666".into())) // Provide a default if nothing is provided
        .unwrap().parse() // We want it as a number
        .map_err(|e| Error::UserStr(format!("Couldn't parse the port number: {}", e)))?;
    let sockaddr = SocketAddr::new(listen_addr, port);
    // Create the loop and fill the basic set of events in there.
    let mut l: Loop<_, AllEvents> = Loop::new(())?;
    l.insert(Shutdown)?;
    l.insert(Listener(sockaddr))?;
    // The network is a dangerous place and connections get killed all the time, producing errors.
    // So in case of errors on connections, we simply just drop the connections and go on. We
    // however pass the other errors.
    l.error_handler_set(Box::new(|ev, error| -> Result<()> {
        // We may want to do some logging here...
        match ev {
            AllEvents::C(_) => Ok(()),
            _ => Err(error),
        }
    }));
    // And run until we get stopped
    l.run()
}

fn main() {
    if let Err(e) = run() {
        writeln!(stderr(), "{}", e).unwrap();
        std::process::exit(1);
    }
}
