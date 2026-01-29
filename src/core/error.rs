use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Corrupt(&'static str),
    CorruptMetadata(&'static str),
    Unsupported(&'static str),
    UnsupportedVersion(u32),
    PayloadTooLarge,
    QueueFull,
    WriterAlreadyActive,
    InvalidPartition(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => write!(f, "io error: {err}"),
            Error::Corrupt(msg) => write!(f, "corrupt data: {msg}"),
            Error::CorruptMetadata(msg) => write!(f, "corrupt metadata: {msg}"),
            Error::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            Error::UnsupportedVersion(version) => write!(f, "unsupported version: {version}"),
            Error::PayloadTooLarge => write!(f, "payload too large"),
            Error::QueueFull => write!(f, "queue full"),
            Error::WriterAlreadyActive => write!(f, "writer already active"),
            Error::InvalidPartition(msg) => write!(f, "invalid partition: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Error::Io(value)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
