//! Fixed task inputs. The transport may contain credentials; persisted identities never do.

use crate::tr;
use crate::{config::PipelineConfig, settings::ConfigFile, timeline::TranscriptEvent};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub use crate::digest::{digest, file_digest, file_digest_cancellable, hex};
pub use crate::job::Job;

pub const REQUEST_SCHEMA: u32 = 1;
pub(crate) const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", try_from = "OperationWire")]
pub enum Operation {
    #[default]
    Generate,
    /// Redoes `components` of the note in `note_dir` and replaces it in place.
    Reprocess {
        /// The note folder; the same as the request's `course_dir`.
        note_dir: PathBuf,
        /// The task whose note this repair was planned from, as its manifest
        /// recorded it. A note since replaced by another task is not repaired.
        base_task_id: String,
        components: Vec<Component>,
        prior_work_dir: Option<PathBuf>,
    },
}

/// What a stored plan holds. Plans saved before notes were flat name the base
/// version folder `<note>/versions/<id>`, whose id was the task id of its task.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OperationWire {
    Generate,
    Reprocess {
        #[serde(default)]
        note_dir: Option<PathBuf>,
        #[serde(default)]
        base_task_id: Option<String>,
        #[serde(default)]
        base_version_dir: Option<PathBuf>,
        components: Vec<Component>,
        #[serde(default)]
        prior_work_dir: Option<PathBuf>,
    },
}

impl TryFrom<OperationWire> for Operation {
    type Error = String;

    fn try_from(wire: OperationWire) -> std::result::Result<Self, String> {
        let OperationWire::Reprocess {
            note_dir,
            base_task_id,
            base_version_dir,
            components,
            prior_work_dir,
        } = wire
        else {
            return Ok(Self::Generate);
        };
        let legacy = base_version_dir.as_deref().and_then(|version| {
            let id = version.file_name()?.to_str()?.to_owned();
            let versions = version.parent()?;
            let note = versions.parent()?.to_path_buf();
            (versions.file_name()? == "versions").then_some((note, id))
        });
        let (note_dir, base_task_id) = match (note_dir, base_task_id, legacy) {
            (Some(note_dir), Some(base_task_id), _) => (note_dir, base_task_id),
            (_, _, Some(legacy)) => legacy,
            _ => return Err("reprocess needs note_dir and base_task_id".into()),
        };
        Ok(Self::Reprocess {
            note_dir,
            base_task_id,
            components,
            prior_work_dir,
        })
    }
}

/// A version component that a reprocess task can redo. The wire format stays the
/// lowercase name so stored desktop plans keep loading.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    strum::IntoStaticStr,
    strum::EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Component {
    Screenshots,
    Proofreading,
    /// The bilingual translation of the proofread text. Its name is the request
    /// ledger's purpose, so a receipt names the component it belongs to.
    Translation,
    Summary,
    Exports,
}

impl Component {
    pub const ALL: [Self; 5] = [
        Self::Screenshots,
        Self::Proofreading,
        Self::Translation,
        Self::Summary,
        Self::Exports,
    ];

    /// A component made by AI requests, which the request ledger names by purpose.
    pub fn is_ai(self) -> bool {
        matches!(self, Self::Proofreading | Self::Translation | Self::Summary)
    }

    pub fn as_str(self) -> &'static str {
        self.into()
    }

    pub fn parse(name: &str) -> Option<Self> {
        name.parse().ok()
    }
}

/// Sent once over stdin. Do not derive Debug: config contains decrypted credentials.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    #[serde(default)]
    pub operation: Operation,
    pub task_id: String,
    pub course_id: String,
    /// The id a task's identity records as `version_id`: always its task id since
    /// notes are flat. Only a task bound before then may carry another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    pub source: String,
    pub source_id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub subtitle: Option<PathBuf>,
    #[serde(default)]
    pub subtitle_events: Option<Vec<TranscriptEvent>>,
    pub config: ConfigFile,
    #[serde(default)]
    pub allow_unauthenticated_asr: bool,
    pub work_dir: PathBuf,
    /// The note folder.
    pub course_dir: PathBuf,
    /// Reserved for the cooperative pause/cancel protocol; never sent as argv.
    #[serde(default)]
    pub control_path: Option<PathBuf>,
    #[serde(default)]
    pub service_versions: BTreeMap<String, String>,
}

