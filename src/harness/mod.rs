//! Daemon-backed coding workflow with mandatory reading and capture.

/// The project's local configuration file (see [`crate::config`]); `moosedev
/// init` ignores it in git.
pub const CONFIG_FILE_NAME: &str = crate::config::FILE_NAME;

#[cfg(feature = "harness")]
mod clipboard;
#[cfg(feature = "harness")]
pub mod config;
pub mod coverage;
pub mod daemon;
pub mod digest;
#[cfg(feature = "harness")]
pub mod executor;
#[cfg(feature = "harness")]
mod markdown;
#[cfg(feature = "harness")]
pub mod progress;
pub mod protocol;
#[cfg(feature = "harness")]
pub mod response;
#[cfg(feature = "harness")]
pub mod runner;
#[cfg(feature = "harness")]
mod selection;
#[cfg(feature = "harness")]
pub mod session;
#[cfg(feature = "harness")]
pub mod startup;
#[cfg(feature = "harness")]
pub mod tui;
