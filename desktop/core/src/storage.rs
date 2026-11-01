//! Library moves and links. A move copies the files and checks them by count and size; the
//! original stays as a backup until someone deletes it, and only a check that passes switches
//! the library. An interrupted move starts its copy over. Registry publication is owned by
//! the UI transaction.
use crate::workspace::{self, LibraryLocation, TaskRecord};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

/// Identity marker written at the root of every registered library.
pub const LIBRARY_ID_FILE: &str = ".course2md-library-id";
/// Task bindings rewrite this file when a verified copy is relocated.
const TASK_IDENTITY_FILE: &str = "task-identity.json";
const MOVE_MARKER_FILE: &str = ".course2md-move.json";
/// Every temporary copy starts with this.
const TEMPORARY_PREFIX: &str = ".course2md-copy-";
const MAX_DEPTH: usize = 64;

/// Older records also carry a digest; a move no longer needs it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FileRecord {
    pub path: PathBuf,
    pub bytes: u64,
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
    /// The record of the move that left this backup, rejected when it describes another.
    fn load_journal(&self) -> Result<Journal> {
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
    pub cleanup_started: bool,
    #[serde(default)]
    pub cleanup_complete: bool,
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

/// Keeps a confirmation attached to the directory the person reviewed, even if a removable
/// disk or a directory at that path changes meanwhile. The open handle keeps the directory's
/// identity from being recycled while they decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryStamp(Arc<same_file::Handle>);

pub fn directory_stamp(path: &Path) -> Result<DirectoryStamp> {
    let canonical = std::fs::canonicalize(path).context(course2md::tr!("保存位置暂时不可访问"))?;
    ensure!(canonical.is_dir(), course2md::tr!("保存位置不是文件夹"));
    Ok(DirectoryStamp(Arc::new(same_file::Handle::from_path(
        &canonical,
    )?)))
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

/// A library's folder at one moment: size and modification time per file, sorted.
#[derive(Default, PartialEq)]
struct Tree {
    files: Vec<(FileRecord, Option<SystemTime>)>,
    directories: Vec<PathBuf>,
}

impl Tree {
    /// Everything a move has to carry. A link or a special file stops it rather than
    /// being left behind, and a file this app put there for itself does not count.
    fn scan(root: &Path, cancel: &AtomicBool) -> Result<Self> {
        let mut tree = Self::default();
        let walk = walkdir::WalkDir::new(root).min_depth(1).into_iter();
        for entry in walk.filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(".DS_Store" | "Thumbs.db" | MOVE_MARKER_FILE)
            )
        }) {
            check_cancel(cancel)?;
            let entry = entry.with_context(|| course2md::tr!("无法读取 {}", root.display()))?;
            let path = entry.path();
            ensure!(
                entry.depth() <= MAX_DEPTH,
                course2md::tr!("笔记库目录层级过深，请检查 {}", path.display())
            );
            let relative = path.strip_prefix(root)?.to_path_buf();
            let kind = entry.file_type();
            ensure!(
                !kind.is_symlink(),
                course2md::tr!(
                    "笔记库包含符号链接，尚未移动：{}。请先把需要的实际文件放入笔记库。",
                    path.display()
                )
            );
            if kind.is_dir() {
                tree.directories.push(relative);
            } else {
                ensure!(
                    kind.is_file(),
                    course2md::tr!("笔记库包含无法复制的特殊文件：{}", path.display())
                );
                let metadata = entry.metadata()?;
                tree.files.push((
                    FileRecord {
                        path: relative,
                        bytes: metadata.len(),
                    },
                    metadata.modified().ok(),
                ));
            }
        }
        tree.files.sort_by(|a, b| a.0.path.cmp(&b.0.path));
        tree.directories.sort();
        Ok(tree)
    }

    fn records(&self) -> Vec<FileRecord> {
        self.files.iter().map(|(file, _)| file.clone()).collect()
    }
}

pub fn save_journal(path: &Path, journal: &Journal) -> Result<()> {
    course2md::checkpoint::atomic_write(path, &serde_json::to_vec_pretty(journal)?)
}

