//! A course has one note: a folder with a manifest. A retry, repair or regenerate
//! builds the new note beside it and swaps it into place, so a crash leaves either
//! the old note or the new one. Work directories and diagnostic logs are not notes.
//!
//! Beside the notes of a library folder, `.course2md/` holds what is not a note:
//! `locks/<note>.lock`, `swap/<note>/` (the note being built and the note being
//! replaced) and `work/<task>` (task progress).

pub use crate::outcome::{FailureKind, Outcome, Outcomes, Status};
use crate::tr;
use crate::{
    digest::file_digest,
    dispatch::TaskControl,
    fsio::atomic_write_shared,
    meta::Summary,
    meta::VideoMeta,
    render,
    timeline::{FrameEvent, Section},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// The manifest schema this app writes. Schema 1 was a manifest inside
/// `versions/<id>/`; it is read only to flatten such a course.
pub const MANIFEST_SCHEMA: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub schema: u32,
    pub meta: VideoMeta,
    pub sections: Vec<Section>,
    pub summary: Option<Summary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    /// The task whose result this note is.
    pub task_id: String,
    pub course_id: String,
    pub source_id: String,
    pub title: String,
    pub created_at_ms: u64,
    pub document: String,
    pub markdown: String,
    pub frames: Vec<FrameEvent>,
    pub assets: Vec<Asset>,
    pub outputs: Vec<String>,
    pub outcomes: Outcomes,
    pub partial: bool,
    /// Unique to this publication of the note (two notes of one task differ):
    /// what proves, after a stopped replacement, that the note in place is the
    /// one that replacement made. Empty in a note written before it was recorded.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub publication: String,
    /// Schema 1 only, read and never written; empty in a schema 2 note.
    #[serde(default, skip_serializing)]
    pub version_id: String,
    /// Schema 1 only, read and never written; 0 in a schema 2 note.
    #[serde(default, skip_serializing)]
    pub revision: u64,
}

/// A schema 1 course's `current.json`, which named its current version. Read only
/// to flatten such a course.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentVersion {
    pub schema: u32,
    pub course_id: String,
    pub version_id: String,
    pub manifest: String,
}

/// 发布/校验阶段的文件 I/O 并行度（复制、fsync、SHA-256）：
/// 小文件居多，8 路即可吃满磁盘/CPU，又不至于进程风暴。
const IO_PARALLELISM: usize = 8;

/// How long a publication waits for another writer of the same note.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    control: &TaskControl,
    f: impl Fn(&T) -> Result<R> + Sync,
) -> Result<Vec<R>> {
    crate::pool::map_pool(items.iter().collect(), IO_PARALLELISM, control, f)
}

/// Where a task publishes, and whose note it may replace.
#[derive(Debug, Clone)]
pub struct Target {
    pub task_id: String,
    pub course_id: String,
    pub source_id: String,
    /// The note folder.
    pub course_dir: PathBuf,
    /// A repair replaces only the note it was planned from: the task id recorded
    /// in that note's manifest. `None` (a generate) replaces whatever is there.
    pub replaces: Option<String>,
}

/// A manifest written by a newer app. Unlike damage, it is neither read as this
/// app's nor replaced.
#[derive(Debug)]
pub struct NewerSchema;

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!(
            "此笔记需要更新版本的软件 / This note needs a newer version of course2md"
        ))
    }
}

impl std::error::Error for NewerSchema {}

/// The note a repair was planned from has since been replaced by another task.
#[derive(Debug)]
pub struct NoteChanged;

impl std::fmt::Display for NoteChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!(
            "计划补做后笔记已被更改，请重新补做 / The note changed since this repair was planned"
        ))
    }
}

impl std::error::Error for NoteChanged {}

fn check_schema(schema: u32, newest: u32) -> Result<()> {
    if schema > newest {
        return Err(NewerSchema.into());
    }
    anyhow::ensure!(schema >= 1, tr!("笔记清单损坏 / Invalid note manifest"));
    Ok(())
}

pub fn read_manifest(path: &Path) -> Result<Manifest> {
    let value: Manifest = serde_json::from_slice(&std::fs::read(path)?)
        .context(tr!("笔记清单损坏 / Invalid note manifest"))?;
    check_schema(value.schema, MANIFEST_SCHEMA)?;
    Ok(value)
}

/// A schema 1 course's `current.json`, refused when a newer app wrote it.
pub fn read_current(path: &Path) -> Result<CurrentVersion> {
    let value: CurrentVersion = serde_json::from_slice(&std::fs::read(path)?)?;
    check_schema(value.schema, 1)?;
    Ok(value)
}

/// The note folder's manifest; `None` when it has none.
pub fn note_manifest(note_dir: &Path) -> Result<Option<Manifest>> {
    let path = note_dir.join("manifest.json");
    if !path.is_file() {
        return Ok(None);
    }
    read_manifest(&path).map(Some)
}

/// Refuses before any work when a newer app wrote the note: replacing it would
/// fail only after the requests were paid for. Damage is not refused here;
/// publishing may still replace what is broken.
pub fn refuse_newer(note_dir: &Path) -> Result<()> {
    let checks = [
        note_dir
            .join("manifest.json")
            .is_file()
            .then(|| note_manifest(note_dir).map(drop)),
        note_dir
            .join("current.json")
            .is_file()
            .then(|| read_current(&note_dir.join("current.json")).map(drop)),
    ];
    for check in checks.into_iter().flatten() {
        if let Err(error) = check
            && error.is::<NewerSchema>()
        {
            return Err(error);
        }
    }
    Ok(())
}

/// The note in `note_dir` when task `task_id` published it and it is intact:
/// that task's work is done, and running it again must send nothing.
pub fn published_by(note_dir: &Path, task_id: &str) -> Result<Option<Manifest>> {
    let manifest = match note_manifest(note_dir) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return Ok(None),
        Err(error) if error.is::<NewerSchema>() => return Err(error),
        // A damaged note is nobody's finished work.
        Err(_) => return Ok(None),
    };
    Ok(
        (manifest.task_id == task_id && validate_note(note_dir, &manifest).is_ok())
            .then_some(manifest),
    )
}

/// A course folder in the schema 1 layout: `current.json` and `versions/`, no
/// manifest of its own.
pub fn is_legacy(note_dir: &Path) -> bool {
    !note_dir.join("manifest.json").exists()
        && (note_dir.join("current.json").is_file() || note_dir.join("versions").is_dir())
}

/// `<library>/.course2md`: beside the note, on the same volume.
pub fn library_dir(note_dir: &Path) -> Result<PathBuf> {
    Ok(note_parent(note_dir)?.join(".course2md"))
}

/// Where a task's work directory lives: outside the note, which is replaced as a whole.
pub fn work_dir(note_dir: &Path, task_id: &str) -> Result<PathBuf> {
    Ok(library_dir(note_dir)?.join("work").join(task_id))
}

fn note_parent(note_dir: &Path) -> Result<PathBuf> {
    note_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .context(tr!("笔记位置无效 / Invalid note location"))
}

fn note_name(note_dir: &Path) -> Result<String> {
    note_dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .context(tr!("笔记位置无效 / Invalid note location"))
}

/// The real folder of a note reached through a link: it is replaced beside its
/// target, on the target's volume, so the link keeps pointing at the note.
fn resolve_note(note_dir: &Path) -> Result<PathBuf> {
    // Read, not followed: while the note is moved aside its target is missing,
    // and recovery must still find where it lives.
    let mut path = note_dir.to_path_buf();
    for _ in 0..8 {
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Ok(path);
        }
        let target = std::fs::read_link(&path)?;
        path = if target.is_absolute() {
            target
        } else {
            note_parent(&path)?.join(target)
        };
    }
    anyhow::bail!(tr!(
        "笔记文件夹是一个无法打开的链接：{} / The note folder is a link that cannot be followed: {}",
        note_dir.display(),
        note_dir.display()
    ))
}

