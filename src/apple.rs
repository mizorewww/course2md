//! Apple 原生后端（仅 `apple_native` 构建生效）：
//! Silero VAD（CoreML/ANE）+ Qwen3-ASR（CoreML，来自 speech-swift）。
//! 模型首次使用时自动下载到 ~/Library/Caches/qwen3-speech/（HF_ENDPOINT 可换镜像）。

#![cfg(apple_native)]

use crate::config::{AsrProvider, DEFAULT_ASR_MODEL, canonical_model};
use crate::progress::Stage;
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod ffi {
    use std::os::raw::{c_char, c_double, c_int};

    unsafe extern "C" {
        pub fn c2m_vad_detect(
            wav_path: *const c_char,
            min_speech: c_double,
            min_silence: c_double,
            out_starts: *mut *mut c_double,
            out_ends: *mut *mut c_double,
            out_n: *mut c_int,
        ) -> c_int;
        pub fn c2m_free_doubles(p: *mut c_double);
        pub fn c2m_asr_create(model: *const c_char) -> *mut std::ffi::c_void;
        pub fn c2m_asr_prepare_cache(model: *const c_char, manifest: *const c_char) -> c_int;
        pub fn c2m_vad_load() -> c_int;
        pub fn c2m_asr_transcribe(
            handle: *mut std::ffi::c_void,
            wav_path: *const c_char,
            out_text: *mut c_char,
            out_len: usize,
        ) -> c_int;
        pub fn c2m_asr_destroy(handle: *mut std::ffi::c_void);
        pub fn c2m_last_error() -> *const c_char;
    }
}

// Swift downloads report through the same serialized NDJSON stream as Rust.
#[unsafe(no_mangle)]
extern "C" fn c2m_model_progress(
    fraction: f64,
    phase: *const std::ffi::c_char,
    message: *const std::ffi::c_char,
) {
    if message.is_null() {
        return;
    }
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let phase = if phase.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(phase) }.to_str().ok()
    };
    let phase = match phase {
        Some("download") => crate::progress::Phase::Download,
        _ => crate::progress::Phase::Load,
    };
    let fraction = fraction.clamp(0.0, 1.0);
    if crate::progress::is_json() {
        crate::progress::emit(&crate::progress::Event::Progress {
            stage: Stage::ModelApple,
            current: (fraction * 10000.0) as u64,
            total: 10000,
            message: Some(message.into_owned()),
            phase: Some(phase),
        });
    } else if !crate::progress::is_quiet() {
        // 终端只在整百分比变化时打印一行，避免下载回调刷屏
        static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
        let percent = (fraction * 100.0) as i64;
        if LAST.swap(percent, std::sync::atomic::Ordering::Relaxed) != percent {
            crate::eprintln_tr!("模型 {}% · {}", percent, message);
        }
    }
}

