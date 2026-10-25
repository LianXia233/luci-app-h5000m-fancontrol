//! H5000M fan controller backend.
//!
//! The crate is deliberately dependency free: the controller runs at boot,
//! before optional tooling is available, and must not grow a supply chain on a
//! thermally safety relevant path.

pub mod cli;
pub mod config;
pub mod controller;
pub mod curve;
pub mod daemon;
pub mod error;
pub mod events;
pub mod fan;
pub mod json;
pub mod lock;
pub mod logging;
pub mod modem;
pub mod paths;
pub mod platform;
pub mod report;
pub mod safety;
pub mod sensor;
pub mod state;
pub mod uci;

pub use error::{Error, Result};

/// Version reported by `--version`, `status` and the package Makefile.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const LOG_TAG: &str = "h5000m-fancontrol";
