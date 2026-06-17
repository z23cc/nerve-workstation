use crate::{RuntimeCommand, RuntimeJobError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Runtime event emitted by human-facing adapters while executing jobs.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    JobStarted {
        job_id: String,
        command: String,
        tool_name: Option<String>,
    },
    JobProgress {
        job_id: String,
        stage: String,
        message: String,
        current: Option<u64>,
        total: Option<u64>,
    },
    JobCancelRequested {
        job_id: String,
    },
    JobCompleted {
        job_id: String,
    },
    JobFailed {
        job_id: String,
        error: RuntimeJobError,
    },
    JobCancelled {
        job_id: String,
    },
}

impl RuntimeEvent {
    #[must_use]
    pub fn job_started(job_id: impl Into<String>, command: &RuntimeCommand) -> Self {
        Self::JobStarted {
            job_id: job_id.into(),
            command: command.name().to_string(),
            tool_name: command.tool_name().map(str::to_string),
        }
    }

    #[must_use]
    pub fn job_progress(
        job_id: impl Into<String>,
        stage: impl Into<String>,
        message: impl Into<String>,
        current: Option<u64>,
        total: Option<u64>,
    ) -> Self {
        Self::JobProgress {
            job_id: job_id.into(),
            stage: stage.into(),
            message: message.into(),
            current,
            total,
        }
    }

    #[must_use]
    pub fn job_cancel_requested(job_id: impl Into<String>) -> Self {
        Self::JobCancelRequested {
            job_id: job_id.into(),
        }
    }

    #[must_use]
    pub fn job_completed(job_id: impl Into<String>) -> Self {
        Self::JobCompleted {
            job_id: job_id.into(),
        }
    }

    #[must_use]
    pub fn job_failed(job_id: impl Into<String>, error: RuntimeJobError) -> Self {
        Self::JobFailed {
            job_id: job_id.into(),
            error,
        }
    }

    #[must_use]
    pub fn job_cancelled(job_id: impl Into<String>) -> Self {
        Self::JobCancelled {
            job_id: job_id.into(),
        }
    }
}
