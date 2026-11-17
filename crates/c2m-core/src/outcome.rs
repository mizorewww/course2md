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
    /// For a step in batches that finished only some: where its first
    /// unfinished part is, and what the service said about it, so a reader
    /// words them in its own language instead of parsing `message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_failure: Option<FirstFailure>,
}

/// The first unfinished part of a step in batches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FirstFailure {
    /// "12:30–12:41": where in the video it is.
    pub range: String,
    /// What the service said, when it said anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The service refused that part as it is (its content), while it accepted
    /// the rest.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub refused: bool,
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
            first_failure: None,
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
    pub fn is_incomplete(&self) -> bool {
        matches!(self.status, Status::Failed | Status::Partial)
    }
    /// `None` when not incomplete; `Some(Some(n))` when `total` and `completed`
    /// give an exact remaining count, or `Some(None)` when incomplete without counts.
    pub fn unfinished_count(&self) -> Option<Option<usize>> {
        self.is_incomplete()
            .then(|| match (self.completed, self.total) {
                (Some(done), Some(total)) => Some(total.saturating_sub(done)),
                _ => None,
            })
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
        .any(Outcome::is_incomplete)
    }
}

/// A service's answer, read in full.
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// How long the service asked to be left alone (`Retry-After` in seconds).
    pub retry_after: Option<std::time::Duration>,
}

impl HttpResponse {
    /// An answer that asks for no particular wait.
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            body,
            retry_after: None,
        }
    }
}

/// Why an answer is unusable, when sending the same request again would only
/// bring back the same: an automatic retry leaves it to the caller to ask
/// differently. The receipt still records a failure, so nothing replays it.
#[derive(Debug)]
pub struct Repeats(pub String);

impl std::fmt::Display for Repeats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Repeats {}

/// A request that got no complete answer, and whether it provably never left.
pub struct NetworkFailure {
    pub message: String,
    pub definitely_unsent: bool,
}

/// Another writer has the note: another task, and this one stopped before any
/// work; or an older app saving into it past the wait, which a publication can
/// meet after the task's work. That work stays in the task's work folder, and
/// running the same task again takes its finished requests from their receipts
/// instead of sending them again.
#[derive(Debug)]
pub struct NoteBusy(pub String);

impl std::fmt::Display for NoteBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoteBusy {}

#[derive(Debug)]
pub struct Failure {
    pub status: Option<u16>,
    pub retryable: bool,
    pub message: String,
    pub unsupported_response_format: bool,
    /// An optional request parameter the service named when it refused the
    /// request (`stream_options`, `stream`, `reasoning`): sent without it, the
    /// request may be accepted.
    pub refused_parameter: Option<String>,
    /// The answer was unusable in a way the same request would only repeat
    /// ([`Repeats`]); the caller has to ask differently.
    pub repeats: bool,
    /// What the service said about a refused or failed request, as its receipt
    /// keeps it ([`crate::outcome`] callers word the cause from it).
    pub detail: Option<String>,
    /// The service answered, but the answer did not fit what was asked (a
    /// paragraph missing or out of place, no readable structure). Asking for
    /// less at once may get a usable one.
    pub unusable: bool,
    pub kind: FailureKind,
    /// The wait the service asked for before another attempt, if it named one.
    pub retry_after: Option<std::time::Duration>,
    /// Whether this attempt transmitted a network request now (as opposed to
    /// replaying a recorded receipt or stopping before bytes left the host).
    pub sent: bool,
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
            refused_parameter: None,
            repeats: false,
            detail: None,
            unusable: false,
            kind: FailureKind::Local,
            retry_after: None,
            sent: false,
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