impl Request {
    /// Where this task publishes its note.
    pub fn target(&self) -> crate::artifact::Target {
        crate::artifact::Target {
            task_id: self.task_id.clone(),
            course_id: self.course_id.clone(),
            source_id: self.source_id.clone(),
            course_dir: self.course_dir.clone(),
            replaces: match &self.operation {
                Operation::Generate => None,
                Operation::Reprocess { base_task_id, .. } => Some(base_task_id.clone()),
            },
        }
    }

    /// The id the identity records as `version_id`.
    fn identity_version_id(&self) -> &str {
        self.version_id.as_deref().unwrap_or(&self.task_id)
    }

    pub fn read(reader: impl Read) -> Result<Self> {
        let mut data = Vec::new();
        reader.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut data)?;
        anyhow::ensure!(
            data.len() as u64 <= MAX_REQUEST_BYTES,
            tr!(
                "任务参数过大（上限 {} MiB） / Task input exceeds {} MiB",
                MAX_REQUEST_BYTES / (1024 * 1024),
                MAX_REQUEST_BYTES / (1024 * 1024)
            )
        );
        // serde's error can include unknown field names. Do not echo attacker-supplied
        // values or accidentally repeat a secret from a malformed JSON document.
        let request: Self = serde_json::from_slice(&data).map_err(|e| {
            crate::invalid::invalid_input(anyhow::anyhow!(
                "{}",
                tr!(
                    "无法读取任务参数（第 {} 行，第 {} 列）/ Invalid task JSON",
                    e.line(),
                    e.column()
                )
            ))
        })?;
        request.validate().map_err(crate::invalid::invalid_input)?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema == REQUEST_SCHEMA,
            tr!("不支持此任务格式 / Unsupported task schema")
        );
        for (name, value) in [
            ("task_id", self.task_id.as_str()),
            ("course_id", &self.course_id),
            ("version_id", self.identity_version_id()),
        ] {
            anyhow::ensure!(
                valid_id(value),
                "任务中的 {name} 无效 / Invalid task identifier"
            );
        }
        anyhow::ensure!(
            !self.source.trim().is_empty() && !self.source_id.trim().is_empty(),
            tr!("任务缺少视频来源 / Task source is missing")
        );
        anyhow::ensure!(
            !self.title.trim().is_empty(),
            tr!("任务缺少已确认的笔记名称 / Confirmed title is missing")
        );
        anyhow::ensure!(
            self.duration.is_finite() && self.duration >= 0.0,
            tr!("视频时长无效 / Invalid duration")
        );
        anyhow::ensure!(
            self.work_dir.is_absolute() && self.course_dir.is_absolute(),
            tr!("任务与笔记位置必须是绝对路径 / Task paths must be absolute")
        );
        anyhow::ensure!(
            !self.work_dir.starts_with(&self.course_dir)
                && !self.course_dir.starts_with(&self.work_dir),
            tr!(
                "工作目录不能位于笔记文件夹内 / The work directory must be outside the note folder"
            )
        );
        anyhow::ensure!(
            self.subtitle.is_none() || self.subtitle_events.is_none(),
            tr!("任务只能指定一种已选字幕内容 / Specify subtitle file or parsed events, not both")
        );
        if let Some(path) = &self.subtitle {
            anyhow::ensure!(
                path.is_absolute(),
                tr!("所选字幕必须使用绝对路径 / Subtitle path must be absolute")
            );
        }
        if let Some(events) = &self.subtitle_events {
            validate_events(events)?;
        }
        if let Operation::Reprocess {
            note_dir,
            base_task_id,
            components,
            prior_work_dir,
        } = &self.operation
        {
            anyhow::ensure!(
                note_dir == &self.course_dir && valid_id(base_task_id),
                tr!("补做的笔记与任务不一致 / The repaired note does not match the task")
            );
            anyhow::ensure!(
                !components.is_empty(),
                tr!("补做内容无效 / Invalid requested components")
            );
            anyhow::ensure!(
                prior_work_dir
                    .as_ref()
                    .is_none_or(|p| p.is_absolute() && p != &self.work_dir),
                tr!("原任务进度位置无效 / Invalid prior work directory")
            );
        }
        if let Some(path) = &self.control_path {
            anyhow::ensure!(
                path.is_absolute() && path.starts_with(&self.work_dir),
                tr!("任务控制文件必须位于工作目录 / Invalid control file location")
            );
        }
        Ok(())
    }

    /// Expects a request that passed [`Request::validate`] (as [`Request::read`] ensures).
    pub fn resolve(&self) -> Result<PipelineConfig> {
        self.resolve_with(&Default::default())
    }

    /// [`Self::resolve`] for settings a person typed on the command line: a speech model
    /// they named is the model, as `options::resolve` keeps one given as an option, and a
    /// retired one is refused, not replaced.
    pub(crate) fn resolve_typed(&self) -> Result<PipelineConfig> {
        self.resolve_with(&crate::options::RunOpts {
            asr_model: self.config.defaults.asr_model.clone(),
            ..Default::default()
        })
    }

    fn resolve_with(&self, opts: &crate::options::RunOpts) -> Result<PipelineConfig> {
        let mut cfg = crate::options::resolve(self.source.clone(), opts, &self.config)?;
        // Recovery is a task invariant, not a configurable preference. Where the
        // task works comes from `work_dir` and `course_dir`, not the settings.
        cfg.resume = true;
        // Never enter Apple's model-choice prompt.
        if cfg
            .asr_model
            .as_deref()
            .is_none_or(|model| model.trim().is_empty())
        {
            cfg.asr_model = match cfg.provider {
                crate::config::AsrProvider::Coreml
                | crate::config::AsrProvider::Cpu
                | crate::config::AsrProvider::Gpu => Some(crate::config::DEFAULT_ASR_MODEL.into()),
                crate::config::AsrProvider::Npu => Some(crate::npu::resolve_npu_model(None)),
                crate::config::AsrProvider::Api => None,
            };
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Identity includes effective parameters and selected content, excluding secret values.
    pub fn binding(&self, cfg: &PipelineConfig) -> Result<serde_json::Value> {
        // Storage moves are resolved locations, not a change in the processing plan.
        let mut config = cfg.redacted_for_identity(Path::new(""));
        config["out_root"] = serde_json::Value::String(String::new());
        // A missing original file must not change the identity of an AI/export-only
        // continuation. Local content identity is independent of current file presence.
        if Path::new(&self.source).is_absolute() || self.source_id.starts_with("local:") {
            config["url"] = serde_json::Value::String(self.source_id.clone());
        }
        let generating = matches!(self.operation, Operation::Generate);
        let subtitle_digest = if generating {
            self.subtitle
                .as_ref()
                .map(|path| file_digest(path))
                .transpose()?
        } else {
            None
        };
        let needs_source = generating
            || matches!(&self.operation, Operation::Reprocess { components, .. }
                if components.contains(&Component::Screenshots));
        let local_digest = if needs_source && Path::new(&self.source).is_file() {
            Some(file_digest(Path::new(&self.source))?)
        } else {
            None
        };
        if let (Some(expected), Some(actual)) = (
            self.source_id.strip_prefix("local:sha256:"),
            local_digest.as_deref(),
        ) {
            anyhow::ensure!(
                expected == actual,
                tr!(
                    "视频文件已在读取后发生变化，请重新读取来源；原任务进度已保留 / Source content changed after confirmation; reread the source"
                )
            );
        }
        // The keys and values tasks bound before notes were flat recorded: a base
        // version's id was its task's id, and a repair's source is the note's.
        let operation = match &self.operation {
            Operation::Generate => serde_json::json!({"kind":"generate"}),
            Operation::Reprocess {
                base_task_id,
                components,
                ..
            } => {
                serde_json::json!({"kind":"reprocess", "source_id":self.source_id, "base_version_id":base_task_id,"components":components})
            }
        };
        Ok(serde_json::json!({
            "operation":operation,
            "schema": self.schema, "task_id": self.task_id, "course_id": self.course_id,
            "version_id": self.identity_version_id(), "source_id": self.source_id, "title": self.title,
            "author": self.author, "duration": self.duration, "config": config,
            "subtitle_digest": subtitle_digest,
            "subtitle_events": self.subtitle_events, "local_digest": local_digest,
            "service_versions": self.service_versions, "allow_unauthenticated_asr":self.allow_unauthenticated_asr,
        }))
    }
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && value != "."
        && value != ".."
        // IDs name directories: Windows drops a trailing dot and maps device
        // names (`con`, `nul.json`) to devices.
        && !value.ends_with('.')
        && !crate::config::windows_reserved_name(value)
}

pub(crate) fn validate_events(events: &[TranscriptEvent]) -> Result<()> {
    anyhow::ensure!(
        events.iter().all(|e| e.start.is_finite()
            && e.end.is_finite()
            && e.start >= 0.0
            && e.end >= e.start),
        tr!("字幕时间无效 / Invalid subtitle timestamps")
    );
    anyhow::ensure!(
        events.iter().any(|e| !e.text.trim().is_empty()),
        tr!("所选字幕没有可读文字 / Selected subtitles contain no readable text")
    );
    Ok(())
}

/// Holds a task's work directory: the process lock and the task's job.
pub(crate) struct WorkDir {
    job: Job,
    _lock: crate::runtime::FileLock,
}

impl WorkDir {
    pub fn job(&self) -> &Job {
        &self.job
    }
}

/// Enters a task's work directory in one order for every entry point: create
/// it, take the task lock, check the identity it was bound to, then open the
/// request ledger.
pub(crate) fn enter_work_dir(
    work_dir: &Path,
    binding: &serde_json::Value,
    former: Option<&serde_json::Value>,
    source: crate::dispatch::ControlSource,
) -> Result<WorkDir> {
    std::fs::create_dir_all(work_dir)?;
    // The desktop briefly takes this lock to learn whether a worker from an earlier
    // session still runs here; a short wait rides that out, while another worker that
    // really holds the directory is still refused.
    let path = work_dir.join(".task.lock");
    let lock = crate::runtime::lock_file_within(&path, std::time::Duration::from_secs(2))
        .with_context(|| {
            crate::tr!("目录正被另一任务使用：{}。请等待任务结束", path.display()).to_string()
        })?;
    bind_work_dir(work_dir, binding, former)?;
    let control = crate::dispatch::TaskControl::open(work_dir, source)?;
    Ok(WorkDir {
        job: Job {
            dir: work_dir.to_path_buf(),
            control,
        },
        _lock: lock,
    })
}

/// A mismatch is evidence that this is a different task; preserve both old work and error.
/// `former` is the identity the same task bound before, which takes the current one.
pub(crate) fn bind_work_dir(
    work_dir: &Path,
    binding: &serde_json::Value,
    former: Option<&serde_json::Value>,
) -> Result<()> {
    std::fs::create_dir_all(work_dir)?;
    let path = work_dir.join("task-identity.json");
    if path.exists() {
        let old: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?).context(
            tr!("任务进度身份损坏，原文件已保留 / Task identity is damaged"),
        )?;
        if former == Some(&old) {
            return crate::fsio::atomic_write(&path, &serde_json::to_vec_pretty(binding)?);
        }
        anyhow::ensure!(
            old == *binding,
            tr!(
                "任务来源或参数与已保存进度不一致，原进度已保留 / Task inputs changed; create a new task instead of reusing this work directory"
            )
        );
    } else {
        anyhow::ensure!(
            !work_dir.join("asr.jsonl").exists() && !work_dir.join("media.mp4").exists(),
            tr!(
                "此文件夹里有无法确认来源的旧进度，请换一个位置 / This folder contains earlier progress of unknown origin; choose another location"
            )
        );
        crate::fsio::atomic_write(&path, &serde_json::to_vec_pretty(binding)?)?;
    }
    Ok(())
}

