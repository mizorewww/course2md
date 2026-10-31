//! SHA-256 in the lowercase hexadecimal form that receipts, manifests and identities store.

use crate::tr;
use anyhow::{Context, Result};
use std::{io::Read, path::Path};

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

/// Lowercase hexadecimal, the form digests take in receipts and manifests.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
        tr!(
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
            tr!("已取消读取文件 / File reading cancelled")
        );
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex(&hash.finalize()))
}
