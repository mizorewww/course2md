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

use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const HF_REPO_PATH: &str = "ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main";

/// llama GGUF 模型 slug：模型目录名由它派生，身份字符串与它同源
///（concat! 不接受常量，身份字符串用字面量 + 测试守住一致性）。
const LLAMA_MODEL_SLUG: &str = "qwen3-1.7b";
const LLAMA_GGUF_IDENTITY: &str = "qwen3-1.7b-gguf";
const LLAMA_MODEL_FILE: &str = "Qwen3-ASR-1.7B-Q8_0.gguf";
const LLAMA_MMPROJ_FILE: &str = "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf";

/// 当前 llama GGUF 模型的身份字符串（checkpoint/日志用），与模型目录同源。
pub fn llama_gguf_identity() -> &'static str {
    LLAMA_GGUF_IDENTITY
}

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
    let d = root.join(format!("llama-{LLAMA_MODEL_SLUG}"));
    LlamaAsr {
        model: d.join(LLAMA_MODEL_FILE),
        mmproj: d.join(LLAMA_MMPROJ_FILE),
    }
}

pub fn llama_ready(root: &Path) -> bool {
    let p = llama_paths(root);
    file_complete(&p.model) && file_complete(&p.mmproj)
}

/// 文件完整性：有 manifest（下载完成时记录的精确字节数）时按字节数校验；
/// 无 manifest 的旧缓存退回 >1MB 启发式。
fn file_complete(path: &Path) -> bool {
    let Ok(md) = fs::metadata(path) else {
        return false;
    };
    // >1MB 启发式：GGUF 合法文件头（magic + 版本 + 张量元数据索引）加上任何
    // 可用权重都远超 1MB；≤1MB 必是截断残留或代理/镜像返回的错误页面。
    if !path.is_file() || md.len() <= 1_000_000 {
        return false;
    }
    let manifest = path.with_extension("manifest.json");
    if let Ok(s) = fs::read_to_string(&manifest)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
        && let Some(expected) = v.get("size").and_then(|s| s.as_u64())
    {
        return md.len() == expected;
    }
    true
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
        // eprintln 而非 tracing::warn：-q 下 warn 被静默，2.4GB 下载前必须可见
        eprintln!(
            "识别模型未就绪，正在下载（约 2.4GB）到 {}，请不要退出。",
            root.display()
        );
        download_models(root).await?;
    }
    Ok(llama_paths(root))
}

/// 下载失败的附加提示：未设镜像时提示 HF_ENDPOINT（直连 Hugging Face
/// 不稳定是常见失败原因）；已设镜像则回显当前端点便于排查。
fn mirror_hint() -> String {
    match current_hf_endpoint() {
        Some(ep) => format!("下载失败（当前 HF_ENDPOINT={ep}）"),
        None => "下载失败：如直连 Hugging Face 不稳定，可设镜像环境变量 \
                 HF_ENDPOINT=https://hf-mirror.com 后重试"
            .into(),
    }
}

/// 下载 llama.cpp Qwen3-ASR GGUF。
pub async fn download_models(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    let _download_lock = crate::runtime::lock_file(&root.join(".download.lock"))?;
    let p = llama_paths(root);
    let base = hf_base(current_hf_endpoint());
    tracing::info!(endpoint = %base, "huggingface endpoint");
    let model_url = format!("{base}/{HF_REPO_PATH}/{LLAMA_MODEL_FILE}");
    let projector_url = format!("{base}/{HF_REPO_PATH}/{LLAMA_MMPROJ_FILE}");
    let (model, projector) = tokio::join!(
        download_file(&model_url, &p.model, LLAMA_MODEL_FILE),
        download_file(&projector_url, &p.mmproj, LLAMA_MMPROJ_FILE),
    );
    model.with_context(mirror_hint)?;
    projector.with_context(mirror_hint)?;
    tracing::info!(path = %root.display(), "models ready");
    Ok(())
}

/// 下载重试次数（2.4GB 大文件，网络抖动/代理断流常见）。
const DOWNLOAD_ATTEMPTS: usize = 3;

/// 4xx（除 429 限流）是确定性错误（鉴权失败/路径错/镜像缺文件），
/// 退避重试无意义——来自 PR #9 的重试分类思路。
fn is_permanent_status(code: u16) -> bool {
    code != 429 && (400..500).contains(&code)
}

/// 确定性 HTTP 错误标记：重试循环见此类型直接失败。
#[derive(Debug)]
struct PermanentHttp(String);

impl std::fmt::Display for PermanentHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PermanentHttp {}

