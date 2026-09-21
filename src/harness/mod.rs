//! Daemon-backed coding workflow with mandatory reading and capture.

/// The harness's local model configuration, in the project root. Named here,
/// outside the `harness` feature, because `moosedev init` must ignore it.
pub const CONFIG_FILE_NAME: &str = "moosedev.toml";

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
