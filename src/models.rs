//! 模型缓存管理与下载（llama.cpp Qwen3-ASR GGUF）。
//!
//! 默认目录：`~/.cache/course2md/models/`（可用 `--model-dir` / `models download --dir` 覆盖）
//!
//! ```text
//! models/
//!   llama-qwen3-1.7b/
//!     Qwen3-ASR-1.7B-Q8_0.gguf
//!     mmproj-Qwen3-ASR-1.7B-Q8_0.gguf
//! ```

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

// 下载/校验/进度逻辑已泛化到 net.rs（外部工具自动安装共用同一套实现）。
use crate::net::{self, Verify};

const HF_REPO_PATH: &str = "ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main";

/// Hugging Face 端点：尊重 HF_ENDPOINT 镜像（与 CoreML 路径行为一致）。
/// 此前 GGUF 下载硬编码 huggingface.co，网络受限环境（如 Windows 直连
/// 失败）设置镜像也无效（issue #2）。
fn hf_base(endpoint: Option<String>) -> String {
    endpoint
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://huggingface.co".into())
}

fn current_hf_endpoint() -> Option<String> {
    std::env::var("HF_ENDPOINT").ok()
}

#[derive(Debug, Clone)]
pub struct LlamaAsr {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

pub fn llama_paths(root: &Path) -> LlamaAsr {
    let d = root.join("llama-qwen3-1.7b");
    LlamaAsr {
        model: d.join("Qwen3-ASR-1.7B-Q8_0.gguf"),
        mmproj: d.join("mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
    }
}

pub fn llama_ready(root: &Path) -> bool {
    let p = llama_paths(root);
    file_complete(&p.model) && file_complete(&p.mmproj)
}

/// 文件完整性：有 manifest（下载完成时记录的精确字节数）时按字节数校验；
/// 无 manifest 的旧缓存退回 >1MB 启发式。
fn file_complete(path: &Path) -> bool {
    net::is_complete(path, &Verify::Size)
}

pub fn ensure_llama(root: &Path) -> Result<LlamaAsr> {
    if !llama_ready(root) {
        anyhow::bail!(
            "缺少识别模型，请运行：course2md models download\n目录：{}",
            root.display()
        );
    }
    Ok(llama_paths(root))
}

/// 没有模型就下载；下载过程请保持进程运行。
pub async fn ensure_llama_or_download(root: &Path) -> Result<LlamaAsr> {
    if !llama_ready(root) {
        let (zh, en) = (
            "第一次运行，正在下载识别模型（约 2.4GB），请不要退出。",
            "First run: downloading the ASR model (~2.4GB), please keep this process running.",
        );
        tracing::warn!("{}", crate::i18n::tr(en, zh));
        eprintln!("{}", crate::i18n::tr(en, zh));
        download_models(root).await?;
    }
    Ok(llama_paths(root))
}

/// 下载 llama.cpp Qwen3-ASR GGUF。
pub async fn download_models(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    let p = llama_paths(root);
    let base = hf_base(current_hf_endpoint());
    tracing::info!(endpoint = %base, "huggingface endpoint");
    download_file(
        &format!("{base}/{HF_REPO_PATH}/Qwen3-ASR-1.7B-Q8_0.gguf"),
        &p.model,
        "Qwen3-ASR-1.7B-Q8_0.gguf",
    )
    .await?;
    download_file(
        &format!("{base}/{HF_REPO_PATH}/mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
        &p.mmproj,
        "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
    )
    .await?;
    tracing::info!(path = %root.display(), "models ready");
    Ok(())
}

async fn download_file(url: &str, dest: &Path, label: &str) -> Result<()> {
    net::download_file(&net::Download {
        url: url.to_string(),
        dest: dest.to_path_buf(),
        label: label.to_string(),
        verify: Verify::Size,
    })
    .await
}

pub fn list_models(root: &Path) {
    let p = llama_paths(root);
    println!("模型目录：{}", root.display());
    println!(
        "  model  {} {}",
        if p.model.is_file() { "OK" } else { "缺" },
        p.model.display()
    );
    println!(
        "  mmproj {} {}",
        if p.mmproj.is_file() { "OK" } else { "缺" },
        p.mmproj.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hf_endpoint_mirror_is_honored() {
        assert_eq!(hf_base(None), "https://huggingface.co");
        assert_eq!(
            hf_base(Some("https://hf-mirror.com".into())),
            "https://hf-mirror.com"
        );
        // 尾部斜杠归一；空白视为未设置
        assert_eq!(
            hf_base(Some("https://hf-mirror.com/".into())),
            "https://hf-mirror.com"
        );
        assert_eq!(hf_base(Some("  ".into())), "https://huggingface.co");
    }
}