/// Held by a task from before its first paid request until it ends: a second
/// task for the same note, from this app or the command line, stops at once
/// instead of finding the note taken only after it paid for its requests.
pub fn claim_note(note_dir: &Path) -> Result<crate::runtime::FileLock> {
    let note_dir = resolve_note(note_dir)?;
    let locks = library_dir(&note_dir)?.join("locks");
    let path = locks.join(format!("{}.task.lock", note_name(&note_dir)?));
    let opened =
        std::fs::create_dir_all(&locks).and_then(|()| crate::runtime::try_lock_file(&path));
    match opened {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(crate::outcome::NoteBusy(
            tr!(
                "另一个任务正在处理这份笔记，请等它结束后再试 / Another task is working on this note; try again when it ends"
            )
            .to_string(),
        )
        .into()),
        Err(error) => Err(anyhow::Error::new(error).context(tr!(
            "无法打开这份笔记的任务锁：{} / Could not open the note's task lock: {}",
            path.display(),
            path.display()
        ))),
    }
}

/// A note in a folder of notes that needs the person after recovery.
#[derive(Debug, Clone, PartialEq)]
pub enum NoteAttention {
    /// Its replacement stopped and cannot be finished on its own; nothing changed.
    Blocked { note: PathBuf, swap: PathBuf },
    /// Copies are kept beside it for the person to look at.
    Kept {
        note: PathBuf,
        swap: PathBuf,
        copies: Vec<PathBuf>,
    },
    /// Recovery could not run (permissions, a full disk).
    Failed { note: PathBuf, message: String },
}

/// Completes the interrupted replacements of every note whose swap folder is in
/// `folder/.course2md/swap`, the notes directly in `folder`, and returns those
/// that need the person.
pub fn recover_folder(folder: &Path) -> Vec<NoteAttention> {
    let Ok(entries) = std::fs::read_dir(folder.join(".course2md").join("swap")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| note_attention(&folder.join(entry.file_name())))
        .collect()
}

/// Completes an interrupted replacement of the note, and says whether the note
/// needs the person.
pub fn note_attention(note: &Path) -> Option<NoteAttention> {
    let note = note.to_path_buf();
    let swap = resolve_note(&note)
        .and_then(|real| swap_dir(&real))
        .unwrap_or_default();
    match recover(&note) {
        Err(error) if error.is::<crate::outcome::RecoveryBlocked>() => {
            Some(NoteAttention::Blocked { note, swap })
        }
        Err(error) => Some(NoteAttention::Failed {
            note,
            message: format!("{error:#}"),
        }),
        // Another writer holds the note: its copies may be its own work in progress.
        Ok(false) => None,
        Ok(true) => {
            let copies = kept_copies(&note);
            (!copies.is_empty()).then_some(NoteAttention::Kept { note, swap, copies })
        }
    }
}

/// The folder that holds a note's replacement and the note it replaces.
fn swap_dir(note_dir: &Path) -> Result<PathBuf> {
    Ok(library_dir(note_dir)?
        .join("swap")
        .join(note_name(note_dir)?))
}

/// One writer of a note at a time: publications, export writes, recovery, flattening.
fn lock_note(note_dir: &Path) -> Result<crate::runtime::FileLock> {
    let locks = library_dir(note_dir)?.join("locks");
    std::fs::create_dir_all(&locks)?;
    crate::runtime::lock_file_within(
        &locks.join(format!("{}.lock", note_name(note_dir)?)),
        LOCK_WAIT,
    )
}

pub fn safe_asset_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    anyhow::ensure!(
        !relative.is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))),
        tr!("笔记包含无效资源路径 / Invalid note asset path")
    );
    let full = root.join(path);
    let canonical = full.canonicalize()?;
    anyhow::ensure!(
        canonical.starts_with(root.canonicalize()?),
        tr!("资源位置超出此笔记 / Asset escapes note directory")
    );
    Ok(full)
}

fn verified_digest(path: &Path) -> Result<String> {
    use std::sync::{Mutex, OnceLock};
    use std::time::UNIX_EPOCH;
    type Cache = std::collections::HashMap<PathBuf, (u64, u128, String)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let metadata = std::fs::metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // One entry per file a long-running desktop process has verified; the
    // library can be large, so the cache is dropped once it reaches this size.
    const MAX_ENTRIES: usize = 16_384;
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((_, _, digest)) = crate::runtime::lock_recover(cache)
        .get(&key)
        .filter(|(len, stamp, _)| *len == metadata.len() && *stamp == modified)
    {
        return Ok(digest.clone());
    }
    let digest = file_digest(path)?;
    let mut cache = crate::runtime::lock_recover(cache);
    if cache.len() >= MAX_ENTRIES {
        cache.clear();
    }
    cache.insert(key, (metadata.len(), modified, digest.clone()));
    Ok(digest)
}

/// Verifies a note outside a task.
pub fn validate_note(dir: &Path, manifest: &Manifest) -> Result<()> {
    validate_note_in_task(dir, manifest, &TaskControl::default())
}

/// Verifies the files a note is read and repaired from: its document, metadata,
/// transcript and screenshots. Its Markdown and exports are presentation files a
/// person may edit; an edit leaves the note intact.
pub fn validate_note_in_task(dir: &Path, manifest: &Manifest, control: &TaskControl) -> Result<()> {
    let required: Vec<&Asset> = manifest
        .assets
        .iter()
        .filter(|asset| !asset.path.starts_with("exports/") && asset.path != manifest.markdown)
        .collect();
    parallel_map(&required, control, |asset| {
        let path = safe_asset_path(dir, &asset.path)?;
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() == asset.bytes
                && verified_digest(&path)? == asset.sha256,
            "笔记文件已更改或损坏：{} / Note asset changed",
            asset.path
        );
        Ok(())
    })?;
    let body: Document =
        serde_json::from_slice(&std::fs::read(safe_asset_path(dir, &manifest.document)?)?)?;
    anyhow::ensure!(
        has_readable_body(&body.sections),
        tr!("笔记没有可读正文 / Note has no readable body")
    );
    for frame in &manifest.frames {
        safe_asset_path(dir, &frame.image)?;
    }
    Ok(())
}

pub fn has_readable_body(sections: &[Section]) -> bool {
    sections
        .iter()
        .flat_map(|s| &s.speech)
        .any(|e| !e.text.trim().is_empty())
}

/// A published note, and the files of the note it replaced that were kept beside
/// it under another name because the new note has a file of the same name: a
/// hand-edited `course.md` is kept as `course.edited.md`.
#[derive(Debug, Clone)]
pub struct Publication {
    pub manifest: Manifest,
    pub kept_edits: Vec<String>,
}

/// [`publish`] outside a task.
pub fn publish_blocking(
    target: &Target,
    work_dir: &Path,
    meta: &VideoMeta,
    sections: &[Section],
    summary: Option<&Summary>,
    formats: &[crate::config::OutputFormat],
    outcomes: Outcomes,
) -> Result<Manifest> {
    publish(
        target,
        work_dir,
        meta,
        sections,
        summary,
        formats,
        outcomes,
        &TaskControl::default(),
    )
    .map(|publication| publication.manifest)
}

