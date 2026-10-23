//! Verified library copies. Registry publication is owned by the UI transaction.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Identity marker written at the root of every registered library.
pub const LIBRARY_ID_FILE: &str = ".course2md-library-id";
/// Task bindings rewrite this file when a verified copy is relocated.
const TASK_IDENTITY_FILE: &str = "task-identity.json";
const MOVE_MARKER_FILE: &str = ".course2md-move.json";
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRecord {
    pub path: PathBuf,
    pub bytes: u64,
    pub digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupRecord {
    pub id: String,
    pub library_id: String,
    pub path: PathBuf,
    pub current_root: PathBuf,
    pub created: u64,
    pub verified: bool,
    pub journal_path: PathBuf,
}

impl BackupRecord {
    /// The verification journal of this backup, rejected when it describes a
    /// different move.
    pub fn load_journal(&self) -> Result<Journal> {
        let journal: Journal = serde_json::from_slice(
            &std::fs::read(&self.journal_path)
                .context(course2md::tr!("缺少这份备份的核验记录，未清理任何文件"))?,
        )?;
        ensure!(
            journal.id == self.id
                && journal.source == self.path
                && journal.library_id == self.library_id,
            course2md::tr!("备份与核验记录不一致，未清理任何文件")
        );
        Ok(journal)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Copying,
    Verified,
    Committed,
    Failed,
    Abandoned,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Journal {
    pub schema: u32,
    pub id: String,
    pub library_id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub created: u64,
    pub phase: Phase,
    pub files: Vec<FileRecord>,
    #[serde(default)]
    pub directories: Vec<PathBuf>,
    #[serde(default)]
    pub temporary_files: Vec<PathBuf>,
    #[serde(default)]
    pub cleanup_started: bool,
    #[serde(default)]
    pub cleanup_complete: bool,
    #[serde(default)]
    pub settings_relocated: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub message: String,
    pub completed: u64,
    pub total: u64,
}

pub struct PreparedMove {
    pub journal: Journal,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryStamp {
    canonical: PathBuf,
    // Keep the Windows handle alive so its volume/file identity cannot be
    // recycled while the user is confirming this directory.
    #[cfg(windows)]
    identity: std::sync::Arc<same_file::Handle>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

/// Keep an association confirmation attached to the directory the user reviewed,
/// even if a removable disk or a directory at that path changes meanwhile.
pub fn directory_stamp(path: &Path) -> Result<DirectoryStamp> {
    let canonical = std::fs::canonicalize(path).context(course2md::tr!("保存位置暂时不可访问"))?;
    #[cfg(windows)]
    let identity = std::sync::Arc::new(same_file::Handle::from_path(&canonical)?);
    #[cfg(windows)]
    let metadata = identity.as_file().metadata()?;
    #[cfg(not(windows))]
    let metadata = std::fs::metadata(&canonical)?;
    ensure!(metadata.is_dir(), course2md::tr!("保存位置不是文件夹"));
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(DirectoryStamp {
        canonical,
        #[cfg(windows)]
        identity,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum LibraryCoverage {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Clone)]
pub struct LibraryAccess {
    pub available: Vec<PathBuf>,
    pub unavailable: Vec<PathBuf>,
}

impl LibraryAccess {
    pub fn coverage(&self) -> LibraryCoverage {
        if self.available.is_empty() {
            LibraryCoverage::Unavailable
        } else if self.unavailable.is_empty() {
            LibraryCoverage::Complete
        } else {
            LibraryCoverage::Partial
        }
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        course2md::tr!("已取消移动，原笔记库仍保留")
    );
    Ok(())
}
fn ignored(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some(".DS_Store" | "Thumbs.db" | MOVE_MARKER_FILE)
    )
}

pub fn validate_destination(source: &Path, destination: &Path) -> Result<(PathBuf, PathBuf)> {
    let source = std::fs::canonicalize(source)
        .context(course2md::tr!("原笔记库无法访问，请重新连接这个保存位置"))?;
    let destination = std::fs::canonicalize(destination)
        .context(course2md::tr!("目标文件夹无法访问，请选择已有的空文件夹"))?;
    ensure!(
        source.is_dir() && destination.is_dir(),
        course2md::tr!("请选择文件夹作为笔记库位置")
    );
    ensure!(
        source != destination
            && !source.starts_with(&destination)
            && !destination.starts_with(&source),
        course2md::tr!("目标不能是原笔记库、它的父文件夹或子文件夹")
    );
    ensure!(
        std::fs::read_dir(&destination)?.next().is_none(),
        course2md::tr!("目标文件夹不是空的，请选择空文件夹")
    );
    Ok((source, destination))
}

fn walk(
    root: &Path,
    dir: &Path,
    files: &mut Vec<PathBuf>,
    directories: &mut Vec<PathBuf>,
    cancel: &AtomicBool,
) -> Result<()> {
    check_cancel(cancel)?;
    for entry in
        std::fs::read_dir(dir).with_context(|| course2md::tr!("无法读取 {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if ignored(&path) {
            continue;
        }
        let relative = path.strip_prefix(root)?.to_path_buf();
        ensure!(
            relative.components().count() <= MAX_DEPTH,
            course2md::tr!("笔记库目录层级过深，请检查 {}", path.display())
        );
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            course2md::tr!(
                "笔记库包含符号链接，尚未移动：{}。请先把需要的实际文件放入笔记库。",
                path.display()
            )
        );
        if kind.is_dir() {
            directories.push(relative);
            walk(root, &path, files, directories, cancel)?;
        } else {
            ensure!(
                kind.is_file(),
                course2md::tr!("笔记库包含无法复制的特殊文件：{}", path.display())
            );
            files.push(relative);
        }
    }
    Ok(())
}

fn digest(path: &Path, cancel: &AtomicBool) -> Result<String> {
    course2md::fetch::local_content_identity(path, cancel)
        .with_context(|| course2md::tr!("无法校验 {}", path.display()))
}

fn inventory(
    root: &Path,
    message: &str,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<(Vec<FileRecord>, Vec<PathBuf>)> {
    let mut paths = Vec::new();
    let mut directories = Vec::new();
    walk(root, root, &mut paths, &mut directories, cancel)?;
    paths.sort();
    directories.sort();
    let total = paths.len() as u64;
    let mut files = Vec::new();
    for path in paths {
        check_cancel(cancel)?;
        let absolute = root.join(&path);
        let bytes = std::fs::metadata(&absolute)?.len();
        let hash = digest(&absolute, cancel)?;
        files.push(FileRecord {
            path,
            bytes,
            digest: hash,
        });
        progress(Progress {
            message: message.into(),
            completed: files.len() as u64,
            total,
        });
    }
    Ok((files, directories))
}

pub fn save_journal(path: &Path, journal: &Journal) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    course2md::checkpoint::atomic_write(path, &serde_json::to_vec_pretty(journal)?)?;
    sync_directory(
        path.parent()
            .context(course2md::tr!("移动记录缺少保存目录"))?,
    )
}

/// Makes a directory entry durable; Windows has no directory fsync.
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn valid_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().count() <= MAX_DEPTH
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn validate_journal_paths(journal: &Journal) -> Result<()> {
    let mut paths = std::collections::BTreeSet::new();
    ensure!(
        journal.files.iter().all(|file| valid_relative(&file.path)
            && !ignored(&file.path)
            && paths.insert(file.path.clone()))
            && journal
                .directories
                .iter()
                .all(|path| valid_relative(path) && !ignored(path) && paths.insert(path.clone()))
            && journal
                .temporary_files
                .iter()
                .all(|path| valid_relative(path)
                    && paths.insert(path.clone())
                    && path.file_name().is_some_and(|name| name
                        .to_string_lossy()
                        .starts_with(".course2md-copy-"))),
        course2md::tr!("移动记录包含无效文件位置，未修改任何内容")
    );
    Ok(())
}

/// One directory scan with two outcomes: readable pending journals (sorted by
/// creation time) and corrupt records. Committed/abandoned journals and other
/// schemas stay invisible, matching the old separate scans; a half-broken
/// journal from a crash remains visible instead of being silently skipped,
/// consistent with the "keep the original file" recovery policy.
pub fn scan_journals(directory: &Path) -> (Vec<(PathBuf, Journal)>, Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return (Vec::new(), Vec::new());
    };
    let mut pending = Vec::new();
    let mut corrupt = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Journal>(&bytes) {
                Ok(journal)
                    if journal.schema == 1
                        && !matches!(journal.phase, Phase::Committed | Phase::Abandoned) =>
                {
                    pending.push((path, journal))
                }
                Ok(_) => {}
                Err(_) => corrupt.push(path),
            },
            Err(_) => corrupt.push(path),
        }
    }
    pending.sort_by_key(|(_, journal)| journal.created);
    (pending, corrupt)
}

pub fn prepare_move(
    library_id: String,
    source: &Path,
    destination: &Path,
    journal_dir: &Path,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<PreparedMove> {
    let (source, destination) = validate_destination(source, destination)?;
    let (files, directories) = inventory(
        &source,
        course2md::tr!("正在检查原笔记库…"),
        cancel,
        progress,
    )?;
    ensure!(
        std::fs::read_dir(&destination)?.next().is_none(),
        course2md::tr!("目标文件夹在准备期间出现了内容，请选择另一个空文件夹")
    );
    let journal = Journal {
        schema: 1,
        id: crate::workspace::new_id("move"),
        library_id,
        source,
        destination,
        created: crate::workspace::now(),
        phase: Phase::Copying,
        files,
        directories,
        temporary_files: Vec::new(),
        cleanup_started: false,
        cleanup_complete: false,
        settings_relocated: false,
        error: None,
    };
    let path = journal_dir.join(format!("{}.json", journal.id));
    save_journal(&path, &journal)?;
    // The ownership marker makes a partial destination recognizable after a crash.
    // Publish it exclusively: another process must not be able to claim the
    // same empty directory and have its marker silently overwritten.
    let mut marker = tempfile::Builder::new()
        .prefix(".course2md-owner-")
        .tempfile_in(&journal.destination)?;
    marker.write_all(&serde_json::to_vec(&journal)?)?;
    marker.as_file().sync_all()?;
    marker
        .persist_noclobber(journal.destination.join(MOVE_MARKER_FILE))
        .map_err(|error| error.error)
        .context(course2md::tr!("目标已被其他操作使用，原笔记库尚未改变"))?;
    sync_directory(&journal.destination)?;
    // The inventory above is the one just recorded; only a resumed copy has to
    // check that the source still matches it.
    copy_journal(path, journal, false, cancel, progress)
}

pub fn read_journal(path: &Path) -> Result<Journal> {
    serde_json::from_slice(&std::fs::read(path)?).context(course2md::tr!("移动记录无法读取"))
}

/// Marks an unfinished move as given up; the copy and the original both stay.
pub fn abandon_move(path: &Path) -> Result<()> {
    let mut journal = read_journal(path)?;
    journal.phase = Phase::Abandoned;
    save_journal(path, &journal)
}

/// What the move records on disk say, read once at startup and after each
/// storage operation.
#[derive(Default)]
pub struct Records {
    /// Moves that stopped before the new location was published.
    pub pending: Vec<(PathBuf, Journal)>,
    pub corrupt: Vec<PathBuf>,
    /// Published moves whose model paths have not been updated yet.
    pub settings_pending: Vec<(PathBuf, Journal)>,
    /// Backups whose cleanup already finished.
    pub cleaned: Vec<String>,
}

/// `libraries` are the registered (id, root) pairs.
pub fn read_records(
    directory: &Path,
    libraries: &[(String, PathBuf)],
    backups: &[BackupRecord],
) -> Records {
    let (mut pending, corrupt) = scan_journals(directory);
    // A registry update that committed before the app closed leaves its record
    // merely behind; never offer to replay or undo that copy.
    pending.retain(|(path, journal)| {
        let committed = libraries
            .iter()
            .any(|(id, root)| *id == journal.library_id && *root == journal.destination);
        if committed {
            let mut journal = journal.clone();
            journal.phase = Phase::Committed;
            let _ = save_journal(path, &journal);
        }
        !committed
    });
    let mut records = Records {
        pending,
        corrupt,
        ..Records::default()
    };
    for backup in backups {
        let Ok(journal) = backup.load_journal() else {
            continue;
        };
        if journal.cleanup_complete {
            records.cleaned.push(backup.id.clone());
        }
        if !journal.settings_relocated {
            records
                .settings_pending
                .push((backup.journal_path.clone(), journal));
        }
    }
    records
}

pub fn resume_move(
    path: &Path,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<PreparedMove> {
    let journal = read_journal(path)?;
    ensure!(
        journal.schema == 1 && !matches!(journal.phase, Phase::Committed | Phase::Abandoned),
        course2md::tr!("这份移动记录不能继续")
    );
    ensure!(
        std::fs::canonicalize(&journal.source)? == journal.source
            && std::fs::canonicalize(&journal.destination)? == journal.destination,
        course2md::tr!("移动位置已经变化，原数据和副本已保留")
    );
    let marker: Journal = serde_json::from_slice(
        &std::fs::read(journal.destination.join(MOVE_MARKER_FILE))
            .context(course2md::tr!("目标缺少这次移动的识别记录，未修改目标内容"))?,
    )?;
    ensure!(
        marker.id == journal.id
            && marker.source == journal.source
            && marker.destination == journal.destination
            && marker.files == journal.files,
        course2md::tr!("目标不属于这次移动，未修改目标内容")
    );
    copy_journal(path.to_owned(), journal, true, cancel, progress)
}

fn copy_journal(
    path: PathBuf,
    mut journal: Journal,
    resumed: bool,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<PreparedMove> {
    validate_journal_paths(&journal)?;
    let mut copy = || -> Result<()> {
        // Validate the whole target before following any parent of a recorded
        // temporary file; a replaced parent must never lead outside the copy.
        let mut existing_files = Vec::new();
        let mut existing_dirs = Vec::new();
        walk(
            &journal.destination,
            &journal.destination,
            &mut existing_files,
            &mut existing_dirs,
            cancel,
        )?;
        ensure!(
            existing_files
                .iter()
                .all(|path| journal.temporary_files.contains(path)
                    || journal.files.iter().any(|record| &record.path == path))
                && existing_dirs
                    .iter()
                    .all(|path| journal.directories.contains(path)),
            course2md::tr!("目标副本出现了非预期内容，未覆盖任何文件")
        );
        for temporary in &journal.temporary_files {
            let owned = journal.destination.join(temporary);
            if owned.exists() {
                ensure!(
                    std::fs::symlink_metadata(&owned)?.is_file()
                        && owned.file_name().is_some_and(|name| name
                            .to_string_lossy()
                            .starts_with(".course2md-copy-")),
                    course2md::tr!("移动临时文件位置发生变化，未修改内容")
                );
                std::fs::remove_file(owned)?;
            }
        }
        journal.temporary_files.clear();
        if resumed {
            let (current, directories) = inventory(
                &journal.source,
                course2md::tr!("正在检查原笔记库…"),
                cancel,
                progress,
            )?;
            ensure!(
                current == journal.files && directories == journal.directories,
                course2md::tr!(
                    "原笔记库在上次移动后发生变化，尚未切换位置。请保留这份副本，并重新选择空文件夹移动。"
                )
            );
        }
        for directory in &journal.directories {
            std::fs::create_dir_all(journal.destination.join(directory))?;
        }
        let total = journal.files.iter().map(|file| file.bytes).sum();
        let mut completed = 0;
        let mut buffer = vec![0; 1024 * 1024];
        for index in 0..journal.files.len() {
            let record = &journal.files[index];
            check_cancel(cancel)?;
            let source = journal.source.join(&record.path);
            let destination = journal.destination.join(&record.path);
            if destination.exists() {
                ensure!(
                    std::fs::symlink_metadata(&destination)?.is_file(),
                    course2md::tr!(
                        "目标出现非预期文件，原文件已保留：{}",
                        destination.display()
                    )
                );
                // A previous verified relocation may already have changed task bindings.
                if digest(&destination, cancel)? != record.digest
                    && record
                        .path
                        .file_name()
                        .is_none_or(|name| name != TASK_IDENTITY_FILE)
                {
                    anyhow::bail!(
                        "{}",
                        course2md::tr!("目标副本已被修改，未覆盖：{}", destination.display())
                    );
                }
            } else {
                let parent = destination
                    .parent()
                    .context(course2md::tr!("目标文件缺少目录"))?;
                std::fs::create_dir_all(parent)?;
                let mut input = std::fs::File::open(&source)?;
                let mut output = tempfile::Builder::new()
                    .prefix(".course2md-copy-")
                    .tempfile_in(parent)?;
                journal.temporary_files.push(
                    output
                        .path()
                        .strip_prefix(&journal.destination)?
                        .to_path_buf(),
                );
                save_journal(&path, &journal)?;
                let message = course2md::tr!("正在复制 {}", record.path.display());
                let mut copied = 0;
                loop {
                    check_cancel(cancel)?;
                    let count = input.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    output.write_all(&buffer[..count])?;
                    copied += count as u64;
                    progress(Progress {
                        message: message.clone(),
                        completed: completed + copied,
                        total,
                    });
                }
                output.as_file().sync_all()?;
                ensure!(
                    digest(output.path(), cancel)? == record.digest,
                    course2md::tr!(
                        "复制内容未通过校验，尚未切换笔记库：{}",
                        record.path.display()
                    )
                );
                std::fs::set_permissions(output.path(), std::fs::metadata(&source)?.permissions())?;
                output
                    .persist_noclobber(&destination)
                    .map_err(|error| error.error)
                    .with_context(|| {
                        course2md::tr!("无法保存移动副本 {}", destination.display())
                    })?;
            }
            completed += record.bytes;
            progress(Progress {
                message: course2md::tr!("正在复制笔记库…").into(),
                completed,
                total,
            });
        }
        check_cancel(cancel)?;
        let (source_files, source_dirs) = inventory(
            &journal.source,
            course2md::tr!("正在检查原笔记库…"),
            cancel,
            progress,
        )?;
        ensure!(
            source_files == journal.files && source_dirs == journal.directories,
            course2md::tr!("原笔记库在复制期间发生变化，尚未切换保存位置")
        );
        course2md::execution::relocate_work_bindings(&journal.source, &journal.destination)?;
        let (target_files, target_dirs) = inventory(
            &journal.destination,
            course2md::tr!("正在核验副本…"),
            cancel,
            progress,
        )?;
        ensure!(
            target_dirs == journal.directories && target_files.len() == journal.files.len(),
            course2md::tr!("目标副本包含非预期内容，尚未切换保存位置")
        );
        for (original, target) in journal.files.iter().zip(&target_files) {
            ensure!(
                original.path == target.path
                    && (original.digest == target.digest
                        || original
                            .path
                            .file_name()
                            .is_some_and(|name| name == TASK_IDENTITY_FILE)),
                course2md::tr!(
                    "目标副本未通过校验，尚未切换保存位置：{}",
                    original.path.display()
                )
            );
        }
        for directory in journal.directories.iter().rev() {
            sync_directory(&journal.destination.join(directory))?;
        }
        sync_directory(&journal.destination)?;
        journal.temporary_files.clear();
        Ok(())
    };
    if let Err(error) = copy() {
        journal.phase = Phase::Failed;
        journal.error = Some(format!("{error:#}"));
        let _ = save_journal(&path, &journal);
        return Err(error);
    }
    journal.phase = Phase::Verified;
    journal.error = None;
    save_journal(&path, &journal)?;
    Ok(PreparedMove { journal, path })
}

pub fn relocate_path(path: &mut PathBuf, source: &Path, destination: &Path) {
    if path.is_absolute()
        && let Ok(relative) = path.strip_prefix(source)
    {
        *path = destination.join(relative);
    }
}

pub fn cleanup_backup(
    backup: &BackupRecord,
    active_roots: &[PathBuf],
    cancel: &AtomicBool,
) -> Result<()> {
    ensure!(
        backup.verified,
        course2md::tr!("这份备份尚未核验，不能清理")
    );
    let mut journal = backup.load_journal()?;
    if journal.cleanup_complete {
        return Ok(());
    }
    validate_journal_paths(&journal)?;
    let path = match std::fs::canonicalize(&backup.path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && journal.cleanup_started => {
            // A crash after deletion but before the completion marker must not
            // leave a permanent, unusable backup row in the app.
            journal.cleanup_complete = true;
            return save_journal(&backup.journal_path, &journal);
        }
        Err(error) => return Err(error).context(course2md::tr!("旧位置备份已移动或无法访问")),
    };
    ensure!(
        path == backup.path,
        course2md::tr!("旧位置现在指向其他位置，未清理任何文件")
    );
    for root in active_roots {
        let root = std::fs::canonicalize(root)?;
        ensure!(
            !root.starts_with(&path) && !path.starts_with(&root),
            course2md::tr!("这个位置仍被笔记库使用，不能作为备份清理")
        );
    }
    let (files, directories) = inventory(&path, course2md::tr!("正在检查备份…"), cancel, &|_| {})?;
    let unchanged = if journal.cleanup_started {
        files.iter().all(|file| journal.files.contains(file))
            && directories
                .iter()
                .all(|path| journal.directories.contains(path))
    } else {
        files == journal.files && directories == journal.directories
    };
    ensure!(
        unchanged,
        course2md::tr!(
            "旧位置备份在移动后发生变化，未清理任何内容。请打开该位置检查新增或修改的文件。"
        )
    );
    check_cancel(cancel)?;
    journal.cleanup_started = true;
    save_journal(&backup.journal_path, &journal)?;
    std::fs::remove_dir_all(&path).context(course2md::tr!("旧位置备份尚未清理完成，请重试"))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    journal.cleanup_complete = true;
    save_journal(&backup.journal_path, &journal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn association_confirmation_detects_a_replaced_directory() {
        let temp = tempfile::tempdir().unwrap();
        let location = temp.path().join("library");
        std::fs::create_dir(&location).unwrap();
        std::fs::write(location.join("note.md"), "saved notes").unwrap();
        let stamp = directory_stamp(&location).unwrap();
        assert_eq!(stamp, directory_stamp(&location).unwrap());
        std::fs::write(location.join("another-note.md"), "new notes").unwrap();
        assert_eq!(stamp, directory_stamp(&location).unwrap());
        std::fs::rename(&location, temp.path().join("original-library")).unwrap();
        std::fs::create_dir(&location).unwrap();
        assert_ne!(stamp, directory_stamp(&location).unwrap());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("original-library/note.md")).unwrap(),
            "saved notes"
        );
    }
    #[test]
    fn rejects_same_nested_nonempty_and_unknown_targets() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        assert!(validate_destination(&source, &source).is_err());
        assert!(validate_destination(&source, &source.join("nested")).is_err());
        assert!(validate_destination(&source, dir.path()).is_err());
        assert!(validate_destination(&source, &empty).is_ok());
        std::fs::write(empty.join("existing.txt"), "keep").unwrap();
        assert!(validate_destination(&source, &empty).is_err());
    }
    #[test]
    fn complete_copy_is_verified_before_publication_and_old_files_survive() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::create_dir_all(old.join("note/empty")).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("note/text.md"), "valuable notes 世界").unwrap();
        let prepared = prepare_move(
            "library".into(),
            &old,
            &new,
            &dir.path().join("journal"),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        assert_eq!(prepared.journal.phase, Phase::Verified);
        assert_eq!(
            std::fs::read(old.join("note/text.md")).unwrap(),
            std::fs::read(new.join("note/text.md")).unwrap()
        );
        assert!(new.join("note/empty").is_dir());
        let backup = BackupRecord {
            id: prepared.journal.id.clone(),
            library_id: "library".into(),
            path: std::fs::canonicalize(&old).unwrap(),
            current_root: new.clone(),
            created: 0,
            verified: true,
            journal_path: prepared.path,
        };
        std::fs::write(old.join("later.md"), "user addition").unwrap();
        assert!(cleanup_backup(&backup, &[new], &AtomicBool::new(false)).is_err());
        assert!(old.join("later.md").exists());
    }
    #[test]
    fn relocation_changes_only_paths_inside_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let new = directory.path().join("new");
        let external = directory.path().join("external/video.mp4");
        let mut outside = external.clone();
        let mut inside = old.join("work/task");
        relocate_path(&mut outside, &old, &new);
        relocate_path(&mut inside, &old, &new);
        assert_eq!(outside, external);
        assert_eq!(inside, new.join("work/task"));
    }

    fn fixture() -> (tempfile::TempDir, PreparedMove) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old");
        let destination = dir.path().join("new");
        std::fs::create_dir_all(source.join("notes/empty")).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("notes/course.md"), "user-edited notes").unwrap();
        let prepared = prepare_move(
            "library".into(),
            &source,
            &destination,
            &dir.path().join("journals"),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        (dir, prepared)
    }

    #[test]
    fn resumed_copy_removes_only_recorded_temporary_files_and_keeps_originals() {
        let (_dir, mut prepared) = fixture();
        let temporary = PathBuf::from("notes/.course2md-copy-interrupted");
        std::fs::remove_file(prepared.journal.destination.join("notes/course.md")).unwrap();
        std::fs::write(
            prepared.journal.destination.join(&temporary),
            "partial copy",
        )
        .unwrap();
        prepared.journal.temporary_files.push(temporary.clone());
        prepared.journal.phase = Phase::Copying;
        save_journal(&prepared.path, &prepared.journal).unwrap();
        let resumed = resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(resumed.journal.phase, Phase::Verified);
        assert!(!resumed.journal.destination.join(temporary).exists());
        assert_eq!(
            std::fs::read(resumed.journal.destination.join("notes/course.md")).unwrap(),
            std::fs::read(resumed.journal.source.join("notes/course.md")).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resumed_copy_rejects_replaced_parents_before_cleaning_temporary_files() {
        let (dir, mut prepared) = fixture();
        let external = dir.path().join("external");
        std::fs::create_dir(&external).unwrap();
        let valuable = external.join(".course2md-copy-interrupted");
        std::fs::write(&valuable, "keep this external file").unwrap();
        std::fs::remove_dir_all(prepared.journal.destination.join("notes")).unwrap();
        std::os::unix::fs::symlink(&external, prepared.journal.destination.join("notes")).unwrap();
        prepared
            .journal
            .temporary_files
            .push(PathBuf::from("notes/.course2md-copy-interrupted"));
        save_journal(&prepared.path, &prepared.journal).unwrap();
        assert!(resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).is_err());
        assert_eq!(
            std::fs::read_to_string(valuable).unwrap(),
            "keep this external file"
        );
        assert!(prepared.journal.source.join("notes/course.md").is_file());
    }

    #[test]
    fn resumed_copy_does_not_overwrite_changes_to_source_or_destination() {
        let (_dir, prepared) = fixture();
        let target = prepared.journal.destination.join("notes/course.md");
        std::fs::write(&target, "new user edit in target").unwrap();
        assert!(resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "new user edit in target"
        );
        std::fs::write(&target, "user-edited notes").unwrap();
        let original = prepared.journal.source.join("notes/course.md");
        std::fs::write(&original, "new user edit in original").unwrap();
        assert!(resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).is_err());
        assert_eq!(
            std::fs::read_to_string(target).unwrap(),
            "user-edited notes"
        );
        assert_eq!(
            std::fs::read_to_string(original).unwrap(),
            "new user edit in original"
        );
    }

    #[test]
    fn cleanup_is_recoverable_and_never_deletes_an_active_library() {
        let (_dir, mut prepared) = fixture();
        prepared.journal.phase = Phase::Committed;
        save_journal(&prepared.path, &prepared.journal).unwrap();
        let backup = BackupRecord {
            id: prepared.journal.id.clone(),
            library_id: "library".into(),
            path: prepared.journal.source.clone(),
            current_root: prepared.journal.destination.clone(),
            created: 0,
            verified: true,
            journal_path: prepared.path.clone(),
        };
        assert!(
            cleanup_backup(
                &backup,
                std::slice::from_ref(&backup.path),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert!(backup.path.join("notes/course.md").is_file());
        prepared.journal.cleanup_started = true;
        save_journal(&prepared.path, &prepared.journal).unwrap();
        std::fs::remove_dir_all(&backup.path).unwrap();
        cleanup_backup(
            &backup,
            std::slice::from_ref(&backup.current_root),
            &AtomicBool::new(false),
        )
        .unwrap();
        cleanup_backup(
            &backup,
            std::slice::from_ref(&backup.current_root),
            &AtomicBool::new(false),
        )
        .unwrap();
        let journal: Journal =
            serde_json::from_slice(&std::fs::read(prepared.path).unwrap()).unwrap();
        assert!(journal.cleanup_complete);
        assert!(backup.current_root.join("notes/course.md").is_file());
    }

    #[test]
    fn cancellation_leaves_a_resumable_copy_without_altering_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old");
        let destination = dir.path().join("new");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(source.join("large.bin"), vec![b'a'; 2 * 1024 * 1024]).unwrap();
        let cancel = AtomicBool::new(false);
        let journal_dir = dir.path().join("journals");
        let result = prepare_move(
            "library".into(),
            &source,
            &destination,
            &journal_dir,
            &cancel,
            &|progress| {
                if progress.message.starts_with("正在复制 ") {
                    cancel.store(true, Ordering::Relaxed);
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::metadata(source.join("large.bin")).unwrap().len(),
            2 * 1024 * 1024
        );
        let (pending, _) = scan_journals(&journal_dir);
        assert_eq!(pending.len(), 1);
        resume_move(&pending[0].0, &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(
            std::fs::read(source.join("large.bin")).unwrap(),
            std::fs::read(destination.join("large.bin")).unwrap()
        );
    }
}