/// One directory scan with two outcomes: readable pending journals (sorted by
/// creation time) and corrupt records. A half-broken journal from a crash stays
/// visible instead of being silently skipped.
fn scan_journals(directory: &Path) -> (Vec<(PathBuf, Journal)>, Vec<PathBuf>) {
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
        match read_journal(&path) {
            Ok(journal)
                if journal.schema == 1
                    && !matches!(journal.phase, Phase::Committed | Phase::Abandoned) =>
            {
                pending.push((path, journal))
            }
            Ok(_) => {}
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
    let journal = Journal {
        schema: 1,
        id: crate::store::new_id("move"),
        library_id,
        source,
        destination,
        created: crate::store::now(),
        phase: Phase::Copying,
        files: Vec::new(),
        directories: Vec::new(),
        cleanup_started: false,
        cleanup_complete: false,
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
    copy_library(path, journal, cancel, progress)
}

pub fn read_journal(path: &Path) -> Result<Journal> {
    serde_json::from_slice(&std::fs::read(path)?).context(course2md::tr!("移动记录无法读取"))
}

/// Marks an unfinished move as given up; the copy and the original both stay.
pub fn abandon_move(path: &Path) -> Result<()> {
    let mut journal = read_journal(path)?;
    journal.phase = Phase::Abandoned;
    save_journal(path, &journal)?;
    // Without a resumable move the copy is an ordinary folder.
    let marker = journal.destination.join(MOVE_MARKER_FILE);
    if read_journal(&marker).is_ok_and(|owner| owner.id == journal.id) {
        let _ = std::fs::remove_file(marker);
    }
    Ok(())
}

/// What the move records on disk say, read once at startup and after each
/// storage operation.
#[derive(Default)]
pub struct Records {
    /// Moves that stopped before the new location was published.
    pub pending: Vec<(PathBuf, Journal)>,
    pub corrupt: Vec<PathBuf>,
    /// Backups whose cleanup already finished.
    pub cleaned: Vec<String>,
    /// Published moves whose records have not caught up; see `settle_records`.
    unsettled: Vec<(PathBuf, Journal)>,
    /// Ownership markers of moves that have been published.
    stale_markers: Vec<PathBuf>,
}

/// `libraries` are the registered (id, root) pairs.
pub fn read_records(
    directory: &Path,
    libraries: &[(String, PathBuf)],
    backups: &[BackupRecord],
) -> Records {
    let (pending, corrupt) = scan_journals(directory);
    // A registry update that committed before the app closed leaves its record
    // merely behind; never offer to replay or undo that copy.
    let published = |journal: &Journal| {
        libraries
            .iter()
            .any(|(id, root)| *id == journal.library_id && *root == journal.destination)
    };
    let (mut unsettled, pending): (Vec<_>, Vec<_>) = pending
        .into_iter()
        .partition(|(_, journal)| published(journal));
    for (_, journal) in &mut unsettled {
        journal.phase = Phase::Committed;
    }
    Records {
        pending,
        corrupt,
        cleaned: backups
            .iter()
            .filter(|backup| backup.load_journal().is_ok_and(|j| j.cleanup_complete))
            .map(|backup| backup.id.clone())
            .collect(),
        unsettled,
        // The marker identifies a partial copy for a restart; a published move has none.
        stale_markers: libraries
            .iter()
            .map(|(_, root)| root.join(MOVE_MARKER_FILE))
            .filter(|marker| read_journal(marker).is_ok_and(|owner| published(&owner)))
            .collect(),
    }
}

/// Writes what `read_records` found behind. Failures only repeat this at the next read.
pub fn settle_records(records: &Records) {
    for (path, journal) in &records.unsettled {
        let _ = save_journal(path, journal);
    }
    for marker in &records.stale_markers {
        let _ = std::fs::remove_file(marker);
    }
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
            && marker.destination == journal.destination,
        course2md::tr!("目标不属于这次移动，未修改目标内容")
    );
    copy_library(path.to_owned(), journal, cancel, progress)
}

/// Copies one file through a temporary name, so a file at its final name is complete.
fn copy_file(
    source: &Path,
    destination: &Path,
    start: u64,
    total: u64,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<()> {
    let parent = destination
        .parent()
        .context(course2md::tr!("目标文件缺少目录"))?;
    let mut input = std::fs::File::open(source)?;
    let mut output = tempfile::Builder::new()
        .prefix(TEMPORARY_PREFIX)
        .tempfile_in(parent)?;
    let message = course2md::tr!(
        "正在复制 {}",
        destination.file_name().unwrap_or_default().display()
    );
    let mut buffer = vec![0; 1024 * 1024];
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
            completed: start + copied,
            total,
        });
    }
    output.as_file().sync_all()?;
    std::fs::set_permissions(output.path(), std::fs::metadata(source)?.permissions())?;
    output
        .persist(destination)
        .map_err(|error| error.error)
        .with_context(|| course2md::tr!("无法保存移动副本 {}", destination.display()))?;
    Ok(())
}