/// Builds and syncs the whole note in the swap folder on the note's volume, then
/// replaces the note with two renames. The files of the old note that are not its
/// own (added by hand, or edited) come along. A task that already published here
/// gets its note back unchanged.
#[allow(clippy::too_many_arguments)]
pub fn publish(
    target: &Target,
    work_dir: &Path,
    meta: &VideoMeta,
    sections: &[Section],
    summary: Option<&Summary>,
    formats: &[crate::config::OutputFormat],
    mut outcomes: Outcomes,
    control: &TaskControl,
) -> Result<Publication> {
    anyhow::ensure!(
        has_readable_body(sections),
        tr!(
            "文字未生成，已保留可用素材 / No readable text; available materials are retained in the task"
        )
    );
    let note = &resolve_note(&target.course_dir)?;
    let _lock = lock_note(note)?;
    recover_locked(note)?;
    flatten_tolerant(note)?;
    release_legacy_lock(note);
    salvage_work(note, &note.join(".work"))?;
    let old = match note_manifest(note) {
        Ok(manifest) => manifest,
        Err(error) if error.is::<NewerSchema>() => return Err(error),
        Err(_) => None,
    };
    if let Some(manifest) = &old
        && manifest.task_id == target.task_id
        && validate_note(note, manifest).is_ok()
    {
        return Ok(Publication {
            manifest: manifest.clone(),
            kept_edits: Vec::new(),
        });
    }
    if let Some(base) = &target.replaces {
        anyhow::ensure!(
            old.as_ref()
                .is_some_and(|manifest| &manifest.task_id == base),
            NoteChanged
        );
    }
    if let Some(old) = &old {
        refuse_worse(&old.outcomes, &outcomes)?;
    }
    // The note keeps every export it had, made again from the new content.
    let mut formats = formats.to_vec();
    for format in old.iter().flat_map(|old| export_formats(&old.outputs)) {
        if !formats.contains(&format) {
            formats.push(format);
        }
    }
    let formats = &formats[..];
    let swap = swap_dir(note)?;
    std::fs::create_dir_all(&swap).map_err(|error| disk_full(error.into()))?;
    let staging = swap.join(format!("new-{}", target.task_id));
    remove_tree(&staging)?;
    std::fs::create_dir(&staging).map_err(|error| disk_full(error.into()))?;
    let stamp = new_publication(&target.task_id);
    let built = (|| {
        let mut frames = Vec::new();
        let mut copied = std::collections::HashSet::new();
        for section in sections.iter().filter(|s| !s.image.is_empty()) {
            copied.insert(section.image.clone());
            frames.push(FrameEvent {
                t: section.t,
                image: section.image.clone(),
            });
        }
        // 截图复制并行；目标目录先在主线程建好，
        // frames 列表仍按 section 顺序生成，产物内容与串行版一致。
        let images: Vec<&String> = copied.iter().collect();
        for image in &images {
            let dest = staging.join(image.as_str());
            std::fs::create_dir_all(dest.parent().unwrap())?;
        }
        parallel_map(&images, control, |image| {
            let source = safe_asset_path(work_dir, image.as_str())?;
            std::fs::copy(source, staging.join(image.as_str()))?;
            Ok(())
        })?;
        let document = Document {
            schema: 1,
            meta: meta.clone(),
            sections: sections.to_vec(),
            summary: summary.cloned(),
        };
        atomic_write_shared(
            &staging.join("document.json"),
            &serde_json::to_vec_pretty(&document)?,
        )?;
        let mut markdown = render::render_markdown(meta, sections);
        if let Some(summary) = summary {
            markdown = crate::summary_doc::insert_into_md(&markdown, summary);
        }
        atomic_write_shared(&staging.join("course.md"), markdown.as_bytes())?;
        meta.save(&staging.join("meta.json"))?;
        // Preserve original fine-grained transcript for later independent local/AI work.
        if work_dir.join("timeline.jsonl").is_file() {
            std::fs::copy(
                work_dir.join("timeline.jsonl"),
                staging.join("timeline.jsonl"),
            )?;
        }
        let mut outputs = Vec::new();
        for format in formats {
            let name = crate::portable::file_name(*format);
            let path = staging.join("exports").join(name);
            match crate::portable::write_document(&staging, &document, *format, &path) {
                Ok(_) => {
                    outputs.push(format!("exports/{name}"));
                    outcomes
                        .exports
                        .insert(format.to_string(), Outcome::succeeded());
                }
                Err(error) => {
                    outcomes
                        .exports
                        .insert(format.to_string(), Outcome::from_error(&error));
                }
            }
        }
        let mut paths = vec![
            "document.json".to_string(),
            "course.md".to_string(),
            "meta.json".to_string(),
        ];
        if staging.join("timeline.jsonl").is_file() {
            paths.push("timeline.jsonl".into());
        }
        paths.extend(copied);
        paths.extend(outputs.iter().cloned());
        paths.sort();
        paths.dedup();
        // 逐文件 fsync + SHA-256 并行，结果按 paths 顺序写回
        let assets = parallel_map(&paths, control, |path| {
            let full = staging.join(path);
            sync_file(&full).with_context(|| tr!("无法同步笔记文件：{}", path))?;
            Ok(Asset {
                bytes: std::fs::metadata(&full)?.len(),
                sha256: file_digest(&full)?,
                path: path.clone(),
            })
        })?;
        let (kept_edits, kept_exports) = if note.is_dir() {
            // The lock a flatten or an older app left in the note is not the
            // person's file.
            carry_over(note, old.as_ref(), &staging, &|relative| {
                relative == LEGACY_PUBLISH_LOCK
            })?
        } else {
            Default::default()
        };
        // An export the new note could not make again stays the note's own file.
        let mut assets = assets;
        for asset in kept_exports {
            outputs.push(asset.path.clone());
            assets.push(asset);
        }
        assets.sort_by(|a, b| a.path.cmp(&b.path));
        outputs.sort();
        let manifest = Manifest {
            schema: MANIFEST_SCHEMA,
            task_id: target.task_id.clone(),
            course_id: target.course_id.clone(),
            source_id: target.source_id.clone(),
            title: meta.title.clone(),
            created_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
                .try_into()?,
            document: "document.json".into(),
            markdown: "course.md".into(),
            frames,
            assets,
            outputs,
            partial: outcomes.is_partial(),
            outcomes: outcomes.clone(),
            publication: stamp.clone(),
            version_id: String::new(),
            revision: 0,
        };
        atomic_write_shared(
            &staging.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        sync_tree_dirs(&staging)?;
        fail_point(FailPoint::Staged)?;
        Ok(Publication {
            manifest,
            kept_edits,
        })
    })();
    let publication = match built {
        Ok(publication) => publication,
        Err(error) => {
            if !simulated_crash(&error) {
                let _ = remove_tree(&staging);
            }
            return Err(disk_full(error));
        }
    };
    swap_in(note, &staging, &target.task_id, &stamp)?;
    Ok(publication)
}

/// The export formats whose files a note lists.
fn export_formats(outputs: &[String]) -> Vec<crate::config::OutputFormat> {
    use crate::config::OutputFormat;
    [OutputFormat::Md, OutputFormat::Html, OutputFormat::Json]
        .into_iter()
        .filter(|format| {
            let path = format!("exports/{}", crate::portable::file_name(*format));
            outputs.contains(&path)
        })
        .collect()
}

/// How much of a part a note holds: all of it, the finished batches of it, or none.
fn finished(outcome: &Outcome) -> usize {
    match outcome.status {
        Status::Succeeded => usize::MAX,
        Status::Partial => outcome.completed.unwrap_or(0),
        Status::NotRequested | Status::Failed => 0,
    }
}

/// Refuses to replace a note with a result that holds less of any part the new
/// task tried to make: its transcript, screenshots, proofreading, translation or
/// summary. A part the new task did not ask for (AI turned off, a transcript
/// already in the target language) is a choice, not a loss. The saved note stays,
/// and the task ends failed, naming the parts; what it paid for stays in its work.
pub fn refuse_worse(saved: &Outcomes, new: &Outcomes) -> Result<()> {
    let parts = [
        ("transcript", &saved.transcript, &new.transcript),
        ("screenshots", &saved.screenshots, &new.screenshots),
        ("proofreading", &saved.proofreading, &new.proofreading),
        ("translation", &saved.translation, &new.translation),
        ("summary", &saved.summary, &new.summary),
    ];
    let worse: Vec<crate::outcome::KeptPart> = parts
        .iter()
        .filter(|(_, saved, new)| {
            matches!(new.status, Status::Failed | Status::Partial)
                && finished(new) < finished(saved)
        })
        .map(|(part, _, new)| crate::outcome::KeptPart {
            part: (*part).to_owned(),
            cause: new.message.clone(),
        })
        .collect();
    if worse.is_empty() {
        return Ok(());
    }
    Err(crate::outcome::NoteKept::new(worse).into())
}

/// Replaces `note` with the finished `staging`, which is on the same volume.
/// Each step leaves a state [`recover`] completes or undoes.
/// `publication` is the one the new note's manifest records: recovery deletes the
/// replaced copy only when the note in place is that one.
fn swap_in(note: &Path, staging: &Path, label: &str, publication: &str) -> Result<()> {
    let parent = note_parent(note)?;
    let swap = swap_dir(note)?;
    if !note.exists() {
        std::fs::rename(staging, note).context(tr!(
            "无法保存笔记；可稍后重试 / Could not save the note; try again later"
        ))?;
        sync_dir(&swap)?;
        sync_dir(&parent)?;
        let _ = std::fs::remove_dir(&swap);
        return Ok(());
    }
    // A copy an earlier replacement could not delete may hold this name.
    let name = (1..)
        .map(|n| match n {
            1 => format!("old-{label}"),
            n => format!("old-{label}-{n}"),
        })
        .find(|name| swap.join(name).symlink_metadata().is_err())
        .expect("an unused name");
    let old = swap.join(&name);
    // Names the copy that is the note while it is moved aside, before it is: only
    // that copy is ever put back, never one an earlier replacement left behind.
    let live = swap.join(LIVE_MARKER);
    if let Err(error) = serde_json::to_vec(&LiveRecord {
        copy: name.clone(),
        placed: None,
        publication: Some(publication.to_owned()),
    })
    .map_err(anyhow::Error::from)
    .and_then(|bytes| crate::fsio::atomic_write(&live, &bytes))
    .and_then(|()| sync_dir(&swap))
    {
        let _ = remove_tree(staging);
        // The record's own temporary file, which a failed write may leave.
        for entry in std::fs::read_dir(&swap).into_iter().flatten().flatten() {
            if entry.file_name().to_string_lossy().starts_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        return Err(disk_full(error));
    }
    if let Err(error) = std::fs::rename(note, &old) {
        let _ = std::fs::remove_file(&live);
        let _ = remove_tree(staging);
        return Err(replace_error(error));
    }
    let placed = fail_point(FailPoint::BetweenRenames).and_then(|()| {
        std::fs::rename(staging, note).context(tr!(
            "无法保存笔记；原笔记未改动，可稍后重试 / Could not save the note; it is unchanged; try again later"
        ))
    });
    if let Err(error) = placed {
        if !simulated_crash(&error) {
            // Put the old note back; recovery does it later if this fails too.
            if std::fs::rename(&old, note).is_ok() {
                let _ = std::fs::remove_file(&live);
                let _ = remove_tree(staging);
            }
        }
        return Err(error);
    }
    sync_dir(&swap)?;
    sync_dir(&parent)?;
    fail_point(FailPoint::AfterRenames)?;
    // The new note is in place: what is left is tidying, which recovery finishes
    // from the record if it fails here. The replaced note is now known to be
    // stale; only a stale copy is ever deleted.
    let stale = unused_name(&swap, &format!("stale-{}", &name["old-".len()..]));
    let tidied = std::fs::rename(&old, &stale)
        .and_then(|()| sync_dir(&swap).map_err(std::io::Error::other))
        .and_then(|()| std::fs::remove_file(&live))
        .and_then(|()| sync_dir(&swap).map_err(std::io::Error::other));
    if let Err(error) = tidied {
        tracing::warn!(
            "新笔记已就位，旧笔记稍后清理 / The new note is in place; the replaced one is cleared later: {error}"
        );
        return Ok(());
    }
    fail_point(FailPoint::BeforeDeletingOld)?;
    discard_old(note, &stale);
    let _ = std::fs::remove_dir(&swap);
    Ok(())
}

/// The file in a note's swap folder that names the copy which is the note while
/// it is moved aside.
const LIVE_MARKER: &str = "live";

/// A full disk while the new note is written, in the words of every other step:
/// the note is unchanged.
fn disk_full(error: anyhow::Error) -> anyhow::Error {
    let full = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull)
    });
    if full {
        error.context(tr!(
            "磁盘空间不足，无法保存新笔记；原笔记未改动 / Not enough disk space to save the new note; it is unchanged"
        ))
    } else {
        error
    }
}

