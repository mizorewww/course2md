//! Logical folders never move or delete exported course files.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Library {
    pub folders: BTreeMap<u64, String>,
    pub courses: BTreeMap<PathBuf, u64>,
    #[serde(default)]
    next_id: u64,
}
impl Library {
    pub fn load(root: &Path) -> Result<Self> {
        match std::fs::read(root.join(".course2md-library.json")) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("文件夹信息损坏；原文件已保留，请修复后重试")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn edit(root: &Path, change: impl FnOnce(&mut Self) -> Result<()>) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let _lock = course2md::runtime::lock_file(&root.join(".course2md-library.lock"))?;
        let mut library = Self::load(root)?;
        change(&mut library)?;
        course2md::checkpoint::atomic_write(
            &root.join(".course2md-library.json"),
            &serde_json::to_vec_pretty(&library)?,
        )?;
        Ok(library)
    }
    pub fn rename(&mut self, id: Option<u64>, name: &str) -> Result<u64> {
        let name = name.trim();
        ensure!(!name.is_empty(), "请输入文件夹名称");
        ensure!(name.chars().count() <= 60, "文件夹名称最多 60 个字符");
        ensure!(!self.folders.iter().any(|(key, value)| Some(*key) != id && value.to_lowercase() == name.to_lowercase()), "已有同名文件夹");
        let id = match id {
            Some(id) => {
                ensure!(
                    self.folders.contains_key(&id),
                    "文件夹已不存在，请刷新课程库"
                );
                id
            }
            None => {
                self.next_id = self
                    .next_id
                    .max(self.folders.keys().copied().max().unwrap_or(0))
                    .checked_add(1)
                    .context("文件夹编号已耗尽")?;
                self.next_id
            }
        };
        self.folders.insert(id, name.to_owned());
        Ok(id)
    }
    pub fn remove(&mut self, id: u64) {
        self.folders.remove(&id);
        self.courses.retain(|_, folder| *folder != id);
    }
    pub fn folder(&self, root: &Path, course: &Path) -> Option<u64> {
        self.courses
            .get(course.strip_prefix(root).ok()?)
            .copied()
            .filter(|id| self.folders.contains_key(id))
    }
    pub fn assign(&mut self, root: &Path, course: &Path, folder: Option<u64>) -> Result<()> {
        let key = course
            .strip_prefix(root)
            .context("课程不在当前课程库中")?
            .to_path_buf();
        if let Some(id) = folder {
            ensure!(
                self.folders.contains_key(&id),
                "文件夹已不存在，请选择其他文件夹"
            );
            self.courses.insert(key, id);
        } else {
            self.courses.remove(&key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folders_survive_rename_restart_and_deletion_keeps_notes() {
        let root = tempfile::tempdir().unwrap();
        let course = root.path().join("local/lecture");
        std::fs::create_dir_all(&course).unwrap();
        std::fs::write(course.join("course.md"), "valuable notes").unwrap();
        let mut id = 0;
        Library::edit(root.path(), |library| {
            id = library.rename(None, "  数学  ")?;
            library.assign(root.path(), &course, Some(id))
        })
        .unwrap();
        Library::edit(root.path(), |library| {
            library.rename(Some(id), "线性代数")?;
            Ok(())
        })
        .unwrap();
        let reopened = Library::load(root.path()).unwrap();
        assert_eq!(reopened.folders[&id], "线性代数");
        assert_eq!(reopened.folder(root.path(), &course), Some(id));
        Library::edit(root.path(), |library| {
            library.remove(id);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            Library::load(root.path())
                .unwrap()
                .folder(root.path(), &course),
            None
        );
        assert_eq!(
            std::fs::read_to_string(course.join("course.md")).unwrap(),
            "valuable notes"
        );
    }
    #[test]
    fn stale_writers_merge_latest_state_and_failed_edits_do_not_write() {
        let root = tempfile::tempdir().unwrap();
        Library::edit(root.path(), |library| {
            library.rename(None, "First")?;
            Ok(())
        })
        .unwrap();
        Library::edit(root.path(), |library| {
            library.rename(None, "Second")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(Library::load(root.path()).unwrap().folders.len(), 2);
        assert!(
            Library::edit(root.path(), |library| {
                library.rename(None, "FIRST")?;
                Ok(())
            })
            .is_err()
        );
        let path = root.path().join(".course2md-library.json");
        std::fs::write(&path, "broken metadata").unwrap();
        assert!(
            Library::edit(root.path(), |library| {
                library.rename(None, "Replacement")?;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken metadata");
    }
}
