//! Crash-safe file replacement, the one way this project writes small files.

use crate::tr;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

/// 小文件原子写：tmp → fsync → rename，避免崩溃留下半截文件。
/// 保证级别：崩溃安全（文件本体已 fsync 后才 rename）；
/// 不含掉电场景下父目录的 fsync（极端掉电时目录项本身可能未落盘）。
/// 文件仅本人可读（0600）：配置、凭据与任务记录都走这里。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomically(path, bytes, false)
}

/// 同 [`atomic_write`]，但产物是用户要阅读、分享的笔记文件：权限与普通文件一致（0644）。
pub fn atomic_write_shared(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomically(path, bytes, true)
}

/// 把临时文件放宽为普通文件权限（0644）；非 Unix 无此概念。
pub fn make_shared(file: &std::fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn write_atomically(path: &Path, bytes: &[u8], shared: bool) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| {
        tr!(
            "创建目录 {} / Failed to create directory {}",
            dir.display(),
            dir.display()
        )
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)
        .with_context(|| tr!("写 {} / Failed to write {}", path.display(), path.display()))?;
    if shared {
        make_shared(tmp.as_file())?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path).with_context(|| {
        tr!(
            "替换 {} / Failed to replace {}",
            path.display(),
            path.display()
        )
    })?;
    Ok(())
}