/// Why the note could not be moved aside, in words that say what to do. The note
/// is unchanged in every case.
fn replace_error(error: std::io::Error) -> anyhow::Error {
    use std::io::ErrorKind;
    // Windows: ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION, ERROR_ACCESS_DENIED on
    // a file another program holds open.
    let held_open = cfg!(windows)
        && (matches!(error.raw_os_error(), Some(5 | 32 | 33))
            || error.kind() == ErrorKind::PermissionDenied);
    let message = if error.kind() == ErrorKind::StorageFull {
        tr!(
            "磁盘空间不足，无法替换笔记；原笔记未改动 / Not enough disk space to replace the note; it is unchanged"
        )
    } else if held_open || error.kind() == ErrorKind::ResourceBusy {
        tr!(
            "笔记正被其他程序使用，暂时无法替换；原笔记未改动，关闭后重试即可 / The note is in use by another program; it is unchanged; close it and try again"
        )
    } else if error.kind() == ErrorKind::PermissionDenied {
        tr!("没有权限替换笔记；原笔记未改动 / No permission to replace the note; it is unchanged")
    } else {
        tr!(
            "无法保存笔记；原笔记未改动，可稍后重试 / Could not save the note; it is unchanged; try again later"
        )
    };
    anyhow::Error::new(error).context(message)
}

/// Copies into `staging` the files of the old note that are not its own: anything
/// its manifest does not list, and listed files that were changed since. A file
/// the new note also has is kept under an `.edited` name and reported. The old
/// note's own unchanged files are left behind: the new note replaces them, except
/// an export the new note could not make again, which stays. Folders that only
/// held the old note's own files are not recreated empty. Paths `skip` claims, and
/// what is below them, stay behind.
fn carry_over(
    note: &Path,
    old: Option<&Manifest>,
    staging: &Path,
    skip: &dyn Fn(&str) -> bool,
) -> Result<(Vec<String>, Vec<Asset>)> {
    let listed: HashMap<&str, &Asset> = old
        .map(|manifest| {
            manifest
                .assets
                .iter()
                .map(|asset| (asset.path.as_str(), asset))
                .collect()
        })
        .unwrap_or_default();
    let owned_dirs: std::collections::HashSet<&str> = listed
        .keys()
        .flat_map(|path| {
            path.match_indices('/')
                .map(move |(index, _)| &path[..index])
        })
        .collect();
    let mut kept_edits = Vec::new();
    let mut kept_exports = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for (relative, kind) in walk(note)? {
        // Parents come before their children.
        if skipped
            .iter()
            .any(|dir| relative.starts_with(dir.as_str()) && relative[dir.len()..].starts_with('/'))
        {
            continue;
        }
        if skip(&relative) {
            skipped.push(relative);
            continue;
        }
        if kind == EntryKind::Dir {
            if !owned_dirs.contains(relative.as_str()) {
                std::fs::create_dir_all(staging.join(&relative))?;
            }
            continue;
        }
        if relative == "manifest.json" {
            continue;
        }
        let source = note.join(&relative);
        let own = listed.get(relative.as_str()).is_some_and(|asset| {
            std::fs::metadata(&source).is_ok_and(|m| m.len() == asset.bytes)
                && file_digest(&source).is_ok_and(|digest| digest == asset.sha256)
        });
        let existing = staging.join(&relative);
        if own {
            if relative.starts_with("exports/") && existing.symlink_metadata().is_err() {
                copy_entry(&source, &existing, kind)?;
                kept_exports.extend(listed.get(relative.as_str()).map(|asset| (*asset).clone()));
            }
            continue;
        }
        let destination = if existing.symlink_metadata().is_err() {
            relative.clone()
        } else if kind == EntryKind::File
            && existing.is_file()
            && file_digest(&existing)? == file_digest(&source)?
        {
            continue;
        } else {
            let kept = edited_name(&relative, |name| {
                staging.join(name).symlink_metadata().is_ok()
                    || note.join(name).symlink_metadata().is_ok()
            });
            kept_edits.push(kept.clone());
            kept
        };
        copy_entry(&source, &staging.join(&destination), kind)?;
    }
    Ok((kept_edits, kept_exports))
}

