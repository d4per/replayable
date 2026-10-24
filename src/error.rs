use std::fmt;

/// Why a log couldn't be loaded.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Reading the log failed.
    Io(std::io::Error),
    /// A line wasn't a valid event. `line` starts at 1.
    Json {
        /// The line number, starting at 1.
        line: usize,
        /// What serde_json reported.
        source: serde_json::Error,
    },
    /// A binary log, as written by
    /// [`Store::write_binary`](crate::Store::write_binary), couldn't be read.
    Binary {
        /// The byte offset of the event that couldn't be read.
        offset: usize,
        /// What was wrong.
        reason: String,
    },
    /// An event was well-formed but doesn't fit the state built up so far,
    /// for example an update to an entity that doesn't exist or a list index
    /// out of range. This means the log is corrupt or was changed by hand.
    InvalidEvent {
        /// The position of the event in the log, starting at 0.
        index: usize,
        /// What was wrong.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "couldn't read the log: {e}"),
            Error::Json { line, source } => write!(f, "invalid event on line {line}: {source}"),
            Error::Binary { offset, reason } => write!(f, "invalid binary log at byte {offset}: {reason}"),
            Error::InvalidEvent { index, reason } => write!(f, "event {index} can't be applied: {reason}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Json { source, .. } => Some(source),
            Error::Binary { .. } | Error::InvalidEvent { .. } => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
