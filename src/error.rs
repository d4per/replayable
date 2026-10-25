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
        offset: u64,
        /// What was wrong.
        reason: String,
    },
    /// An event was well-formed but doesn't fit the state built up so far,
    /// for example an update to an entity that doesn't exist or a list index
    /// out of range. This means the log is corrupt or was changed by hand.
    InvalidEvent {
        /// The number of events before it, as [`Store::as_of`](crate::Store::as_of)
        /// counts them: blank lines and events skipped for a removed field or
        /// entity type aren't counted.
        index: usize,
        /// Where the event is in the file, if it was read from one.
        at: Option<Position>,
        /// What was wrong.
        reason: String,
    },
}

/// Where an event is in a log file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// A line of a JSON Lines log, starting at 1.
    Line(usize),
    /// The byte offset of an event in a binary log.
    Offset(u64),
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Position::Line(line) => write!(f, "line {line}"),
            Position::Offset(offset) => write!(f, "byte {offset}"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "couldn't read the log: {e}"),
            Error::Json { line, source } => write!(f, "invalid event on line {line}: {source}"),
            Error::Binary { offset, reason } => write!(f, "invalid binary log at byte {offset}: {reason}"),
            Error::InvalidEvent { index, at: None, reason } => write!(f, "event {index} can't be applied: {reason}"),
            Error::InvalidEvent { index, at: Some(at), reason } => {
                write!(f, "event {index}, at {at}, can't be applied: {reason}")
            }
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