/// `course.md` → `course.edited.md`, then `course.edited-2.md`, … — the first name
/// `taken` does not claim.
fn edited_name(relative: &str, taken: impl Fn(&str) -> bool) -> String {
    let (dir, file) = relative
        .rsplit_once('/')
        .map_or(("", relative), |(dir, file)| (dir, file));
    let (stem, extension) = match file.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (file, String::new()),
    };
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    (1..)
        .map(|n| match n {
            1 => format!("{prefix}{stem}.edited{extension}"),
            n => format!("{prefix}{stem}.edited-{n}{extension}"),
        })
        .find(|name| !taken(name))
        .expect("an unused name")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EntryKind {
    Dir,
    File,
    Link,
}

/// Every entry below `root`, parents before children, as `/`-separated paths.
/// Links are listed, not followed.
fn walk(root: &Path) -> Result<Vec<(String, EntryKind)>> {
    let mut out = Vec::new();
    let mut pending = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = pending.pop() {
        anyhow::ensure!(
            depth <= 64,
            tr!("笔记目录层级过深 / Note directory depth exceeds the safe limit")
        );
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                tracing::warn!(
                    "跳过名称无法读取的文件 / Skipped a file whose name is not valid Unicode: {}",
                    entry.path().display()
                );
                continue;
            };
            let relative = format!("{prefix}{name}");
            let kind = entry.file_type()?;
            if kind.is_dir() {
                out.push((relative.clone(), EntryKind::Dir));
                pending.push((entry.path(), format!("{relative}/"), depth + 1));
            } else if kind.is_symlink() {
                out.push((relative, EntryKind::Link));
            } else {
                out.push((relative, EntryKind::File));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// A hard link where the volume allows one (the source is about to be deleted,
/// and nothing writes into either name), otherwise a copy; then synced.
fn copy_entry(source: &Path, destination: &Path, kind: EntryKind) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match kind {
        EntryKind::Dir => std::fs::create_dir_all(destination)?,
        EntryKind::Link => {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(source)?, destination)?;
            #[cfg(not(unix))]
            if std::fs::metadata(source).is_ok_and(|m| m.is_file()) {
                std::fs::copy(source, destination)?;
            }
        }
        EntryKind::File => {
            if std::fs::hard_link(source, destination).is_err() {
                std::fs::copy(source, destination)?;
            }
            sync_file(destination)?;
        }
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    // Windows FlushFileBuffers requires a writable handle; opening must not truncate.
    std::fs::File::options()
        .read(true)
        .write(cfg!(windows))
        .open(path)?
        .sync_all()?;
    Ok(())
}

/// Syncs every directory of a finished tree, so its entries survive power loss.
fn sync_tree_dirs(root: &Path) -> Result<()> {
    for (relative, kind) in walk(root)? {
        if kind == EntryKind::Dir {
            sync_dir(&root.join(relative))?;
        }
    }
    sync_dir(root)
}

fn remove_tree(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Deletes a replaced note once its replacement is in place. Task progress an
/// older app kept inside the note (`.work`) moves to the library's work folder
/// first. A failure only warns: the next recovery deletes it.
fn discard_old(note: &Path, old: &Path) {
    let result = salvage_work(note, &old.join(".work"))
        .and_then(|()| remove_tree(old))
        // Read-only files (a Windows attribute, a copied CD) refuse deletion until writable.
        .or_else(|_| {
            make_writable(old);
            remove_tree(old)
        });
    if let Err(error) = result {
        use std::sync::{Mutex, OnceLock};
        static WARNED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
        let first = crate::runtime::lock_recover(WARNED.get_or_init(Default::default))
            .insert(old.to_path_buf());
        if first {
            tracing::warn!(
                "旧笔记暂时无法删除，稍后会再次清理 / The replaced note could not be deleted yet: {}: {error:#}",
                old.display()
            );
        }
    }
}

/// Replaced notes that could not be deleted. This many refuses a new task until
/// they are cleared, so copies of a whole note do not pile up.
pub const MAX_KEPT_COPIES: usize = 3;

fn stuck_olds(note: &Path) -> Result<Vec<PathBuf>> {
    Ok(kept_copies(note))
}

/// Lets the owner delete everything below `root`.
fn make_writable(root: &Path) {
    let entries = walk(root).unwrap_or_default();
    for path in std::iter::once(root.to_path_buf())
        .chain(entries.into_iter().map(|(relative, _)| root.join(relative)))
    {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o700);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(&path, permissions);
    }
}

/// Moves each task folder of a schema 1 note's `.work` to `<library>/.course2md/work/`.
fn salvage_work(note: &Path, legacy: &Path) -> Result<()> {
    if !legacy.is_dir() {
        return Ok(());
    }
    let work = library_dir(note)?.join("work");
    std::fs::create_dir_all(&work)?;
    for entry in std::fs::read_dir(legacy)? {
        let entry = entry?;
        let mut destination = work.join(entry.file_name());
        if destination.exists() {
            destination = work.join(format!(
                "{}.moved-{}",
                entry.file_name().to_string_lossy(),
                std::process::id()
            ));
        }
        std::fs::rename(entry.path(), &destination)?;
    }
    remove_tree(legacy)
}

/// Completes or undoes a replacement that a crash interrupted: with the note
/// missing, the note it replaced is put back; otherwise the replaced note is
/// deleted. An unfinished new note is deleted. Returns `false` without waiting
/// when another writer holds the note (the leftovers are its own).
pub fn recover(note_dir: &Path) -> Result<bool> {
    let note_dir = &resolve_note(note_dir)?;
    let locks = library_dir(note_dir)?.join("locks");
    if !swap_dir(note_dir)?.exists() {
        return Ok(true);
    }
    std::fs::create_dir_all(&locks)?;
    let Ok(_lock) =
        crate::runtime::lock_file(&locks.join(format!("{}.lock", note_name(note_dir)?)))
    else {
        return Ok(false);
    };
    recover_locked(note_dir)?;
    Ok(true)
}

fn recover_locked(note: &Path) -> Result<()> {
    let swap = swap_dir(note)?;
    recover_in(note, &swap).map_err(|error| {
        if error.is::<crate::outcome::RecoveryBlocked>() {
            return error;
        }
        // The message names both causes: the error below says which it was.
        error.context(tr!(
            "无法完成这份笔记的恢复，所有文件都已保留；请检查 {} 的权限和可用空间后重试",
            swap.display()
        ))
    })
}

fn recover_in(note: &Path, swap: &Path) -> Result<()> {
    if !swap.is_dir() {
        return Ok(());
    }
    // Everything is read before anything changes.
    let record = read_record(swap);
    let mut olds = Vec::new();
    let mut discardable = Vec::new();
    for entry in std::fs::read_dir(swap)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type()?;
        if name == LIVE_MARKER || name.starts_with("displaced") {
            continue;
        }
        if name.starts_with("old-") {
            olds.push((name, entry.path()));
        } else if name.starts_with("stale-") || name.starts_with("new-") {
            // A replaced note known to be stale, or a new note never put in place.
            discardable.push(entry.path());
        } else if kind.is_file() && name.starts_with(".tmp") {
            // The temporary file of a record a crash stopped writing.
            discardable.push(entry.path());
        }
    }
    let blocked = || -> anyhow::Error {
        crate::outcome::RecoveryBlocked {
            swap: swap.to_path_buf(),
        }
        .into()
    };
    let note_whole = holds_note(note);
    match record {
        // A note is in place: it stays usable, and every copy and the record stay
        // for the person to look at.
        Record::Damaged(why) if note_whole => {
            tracing::warn!(
                "笔记的替换记录无法读取，已保留 / The note's replacement record cannot be read; kept: {}: {why}",
                swap.display()
            );
            return Ok(());
        }
        Record::Damaged(_) => return Err(blocked()),
        Record::Names { copy, publication } => {
            let copy_path = swap.join(&copy);
            // Only the note this replacement made proves the copy is the one it
            // replaced: a note restored from a backup, or written by another app
            // meanwhile, does not.
            let replacement = note_whole
                && publication.as_deref().is_some_and(|publication| {
                    note_manifest(note).is_ok_and(|manifest| {
                        manifest.is_some_and(|manifest| manifest.publication == publication)
                    })
                });
            if replacement {
                if copy_path.symlink_metadata().is_ok() {
                    let stale = unused_name(swap, &format!("stale-{}", &copy["old-".len()..]));
                    std::fs::rename(&copy_path, &stale)?;
                    sync_dir(swap)?;
                    discardable.push(stale);
                    olds.retain(|(old, _)| old != &copy);
                }
            } else if note_whole {
                // Another note is in place: both are kept, the copy for the person.
                tracing::warn!(
                    "笔记位置上是另一份笔记，替换前的副本已保留 / Another note is in place; the copy from before the replacement was kept: {}",
                    copy_path.display()
                );
            } else if holds_note(&copy_path) {
                restore(note, &copy_path, swap)?;
                olds.retain(|(old, _)| old != &copy);
            } else {
                return Err(blocked());
            }
            std::fs::remove_file(swap.join(LIVE_MARKER))?;
            sync_dir(swap)?;
        }
        // No record and no note: the one copy that holds a note goes back. With
        // several, which one was the note is not known: all stay for the person.
        Record::None if !note_whole => {
            let notes: Vec<_> = olds.iter().filter(|(_, path)| holds_note(path)).collect();
            if let [(name, path)] = notes[..] {
                let name = name.clone();
                restore(note, path, swap)?;
                olds.retain(|(old, _)| old != &name);
            }
        }
        Record::None => {}
    }
    for path in discardable {
        if path.is_dir() {
            discard_old(note, &path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::remove_dir(swap);
    Ok(())
}

/// A token unique to one publication of a note: the task, the time, this process
/// and a counter, digested.
fn new_publication(task: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_nanos())
        .unwrap_or_default();
    let seed = format!(
        "{task}:{nanos}:{}:{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    crate::digest::digest(seed.as_bytes())[..32].to_owned()
}

/// What a replacement records before it moves the note aside.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveRecord {
    /// The copy that holds the note while it is moved aside.
    copy: String,
    /// The task id of the new note, as an earlier build recorded it: not proof,
    /// since two notes of one task share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    placed: Option<String>,
    /// The publication the new note's manifest records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<String>,
}

/// What a note's swap folder records about a replacement in progress.
enum Record {
    None,
    /// The copy that holds the note while it is moved aside, and the publication
    /// of the note that replaces it (unknown in a record of an earlier build).
    Names {
        copy: String,
        publication: Option<String>,
    },
    /// A record that exists but cannot be trusted: nothing may be decided from it.
    Damaged(String),
}

fn read_record(swap: &Path) -> Record {
    let path = swap.join(LIVE_MARKER);
    let metadata = match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Record::None,
        Err(error) => return Record::Damaged(error.to_string()),
        Ok(metadata) => metadata,
    };
    if !metadata.file_type().is_file() || metadata.len() > 1024 {
        return Record::Damaged(path.display().to_string());
    }
    let text = match std::fs::read(&path).map(String::from_utf8) {
        Ok(Ok(text)) => text,
        Ok(Err(_)) => return Record::Damaged(path.display().to_string()),
        Err(error) => return Record::Damaged(error.to_string()),
    };
    if let Ok(record) = serde_json::from_str::<LiveRecord>(&text) {
        let valid = |id: &Option<String>| id.as_deref().is_none_or(valid_placed);
        return if is_copy_name(&record.copy) && valid(&record.placed) && valid(&record.publication)
        {
            Record::Names {
                copy: record.copy,
                publication: record.publication,
            }
        } else {
            Record::Damaged(path.display().to_string())
        };
    }
    // An earlier build wrote the copy's name alone; one line ending is allowed.
    let name = text.strip_suffix('\n').unwrap_or(&text);
    if is_copy_name(name) {
        Record::Names {
            copy: name.to_owned(),
            publication: None,
        }
    } else {
        Record::Damaged(path.display().to_string())
    }
}

/// A task id as a record holds it.
fn valid_placed(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `old-<task>` or `old-<task>-<n>`, as a replacement names the copy it moves aside.
fn is_copy_name(name: &str) -> bool {
    name.strip_prefix("old-").is_some_and(|rest| {
        !rest.is_empty()
            && !rest.contains("..")
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

/// A folder holding a note this or a newer app can recognize: one with a
/// manifest, or a course in the layout of versions (what flattening moves aside).
/// An empty folder, or one a sync tool made, is not one.
fn holds_note(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && (is_legacy(dir)
            || match read_manifest(&dir.join("manifest.json")) {
                Ok(_) => true,
                Err(error) => error.is::<NewerSchema>(),
            })
}

/// Puts the copy back as the note. Whatever stands at the note path instead is
/// moved aside into the swap folder, never deleted.
fn restore(note: &Path, copy: &Path, swap: &Path) -> Result<()> {
    if note.symlink_metadata().is_ok() {
        let aside = unused_name(swap, "displaced");
        std::fs::rename(note, &aside)?;
        tracing::warn!(
            "笔记位置上的文件夹不是笔记，已移到 / The folder at the note's place was not a note; moved to {}",
            aside.display()
        );
    }
    std::fs::rename(copy, note)?;
    sync_dir(swap)?;
    sync_dir(&note_parent(note)?)
}

/// Copies kept beside a note for the person to look at: copies from before a
/// replacement that could not be confirmed stale, folders moved aside from the
/// note's place, and replaced notes that could not be deleted.
pub fn kept_copies(note_dir: &Path) -> Vec<PathBuf> {
    let Ok(swap) = resolve_note(note_dir).and_then(|note| swap_dir(&note)) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&swap) else {
        return Vec::new();
    };
    let mut copies: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            ["old-", "displaced", "stale-"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .map(|entry| entry.path())
        .collect();
    copies.sort();
    copies
}

/// `<dir>/<base>`, or `<base>-2`, … — the first that does not exist.
fn unused_name(dir: &Path, base: &str) -> PathBuf {
    (1..)
        .map(|n| match n {
            1 => dir.join(base),
            n => dir.join(format!("{base}-{n}")),
        })
        .find(|path| path.symlink_metadata().is_err())
        .expect("an unused name")
}

/// Makes `note_dir` ready for a task, before any paid work: completes an
/// interrupted replacement, flattens a schema 1 course, moves task progress an
/// older app kept inside the note out of it, and refuses a newer app's note.
pub fn prepare_note(note_dir: &Path) -> Result<()> {
    let note_dir = &resolve_note(note_dir)?;
    refuse_newer(note_dir)?;
    if !note_dir.exists() && !swap_dir(note_dir)?.exists() {
        return Ok(());
    }
    let _lock = lock_note(note_dir)?;
    recover_locked(note_dir)?;
    if stuck_olds(note_dir)?.len() >= MAX_KEPT_COPIES {
        return Err(crate::outcome::CopiesKept {
            swap: swap_dir(note_dir)?,
        }
        .into());
    }
    flatten_tolerant(note_dir)?;
    release_legacy_lock(note_dir);
    salvage_work(note_dir, &note_dir.join(".work"))?;
    // A folder an older app made for a task that never published.
    if note_dir.is_dir() && std::fs::read_dir(note_dir)?.next().is_none() {
        let _ = std::fs::remove_dir(note_dir);
    }
    refuse_newer(note_dir)
}

/// Turns a schema 1 course (`current.json` naming one of `versions/<id>/`) into
/// one flat note: the current version (or, with the pointer damaged, the newest
/// intact one) becomes the note, its exports under `exports/<id>/<task>/` join the
/// note's `exports/`, and files added to the course folder come along. Every other
/// version is deleted, only once the flat note is in place. A crash at any point
/// leaves the course as it was or flat; running it again finishes the job.
/// Returns the flat note's manifest, or `None` when the folder was not schema 1.
pub fn flatten_legacy(note_dir: &Path) -> Result<Option<Manifest>> {
    let note_dir = &resolve_note(note_dir)?;
    if !note_dir.exists() && !swap_dir(note_dir)?.exists() {
        return Ok(None);
    }
    let _lock = lock_note(note_dir)?;
    recover_locked(note_dir)?;
    let flat = flatten_locked(note_dir)?;
    release_legacy_lock(note_dir);
    Ok(flat)
}

/// [`flatten_locked`] for a task: a course with no readable version stays as it
/// is, and a new note is published beside its versions, which it keeps.
fn flatten_tolerant(note: &Path) -> Result<()> {
    match flatten_locked(note) {
        Err(error) if error.is::<NothingReadable>() => Ok(()),
        result => result.map(drop),
    }
}

fn flatten_locked(note: &Path) -> Result<Option<Manifest>> {
    if !is_legacy(note) {
        return Ok(None);
    }
    // The older app holds this lock for its whole publication of a version: one
    // that started first finishes before the course is read, and one that starts
    // meanwhile fails to take it and stops, its paid work kept, instead of
    // landing in the course this replaces.
    let older_app = take_legacy_lock(note)?;
    let (version, mut manifest, passed_over) = current_version(note)?;
    let swap = swap_dir(note)?;
    std::fs::create_dir_all(&swap).map_err(|error| disk_full(error.into()))?;
    let staging = swap.join("new-flatten");
    let publication = new_publication("flatten");
    remove_tree(&staging)?;
    let built = (|| {
        std::fs::create_dir(&staging)?;
        let source = note.join("versions").join(&version);
        for (relative, kind) in walk(&source)? {
            copy_entry(&source.join(&relative), &staging.join(&relative), kind)?;
        }
        let taken = |name: &str| staging.join(name).symlink_metadata().is_ok();
        // Exports made from this version later, for the tasks that made them.
        let exports = note.join("exports");
        let current_exports = exports.join(&version);
        if current_exports.is_dir() {
            for (relative, kind) in walk(&current_exports)? {
                if kind == EntryKind::Dir {
                    continue;
                }
                let (task, file) = relative.split_once('/').unwrap_or(("", &relative));
                let mut destination = format!("exports/{file}");
                if taken(&destination) {
                    let source = current_exports.join(&relative);
                    if file_digest(&source).ok() == file_digest(&staging.join(&destination)).ok() {
                        continue;
                    }
                    destination = labelled_name(&destination, task, taken);
                }
                copy_entry(
                    &current_exports.join(&relative),
                    &staging.join(destination),
                    kind,
                )?;
            }
        }
        // Anything else a person put in the course folder.
        for entry in std::fs::read_dir(note)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if ["current.json", "versions", ".publish.lock", ".work"].contains(&name.as_str()) {
                continue;
            }
            let base = if name == "exports" {
                // Exports of the deleted versions go with them.
                for (relative, kind) in walk(&entry.path())? {
                    let first = relative.split('/').next().unwrap_or_default();
                    if note.join("versions").join(first).is_dir() || relative == version {
                        continue;
                    }
                    let mut destination = format!("exports/{relative}");
                    if taken(&destination) {
                        if kind == EntryKind::Dir {
                            continue;
                        }
                        destination = edited_name(&destination, taken);
                    }
                    copy_entry(
                        &entry.path().join(&relative),
                        &staging.join(destination),
                        kind,
                    )?;
                }
                continue;
            } else {
                name
            };
            let kind = entry.file_type()?;
            let kind = if kind.is_dir() {
                EntryKind::Dir
            } else if kind.is_symlink() {
                EntryKind::Link
            } else {
                EntryKind::File
            };
            let destination = if taken(&base) {
                edited_name(&base, taken)
            } else {
                base
            };
            if kind == EntryKind::Dir {
                for (relative, inner) in walk(&entry.path())? {
                    copy_entry(
                        &entry.path().join(&relative),
                        &staging.join(&destination).join(relative),
                        inner,
                    )?;
                }
                std::fs::create_dir_all(staging.join(&destination))?;
            } else {
                copy_entry(&entry.path(), &staging.join(&destination), kind)?;
            }
        }
        // The version the course pointed at, though unreadable, stays as it was.
        if let Some(pointed) = &passed_over {
            let source = note.join("versions").join(pointed);
            let name = format!("damaged-version-{pointed}");
            let destination = if taken(&name) {
                edited_name(&name, taken)
            } else {
                name
            };
            for (relative, kind) in walk(&source)? {
                copy_entry(
                    &source.join(&relative),
                    &staging.join(&destination).join(relative),
                    kind,
                )?;
            }
            std::fs::create_dir_all(staging.join(&destination))?;
        }
        manifest.schema = MANIFEST_SCHEMA;
        manifest.publication = publication.clone();
        atomic_write_shared(
            &staging.join("manifest.json"),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        sync_tree_dirs(&staging)?;
        fail_point(FailPoint::Staged)
    })();
    if let Err(error) = built {
        if !simulated_crash(&error) {
            let _ = remove_tree(&staging);
        }
        return Err(disk_full(error));
    }
    // The flat note carries the lock too, held across the swap: the older app's
    // path to it then reaches this held lock, not a free one.
    let flat_lock = crate::runtime::lock_file(&staging.join(LEGACY_PUBLISH_LOCK))?;
    // Windows refuses to rename a folder with a file open inside it.
    #[cfg(windows)]
    drop((older_app, flat_lock));
    swap_in(note, &staging, "flatten", &publication)?;
    // Not part of the flat note: removed while still held, then let go.
    let _ = std::fs::remove_file(note.join(LEGACY_PUBLISH_LOCK));
    // Already let go on Windows, above.
    #[cfg(not(windows))]
    drop((older_app, flat_lock));
    Ok(Some(manifest))
}

/// The lock an older app holds in a course while it publishes a version.
const LEGACY_PUBLISH_LOCK: &str = ".publish.lock";

/// Takes the lock an older app holds while it publishes a version, waiting for a
/// publication under way to finish. A lock that stays held is told apart from one
/// that cannot be opened.
fn take_legacy_lock(note: &Path) -> Result<crate::runtime::FileLock> {
    let path = note.join(LEGACY_PUBLISH_LOCK);
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match crate::runtime::try_lock_file(&path) {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(None) => anyhow::bail!(tr!(
                "旧版应用正在保存这份笔记，请等它完成后重试 / An older version of the app is saving this note; try again when it finishes"
            )),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem
                ) =>
            {
                return Err(anyhow::Error::new(error).context(tr!(
                    "没有权限打开这份笔记的保存锁 {}，请检查这个文件夹的权限后重试 / No permission to open the note's save lock {}; check the folder's permissions and try again",
                    path.display(),
                    path.display()
                )));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context(tr!(
                    "无法打开这份笔记的保存锁 {} / Could not open the note's save lock {}",
                    path.display(),
                    path.display()
                )));
            }
        }
    }
}

/// Removes the empty lock a flatten interrupted after its swap left in the flat
/// note, or an older app left there, unless someone holds it. A course in the
/// layout of versions keeps it: it is the older app's.
pub fn release_legacy_lock(note_dir: &Path) {
    let path = note_dir.join(LEGACY_PUBLISH_LOCK);
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return;
    };
    if !metadata.is_file()
        || metadata.len() > 0
        || is_legacy(note_dir)
        || note_dir.join("current.json").exists()
    {
        return;
    }
    if let Ok(Some(lock)) = crate::runtime::try_lock_file(&path) {
        #[cfg(windows)]
        drop(lock);
        let _ = std::fs::remove_file(&path);
        #[cfg(not(windows))]
        drop(lock);
    }
}

/// A schema 1 course in which no version has a readable body. Flattening
/// leaves it as it is; a new note can still be published beside its versions.
#[derive(Debug)]
pub struct NothingReadable;

impl std::fmt::Display for NothingReadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(tr!(
            "这份笔记的各个版本都已损坏，已原样保留 / Every version of this note is damaged; it was left as it is"
        ))
    }
}