/// Copies the source tree and switches nothing unless, afterwards, the source is unchanged
/// and the copy has the same files at the same sizes. The journal only changes with the
/// phase: a copy that stops is started over, skipping files already finished, which sit at
/// their final names and are no older than their originals.
fn copy_library(
    path: PathBuf,
    mut journal: Journal,
    cancel: &AtomicBool,
    progress: &impl Fn(Progress),
) -> Result<PreparedMove> {
    let mut copy = || -> Result<()> {
        let report = |message: &str, completed, total| {
            progress(Progress {
                message: message.to_owned(),
                completed,
                total,
            })
        };
        report(course2md::tr!("正在检查原笔记库…"), 0, 0);
        let source = Tree::scan(&journal.source, cancel)?;
        journal.files = source.records();
        journal.directories = source.directories.clone();
        // A restart may only clean up after itself; a replaced parent must never lead
        // outside the copy, and anything else there is not ours to overwrite.
        let existing = Tree::scan(&journal.destination, cancel)?;
        let expected: HashSet<&Path> = journal.files.iter().map(|f| f.path.as_path()).collect();
        let temporary = |path: &Path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(TEMPORARY_PREFIX))
        };
        ensure!(
            existing
                .files
                .iter()
                .all(|(file, _)| expected.contains(file.path.as_path()) || temporary(&file.path))
                && existing
                    .directories
                    .iter()
                    .all(|directory| journal.directories.contains(directory)),
            course2md::tr!("目标副本出现了非预期内容，未覆盖任何文件")
        );
        for (file, _) in existing
            .files
            .iter()
            .filter(|(file, _)| temporary(&file.path))
        {
            std::fs::remove_file(journal.destination.join(&file.path))?;
        }
        for directory in &journal.directories {
            std::fs::create_dir_all(journal.destination.join(directory))?;
        }
        let total = journal.files.iter().map(|file| file.bytes).sum();
        let mut completed = 0;
        for (file, modified) in &source.files {
            check_cancel(cancel)?;
            let target = journal.destination.join(&file.path);
            // A previous pass may already have rewritten this binding for the new root.
            let finished = std::fs::metadata(&target).is_ok_and(|copy| {
                (copy.len() == file.bytes && copy.modified().ok() >= *modified)
                    || file
                        .path
                        .file_name()
                        .is_some_and(|n| n == TASK_IDENTITY_FILE)
            });
            if !finished {
                let original = journal.source.join(&file.path);
                copy_file(&original, &target, completed, total, cancel, progress)?;
            }
            completed += file.bytes;
            report(course2md::tr!("正在复制笔记库…"), completed, total);
        }
        check_cancel(cancel)?;
        report(course2md::tr!("正在检查原笔记库…"), 0, 1);
        ensure!(
            Tree::scan(&journal.source, cancel)? == source,
            course2md::tr!("原笔记库在复制期间发生变化，尚未切换保存位置")
        );
        course2md::execution::relocate_work_bindings(&journal.source, &journal.destination)?;
        report(course2md::tr!("正在核验副本…"), 1, 1);
        let copied = Tree::scan(&journal.destination, cancel)?;
        ensure!(
            copied.directories == journal.directories && copied.files.len() == journal.files.len(),
            course2md::tr!("目标副本包含非预期内容，尚未切换保存位置")
        );
        for (original, (copy, _)) in journal.files.iter().zip(&copied.files) {
            ensure!(
                original.path == copy.path
                    && (original.bytes == copy.bytes
                        || copy
                            .path
                            .file_name()
                            .is_some_and(|n| n == TASK_IDENTITY_FILE)),
                course2md::tr!(
                    "目标副本未通过校验，尚未切换保存位置：{}",
                    original.path.display()
                )
            );
        }
        for directory in journal.directories.iter().rev() {
            course2md::artifact::sync_dir(&journal.destination.join(directory))?;
        }
        course2md::artifact::sync_dir(&journal.destination)
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