/// Runs a request obtained from [`Request::read`], which validated it. The task
/// stops as on quit once `parent`, recorded when the command started, is gone.
pub async fn run(request: Request, parent: crate::dispatch::ParentProcess) -> Result<()> {
    let cfg = request.resolve()?;
    // No settings::load, environment credential resolution or wizard on this path.
    crate::pipeline::run_task(&request, &cfg, crate::pipeline::Caller::Task(parent)).await
}

/// Call only while all writers are paused and the library copy has been verified.
/// New bindings omit resolved output locations. This migrates older bindings by changing
/// only owned work/output fields, and verifies each copied identity against the old one.
pub fn relocate_work_bindings(root_old: &Path, root_new: &Path) -> Result<usize> {
    anyhow::ensure!(
        root_old.is_absolute() && root_new.is_absolute() && root_old != root_new,
        tr!("笔记库迁移位置无效 / Invalid relocation roots")
    );
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(root_new).min_depth(1) {
        let entry = entry?;
        anyhow::ensure!(
            entry.depth() <= 64,
            tr!("笔记库目录层级过深 / Library directory depth exceeds the safe limit")
        );
        // Symbolic links are neither followed nor rewritten.
        if entry.file_type().is_file() && entry.file_name() == "task-identity.json" {
            paths.push(entry.into_path());
        }
    }
    let mut changes = Vec::new();
    for path in paths {
        let old_path = root_old.join(path.strip_prefix(root_new)?);
        let old: serde_json::Value = serde_json::from_slice(&std::fs::read(&old_path).context(
            tr!("缺少可核验的原任务身份 / Original task identity is missing"),
        )?)?;
        let current: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let mut relocated = old.clone();
        for pointer in [
            "/work_dir",
            "/course_dir",
            "/config/out_dir",
            "/config/out_root",
            "/config/model_dir",
        ] {
            if let Some(value) = relocated.pointer_mut(pointer)
                && let Some(location) = value.as_str().filter(|s| !s.is_empty())
                && let Ok(relative) = Path::new(location).strip_prefix(root_old)
            {
                *value = serde_json::Value::String(root_new.join(relative).display().to_string());
            }
        }
        anyhow::ensure!(
            current == old || current == relocated,
            tr!(
                "迁移副本中的任务身份已经变化，未覆盖 / Copied task identity changed; not overwritten"
            )
        );
        if relocated != current {
            changes.push((path, relocated));
        }
    }
    let count = changes.len();
    for (path, value) in changes {
        crate::fsio::atomic_write(&path, &serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(count)
}

// The snapshots hold the absolute Unix paths a saved task records; Windows
// builds the same JSON from Windows paths, so they are checked on Unix only.
#[cfg(all(test, unix))]
mod test_identity_snapshot;

#[cfg(test)]
mod tests {
    use super::*;

    /// Receipts and manifests on disk hold this exact form.
    #[test]
    fn digests_keep_their_lowercase_hex_form() {
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn ids_are_valid_windows_directory_names() {
        for id in ["con", "NUL", "aux.json", "abc.", "lpt1"] {
            assert!(!valid_id(id), "{id}");
        }
        assert!(valid_id("task-0123456789abcdef"));
        assert!(valid_id("a.b"));
    }

    #[test]
    fn immutable_work_rejects_changed_inputs_without_destroying_progress() {
        let dir = tempfile::tempdir().unwrap();
        bind_work_dir(dir.path(), &serde_json::json!({"title":"A"}), None).unwrap();
        std::fs::write(dir.path().join("asr.jsonl"), "saved result").unwrap();
        bind_work_dir(dir.path(), &serde_json::json!({"title":"A"}), None).unwrap();
        assert!(bind_work_dir(dir.path(), &serde_json::json!({"title":"B"}), None).is_err());
        // The identity the same task bound before gives way to the current one.
        let former = serde_json::json!({"title":"A"});
        bind_work_dir(dir.path(), &serde_json::json!({"title":"C"}), Some(&former)).unwrap();
        bind_work_dir(dir.path(), &serde_json::json!({"title":"C"}), Some(&former)).unwrap();
        assert!(bind_work_dir(dir.path(), &serde_json::json!({"title":"A"}), None).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("asr.jsonl")).unwrap(),
            "saved result"
        );
    }

    #[test]
    fn a_plan_saved_with_its_base_version_folder_still_loads() {
        let legacy: Operation = serde_json::from_value(serde_json::json!({
            "kind": "reprocess",
            "base_version_dir": "/library/course/versions/task-0",
            "components": ["summary"],
            "prior_work_dir": "/library/.course2md/work/task-0",
        }))
        .unwrap();
        let flat = Operation::Reprocess {
            note_dir: "/library/course".into(),
            base_task_id: "task-0".into(),
            components: vec![Component::Summary],
            prior_work_dir: Some("/library/.course2md/work/task-0".into()),
        };
        assert_eq!(legacy, flat);
        let saved = serde_json::to_value(&flat).unwrap();
        assert!(saved.get("base_version_dir").is_none());
        assert_eq!(serde_json::from_value::<Operation>(saved).unwrap(), flat);
        assert_eq!(
            serde_json::from_value::<Operation>(serde_json::json!({"kind":"generate"})).unwrap(),
            Operation::Generate
        );
        assert!(
            serde_json::from_value::<Operation>(
                serde_json::json!({"kind":"reprocess","components":["summary"]})
            )
            .is_err()
        );
    }

    /// A repair's identity keeps the keys and values a task bound before notes
    /// were flat, so its saved receipts still belong to it.
    #[test]
    fn a_repair_identity_records_its_task_and_base_as_versions() {
        // An absolute path on every platform (`/library` is not one on Windows).
        let root = std::env::temp_dir().join("library");
        let at = |rel: &str| root.join(rel);
        let mut request = Request {
            schema: REQUEST_SCHEMA,
            operation: Operation::Reprocess {
                note_dir: at("course"),
                base_task_id: "task-0".into(),
                components: vec![Component::Summary],
                prior_work_dir: None,
            },
            task_id: "repair-1".into(),
            course_id: "course".into(),
            version_id: None,
            source: "https://example.invalid/video".into(),
            source_id: "example:video".into(),
            title: "Lecture".into(),
            author: String::new(),
            duration: 1.,
            subtitle: None,
            subtitle_events: None,
            config: ConfigFile::default(),
            allow_unauthenticated_asr: false,
            work_dir: at(".course2md/work/repair-1"),
            course_dir: at("course"),
            control_path: None,
            service_versions: Default::default(),
        };
        request.validate().unwrap();
        let cfg = request.resolve().unwrap();
        let binding = request.binding(&cfg).unwrap();
        assert_eq!(binding["version_id"], "repair-1");
        assert_eq!(
            binding["operation"],
            serde_json::json!({"kind":"reprocess","source_id":"example:video","base_version_id":"task-0","components":["summary"]})
        );
        request.version_id = Some("repair-1".into());
        assert_eq!(request.binding(&cfg).unwrap(), binding);
        // The work directory and the note never overlap: the note is replaced whole.
        request.work_dir = at("course/.work/repair-1");
        assert!(request.validate().is_err());
        request.work_dir = at(".course2md/work/repair-1");
        request.course_dir = at("other");
        assert!(
            request.validate().is_err(),
            "the repaired note is the task's"
        );
    }

    /// What a command line chose comes back from the settings its request carries, a
    /// speech model it named included.
    #[test]
    fn a_command_line_request_resolves_to_what_was_typed() {
        use crate::{config::AsrProvider, options::RunOpts};
        let root = std::env::temp_dir().join("library");
        let mut file = ConfigFile::default();
        file.defaults.similarity = Some(0.7);
        file.llm.enabled = true;
        file.llm.api_key = "key".into();
        let typed = [
            RunOpts {
                out: Some("relative/out".into()),
                roi: Some("10.5%,0%-90%,87.25%".into()),
                similarity: Some(0.1 + 0.2),
                provider: Some(AsrProvider::Api),
                asr_api_key: Some("asr".into()),
                formats: Some(vec![crate::config::OutputFormat::Json]),
                no_keep_video: true,
                translate: Some("en".into()),
                gpu_layers: Some(3),
                ..Default::default()
            },
            // Refused when speech is needed, as the command line always refused it.
            RunOpts {
                provider: Some(AsrProvider::Npu),
                asr_model: Some("qwen3-1.7b".into()),
                ..Default::default()
            },
            RunOpts {
                provider: Some(AsrProvider::Npu),
                asr_model: Some("dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO".into()),
                ..Default::default()
            },
            RunOpts {
                provider: Some(AsrProvider::Cpu),
                asr_model: Some("qwen3-0.6b".into()),
                ..Default::default()
            },
        ];
        for opts in typed {
            let cfg = crate::options::resolve("video.mp4".into(), &opts, &file).unwrap();
            let request = Request {
                schema: REQUEST_SCHEMA,
                operation: Operation::Generate,
                task_id: "task".into(),
                course_id: "course".into(),
                version_id: None,
                source: "video.mp4".into(),
                source_id: "local:sha256:0".into(),
                title: "Lecture".into(),
                author: String::new(),
                duration: 0.,
                subtitle: None,
                subtitle_events: None,
                config: crate::options::file_for(&cfg),
                allow_unauthenticated_asr: false,
                work_dir: root.join(".course2md/work/task"),
                course_dir: root.join("course"),
                control_path: None,
                service_versions: Default::default(),
            };
            request.validate().unwrap();
            let mut sent = request.resolve_typed().unwrap();
            // A task always resumes; a fresh command line already chose a new task id.
            sent.resume = cfg.resume;
            assert_eq!(
                serde_json::to_value(&sent).unwrap(),
                serde_json::to_value(&cfg).unwrap()
            );
            if sent.provider == AsrProvider::Npu && sent.asr_model.as_deref() == Some("qwen3-1.7b")
            {
                let error = sent.validate_asr_with_auth(true, true).unwrap_err();
                assert!(format!("{error:#}").contains("Qwen3"), "{error:#}");
            }
        }
    }

    #[test]
    fn malformed_request_does_not_echo_secret() {
        let error = Request::read(br#"{"api_key":"private-test-key"}"#.as_slice())
            .err()
            .unwrap();
        assert!(!format!("{error:#}").contains("private-test-key"));
        assert!(!valid_id("../elsewhere"));
    }
}
