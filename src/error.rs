use std::convert::From;
use std::io;

/// A EveBoros error.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    // The default implementation called
    DefaultImpl,
    // An event waits recursively for itself
    DeadLock,
}

// TODO: Implement the error trait

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// A result for EveBoros operations that may fail
pub type Result<T> = ::std::result::Result<T, Error>;
