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

/// A service's answer, read in full.
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A request that got no complete answer, and whether it provably never left.
pub struct NetworkFailure {
    pub message: String,
    pub definitely_unsent: bool,
}

/// A part of a note a refused result did less of than the saved note: its id
/// (`transcript`, `screenshots`, `proofreading`, `translation`, `summary`) and
/// the cause its outcome recorded, in the words the engine wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptPart {
    pub part: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

/// A task's result was not published: the saved note holds more of the `parts`
/// the task tried to make, so the note was kept. `message` says so in the
/// engine's language; [`note_kept_message`] says it in any other.
#[derive(Debug)]
pub struct NoteKept {
    pub message: String,
    pub parts: Vec<KeptPart>,
}

impl NoteKept {
    pub fn new(parts: Vec<KeptPart>) -> Self {
        Self {
            message: note_kept_message(&parts, c2m_i18n::i18n::lang()),
            parts,
        }
    }
}

impl std::fmt::Display for NoteKept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Why a result was refused and the note kept, in `lang`: the parts by name,
/// in the language's list form, and their causes.
pub fn note_kept_message(parts: &[KeptPart], lang: c2m_i18n::i18n::Lang) -> String {
    use c2m_i18n::i18n::{Lang, engine_message_for, t_for};
    let name = |part: &str| match part {
        "transcript" => t_for("文字", lang).to_owned(),
        "screenshots" => t_for("截图", lang).to_owned(),
        "proofreading" => t_for("校对", lang).to_owned(),
        "translation" => t_for("翻译", lang).to_owned(),
        "summary" => t_for("摘要", lang).to_owned(),
        other => other.to_owned(),
    };
    let (list, causes_separator) = match lang {
        Lang::En => (", ", "; "),
        _ => ("、", "；"),
    };
    let names = parts
        .iter()
        .map(|part| name(&part.part))
        .collect::<Vec<_>>()
        .join(list);
    let mut causes: Vec<String> = Vec::new();
    for cause in parts
        .iter()
        .filter_map(|part| part.cause.as_deref())
        .filter(|cause| !cause.trim().is_empty())
    {
        let cause = engine_message_for(cause, lang);
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    }
    if causes.is_empty() {
        fill(
            t_for("这次的结果在这些部分不如现有笔记，现有笔记已保留：{}", lang),
            &[&names],
        )
    } else {
        fill(
            t_for(
                "这次的结果在这些部分不如现有笔记，现有笔记已保留：{}（{}）",
                lang,
            ),
            &[&names, &causes.join(causes_separator)],
        )
    }
}

impl std::error::Error for NoteKept {}

/// A replacement of a note stopped and cannot be finished on its own: no note is
/// in place, and what the swap folder holds does not say which copy is the note.
/// Nothing was changed. `swap` is `<library>/.course2md/swap/<note>`.
#[derive(Debug)]
pub struct RecoveryBlocked {
    pub swap: std::path::PathBuf,
}

impl RecoveryBlocked {
    /// The note this swap folder belongs to: `<library>/<note>`.
    pub fn note(swap: &std::path::Path) -> Option<std::path::PathBuf> {
        let name = swap.file_name()?;
        Some(swap.parent()?.parent()?.parent()?.join(name))
    }
}

impl std::fmt::Display for RecoveryBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&recovery_blocked_message(
            &self.swap,
            c2m_i18n::i18n::lang(),
        ))
    }
}

impl std::error::Error for RecoveryBlocked {}

/// What to do about a note whose replacement cannot be finished, in `lang`, with
/// the full paths.
pub fn recovery_blocked_message(swap: &std::path::Path, lang: c2m_i18n::i18n::Lang) -> String {
    let note = RecoveryBlocked::note(swap).unwrap_or_else(|| swap.to_path_buf());
    let folder = note
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default();
    let name = note
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    fill(
        c2m_i18n::i18n::t_for(
            "这份笔记上次被替换时中断，无法自动恢复，所有文件都已原样保留。请打开 {}，把其中存有你笔记的 old- 开头的文件夹移到 {} 并改名为 {}，再删除那里的 live 文件，然后重试。",
            lang,
        ),
        &[&swap.display().to_string(), &folder, &name],
    )
}

/// A catalog template with its `{}` filled in order.
fn fill(template: &str, values: &[&str]) -> String {
    let mut out = String::new();
    let mut values = values.iter();
    let mut pieces = template.split("{}");
    out.push_str(pieces.next().unwrap_or_default());
    for piece in pieces {
        out.push_str(values.next().copied().unwrap_or_default());
        out.push_str(piece);
    }
    out
}

/// Another task is working on the note; this one stopped before any work.
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
