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

use crate::progress::Stage;
use crate::{eprintln_tr, println_tr, tr};
use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[path = "model_status.rs"]
pub mod status;

fn unsupported_model(status: &status::LocalModelStatus) -> anyhow::Error {
    crate::config::unsupported_model_error(status.provider, &status.model)
}

/// What Apple preparation does beyond the shared download: `Cache` only fetches files,
/// for the later transcription to load; `Prepare` also loads the model.
enum CoremlMode {
    Cache,
    // Only the Apple native backend reads whether files were already cached.
    #[cfg_attr(not(apple_native), allow(dead_code))]
    Prepare {
        cached: bool,
    },
}

async fn fetch_model(
    provider: crate::config::AsrProvider,
    model: &str,
    root: &Path,
    coreml: CoremlMode,
    control: &crate::dispatch::TaskControl,
) -> Result<()> {
    use crate::config::AsrProvider;
    match provider {
        AsrProvider::Cpu | AsrProvider::Gpu => download_models(root, control).await,
        AsrProvider::Coreml => {
            #[cfg(apple_native)]
            {
                let model = model.to_owned();
                let control = control.clone();
                tokio::task::spawn_blocking(move || {
                    match coreml {
                        CoremlMode::Cache => crate::apple::prepare_cache(&model, &control)?,
                        CoremlMode::Prepare { cached } => {
                            if !cached {
                                crate::apple::prepare_cache(&model, &control)?;
                            }
                            crate::apple::prepare_model(&model)?;
                        }
                    }
                    Ok(())
                })
                .await
                .context(tr!("模型准备未完成 / Model preparation did not complete"))?
            }
            #[cfg(not(apple_native))]
            {
                let _ = (model, coreml, control);
                Err(anyhow::anyhow!(tr!(
                    "此程序未包含 Apple 原生识别运行时 / This build does not include the Apple native transcription runtime"
                )))
            }
        }
        AsrProvider::Npu => {
            let model = crate::npu::resolve_npu_model(Some(model));
            let control = control.clone();
            tokio::task::spawn_blocking(move || crate::npu::prepare_npu_model(&model, &control))
                .await
                .context(tr!(
                    "NPU 模型准备未完成 / NPU model preparation did not complete"
                ))?
        }
        AsrProvider::Api => Ok(()),
    }
}

/// Retrieve missing local model files before a large media download. Cached weights are
/// loaded by the later transcription stage; subtitle-only work never calls this function.
pub async fn ensure_cache(
    provider: crate::config::AsrProvider,
    model: &str,
    root: &Path,
    control: &crate::dispatch::TaskControl,
) -> Result<()> {
    let status = status::inspect(provider, model, root)?;
    if !status.can_prepare {
        return Err(unsupported_model(&status));
    }
    if matches!(
        status.state,
        status::CacheState::Cached | status::CacheState::Loaded
    ) {
        return Ok(());
    }
    fetch_model(provider, &status.model, root, CoremlMode::Cache, control).await
}

/// Prepare the exact selected backend without sending course content or changing settings.
pub async fn prepare(
    provider: crate::config::AsrProvider,
    model: &str,
    root: &Path,
) -> Result<status::LocalModelStatus> {
    use crate::config::AsrProvider;
    let before = status::inspect(provider, model, root)?;
    if !before.can_prepare {
        return Err(unsupported_model(&before));
    }
    crate::progress::start(Stage::ModelPrepare);
    let cached = matches!(
        before.state,
        status::CacheState::Cached | status::CacheState::Loaded
    );
    let result = fetch_model(
        provider,
        &before.model,
        root,
        CoremlMode::Prepare { cached },
        &crate::dispatch::TaskControl::default(),
    )
    .await;
    let after = status::inspect(provider, model, root).unwrap_or_else(|error| {
        tracing::warn!("模型二次检查失败，沿用准备前状态 / Post-prepare inspection failed; keeping the pre-prepare status: {error:#}");
        before
    });
    let loaded = result.is_ok() && matches!(provider, AsrProvider::Coreml | AsrProvider::Npu);
    let error = result.as_ref().err().map(|error| format!("{error:#}"));
    if let Err(error) = status::record_result(&after, loaded, error) {
        tracing::warn!(
            "模型检查结果暂时无法保存 / Could not save the model check result: {error:#}"
        );
    }
    result?;
    crate::progress::done(Stage::ModelPrepare);
    Ok(status::with_receipt(after))
}

const HF_REPO: &str = "ggml-org/Qwen3-ASR-1.7B-GGUF";

/// llama GGUF 模型 slug：模型目录名与 checkpoint 身份都由它派生。
const LLAMA_MODEL_SLUG: &str = crate::config::DEFAULT_ASR_MODEL;
const LLAMA_MODEL: crate::download::Remote = crate::download::Remote {
    repo: HF_REPO,
    file: "Qwen3-ASR-1.7B-Q8_0.gguf",
    size: 2_165_034_944,
    sha256: "58e22d0532d4eacaf034cfac17a6fed159f37c41390c710186783be439d1fc57",
};
const LLAMA_MMPROJ: crate::download::Remote = crate::download::Remote {
    repo: HF_REPO,
    file: "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
    size: 355_709_344,
    sha256: "46c1d533af3f354ceb37ce855dbceff7da7fa7cf1e6a523df3b13440bd164c0d",
};

