//! The one way the desktop keeps a JSON record: atomic writes beside a `.json.bak` of the
//! last readable version, with damaged originals preserved instead of overwritten.
use anyhow::{Context, Result, ensure};
use std::{io::Read, path::Path};

/// Records and task materials this large are damage, not data.
const READ_LIMIT: u64 = 32 * 1024 * 1024;

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

pub fn now() -> u64 {
    chrono::Utc::now().timestamp() as u64
}

/// Reads a whole record, refusing anything that is not a plain file of a sane size.
pub fn read(path: &Path) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), course2md::tr!("任务记录不是普通文件"));
    ensure!(
        metadata.len() <= READ_LIMIT,
        course2md::tr!("任务记录超过可读取范围，原文件已保留")
    );
    let mut bytes = Vec::new();
    file.by_ref().take(READ_LIMIT + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn directory_of(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("."))
}

/// The backup copy is not fsynced: the primary it was copied from stays durable, and
/// a commit should cost the interface one disk flush, not two.
fn write_backup(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let backup = path.with_extension("json.bak");
    let mut temporary = tempfile::NamedTempFile::new_in(directory_of(path))?;
    temporary.write_all(bytes)?;
    temporary.persist(&backup).map_err(|error| error.error)?;
    Ok(())
}

/// Commits `bytes` to `path`, keeping what was there as its `.json.bak`. A damaged file
/// must not replace the last good backup, so it is set aside under a name of its own.
pub fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(directory_of(path))?;
    match std::fs::read(path) {
        Ok(previous) if serde_json::from_slice::<serde::de::IgnoredAny>(&previous).is_ok() => {
            write_backup(path, &previous)?
        }
        Ok(previous) => course2md::checkpoint::atomic_write(
            &path.with_extension(format!("{}.damaged", new_id("record"))),
            &previous,
        )?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => write_backup(path, bytes)?,
        Err(error) => return Err(error).context(course2md::tr!("无法保留原记录")),
    }
    course2md::checkpoint::atomic_write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_backs_up_good_records_and_sets_damaged_ones_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");
        let backup = path.with_extension("json.bak");
        save(&path, b"[1]").unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"[1]");
        save(&path, b"[2]").unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), b"[1]");
        std::fs::write(&path, b"{broken").unwrap();
        save(&path, b"[3]").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[3]");
        assert_eq!(std::fs::read(&backup).unwrap(), b"[1]");
        let damaged = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".damaged"))
            .unwrap();
        assert_eq!(std::fs::read(damaged.path()).unwrap(), b"{broken");
    }

    #[test]
    fn oversized_or_non_file_records_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_err());
        let path = dir.path().join("big.json");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(READ_LIMIT + 1)
            .unwrap();
        assert!(read(&path).is_err());
    }
}
