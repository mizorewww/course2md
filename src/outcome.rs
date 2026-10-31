//! How a piece of work ended: a service request's failure and a note component's outcome.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    NotRequested,
    Succeeded,
    Partial,
    Failed,
}

/// Machine-readable cause of a failed component. Display text stays free-form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Authentication,
    Model,
    Parameters,
    RateLimit,
    Service,
    Local,
    Uncertain,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FailureKind>,
}

impl Default for Outcome {
    fn default() -> Self {
        Self::not_requested()
    }
}

impl Outcome {
    pub fn succeeded() -> Self {
        Self {
            status: Status::Succeeded,
            message: None,
            completed: None,
            total: None,
            kind: None,
        }
    }
    pub fn not_requested() -> Self {
        Self {
            status: Status::NotRequested,
            ..Self::succeeded()
        }
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            status: Status::Failed,
            message: Some(message.into()),
            kind: Some(FailureKind::Local),
            ..Self::succeeded()
        }
    }
    /// Copies the structured cause from a dispatcher failure when present.
    pub fn from_error(error: &anyhow::Error) -> Self {
        let failure = error.downcast_ref::<Failure>();
        let message = failure.map_or_else(|| format!("{error:#}"), |f| f.message.clone());
        Self::from_error_with_message(error, message)
    }
    pub fn from_error_with_message(error: &anyhow::Error, message: impl Into<String>) -> Self {
        let failure = error.downcast_ref::<Failure>();
        Self {
            status: Status::Failed,
            message: Some(message.into()),
            kind: Some(failure.map_or(FailureKind::Local, |f| f.kind)),
            ..Self::succeeded()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outcomes {
    pub transcript: Outcome,
    pub screenshots: Outcome,
    pub proofreading: Outcome,
    pub summary: Outcome,
    /// Bilingual translation; not requested when the note is already in the target.
    pub translation: Outcome,
    pub exports: BTreeMap<String, Outcome>,
}

impl Outcomes {
    pub fn is_partial(&self) -> bool {
        [
            &self.transcript,
            &self.screenshots,
            &self.proofreading,
            &self.summary,
            &self.translation,
        ]
        .into_iter()
        .chain(self.exports.values())
        .any(|o| matches!(o.status, Status::Failed | Status::Partial))
    }
}

#[derive(Debug)]
pub struct Failure {
    pub status: Option<u16>,
    pub retryable: bool,
    pub message: String,
    pub unsupported_response_format: bool,
    pub kind: FailureKind,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

impl Failure {
    /// A definite failure that no service status explains.
    pub fn local(error: impl std::fmt::Display) -> Self {
        Self {
            status: None,
            retryable: false,
            message: error.to_string(),
            unsupported_response_format: false,
            kind: FailureKind::Local,
        }
    }
    pub fn with_status(status: Option<u16>, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            status,
            retryable,
            message: message.into(),
            kind: kind_for_status(status),
            ..Self::local("")
        }
    }
    /// The request may have been processed; sending it again needs consent.
    pub fn uncertain(&self) -> bool {
        self.kind == FailureKind::Uncertain
    }
}

fn kind_for_status(status: Option<u16>) -> FailureKind {
    match status {
        Some(401 | 403) => FailureKind::Authentication,
        Some(404) => FailureKind::Model,
        Some(400 | 422) => FailureKind::Parameters,
        Some(429) => FailureKind::RateLimit,
        Some(500..=599) => FailureKind::Service,
        None => FailureKind::Local,
        _ => FailureKind::Other,
    }
}
