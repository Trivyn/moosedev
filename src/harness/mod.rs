//! Daemon-backed coding workflow with mandatory reading and capture.
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
pub mod session;
#[cfg(feature = "harness")]
pub mod startup;
#[cfg(feature = "harness")]
pub mod tui;