impl std::error::Error for NothingReadable {}

/// The version a schema 1 course reads, as the reader of that app read it: the one
/// `current.json` names whenever its body is readable, whatever else in it was
/// changed by hand; otherwise the newest version with a readable body. Also
/// returns the pointed version when it was passed over, so it is kept.
fn current_version(note: &Path) -> Result<(String, Manifest, Option<String>)> {
    let readable = |version: &str| -> Result<Manifest> {
        let mut parts = Path::new(version).components();
        anyhow::ensure!(
            matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none(),
            tr!("笔记位置无效 / Invalid note location")
        );
        let dir = note.join("versions").join(version);
        let manifest = read_manifest(&dir.join("manifest.json"))?;
        let body: Document =
            serde_json::from_slice(&std::fs::read(safe_asset_path(&dir, &manifest.document)?)?)?;
        anyhow::ensure!(
            has_readable_body(&body.sections),
            tr!("笔记没有可读正文 / Note has no readable body")
        );
        Ok(manifest)
    };
    let pointed = match read_current(&note.join("current.json")) {
        Ok(current) => Some(current.version_id),
        Err(error) if error.is::<NewerSchema>() => return Err(error),
        Err(_) => None,
    };
    if let Some(version) = &pointed {
        match readable(version) {
            Ok(manifest) => return Ok((version.clone(), manifest, None)),
            Err(error) if error.is::<NewerSchema>() => return Err(error),
            Err(_) => {}
        }
    }
    let mut newest: Option<(String, Manifest)> = None;
    if note.join("versions").is_dir() {
        for entry in std::fs::read_dir(note.join("versions"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !entry.file_type()?.is_dir() {
                continue;
            }
            match readable(&name) {
                Ok(manifest) => {
                    let key = |m: &Manifest| (m.revision, m.created_at_ms);
                    if newest.as_ref().is_none_or(|(_, n)| key(&manifest) > key(n)) {
                        newest = Some((name, manifest));
                    }
                }
                Err(error) if error.is::<NewerSchema>() => return Err(error),
                Err(_) => {}
            }
        }
    }
    let (version, manifest) = newest.ok_or(NothingReadable)?;
    // A pointed version that is passed over is never deleted.
    let passed_over = pointed
        .filter(|pointed| pointed != &version && note.join("versions").join(pointed).is_dir());
    Ok((version, manifest, passed_over))
}

/// `exports/course.html` labelled `task` → `exports/course.task.html`, or with a
/// number when that is taken too.
fn labelled_name(relative: &str, label: &str, taken: impl Fn(&str) -> bool) -> String {
    let (head, extension) = match relative.rsplit_once('.') {
        Some((head, extension)) if !head.ends_with('/') => (head, format!(".{extension}")),
        _ => (relative, String::new()),
    };
    (1..)
        .map(|n| match n {
            1 => format!("{head}.{label}{extension}"),
            n => format!("{head}.{label}-{n}{extension}"),
        })
        .find(|name| !taken(name))
        .expect("an unused name")
}

/// One export written into the note.
#[derive(Debug, Clone)]
pub struct ExportWrite {
    /// The export, `<note>/exports/<name>`.
    pub path: PathBuf,
    /// Where the file it replaced was kept, relative to the note, when that file
    /// was not the note's own (added or edited by hand).
    pub kept_edit: Option<String>,
}

/// Writes `bytes` as the note's export in `format`, atomically, and lists it in
/// the note's manifest as the note's own file. A file already there that is the
/// note's own, or has these bytes, is replaced; any other is kept beside it under
/// an `.edited` name.
pub fn write_export(
    note_dir: &Path,
    format: crate::config::OutputFormat,
    bytes: &[u8],
) -> Result<ExportWrite> {
    let note_dir = &resolve_note(note_dir)?;
    let _lock = lock_note(note_dir)?;
    recover_locked(note_dir)?;
    let mut manifest =
        note_manifest(note_dir)?.context(tr!("笔记清单损坏 / Invalid note manifest"))?;
    let relative = format!("exports/{}", crate::portable::file_name(format));
    let path = note_dir.join(&relative);
    let digest = crate::digest::digest(bytes);
    let mut kept_edit = None;
    if path.symlink_metadata().is_ok() {
        let listed = manifest.assets.iter().find(|asset| asset.path == relative);
        let current = file_digest(&path).ok();
        let own = current.as_deref() == Some(digest.as_str())
            || listed.is_some_and(|asset| current.as_deref() == Some(asset.sha256.as_str()));
        if !own {
            let kept = edited_name(&relative, |name| {
                note_dir.join(name).symlink_metadata().is_ok()
            });
            std::fs::rename(&path, note_dir.join(&kept))?;
            kept_edit = Some(kept);
        }
    }
    atomic_write_shared(&path, bytes).map_err(disk_full)?;
    sync_dir(&note_dir.join("exports"))?;
    manifest.assets.retain(|asset| asset.path != relative);
    manifest.assets.push(Asset {
        path: relative.clone(),
        bytes: bytes.len() as u64,
        sha256: digest,
    });
    manifest.assets.sort_by(|a, b| a.path.cmp(&b.path));
    if !manifest.outputs.contains(&relative) {
        manifest.outputs.push(relative);
        manifest.outputs.sort();
    }
    manifest
        .outcomes
        .exports
        .insert(format.to_string(), Outcome::succeeded());
    manifest.partial = manifest.outcomes.is_partial();
    manifest.schema = MANIFEST_SCHEMA;
    atomic_write_shared(
        &note_dir.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    sync_dir(note_dir)?;
    Ok(ExportWrite { path, kept_edit })
}

pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Points where tests stop a publication as a crash would.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
enum FailPoint {
    Staged,
    BetweenRenames,
    AfterRenames,
    BeforeDeletingOld,
}

#[cfg(test)]
thread_local! {
    static CRASH_AT: std::cell::Cell<Option<FailPoint>> = const { std::cell::Cell::new(None) };
}

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
struct SimulatedCrash;

impl std::fmt::Display for SimulatedCrash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("simulated crash")
    }
}

impl std::error::Error for SimulatedCrash {}

#[cfg(test)]
fn fail_point(point: FailPoint) -> Result<()> {
    if CRASH_AT.with(|crash| crash.get()) == Some(point) {
        CRASH_AT.with(|crash| crash.set(None));
        return Err(SimulatedCrash.into());
    }
    Ok(())
}

#[cfg(not(test))]
#[inline]
fn fail_point(_: FailPoint) -> Result<()> {
    Ok(())
}

/// A crash leaves its state behind: nothing is cleaned up after a simulated one.
fn simulated_crash(error: &anyhow::Error) -> bool {
    error.is::<SimulatedCrash>()
}

#[cfg(test)]
mod tests;