fn last_error() -> String {
    unsafe {
        let p = ffi::c2m_last_error();
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Silero VAD（CoreML）。返回 (start, end) 语音段（秒）。
pub fn vad(wav: &Path, min_speech: f64, min_silence: f64) -> Result<Vec<(f64, f64)>> {
    let path = CString::new(wav.to_string_lossy().as_bytes())?;
    let mut starts: *mut f64 = std::ptr::null_mut();
    let mut ends: *mut f64 = std::ptr::null_mut();
    let mut n: i32 = 0;
    let rc = unsafe {
        ffi::c2m_vad_detect(
            path.as_ptr(),
            min_speech,
            min_silence,
            &mut starts,
            &mut ends,
            &mut n,
        )
    };
    if rc != 0 {
        anyhow::bail!("语音检测失败 / Speech detection failed: {}", last_error());
    }
    // FFI 契约防御：rc==0 且 n>0 时指针必须非空
    anyhow::ensure!(
        n >= 0 && (n == 0 || (!starts.is_null() && !ends.is_null())),
        crate::tr!("语音检测返回了空结果指针 / Speech detection returned null result pointers")
    );
    let mut out = Vec::with_capacity(n as usize);
    if n > 0 {
        unsafe {
            for i in 0..n as usize {
                out.push((starts.add(i).read(), ends.add(i).read()));
            }
            ffi::c2m_free_doubles(starts);
            ffi::c2m_free_doubles(ends);
        }
    }
    Ok(out)
}

pub struct CoremlAsr {
    handle: *mut std::ffi::c_void,
}

/// 本进程可执行文件对应的 metallib；搜索顺序见 config::find_metallib。
pub fn find_metallib() -> Option<PathBuf> {
    crate::config::find_metallib(&std::env::current_exe().ok()?)
}

fn ensure_metallib() -> Result<()> {
    if find_metallib().is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    tracing::debug!(
        "源码构建：把 native/apple-asr/.build 下的 mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib 复制为 {}",
        dir.join("mlx.metallib").display()
    );
    anyhow::bail!(crate::tr!(
        "缺少 Apple 识别所需的组件，请重新安装 course2md / Apple speech component missing; reinstall course2md"
    ))
}

fn interactive() -> bool {
    use std::io::IsTerminal as _;
    !crate::progress::is_json()
        && !crate::progress::is_quiet()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
}

/// 解析 coreml 后端用的模型：显式值（CLI 或 config.toml defaults.asr_model）规范化后返回；
/// 未设置时交互终端询问一次并写入 config.toml，非交互环境用默认模型。
/// CLI 在准备模型缓存之前调用，确保下载的正是转写时加载的模型。
pub fn resolve_model(explicit: Option<&str>) -> Result<String> {
    if let Some(model) = explicit.filter(|m| !m.trim().is_empty()) {
        return canonical_model(AsrProvider::Coreml, model);
    }
    if !interactive() {
        tracing::info!(
            "非交互环境，使用默认模型 {DEFAULT_ASR_MODEL}（可用 --asr-model 切换） / Non-interactive; using {DEFAULT_ASR_MODEL} (change with --asr-model)"
        );
        return Ok(DEFAULT_ASR_MODEL.into());
    }
    let chosen = prompt_model_choice()?;
    persist_model_choice(&chosen);
    Ok(chosen)
}

/// 把交互选择的模型写入 config.toml defaults.asr_model。
/// 失败只告警不阻断：本次已拿到选择，下次仍会再询问。
fn persist_model_choice(model: &str) {
    if let Err(e) = (|| -> Result<()> {
        let mut cfg = crate::settings::load()?;
        cfg.defaults.asr_model = Some(model.to_string());
        crate::settings::save(&cfg)?;
        Ok(())
    })() {
        tracing::warn!(
            "无法保存模型选择，下次仍会询问 / Could not save model choice; you will be asked again next time: {e:#}"
        );
    }
}

/// 首次使用：选择下载哪个 Apple 模型。
fn prompt_model_choice() -> Result<String> {
    let models = crate::config::local_models(AsrProvider::Coreml);
    let items: Vec<String> = models
        .iter()
        .map(|m| format!("{}{}", m.label, crate::tr!(m.note)))
        .collect();
    let choice = dialoguer::Select::new()
        .with_prompt(crate::tr!("选择识别模型 / Choose a speech model"))
        .items(&items)
        .default(0)
        .interact_opt()?;
    match choice {
        Some(index) => Ok(models[index].id.to_owned()),
        None => anyhow::bail!(crate::tr!(
            "已取消模型选择，未开始下载 / Model selection cancelled; download not started"
        )),
    }
}

impl CoremlAsr {
    /// 只加载已准备的缓存；缺失文件由模型准备阶段下载。
    pub fn load(model: &str) -> Result<Self> {
        let name = CString::new(model)?;
        let handle = unsafe { ffi::c2m_asr_create(name.as_ptr()) };
        if handle.is_null() {
            anyhow::bail!("{}", last_error());
        }
        Ok(Self { handle })
    }

    /// 转写 16k 单声道 wav。Ok(None) = 无语音内容。
    pub fn transcribe(&self, wav: &Path) -> Result<Option<String>> {
        let path = CString::new(wav.to_string_lossy().as_bytes())?;
        let mut out = vec![0u8; 16 * 1024];
        let rc = unsafe {
            ffi::c2m_asr_transcribe(
                self.handle,
                path.as_ptr(),
                out.as_mut_ptr() as *mut _,
                out.len(),
            )
        };
        match rc {
            0 | 2 => {
                let s = CStr::from_bytes_until_nul(&out)
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if rc == 2 {
                    // shim 侧缓冲（16KB）不够，文本被截断：至少留痕
                    tracing::warn!(
                        "Apple 识别结果过长，部分文字被截断 / Apple transcript exceeded the size limit; some text was truncated"
                    );
                }
                Ok(Some(s))
            }
            1 => Ok(None),
            _ => anyhow::bail!(
                "Apple 语音识别失败 / Apple transcription failed: {}",
                last_error()
            ),
        }
    }
}

/// Verify prepared files using the same offline loaders as transcription, with no user media.
pub fn prepare_model(model: &str) -> Result<()> {
    ensure_metallib()?;
    let _asr = CoremlAsr::load(model)?;
    // VAD is a separate required model; load it directly so readiness never depends
    // on a synthetic WAV or on the ASR files happening to be present.
    if unsafe { ffi::c2m_vad_load() } != 0 {
        anyhow::bail!("{}", last_error());
    }
    Ok(())
}

/// Cache-only preparation before downloading user media; inference loads weights once later.
fn cache_manifest(model: &str) -> Result<String> {
    let Some(parts) = crate::models::status::apple_parts(model) else {
        anyhow::bail!("不支持的 Apple 模型 / Unsupported Apple model: {model}");
    };
    let items: Vec<serde_json::Value> = parts
        .iter()
        .chain(std::iter::once(&crate::models::status::APPLE_VAD))
        .map(|part| {
            let mut files: Vec<String> =
                part.files.iter().map(|file| (*file).to_string()).collect();
            files.extend(part.bundles.iter().map(|bundle| format!("{bundle}/**")));
            serde_json::json!({"repo": part.repo, "files": files})
        })
        .collect();
    Ok(serde_json::to_string(&items)?)
}

pub fn prepare_cache(model: &str) -> Result<()> {
    ensure_metallib()?;
    let name = CString::new(model)?;
    let manifest = CString::new(cache_manifest(model)?)?;
    let attempt = || {
        let status = unsafe { ffi::c2m_asr_prepare_cache(name.as_ptr(), manifest.as_ptr()) };
        if status != 0 {
            anyhow::bail!("{}", last_error());
        }
        Ok(())
    };
    // The Swift downloader reads HF_ENDPOINT; an explicit setting is left alone.
    let mirror = if std::env::var_os("HF_ENDPOINT").is_none() {
        crate::hub::endpoints().hub_override()
    } else {
        None
    };
    let Some(mirror) = mirror else {
        return attempt();
    };
    // SAFETY: Darwin's libc serializes environment access, Rust's std takes
    // its own environment lock, and no download is running while it changes.
    unsafe { std::env::set_var("HF_ENDPOINT", mirror) };
    let mut result = attempt();
    if let Err(error) = &result
        && crate::dispatch::check_control().is_ok()
    {
        tracing::warn!(
            "Apple 模型从镜像下载失败，改用 Hugging Face / Apple model download from the mirror failed; trying Hugging Face: {error:#}"
        );
        // SAFETY: as above.
        unsafe { std::env::set_var("HF_ENDPOINT", crate::hub::ORIGIN) };
        result = attempt();
    }
    // SAFETY: as above.
    unsafe { std::env::remove_var("HF_ENDPOINT") };
    result
}

impl Drop for CoremlAsr {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ffi::c2m_asr_destroy(self.handle) };
        }
    }
}

