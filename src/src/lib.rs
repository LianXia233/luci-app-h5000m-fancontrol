//! H5000M fan controller core.
//!
//! Module map:
//! - [`paths`]: every filesystem location the controller touches,
//!   overridable via environment variables so tests never touch real
//!   hardware.
//! - [`uci`]: reads the `h5000m_fancontrol` UCI config through `uci show`
//!   (with delta awareness) or an in-process source for tests.
//! - [`config`]: validated, clamped effective configuration and defaults.
//! - [`curve`]: fan curve parsing, validation and interpolation.
//! - [`sensor`]: temperature sensor discovery (thermal zones, hwmon, vendor
//!   iwpriv fallback, MT5700M modem cache) with freshness checks.
//! - [`fan`]: PWM node discovery plus read/write helpers.
//! - [`thermal`]: CPU thermal zone inspection and legacy policy recovery.
//! - [`safety`]: device-tree cooling-map analysis and the kernel safety floor.
//! - [`state`]: persisted controller state file.
//! - [`controller`]: one control decision (the former `apply_once`).
//! - [`status`]: unified status model with JSON and legacy text output.
//! - [`service`]: daemon loop, lock handling and signal behaviour.
//! - [`proc`]: bounded subprocess execution and pid liveness.
//! - [`jsonw`]: minimal dependency-free JSON writer.

pub mod config;
pub mod controller;
pub mod curve;
pub mod fan;
pub mod jsonw;
pub mod paths;
pub mod proc;
pub mod safety;
pub mod sensor;
pub mod service;
pub mod state;
pub mod status;
pub mod thermal;
pub mod uci;

/// Controller version, kept in sync with the OpenWrt package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Logger tag used by every `logger` call so logread filtering stays stable.
pub const LOG_TAG: &str = "h5000m-fancontrol";