/// Refuses a folder that overlaps another registered library.
pub fn check_overlap(state: &workspace::State, library_id: &str, folder: &Path) -> Result<()> {
    let folder = std::fs::canonicalize(folder).context(course2md::tr!("目标位置暂时无法访问"))?;
    ensure!(
        !state
            .libraries
            .iter()
            .filter(|library| library.id != library_id)
            .any(|library| {
                let root =
                    std::fs::canonicalize(&library.root).unwrap_or_else(|_| library.root.clone());
                root.starts_with(&folder) || folder.starts_with(root)
            }),
        course2md::tr!("目标与另一个笔记库重叠，请选择单独的文件夹")
    );
    Ok(())
}

/// Switches the registry to a verified copy, keeping the original as a backup.
pub fn publish_move(state: &mut workspace::State, prepared: &PreparedMove) -> Result<()> {
    let journal = &prepared.journal;
    ensure!(
        journal.phase == Phase::Verified,
        course2md::tr!("目标副本尚未核验，不能切换保存位置")
    );
    let location = state
        .library(&journal.library_id)
        .context(course2md::tr!("这个笔记库已被移除"))?;
    ensure!(
        std::fs::canonicalize(&location.root)? == journal.source,
        course2md::tr!("笔记库的位置在移动期间已经变化，尚未切换")
    );
    let roots = [journal.source.clone(), location.root.clone()];
    check_overlap(state, &journal.library_id, &journal.destination)?;
    state.rebind_library(&journal.library_id, &roots, &journal.destination)?;
    state.storage_backups.push(BackupRecord {
        id: journal.id.clone(),
        library_id: journal.library_id.clone(),
        path: journal.source.clone(),
        current_root: journal.destination.clone(),
        created: journal.created,
        verified: true,
        journal_path: prepared.path.clone(),
    });
    Ok(())
}

/// A folder someone chose as where a library now lives: moved outside the app, or the same
/// folder after it lost its identity. Nothing is written until `finish_link`.
pub struct LibraryLink {
    pub library_id: String,
    pub previous_root: PathBuf,
    pub folder: PathBuf,
    stamp: DirectoryStamp,
    /// The folder names no library: someone has to confirm it from what it holds.
    pub unmarked: Option<NotesFound>,
}

#[derive(Default)]
pub struct NotesFound {
    pub names: Vec<String>,
    pub unreadable: usize,
}

/// Blocking: reads the folder's identity and, when it has none, every note in it.
pub fn inspect_link(
    state: &workspace::State,
    library_id: &str,
    folder: &Path,
    cancel: &AtomicBool,
) -> Result<LibraryLink> {
    let location = state
        .library(library_id)
        .context(course2md::tr!("这个笔记库已被移除"))?;
    let folder = folder
        .canonicalize()
        .context(course2md::tr!("所选文件夹暂时无法访问"))?;
    check_overlap(state, library_id, &folder)?;
    let stamp = directory_stamp(&folder)?;
    let current = location.root.canonicalize().ok();
    let unmarked = match std::fs::read_to_string(folder.join(LIBRARY_ID_FILE)) {
        Ok(identity) => {
            ensure!(
                identity.trim() == library_id,
                course2md::tr!("所选文件夹属于另一个笔记库，请选择这个笔记库移动后的文件夹")
            );
            ensure!(
                current.as_ref() != Some(&folder),
                course2md::tr!("所选仍是当前位置，请选择移动后的笔记库文件夹")
            );
            None
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut found = NotesFound::default();
            for course in crate::notes::scan_library(&folder)?.courses {
                ensure!(
                    !cancel.load(Ordering::Relaxed),
                    course2md::tr!("已取消重新定位")
                );
                let title = course.title.clone();
                match crate::notes::read_preview(course) {
                    Ok(_) => found.names.push(title),
                    Err(_) => found.unreadable += 1,
                }
            }
            Some(found)
        }
        Err(error) => {
            return Err(error).context(course2md::tr!(
                "无法确认所选文件夹是这个笔记库，请选择移动后的完整笔记库文件夹"
            ));
        }
    };
    ensure!(
        directory_stamp(&folder)? == stamp,
        course2md::tr!("所选文件夹已变化，请重新选择")
    );
    Ok(LibraryLink {
        library_id: library_id.to_owned(),
        previous_root: location.root.clone(),
        folder,
        stamp,
        unmarked,
    })
}

