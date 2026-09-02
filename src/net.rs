//! 通用 HTTP 下载器：ASR 模型与自动安装的外部工具共用同一套
//! 落盘、完整性校验与进度条逻辑（自 models.rs 泛化而来）。
//!
//! 保证：
//! - `.part` 临时文件 + 原子 rename：进程中断不会留下看似完整的半截文件
//! - Content-Length 校验：截断响应不会伪装成功
//! - manifest.json 记账：记录精确字节数（与可选 sha256），供启动时快速校验
//! - [`Verify::Sha256`]：自动安装的外部可执行文件必须固定哈希

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// 完整性策略。
#[derive(Debug, Clone, Copy)]
pub enum Verify {
    /// 旧模型缓存：以 manifest.json 记录的字节数（无 manifest 时 >1MB 启发式）判断完整。
    /// 模型文件极大（2.4GB），启动时全量哈希不可接受。
    Size,
    /// 自动安装的外部可执行文件：固定 sha256（十六进制小写）。
    /// 工具 ≤80MB，全量哈希可接受，且可执行文件必须防篡改。
    Sha256(&'static str),
}

/// 一次下载请求。
pub struct Download {
    pub url: String,
    pub dest: PathBuf,
    pub label: String,
    pub verify: Verify,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).with_context(|| format!("打开 {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1024 * 512];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// dest 是否已是通过 verify 校验的完整文件。任何失败都视为不完整（返回 false）。
pub fn is_complete(dest: &Path, verify: &Verify) -> bool {
    let Ok(md) = fs::metadata(dest) else {
        return false;
    };
    if !dest.is_file() || md.len() <= 1_000_000 {
        return false;
    }
    match verify {
        Verify::Sha256(expect) => match sha256_file(dest) {
            Ok(got) => got.eq_ignore_ascii_case(expect),
            Err(_) => false,
        },
        Verify::Size => size_manifest_complete(dest, md.len()),
    }
}

/// manifest.json 记账的尺寸校验（模型缓存的旧约定；无 manifest 时 >1MB 启发式）。
pub fn size_manifest_complete(path: &Path, len: u64) -> bool {
    if len <= 1_000_000 {
        return false;
    }
    let manifest = path.with_extension("manifest.json");
    let Ok(s) = fs::read_to_string(&manifest) else {
        return true; // 无 manifest 的旧缓存退回启发式
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return true;
    };
    match v.get("size").and_then(|s| s.as_u64()) {
        Some(expected) => len == expected,
        None => true,
    }
}

/// 下载 `dl.url` 到 `dl.dest`。已完整则跳过；损坏残留自动删除重下。
pub async fn download_file(dl: &Download) -> Result<()> {
    let dest = dl.dest.to_path_buf();
    if is_complete(&dest, &dl.verify) {
        tracing::info!(label = %dl.label, "skip existing");
        return Ok(());
    }
    if dest.is_file() {
        // 校验不过的残留文件（截断/损坏）直接移除，避免"发现坏了却重下不了"
        tracing::warn!(label = %dl.label, "existing file failed integrity check, re-downloading");
        let _ = fs::remove_file(&dest);
        let _ = fs::remove_file(dest.with_extension("manifest.json"));
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = dest.with_extension("part");
    let url = dl.url.clone();
    let label = dl.label.clone();
    let verify = dl.verify;
    tokio::task::spawn_blocking(move || -> Result<()> {
        tracing::info!(label = %label, url = %url, "download");
        // 网络抖动重试：瞬时断流/超时不应让整个安装失败（大文件下载常见）
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=3 {
            match download_once(&url, &tmp, &label) {
                Ok(total) => {
                    finish_download(&tmp, &dest, total, verify, &label)?;
                    return Ok(());
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    last_err = Some(e);
                    if attempt < 3 {
                        let wait = std::time::Duration::from_secs(if attempt == 1 { 2 } else { 5 });
                        tracing::warn!(label = %label, attempt, "下载失败，{wait:?} 后重试");
                        std::thread::sleep(wait);
                    }
                }
            }
        }
        Err(last_err.unwrap())
    })
    .await
    .context("下载线程失败")??;
    Ok(())
}

/// 单次尝试：请求 + 流式落盘到 tmp。返回服务器声明的 Content-Length（0 = 未知）。
fn download_once(url: &str, tmp: &Path, label: &str) -> Result<u64> {
    let resp = ureq::get(url).call().context("请求失败")?;
    let total: u64 = resp
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let pb = indicatif::ProgressBar::new(total.max(1));
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} {msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    pb.set_message(label.to_string());
    let mut reader = resp.into_reader();
    let mut out = fs::File::create(tmp)?;
    let mut buf = vec![0u8; 1024 * 512];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])?;
        done += n as u64;
        pb.set_position(done);
    }
    out.sync_all()?;
    drop(out);
    pb.finish_and_clear();
    // 完整性：以服务器 Content-Length 为准（而非"实际收到多少"——截断响应会伪装成功）
    if total > 0 && done != total {
        anyhow::bail!("下载不完整：期望 {total} 字节，实际收到 {done}（请重试）");
    }
    tracing::info!(label = %label, bytes = done, "downloaded");
    Ok(total)
}

/// rename 前的最终校验：sha256（可执行文件固定）或尺寸记账，然后原子落盘。
fn finish_download(tmp: &Path, dest: &Path, total: u64, verify: Verify, label: &str) -> Result<()> {
    // sha256 校验（在 rename 之前，坏文件根本不落正式位）
    let got_sha = match verify {
        Verify::Sha256(_) => Some(sha256_file(tmp)?),
        Verify::Size => None,
    };
    if let (Verify::Sha256(expect), Some(got)) = (&verify, got_sha)
        && !got.eq_ignore_ascii_case(expect)
    {
        let _ = fs::remove_file(tmp);
        anyhow::bail!(
            "sha256 校验失败：期望 {expect}，实际 {got}（下载源被篡改或网络损坏，请重试）"
        );
    }
    fs::rename(tmp, dest)?;
    // manifest 记录 authoritative Content-Length 与哈希，供后续启动校验
    let mut manifest = serde_json::json!({ "size": if total > 0 { total } else { 0 } });
    if let Verify::Sha256(sha) = verify {
        manifest["sha256"] = serde_json::json!(sha);
    }
    let _ = fs::write(dest.with_extension("manifest.json"), manifest.to_string());
    tracing::debug!(label = %label, "verified & installed");
    Ok(())
}
