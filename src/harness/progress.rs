//! Ephemeral observations from the runner; these events never authorize actions.

#[derive(Debug, Clone)]
pub enum Progress {
    Status(String),
    AssistantDelta(String),
    CommandOutput(String),
}

pub type ProgressSender = tokio::sync::mpsc::UnboundedSender<Progress>;
