//! Error type shared by the controller modules.
//!
//! Every variant carries the path or option name that produced it so that the
//! status report and the diagnostics tab can explain a failure without the
//! caller having to guess.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A sysfs/UCI/state file could not be read or written.
    Io { path: String, message: String },
    /// A value was present but not parseable in the expected form.
    Invalid { what: String, value: String },
    /// No PWM control node exists; cooling cannot be applied at all.
    PwmMissing,
    /// Writing or reading back the PWM node failed.
    PwmWrite { path: String, message: String },
    /// The exclusive controller lock is held by another live process.
    LockBusy { pid: u32 },
    /// A curve could not be used for control.
    Curve(String),
    /// The daemon was asked to stop while performing a bounded wait.
    Interrupted,
}

impl Error {
    pub fn io(path: impl Into<String>, err: &std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            message: err.to_string(),
        }
    }

    pub fn invalid(what: impl Into<String>, value: impl Into<String>) -> Self {
        Error::Invalid {
            what: what.into(),
            value: value.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, message } => write!(f, "{path}: {message}"),
            Error::Invalid { what, value } => write!(f, "invalid {what}: {value:?}"),
            Error::PwmMissing => write!(f, "pwm control node not found"),
            Error::PwmWrite { path, message } => {
                write!(f, "failed to set pwm at {path}: {message}")
            }
            Error::LockBusy { pid } => write!(f, "fan control is busy (pid {pid})"),
            Error::Curve(message) => write!(f, "invalid fan curve: {message}"),
            Error::Interrupted => write!(f, "interrupted"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io {
            path: String::new(),
            message: err.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
