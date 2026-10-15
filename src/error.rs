//! Error type shared across the core.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Cancelled,
    Busy,
    /// The external media tool could not be located.
    ToolMissing(String),
    /// The external media tool ran but failed.
    ToolFailed {
        cmd: String,
        detail: String,
    },
    /// Filesystem problem while reading or staging.
    Io(String),
    /// The export could not be written to the requested place. Kept separate
    /// from [`Error::Io`] so the front end can tell the user something useful
    /// ("that file is in use" / "that folder is not writable") instead of
    /// showing a bare OS message.
    Output(String),
    /// Input did not look like a usable video.
    BadMedia(String),
    /// Caller passed something nonsensical.
    Invalid(String),
    /// GIF encoding problem.
    Encode(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "export cancelled"),
            Error::Busy => write!(f, "the kernel handle is busy"),
            Error::ToolMissing(t) => write!(f, "media tool not found: {t}"),
            Error::ToolFailed { cmd, detail } => write!(f, "{cmd} failed: {detail}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Output(e) => write!(f, "{e}"),
            Error::BadMedia(e) => write!(f, "{e}"),
            Error::Invalid(e) => write!(f, "{e}"),
            Error::Encode(e) => write!(f, "gif encode failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