async fn download_file(url: &str, dest: &Path, label: &str) -> Result<()> {
    if dest.is_file() && file_complete(dest) {
        tracing::info!(label, "skip existing");
        return Ok(());
    }
    if dest.is_file() {
        // 校验不过的残留文件（截断/损坏）直接移除，避免"发现坏了却重下不了"
        tracing::warn!(
            label,
            "existing file failed integrity check, re-downloading"
        );
        let _ = fs::remove_file(dest);
        let _ = fs::remove_file(dest.with_extension("manifest.json"));
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = dest.with_extension("part");
    let url = url.to_string();
    let dest = dest.to_path_buf();
    let label = label.to_string();
    let stage = format!("model/{label}");
    crate::progress::stage(&stage, "start");
    tokio::task::spawn_blocking(move || -> Result<()> {
        // 只设连接/读超时，不设整体超时：2.4GB 大文件下完为止，
        // 读超时 10 分钟无数据视为挂起
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(30))
            .timeout_read(std::time::Duration::from_secs(600))
            .build();
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=DOWNLOAD_ATTEMPTS {
            if attempt > 1 {
                let wait = std::time::Duration::from_secs(1 << (attempt - 1)); // 2s、4s 指数退避
                tracing::warn!(
                    label = %label,
                    attempt,
                    of = DOWNLOAD_ATTEMPTS,
                    ?wait,
                    "下载失败，退避后重试"
                );
                std::thread::sleep(wait);
            }
            match download_once(&agent, &url, &tmp, &dest, &label) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // 确定性 HTTP 错误（4xx≠429）不再退避重试
                    if e.downcast_ref::<PermanentHttp>().is_some() {
                        return Err(e);
                    }
                    // 保留 .part：日志给出断点位置；当前实现重跑会从头下载
                    tracing::warn!(
                        label = %label,
                        part = %tmp.display(),
                        "下载失败（{e:#}），已保留断点文件（重跑将重新下载）"
                    );
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("下载失败")))
    })
    .await
    .context("下载线程失败")??;
    crate::progress::stage(&stage, "done");
    Ok(())
}

/// 单次下载尝试：成功时原子落盘 dest 并写 manifest；失败保留 .part 供排查。
fn download_once(
    agent: &ureq::Agent,
    url: &str,
    tmp: &Path,
    dest: &Path,
    label: &str,
) -> Result<()> {
    tracing::info!(label = %label, url = %url, "download");
    let resp = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, resp)) => {
            // 错误信息携带响应体尾部（服务器返回的错误页/JSON 通常是排查关键）
            let body = resp.into_string().unwrap_or_default();
            let n = body.chars().count();
            let tail: String = body.chars().skip(n.saturating_sub(200)).collect();
            let msg = format!("HTTP {code}: {tail}");
            if is_permanent_status(code) {
                return Err(anyhow::anyhow!(PermanentHttp(msg)));
            }
            return Err(anyhow::anyhow!("请求失败: {msg}"));
        }
        Err(e) => return Err(anyhow::Error::new(e).context("请求失败")),
    };
    let total: u64 = resp
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let pb = crate::progress::Bar::new(format!("model/{label}"), total)
        .with_template("{spinner:.green} {msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({eta})");
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
    // 完整性：以服务器 Content-Length 为准（而非"实际收到多少"——截断响应会伪装成功）；
    // 不完整时保留 .part（断点位置见上方日志）
    if total > 0 && done != total {
        pb.finish();
        anyhow::bail!("下载不完整：期望 {total} 字节，实际收到 {done}（请重试）");
    }
    fs::rename(tmp, dest)?;
    // manifest 记录 authoritative Content-Length，供后续启动校验（原子写，防半截 JSON）
    let _ = crate::checkpoint::atomic_write(
        &dest.with_extension("manifest.json"),
        serde_json::json!({"size": if total > 0 { total } else { done }})
            .to_string()
            .as_bytes(),
    );
    pb.finish();
    tracing::info!(label = %label, bytes = done, "downloaded");
    Ok(())
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
    fn identity_matches_dir_slug() {
        assert_eq!(llama_gguf_identity(), format!("{LLAMA_MODEL_SLUG}-gguf"));
        assert!(
            llama_paths(Path::new("/x"))
                .model
                .starts_with(format!("/x/llama-{LLAMA_MODEL_SLUG}"))
        );
    }

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

    #[test]
    fn permanent_status_classification() {
        // 确定性错误：不重试
        for code in [400, 401, 403, 404, 422] {
            assert!(is_permanent_status(code), "{code} 应直接失败");
        }
        // 429 限流与 5xx 服务端错误：值得退避重试
        for code in [429, 500, 502, 503] {
            assert!(!is_permanent_status(code), "{code} 应重试");
        }
    }
}