/// CoreML 全流程：Silero VAD 分段 → 逐段转写。
pub fn run_coreml(
    wav: &Path,
    max_speech: f64,
    model: &str,
    tmp_dir: &Path,
    cp: &mut crate::checkpoint::Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let raw = vad(wav, 0.25, 0.35)?;
    // 时长缺失时不能按 0 处理：末段语音会被当成静音丢掉
    let dur = crate::media::probe_duration_blocking(wav)
        .filter(|d| d.is_finite() && *d > 0.0)
        .context(crate::tr!(
            "无法读取音频时长 / Could not read the audio duration"
        ))?;
    let segs = crate::asr::normalize_segments(raw, max_speech, wav, dur)?;
    tracing::info!(segs = segs.len(), engine = "silero-coreml", "vad");
    if segs.is_empty() {
        return Ok(crate::asr::no_speech_events());
    }

    ensure_metallib()?;
    tracing::info!(model, "loading cached Apple native ASR");
    crate::progress::start(Stage::ModelApple);
    let asr = CoremlAsr::load(model).context(crate::tr!(
        "CoreML 模型加载失败 / CoreML model loading failed"
    ))?;
    crate::progress::done(Stage::ModelApple);
    tracing::info!(
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "coreml ready"
    );

    let r = crate::asr::run_chunks(wav, &segs, cp, tmp_dir, "asr", |chunk| {
        asr.transcribe(chunk).map(|t| {
            let t = t.map(|s| crate::asr::sanitize_qwen_text(&s));
            t.filter(|s| !s.is_empty())
        })
    })?;
    tracing::info!(
        n = r.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(r)
}
