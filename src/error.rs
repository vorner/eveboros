use std::convert::From;
use std::io;

/// A EveBoros error. Empty for now, but that shall change.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    DefaultImpl,
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// A result for EveBoros operations that may fail
pub type Result<T> = ::std::result::Result<T, Error>;
