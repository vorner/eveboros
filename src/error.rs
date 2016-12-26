use std::convert::From;
use std::io;

/// A EveBoros error.
#[derive(Debug)]
pub enum Error {
    /// An embedded IO error from some low-level operation
    Io(io::Error),
    /**
     * The default implementation called
     *
     * You likely registered for something but haven't implemented the receiver.
     */
    DefaultImpl,
    /// An event waits recursively for itself
    DeadLock,
    /// The referred event is not there
    Missing,
    /// The referred IO is not there or not belonging to the current event
    MissingIo,
    /**
     * The referred event is currently in the middle of a callback and
     * can't be bothered right now (you know, recursion).
     */
    Busy,
    /// The loop is empty and tries to run
    Empty,
    /// The requested IO is of a different type
    IoType,
    /// The requested type of the message does not match (when extracting)
    MsgType,
    /// Message type not expected, can't send
    MsgUnexpected,
}

// TODO: Implement the error trait

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// A result for EveBoros operations that may fail
pub type Result<T> = ::std::result::Result<T, Error>;
