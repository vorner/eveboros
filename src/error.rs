use std::convert::From;
use std::io;

/// A EveBoros error. Empty for now, but that shall change.
pub enum Error {
    Io(io::Error)
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// A result for EveBoros operations that may fail
pub type Result<T> = ::std::result::Result<T, Error>;
