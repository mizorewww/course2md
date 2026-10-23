//! Fixed task inputs. The transport may contain credentials; persisted identities never do.

use crate::{config::PipelineConfig, settings::ConfigFile, timeline::TranscriptEvent};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub const REQUEST_SCHEMA: u32 = 1;
pub const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    #[default]
    Generate,
    Reprocess {
        base_version_dir: PathBuf,
        components: Vec<Component>,
        prior_work_dir: Option<PathBuf>,
    },
}

/// A version component that a reprocess task can redo. The wire format stays the
/// lowercase name so stored desktop plans keep loading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Component {
    Screenshots,
    Proofreading,
    Summary,
    Exports,
}

impl Component {
    pub const ALL: [Self; 4] = [
        Self::Screenshots,
        Self::Proofreading,
        Self::Summary,
        Self::Exports,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screenshots => "screenshots",
            Self::Proofreading => "proofreading",
            Self::Summary => "summary",
            Self::Exports => "exports",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
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
    pub version_id: String,
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
    pub course_dir: PathBuf,
    /// Reserved for the cooperative pause/cancel protocol; never sent as argv.
    #[serde(default)]
    pub control_path: Option<PathBuf>,
    #[serde(default)]
    pub service_versions: BTreeMap<String, String>,
}

impl Request {
    pub fn read(reader: impl Read) -> Result<Self> {
        let mut data = Vec::new();
        reader.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut data)?;
        anyhow::ensure!(
            data.len() as u64 <= MAX_REQUEST_BYTES,
            crate::tr!(
                "任务参数过大（上限 {} MiB） / Task input exceeds {} MiB",
                MAX_REQUEST_BYTES / (1024 * 1024),
                MAX_REQUEST_BYTES / (1024 * 1024)
            )
        );
        // serde's error can include unknown field names. Do not echo attacker-supplied
        // values or accidentally repeat a secret from a malformed JSON document.
        let request: Self = serde_json::from_slice(&data).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                crate::tr!(
                    "无法读取任务参数（第 {} 行，第 {} 列）/ Invalid task JSON",
                    e.line(),
                    e.column()
                )
            )
        })?;
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema == REQUEST_SCHEMA,
            crate::tr!("不支持此任务格式 / Unsupported task schema")
        );
        for (name, value) in [
            ("task_id", &self.task_id),
            ("course_id", &self.course_id),
            ("version_id", &self.version_id),
        ] {
            anyhow::ensure!(
                valid_id(value),
                "任务中的 {name} 无效 / Invalid task identifier"
            );
        }
        anyhow::ensure!(
            !self.source.trim().is_empty() && !self.source_id.trim().is_empty(),
            crate::tr!("任务缺少视频来源 / Task source is missing")
        );
        anyhow::ensure!(
            !self.title.trim().is_empty(),
            crate::tr!("任务缺少已确认的笔记名称 / Confirmed title is missing")
        );
        anyhow::ensure!(
            self.duration.is_finite() && self.duration >= 0.0,
            crate::tr!("视频时长无效 / Invalid duration")
        );
        anyhow::ensure!(
            self.work_dir.is_absolute() && self.course_dir.is_absolute(),
            crate::tr!("任务与笔记位置必须是绝对路径 / Task paths must be absolute")
        );
        anyhow::ensure!(
            self.work_dir != self.course_dir
                && !self.work_dir.starts_with(self.course_dir.join("versions")),
            crate::tr!("工作目录不能位于已发布版本内 / Work directory overlaps published versions")
        );
        anyhow::ensure!(
            self.subtitle.is_none() || self.subtitle_events.is_none(),
            crate::tr!(
                "任务只能指定一种已选字幕内容 / Specify subtitle file or parsed events, not both"
            )
        );
        if let Some(path) = &self.subtitle {
            anyhow::ensure!(
                path.is_absolute(),
                crate::tr!("所选字幕必须使用绝对路径 / Subtitle path must be absolute")
            );
        }
        if let Some(events) = &self.subtitle_events {
            validate_events(events)?;
        }
        if let Operation::Reprocess {
            base_version_dir,
            components,
            prior_work_dir,
        } = &self.operation
        {
            anyhow::ensure!(
                base_version_dir.is_absolute(),
                crate::tr!("原笔记版本需要绝对路径 / Base version requires an absolute path")
            );
            anyhow::ensure!(
                !components.is_empty(),
                crate::tr!("补做内容无效 / Invalid requested components")
            );
            anyhow::ensure!(
                prior_work_dir
                    .as_ref()
                    .is_none_or(|p| p.is_absolute() && p != &self.work_dir),
                crate::tr!("原任务进度位置无效 / Invalid prior work directory")
            );
        }
        if let Some(path) = &self.control_path {
            anyhow::ensure!(
                path.is_absolute() && path.starts_with(&self.work_dir),
                crate::tr!("任务控制文件必须位于工作目录 / Invalid control file location")
            );
        }
        Ok(())
    }

    /// Expects a request that passed [`Request::validate`] (as [`Request::read`] ensures).
    pub fn resolve(&self) -> Result<PipelineConfig> {
        let mut cfg =
            crate::options::resolve(self.source.clone(), &Default::default(), &self.config)?;
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
                crate::tr!(
                    "视频文件已在读取后发生变化，请重新读取来源；原任务进度已保留 / Source content changed after confirmation; reread the source"
                )
            );
        }
        let operation = match &self.operation {
            Operation::Generate => serde_json::json!({"kind":"generate"}),
            Operation::Reprocess {
                base_version_dir,
                components,
                ..
            } => {
                let manifest =
                    crate::artifact::read_manifest(&base_version_dir.join("manifest.json"))?;
                serde_json::json!({"kind":"reprocess", "source_id":manifest.source_id, "base_version_id":manifest.version_id,"components":components})
            }
        };
        Ok(serde_json::json!({
            "operation":operation,
            "schema": self.schema, "task_id": self.task_id, "course_id": self.course_id,
            "version_id": self.version_id, "source_id": self.source_id, "title": self.title,
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
}

pub fn validate_events(events: &[TranscriptEvent]) -> Result<()> {
    anyhow::ensure!(
        events.iter().all(|e| e.start.is_finite()
            && e.end.is_finite()
            && e.start >= 0.0
            && e.end >= e.start),
        crate::tr!("字幕时间无效 / Invalid subtitle timestamps")
    );
    anyhow::ensure!(
        events.iter().any(|e| !e.text.trim().is_empty()),
        crate::tr!("所选字幕没有可读文字 / Selected subtitles contain no readable text")
    );
    Ok(())
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub fn file_digest(path: &Path) -> Result<String> {
    file_digest_cancellable(path, &std::sync::atomic::AtomicBool::new(false))
}

/// Streaming SHA-256 that stops at the next 1 MiB block once `cancel` is set.
pub fn file_digest_cancellable(
    path: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::Ordering;
    let mut file = std::fs::File::open(path).with_context(|| {
        crate::tr!(
            "无法读取 {} / Cannot read {}",
            path.display(),
            path.display()
        )
    })?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        anyhow::ensure!(
            !cancel.load(Ordering::Relaxed),
            crate::tr!("已取消读取文件 / File reading cancelled")
        );
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// A running task's layout and control, kept apart from its settings: the
/// resolved [`PipelineConfig`] never changes, while each entry point decides
/// where the task works.
#[derive(Clone)]
pub struct Job {
    /// The work directory the task owns: checkpoints, receipts, media.
    pub dir: PathBuf,
    /// The request ledger and control file every safe boundary checks.
    pub control: crate::dispatch::TaskControl,
}

impl Job {
    /// A job without a task context: nothing pauses it and requests are not recorded.
    pub fn detached(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            control: Default::default(),
        }
    }
    pub fn media_path(&self) -> PathBuf {
        self.dir.join("media.mp4")
    }
    pub fn audio_path(&self) -> PathBuf {
        self.dir.join("audio.wav")
    }
    pub fn frames_dir(&self) -> PathBuf {
        self.dir.join("frames")
    }
    pub fn timeline_path(&self) -> PathBuf {
        self.dir.join("timeline.jsonl")
    }
    pub fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }
}

/// Holds a task's work directory: the process lock and the task's job.
pub struct WorkDir {
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
pub fn enter_work_dir(
    work_dir: &Path,
    binding: &serde_json::Value,
    source: crate::dispatch::ControlSource,
) -> Result<WorkDir> {
    std::fs::create_dir_all(work_dir)?;
    let lock = crate::runtime::lock_file(&work_dir.join(".task.lock"))?;
    bind_work_dir(work_dir, binding)?;
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
pub fn bind_work_dir(work_dir: &Path, binding: &serde_json::Value) -> Result<()> {
    std::fs::create_dir_all(work_dir)?;
    let path = work_dir.join("task-identity.json");
    if path.exists() {
        let old: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?).context(
            crate::tr!("任务进度身份损坏，原文件已保留 / Task identity is damaged"),
        )?;
        anyhow::ensure!(
            old == *binding,
            crate::tr!(
                "任务来源或参数与已保存进度不一致，原进度已保留 / Task inputs changed; create a new task instead of reusing this work directory"
            )
        );
    } else {
        anyhow::ensure!(
            !work_dir.join("asr.jsonl").exists() && !work_dir.join("media.mp4").exists(),
            crate::tr!(
                "此文件夹里有无法确认来源的旧进度，请换一个位置 / This folder contains earlier progress of unknown origin; choose another location"
            )
        );
        crate::checkpoint::atomic_write(&path, &serde_json::to_vec_pretty(binding)?)?;
    }
    Ok(())
}

/// Runs a request obtained from [`Request::read`], which validated it. The task
/// stops as on quit once `parent`, recorded when the command started, is gone.
pub async fn run(request: Request, parent: crate::dispatch::ParentProcess) -> Result<()> {
    let cfg = request.resolve()?;
    // No settings::load, environment credential resolution or wizard on this path.
    crate::pipeline::run_task(&request, &cfg, parent).await
}

/// Call only while all writers are paused and the library copy has been verified.
/// New bindings omit resolved output locations. This migrates older bindings by changing
/// only owned work/output fields, and verifies each copied identity against the old one.
pub fn relocate_work_bindings(root_old: &Path, root_new: &Path) -> Result<usize> {
    anyhow::ensure!(
        root_old.is_absolute() && root_new.is_absolute() && root_old != root_new,
        crate::tr!("笔记库迁移位置无效 / Invalid relocation roots")
    );
    fn visit(root: &Path, current: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                continue;
            }
            let path = entry.path();
            if path.strip_prefix(root)?.components().count() > 64 {
                anyhow::bail!(crate::tr!(
                    "笔记库目录层级过深 / Library directory depth exceeds the safe limit"
                ));
            }
            if entry.file_type()?.is_dir() {
                visit(root, &path, paths)?;
            } else if entry.file_name() == "task-identity.json" {
                paths.push(path);
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    visit(root_new, root_new, &mut paths)?;
    let mut changes = Vec::new();
    for path in paths {
        let old_path = root_old.join(path.strip_prefix(root_new)?);
        let old: serde_json::Value = serde_json::from_slice(&std::fs::read(&old_path).context(
            crate::tr!("缺少可核验的原任务身份 / Original task identity is missing"),
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
            crate::tr!(
                "迁移副本中的任务身份已经变化，未覆盖 / Copied task identity changed; not overwritten"
            )
        );
        if relocated != current {
            changes.push((path, relocated));
        }
    }
    let count = changes.len();
    for (path, value) in changes {
        crate::checkpoint::atomic_write(&path, &serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(count)
}

#[cfg(test)]
mod identity_snapshot;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immutable_work_rejects_changed_inputs_without_destroying_progress() {
        let dir = tempfile::tempdir().unwrap();
        bind_work_dir(dir.path(), &serde_json::json!({"title":"A"})).unwrap();
        std::fs::write(dir.path().join("asr.jsonl"), "saved result").unwrap();
        bind_work_dir(dir.path(), &serde_json::json!({"title":"A"})).unwrap();
        assert!(bind_work_dir(dir.path(), &serde_json::json!({"title":"B"})).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("asr.jsonl")).unwrap(),
            "saved result"
        );
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
