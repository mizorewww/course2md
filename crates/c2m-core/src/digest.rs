//! SHA-256 in the lowercase hexadecimal form that receipts, manifests and identities store.

use crate::tr;
use anyhow::{Context, Result};
use std::{io::Read, path::Path};

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

/// The digest of a JSON value in its canonical form, keys sorted at every level. An
/// identity must not depend on how serde_json was built: the desktop's stack turns on
/// `preserve_order`, which reaches this crate in any build that includes the desktop, and
/// a different digest would make a resumed task send its finished requests again.
pub fn json_digest(value: &serde_json::Value) -> serde_json::Result<String> {
    let mut sorted = value.clone();
    sorted.sort_all_objects();
    Ok(digest(&serde_json::to_vec(&sorted)?))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_digest_depends_on_content_not_key_order() {
        let value = serde_json::json!({"b": 1, "a": {"d": [{"f": 1, "e": 2}], "c": 2}});
        assert_eq!(
            json_digest(&value).unwrap(),
            digest(br#"{"a":{"c":2,"d":[{"e":2,"f":1}]},"b":1}"#)
        );
    }
}
