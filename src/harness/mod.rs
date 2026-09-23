//! Daemon-backed coding workflow with mandatory reading and capture.

/// The project's local configuration file (see [`crate::config`]); `moosedev
/// init` ignores it in git.
pub const CONFIG_FILE_NAME: &str = crate::config::FILE_NAME;

/// The project's editable standing guidance for the coding model. `moosedev
/// init` keeps it trackable in git and installs `GUIDANCE_FILE.example` beside
/// it; the harness reads it when a task is created. Named here, beside
/// [`CONFIG_FILE_NAME`], because `init` is built without the harness feature.
pub const GUIDANCE_FILE: &str = ".moosedev/GUIDANCE.md";
/// Compiled standing guidance used when a project has no [`GUIDANCE_FILE`].
pub const DEFAULT_GUIDANCE: &str = include_str!("../../templates/harness/GUIDANCE.md");

/// The example `init` installs: an explanatory comment, then the compiled
/// default verbatim. A real file *replaces* the default, so the example has to
/// carry it — composed rather than copied, so the two cannot drift apart, and
/// the comment leads so a project opens the file at the explanation.
pub fn guidance_example() -> String {
    format!(
        "{}\n{}\n",
        include_str!("../../templates/harness/GUIDANCE.example.md").trim(),
        DEFAULT_GUIDANCE.trim()
    )
}

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
