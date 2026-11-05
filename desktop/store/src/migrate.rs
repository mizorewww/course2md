//! The startup migration of a library an older app saved courses in as versions
//! (`current.json` and `versions/<id>/`). Each such course becomes one flat note and
//! its other versions are deleted (`artifact::flatten_legacy`), once per library when
//! the app opens it, before any task is dispatched.
//!
//! A course a worker from an earlier session is still writing is left as it is until
//! that worker lets go: it may be an older app's, which writes versions. Any other
//! unfinished task writes nothing until it runs, and the engine flattens its note
//! first then, so it needs no wait.
//!
//! The task records follow from what is on disk, so a crash at any point is finished
//! by the next opening: a task whose note is now the flat note points at it, and one
//! whose version was deleted is marked replaced (`workspace::probe_artifact`). Reading
//! positions saved per version move to the note (`rekey_positions`).
use crate::workspace::{LibraryLocation, ReadingPosition};
use c2m_artifact::artifact;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// How long the migration waits for another writer of the library's records.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// What migrating one library did.
#[derive(Clone, Debug, Default)]
pub struct Migration {
    pub library_id: String,
    /// Courses flattened by this run.
    pub flattened: usize,
    /// Versions deleted with them.
    pub removed_versions: usize,
    /// Courses left as versions while a worker from an earlier session writes them.
    pub deferred: Vec<PathBuf>,
    pub issues: Vec<String>,
    /// Every flat note found: its course id and the version whose reading positions
    /// it keeps first.
    pub notes: Vec<(String, String)>,
    /// The library was reached and walked; otherwise it is migrated another time.
    pub ran: bool,
}

/// Flattens every course in `location` an older app saved as versions, except the
/// ones in `busy`, which a worker from an earlier session still writes. Blocking.
pub(crate) fn migrate_library(location: &LibraryLocation, busy: &[PathBuf]) -> Migration {
    let mut migration = Migration {
        library_id: location.id.clone(),
        ..Default::default()
    };
    let root = &location.root;
    // The folder record's lock: one migration at a time, and none beside an edit.
    let _lock = match c2m_process::runtime::lock_file_within(
        &root.join(".course2md-library.lock"),
        LOCK_WAIT,
    ) {
        Ok(lock) => lock,
        Err(error) => {
            migration.issues.push(format!("{error:#}"));
            return migration;
        }
    };
    migration.issues.extend(crate::notes::recover_library(root));
    let same = |a: &Path, b: &Path| {
        a == b
            || a.canonicalize()
                .is_ok_and(|a| b.canonicalize().is_ok_and(|b| a == b))
    };
    let mut walk = walkdir::WalkDir::new(root)
        .max_depth(8)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
        });
    while let Some(entry) = walk.next() {
        let Ok(entry) = entry else {
            continue;
        };
        let dir = entry.path().to_path_buf();
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        if artifact::is_legacy(&dir) {
            walk.skip_current_dir();
            if busy.iter().any(|note| same(note, &dir)) {
                migration.deferred.push(dir);
                continue;
            }
            let versions = std::fs::read_dir(dir.join("versions"))
                .map(|entries| {
                    entries
                        .flatten()
                        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                        .count()
                })
                .unwrap_or(0);
            match artifact::flatten_legacy(&dir) {
                Ok(Some(manifest)) => {
                    migration.flattened += 1;
                    migration.removed_versions += versions.saturating_sub(1);
                    let kept = if manifest.version_id.is_empty() {
                        manifest.task_id.clone()
                    } else {
                        manifest.version_id.clone()
                    };
                    migration.notes.push((manifest.course_id, kept));
                }
                Ok(None) => {}
                Err(error) => migration.issues.push(c2m_i18n::tr!(
                    "{}：{}",
                    dir.display(),
                    format!("{error:#}")
                )),
            }
        } else if dir.join("manifest.json").is_file() {
            walk.skip_current_dir();
            if let Ok(Some(manifest)) = artifact::note_manifest(&dir)
                && !migration
                    .notes
                    .iter()
                    .any(|(course, _)| *course == manifest.course_id)
            {
                migration.notes.push((manifest.course_id, manifest.task_id));
            }
        }
    }
    migration.ran = true;
    migration
}

/// The reading position key of a note at one tab (`0` is the note tab).
pub fn reading_key(course_id: &str, tab: usize) -> String {
    format!("{course_id}:{tab}")
}

/// Moves positions saved per version (`<course>:<version>:<tab>`) to their note
/// (`<course>:<tab>`). A position already saved for the note stays; otherwise the
/// one of the version the note kept, or else another version's. Running it again
/// changes nothing.
pub fn rekey_positions(
    positions: &mut BTreeMap<String, ReadingPosition>,
    notes: &[(String, String)],
) -> bool {
    let mut changed = false;
    for (course, kept) in notes {
        let prefix = format!("{course}:");
        let old: Vec<(String, String, usize)> = positions
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .filter_map(|(key, _)| {
                let (version, tab) = key[prefix.len()..].rsplit_once(':')?;
                let tab = tab.parse().ok()?;
                (!version.is_empty() && !version.contains(':'))
                    .then(|| (key.clone(), version.to_owned(), tab))
            })
            .collect();
        let mut chosen: BTreeMap<usize, (bool, String)> = BTreeMap::new();
        for (key, version, tab) in &old {
            let preferred = version == kept;
            let entry = chosen.entry(*tab).or_insert((preferred, key.clone()));
            if preferred && !entry.0 {
                *entry = (true, key.clone());
            }
        }
        for (tab, (_, key)) in chosen {
            let position = positions[&key].clone();
            positions
                .entry(reading_key(course, tab))
                .or_insert(position);
        }
        for (key, _, _) in old {
            positions.remove(&key);
            changed = true;
        }
    }
    changed
}

/// The notice shown once, when this app first removed older versions.
pub fn removed_versions_notice() -> &'static str {
    c2m_i18n::tr!("笔记的旧版本已删除：每个视频现在只保留一份笔记，重新生成或补做会直接更新它。")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_saved_per_version_move_to_the_note_once() {
        let position = |offset: f32| ReadingPosition {
            offset,
            ..Default::default()
        };
        let mut positions: BTreeMap<_, _> = [
            ("course-a:task-1:0", position(1.)),
            ("course-a:task-2:0", position(2.)),
            ("course-a:task-1:1", position(3.)),
            ("course-b:task-9:0", position(4.)),
            ("course-b:0", position(5.)),
            // A note without a manifest is keyed by its library and folder.
            ("library-1:folder/note:0", position(6.)),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
        let notes = [
            ("course-a".to_owned(), "task-2".to_owned()),
            ("course-b".to_owned(), "task-9".to_owned()),
        ];
        assert!(rekey_positions(&mut positions, &notes));
        let offsets: Vec<_> = positions
            .iter()
            .map(|(key, position)| (key.as_str(), position.offset))
            .collect();
        assert_eq!(
            offsets,
            [
                ("course-a:0", 2.),
                ("course-a:1", 3.),
                // A position saved for the note already is newer.
                ("course-b:0", 5.),
                ("library-1:folder/note:0", 6.),
            ]
        );
        let again = positions.clone();
        assert!(!rekey_positions(&mut positions, &notes));
        assert_eq!(positions, again);
    }
}