/// 当前 llama GGUF 模型的身份字符串（checkpoint/日志用），与模型目录同源。
pub fn llama_gguf_identity() -> String {
    format!("{LLAMA_MODEL_SLUG}-gguf")
}

#[derive(Debug, Clone)]
pub struct LlamaAsr {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

pub fn llama_paths(root: &Path) -> LlamaAsr {
    let d = root.join(format!("llama-{LLAMA_MODEL_SLUG}"));
    LlamaAsr {
        model: d.join(LLAMA_MODEL.file),
        mmproj: d.join(LLAMA_MMPROJ.file),
    }
}

pub fn llama_ready(root: &Path) -> bool {
    let p = llama_paths(root);
    file_complete(&p.model) && file_complete(&p.mmproj)
}

/// 文件完整性：有 manifest（下载完成时记录的精确字节数）时按字节数校验；
/// 无 manifest 的旧缓存检查合法 GGUF 文件头及最小大小；这不等于模型已加载成功。
fn file_complete(path: &Path) -> bool {
    let Ok(md) = fs::metadata(path) else {
        return false;
    };
    // >1MB 启发式：GGUF 合法文件头（magic + 版本 + 张量元数据索引）加上任何
    // 可用权重都远超 1MB；≤1MB 必是截断残留或代理/镜像返回的错误页面。
    if !path.is_file() || md.len() <= 1_000_000 {
        return false;
    }
    let mut header = [0_u8; 8];
    if fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_err()
        || &header[..4] != b"GGUF"
        || !matches!(u32::from_le_bytes(header[4..].try_into().unwrap()), 2 | 3)
    {
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

/// 没有模型就下载；下载过程请保持进程运行。
pub async fn ensure_llama_or_download(
    root: &Path,
    control: &crate::dispatch::TaskControl,
) -> Result<LlamaAsr> {
    if !llama_ready(root) {
        if !crate::progress::is_json() && !crate::progress::is_quiet() {
            eprintln_tr!(
                "正在下载识别模型（约 2.4GB）/ Downloading speech models (~2.4GB): {}",
                root.display()
            );
        }
        download_models(root, control).await?;
    }
    Ok(llama_paths(root))
}

/// 下载 llama.cpp Qwen3-ASR GGUF。
pub async fn download_models(root: &Path, control: &crate::dispatch::TaskControl) -> Result<()> {
    fs::create_dir_all(root)?;
    let _download_lock = crate::runtime::lock_file(&root.join(".download.lock"))?;
    let p = llama_paths(root);
    let files = [(LLAMA_MODEL, p.model), (LLAMA_MMPROJ, p.mmproj)];
    let missing: Vec<_> = files
        .into_iter()
        .filter(|(_, path)| !file_complete(path))
        .collect();
    if !missing.is_empty() {
        // The host lookup may wait on the network; keep it off the async workers.
        tokio::task::spawn_blocking(crate::hub::endpoints)
            .await
            .context(tr!("模型下载任务失败 / Model download task failed"))?;
    }
    let downloader = std::sync::Arc::new(crate::download::Downloader::new());
    let tasks: Vec<_> = missing
        .into_iter()
        .map(|(remote, path)| {
            let downloader = downloader.clone();
            let control = control.clone();
            tokio::task::spawn_blocking(move || {
                if path.is_file() {
                    tracing::warn!(
                        file = remote.file,
                        "模型文件不完整，正在重新下载 / Model file incomplete; downloading again"
                    );
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_file(path.with_extension("manifest.json"));
                }
                downloader.fetch(&remote, &path, &control)
            })
        })
        .collect();
    // Await both, so a failed file does not abandon the other mid-transfer.
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await);
    }
    for result in results {
        result.context(tr!("模型下载任务失败 / Model download task failed"))?
            .context(tr!(
                "模型下载失败；可设置 HF_ENDPOINT 指定下载地址 / Model download failed; set HF_ENDPOINT to choose a download host"
            ))?;
    }
    anyhow::ensure!(
        llama_ready(root),
        tr!(
            "模型文件下载不完整，请重新运行 course2md models download / Model download incomplete; run course2md models download again"
        )
    );
    tracing::info!(path = %root.display(), "models ready");
    Ok(())
}

pub fn list_models(root: &Path) {
    let p = llama_paths(root);
    println!(
        "gpu/cpu 模型目录 / gpu/cpu model directory: {}",
        root.display()
    );
    for (name, path) in [("model ", &p.model), ("mmproj", &p.mmproj)] {
        let state = if file_complete(path) {
            tr!("就绪 / Ready")
        } else {
            tr!("缺失或不完整 / Missing or incomplete")
        };
        println!("  {name} {state} {}", path.display());
    }
    if !llama_ready(root) {
        println!(
            "下载或修复 / Download or repair: course2md models download --dir \"{}\"",
            root.display()
        );
    }
    println_tr!(
        "Apple 和 NPU 模型由各自后端管理 / Apple and NPU models are managed by their backends."
    );
}