/// Blocking: after any confirmation, checks the folder is still the one reviewed, gives it
/// its identity if it had none, and reads the tasks it carries.
pub fn finish_link(link: &LibraryLink) -> Result<Vec<TaskRecord>> {
    ensure!(
        directory_stamp(&link.folder)? == link.stamp,
        course2md::tr!("所选文件夹已被替换，请重新选择")
    );
    let location = LibraryLocation {
        id: link.library_id.clone(),
        name: String::new(),
        root: link.folder.clone(),
        previous_roots: Vec::new(),
    };
    match link.unmarked {
        Some(_) => workspace::create_library_marker(&location)?,
        None => workspace::check_library(&location)?,
    }
    Ok(workspace::read_library_tasks(&location).0)
}

/// Applies a finished link; touches no disk.
pub fn publish_link(
    state: &mut workspace::State,
    link: &LibraryLink,
    tasks: Vec<TaskRecord>,
) -> Result<()> {
    ensure!(
        state
            .library(&link.library_id)
            .is_some_and(|location| location.root == link.previous_root),
        course2md::tr!("这个笔记库的位置已变化，请重新选择")
    );
    if link.folder != link.previous_root {
        state.rebind_library(
            &link.library_id,
            std::slice::from_ref(&link.previous_root),
            &link.folder,
        )?;
    }
    state.adopt_tasks(tasks);
    Ok(())
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
    let tree = Tree::scan(&path, cancel)?;
    let files = tree.records();
    let unchanged = if journal.cleanup_started {
        // A crash midway through deleting leaves part of what was recorded.
        let recorded: HashSet<&FileRecord> = journal.files.iter().collect();
        files.iter().all(|file| recorded.contains(file))
            && tree
                .directories
                .iter()
                .all(|path| journal.directories.contains(path))
    } else {
        files == journal.files && tree.directories == journal.directories
    };
    if !unchanged {
        bail!(course2md::tr!(
            "旧位置备份在移动后发生变化，未清理任何内容。请打开该位置检查新增或修改的文件。"
        ));
    }
    check_cancel(cancel)?;
    journal.cleanup_started = true;
    save_journal(&backup.journal_path, &journal)?;
    std::fs::remove_dir_all(&path).context(course2md::tr!("旧位置备份尚未清理完成，请重试"))?;
    if let Some(parent) = path.parent() {
        let _ = course2md::artifact::sync_dir(parent);
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

    /// As a crash during the copy leaves it.
    fn interrupted(prepared: &PreparedMove) {
        let mut journal = prepared.journal.clone();
        journal.phase = Phase::Copying;
        save_journal(&prepared.path, &journal).unwrap();
    }

    #[test]
    fn an_interrupted_move_restarts_and_clears_only_its_own_temporary_files() {
        let (_dir, prepared) = fixture();
        interrupted(&prepared);
        let temporary = PathBuf::from(format!("notes/{TEMPORARY_PREFIX}interrupted"));
        std::fs::remove_file(prepared.journal.destination.join("notes/course.md")).unwrap();
        std::fs::write(prepared.journal.destination.join(&temporary), "partial").unwrap();
        let resumed = resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(resumed.journal.phase, Phase::Verified);
        assert!(!resumed.journal.destination.join(temporary).exists());
        assert_eq!(
            std::fs::read(resumed.journal.destination.join("notes/course.md")).unwrap(),
            std::fs::read(resumed.journal.source.join("notes/course.md")).unwrap()
        );
    }

    #[test]
    fn a_restart_picks_up_what_changed_in_the_source_meanwhile() {
        let (_dir, prepared) = fixture();
        interrupted(&prepared);
        let original = prepared.journal.source.join("notes/course.md");
        std::fs::write(&original, "written after the interruption").unwrap();
        std::fs::write(prepared.journal.source.join("notes/new.md"), "added").unwrap();
        let resumed = resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).unwrap();
        let destination = resumed.journal.destination;
        assert_eq!(
            std::fs::read_to_string(destination.join("notes/course.md")).unwrap(),
            "written after the interruption"
        );
        assert!(destination.join("notes/new.md").is_file());
        assert_eq!(
            std::fs::read_to_string(original).unwrap(),
            "written after the interruption"
        );
    }

    #[test]
    fn a_restart_refuses_a_destination_holding_someone_elses_files() {
        let (_dir, prepared) = fixture();
        interrupted(&prepared);
        let foreign = prepared.journal.destination.join("notes/mine.md");
        std::fs::write(&foreign, "someone else's").unwrap();
        assert!(resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).is_err());
        assert_eq!(std::fs::read_to_string(foreign).unwrap(), "someone else's");
    }

    #[test]
    fn a_journal_from_an_earlier_version_still_restarts() {
        let (_dir, prepared) = fixture();
        interrupted(&prepared);
        let mut old = serde_json::to_value(read_journal(&prepared.path).unwrap()).unwrap();
        old["files"] = serde_json::json!([
            {"path": "notes/course.md", "bytes": 17, "digest": "0123"}
        ]);
        old["temporary_files"] = serde_json::json!(["notes/.course2md-copy-move-x-y"]);
        old["settings_relocated"] = serde_json::json!(true);
        std::fs::write(&prepared.path, serde_json::to_vec(&old).unwrap()).unwrap();
        let resumed = resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(resumed.journal.phase, Phase::Verified);
    }

    #[cfg(unix)]
    #[test]
    fn a_restart_rejects_a_replaced_parent_before_touching_anything() {
        let (dir, prepared) = fixture();
        interrupted(&prepared);
        let external = dir.path().join("external");
        std::fs::create_dir(&external).unwrap();
        let valuable = external.join(format!("{TEMPORARY_PREFIX}interrupted"));
        std::fs::write(&valuable, "keep this external file").unwrap();
        std::fs::remove_dir_all(prepared.journal.destination.join("notes")).unwrap();
        std::os::unix::fs::symlink(&external, prepared.journal.destination.join("notes")).unwrap();
        assert!(resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).is_err());
        assert_eq!(
            std::fs::read_to_string(valuable).unwrap(),
            "keep this external file"
        );
        assert!(prepared.journal.source.join("notes/course.md").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_library_with_a_link_inside_is_not_moved_without_what_it_points_to() {
        let (_dir, source, destination, journals) = moved_library(1);
        std::os::unix::fs::symlink(&journals, source.join("notes/link")).unwrap();
        let error = move_with(&source, &destination, &journals, || {})
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("符号链接"));
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
    fn moved_library(files: usize) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("old");
        let destination = dir.path().join("new");
        std::fs::create_dir_all(source.join("notes")).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        for index in 0..files {
            std::fs::write(source.join(format!("notes/{index}.md")), "notes").unwrap();
        }
        let journals = dir.path().join("journals");
        (dir, source, destination, journals)
    }

    #[test]
    fn a_move_records_every_file_and_keeps_the_original_whole() {
        let (_dir, source, destination, journals) = moved_library(40);
        let prepared = move_with(&source, &destination, &journals, || {}).unwrap();
        assert_eq!(prepared.journal.files.len(), 40);
        assert_eq!(prepared.journal.directories, [PathBuf::from("notes")]);
        for index in 0..40 {
            for root in [&source, &destination] {
                assert!(root.join(format!("notes/{index}.md")).is_file());
            }
        }
    }

    /// Runs a move and calls `after_copy` once the first file has been copied.
    fn move_with(
        source: &Path,
        destination: &Path,
        journals: &Path,
        after_copy: impl Fn(),
    ) -> Result<PreparedMove> {
        let done = std::cell::Cell::new(false);
        prepare_move(
            "library".into(),
            source,
            destination,
            journals,
            &AtomicBool::new(false),
            &|progress| {
                if progress.message == "正在复制笔记库…" && !done.replace(true) {
                    after_copy();
                }
            },
        )
    }

    #[test]
    fn a_source_file_written_after_its_copy_stops_the_move() {
        let (_dir, source, destination, journals) = moved_library(3);
        let first = source.join("notes/0.md");
        let error = move_with(&source, &destination, &journals, || {
            // The same size, so only the modification time gives it away.
            std::fs::write(&first, "edits").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&first)
                .unwrap()
                .set_modified(SystemTime::now() + std::time::Duration::from_secs(5))
                .unwrap();
        })
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("原笔记库在复制期间发生变化"));
        assert_eq!(std::fs::read_to_string(first).unwrap(), "edits");
        let (pending, _) = scan_journals(&journals);
        assert_eq!(pending[0].1.phase, Phase::Failed);
    }

    #[test]
    fn a_source_file_added_after_its_copy_stops_the_move() {
        let (_dir, source, destination, journals) = moved_library(3);
        let added = source.join("notes/added.md");
        assert!(
            move_with(&source, &destination, &journals, || {
                std::fs::write(&added, "new").unwrap();
            })
            .is_err()
        );
        assert!(added.is_file());
    }

    #[test]
    fn a_file_missing_from_the_copy_is_not_published() {
        let (_dir, source, destination, journals) = moved_library(3);
        let copy = destination.join("notes/0.md");
        let error = move_with(&source, &destination, &journals, || {
            std::fs::remove_file(&copy).unwrap();
        })
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("目标副本包含非预期内容"));
        assert_eq!(scan_journals(&journals).0[0].1.phase, Phase::Failed);
        assert!(source.join("notes/0.md").is_file());
    }

    #[test]
    fn a_copy_that_changed_size_after_landing_is_not_published() {
        let (_dir, source, destination, journals) = moved_library(3);
        let copy = destination.join("notes/0.md");
        let error = move_with(&source, &destination, &journals, || {
            std::fs::write(&copy, "truncated by something else").unwrap();
        })
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("目标副本未通过校验"));
    }
    #[test]
    fn reading_records_writes_nothing_and_settling_clears_the_published_move() {
        let (_dir, prepared) = fixture();
        let journal_dir = prepared.path.parent().unwrap();
        let marker = prepared.journal.destination.join(MOVE_MARKER_FILE);
        let before = std::fs::read(&prepared.path).unwrap();
        let libraries = [("library".to_owned(), prepared.journal.destination.clone())];

        // Not registered there yet: still a move in progress, marker and all.
        let waiting = read_records(
            journal_dir,
            &[("library".to_owned(), prepared.journal.source.clone())],
            &[],
        );
        assert_eq!(waiting.pending.len(), 1);
        assert!(waiting.unsettled.is_empty() && waiting.stale_markers.is_empty());

        let records = read_records(journal_dir, &libraries, &[]);
        assert!(records.pending.is_empty());
        assert_eq!(records.unsettled[0].1.phase, Phase::Committed);
        assert_eq!(records.stale_markers, std::slice::from_ref(&marker));
        assert_eq!(std::fs::read(&prepared.path).unwrap(), before);
        assert!(marker.is_file());

        settle_records(&records);
        assert_eq!(
            read_journal(&prepared.path).unwrap().phase,
            Phase::Committed
        );
        assert!(!marker.exists());
        let settled = read_records(journal_dir, &libraries, &[]);
        assert!(settled.unsettled.is_empty() && settled.stale_markers.is_empty());
    }

    #[test]
    fn another_librarys_marker_is_not_stale() {
        let (_dir, prepared) = fixture();
        let marker = prepared.journal.destination.join(MOVE_MARKER_FILE);
        let records = read_records(
            prepared.path.parent().unwrap(),
            &[("other".to_owned(), prepared.journal.destination.clone())],
            &[],
        );
        assert!(records.stale_markers.is_empty());
        assert!(marker.is_file());
    }

    #[test]
    fn abandoning_a_move_removes_its_marker() {
        let (_dir, prepared) = fixture();
        let marker = prepared.journal.destination.join(MOVE_MARKER_FILE);
        abandon_move(&prepared.path).unwrap();
        assert!(!marker.exists());
        assert_eq!(
            read_journal(&prepared.path).unwrap().phase,
            Phase::Abandoned
        );
        assert!(resume_move(&prepared.path, &AtomicBool::new(false), &|_| {}).is_err());
    }
}
