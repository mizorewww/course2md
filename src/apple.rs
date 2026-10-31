//! Apple 原生后端（仅 `apple_native` 构建生效）：
//! Silero VAD（CoreML/ANE）+ Qwen3-ASR（CoreML，来自 speech-swift）。
//! 模型首次使用时自动下载到 ~/Library/Caches/qwen3-speech/（HF_ENDPOINT 可换镜像）。

#![cfg(apple_native)]

use crate::config::{AsrProvider, DEFAULT_ASR_MODEL, canonical_model};
use crate::progress::Stage;
use anyhow::{Context, Result};
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

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
            unit: None,
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

/// The whole-model byte download in progress, once Swift has planned it.
static TRANSFER: std::sync::Mutex<Option<(crate::progress::TransferLedger, crate::progress::Bar)>> =
    std::sync::Mutex::new(None);

/// Swift resolved every file the download will fetch. Returns 0 when byte
/// progress follows, so Swift stops reporting bare fractions.
#[unsafe(no_mangle)]
extern "C" fn c2m_model_transfer_plan(plan: *const std::ffi::c_char) -> std::ffi::c_int {
    if plan.is_null() {
        return -1;
    }
    let plan = unsafe { CStr::from_ptr(plan) }.to_string_lossy();
    let plan: crate::progress::TransferPlan = match serde_json::from_str(&plan) {
        Ok(plan) => plan,
        Err(error) => {
            tracing::debug!("Apple download plan unreadable: {error}");
            return -1;
        }
    };
    let ledger = crate::progress::TransferLedger::new(plan);
    let bar = crate::progress::Bar::new(Stage::ModelApple, ledger.total())
        .with_unit(crate::progress::Unit::Bytes)
        .with_template(crate::download::MODEL_BAR)
        .with_position(ledger.present());
    bar.set_phase(
        crate::progress::Phase::Download,
        ledger.host(),
        crate::tr!("Apple 识别模型 · {}", ledger.host()),
    );
    *crate::runtime::lock_recover(&TRANSFER) = Some((ledger, bar));
    0
}

/// Repository `repo` of the plan has fetched `fraction` of its bytes.
#[unsafe(no_mangle)]
extern "C" fn c2m_model_transfer_progress(repo: std::ffi::c_int, fraction: f64) {
    let mut transfer = crate::runtime::lock_recover(&TRANSFER);
    if let Some((ledger, bar)) = transfer.as_mut()
        && let Ok(repo) = usize::try_from(repo)
    {
        // speech-swift reports at most once per landed range, so every
        // change is sent; cached files and repeats change nothing.
        let before = ledger.reported();
        let now = ledger.advance(repo, fraction);
        if now != before {
            bar.report_position(now);
        }
    }
}

/// Reports the last byte count and clears the terminal bar.
fn finish_transfer() {
    if let Some((_, bar)) = crate::runtime::lock_recover(&TRANSFER).take() {
        bar.finish();
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

/// 解析 coreml 后端用的模型：显式值（CLI 或 config.toml defaults.asr_model）规范化后返回；
/// 未设置时交互终端询问一次并写入 config.toml，非交互环境用默认模型。
/// CLI 在准备模型缓存之前调用，确保下载的正是转写时加载的模型。
pub fn resolve_model(explicit: Option<&str>) -> Result<String> {
    if let Some(model) = explicit.filter(|m| !m.trim().is_empty()) {
        return canonical_model(AsrProvider::Coreml, model);
    }
    if crate::progress::is_json() || crate::progress::is_quiet() || !crate::runtime::interactive() {
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

    /// Checks the runtime, then loads the prepared model for a task.
    pub(crate) fn load_for_task(model: &str) -> Result<Self> {
        ensure_metallib()?;
        tracing::info!(model, "loading cached Apple native ASR");
        crate::progress::start(Stage::ModelApple);
        let asr = Self::load(model).context(crate::tr!(
            "CoreML 模型加载失败 / CoreML model loading failed"
        ))?;
        crate::progress::done(Stage::ModelApple);
        Ok(asr)
    }

    /// 转写 16k 单声道 wav。Ok(None) = 无语音内容。
    fn transcribe_wav(&self, wav: &Path) -> Result<Option<String>> {
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

/// Downloads the model's cache files with the Swift downloader in this process,
/// from whatever HF_ENDPOINT this process started with.
pub fn prepare_cache_here(model: &str) -> Result<()> {
    ensure_metallib()?;
    let name = CString::new(model)?;
    let manifest = CString::new(cache_manifest(model)?)?;
    let status = unsafe { ffi::c2m_asr_prepare_cache(name.as_ptr(), manifest.as_ptr()) };
    finish_transfer();
    if status != 0 {
        anyhow::bail!("{}", last_error());
    }
    Ok(())
}

/// Prepares the model cache, trying the configured mirror first. The Swift
/// downloader only reads HF_ENDPOINT from the environment, and changing this
/// multi-threaded process's environment is unsound, so a mirror attempt runs
/// in a child process started with that endpoint. An explicit HF_ENDPOINT is
/// left alone.
pub fn prepare_cache(model: &str, control: &crate::dispatch::TaskControl) -> Result<()> {
    let mirror = if std::env::var_os("HF_ENDPOINT").is_none() {
        crate::hub::endpoints().hub_override()
    } else {
        None
    };
    let Some(mirror) = mirror else {
        return prepare_cache_here(model);
    };
    let program = std::env::current_exe()?;
    let mut result = prepare_cache_from(&program, model, mirror, control);
    if let Err(error) = &result
        && control.check().is_ok()
    {
        tracing::warn!(
            "Apple 模型从镜像下载失败，改用 Hugging Face / Apple model download from the mirror failed; trying Hugging Face: {error:#}"
        );
        result = prepare_cache_from(&program, model, crate::hub::ORIGIN, control);
    }
    result
}

/// Runs `program apple-cache` (the course2md executable) with HF_ENDPOINT set
/// for that process only. Its progress events share this process's output;
/// its errors come back here.
fn prepare_cache_from(
    program: &Path,
    model: &str,
    endpoint: &str,
    control: &crate::dispatch::TaskControl,
) -> Result<()> {
    let mut command = std::process::Command::new(program);
    command
        .args(["apple-cache", model, "--progress"])
        .arg(if crate::progress::is_json() {
            "json"
        } else if crate::progress::is_quiet() {
            "quiet"
        } else {
            "human"
        })
        .env("HF_ENDPOINT", endpoint)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = crate::runtime::ManagedChild::spawn("course2md apple-cache", &mut command)?;
    let stderr = child.take_stderr();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            use std::io::Read as _;
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    let status = loop {
        if let Some(status) = child.try_wait() {
            break status;
        }
        if let Err(error) = control.check() {
            drop(child);
            return Err(error);
        }
        std::thread::sleep(crate::dispatch::CONTROL_POLL);
    };
    let errors = reader.join().unwrap_or_default();
    anyhow::ensure!(status.success(), "{}", errors.trim());
    Ok(())
}

impl Drop for CoremlAsr {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ffi::c2m_asr_destroy(self.handle) };
        }
    }
}

/// Silero VAD 分段。
pub(crate) fn segments(wav: &Path, max_speech: f64) -> Result<Vec<crate::asr::Seg>> {
    let raw = vad(wav, 0.25, 0.35)?;
    crate::asr::normalize_segments(raw, max_speech, wav, crate::asr::wav_duration(wav)?)
}

/// The shim's handle is used by one thread at a time: one worker, which `map_pool`
/// runs on the thread that loaded it, as its thread-local last error requires.
unsafe impl Sync for CoremlAsr {}
unsafe impl Send for CoremlAsr {}

impl crate::asr::Transcribe for CoremlAsr {
    fn transcribe(&self, chunk: &Path, _seg: crate::asr::Seg) -> Result<String> {
        Ok(self
            .transcribe_wav(chunk)?
            .map(|text| crate::asr::sanitize_qwen_text(&text))
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use std::time::Instant;

    /// Network test: run with `QWEN3_CACHE_DIR=<scratch dir outside /private>
    /// cargo test --lib -- --ignored mirror_attempt` after `cargo build`.
    #[test]
    #[ignore = "downloads part of an Apple model from the Hub mirror"]
    fn mirror_attempt_runs_in_a_helper_and_stops_when_the_task_pauses() {
        let cache = std::path::PathBuf::from(
            std::env::var_os("QWEN3_CACHE_DIR")
                .expect("set QWEN3_CACHE_DIR to a scratch directory"),
        );
        let program = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/course2md");
        assert!(program.is_file(), "build course2md first");
        let work = tempfile::tempdir().unwrap();
        let control_path = work.path().join("control.json");
        let control = crate::dispatch::TaskControl::open(
            work.path(),
            crate::dispatch::ControlSource {
                control_path: Some(control_path.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let pause = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(15));
            std::fs::write(&control_path, br#"{"intent":"pause"}"#).unwrap();
        });
        let started = std::time::Instant::now();
        let result = prepare_cache_from(&program, "qwen3-0.6b", crate::hub::MIRROR, &control);
        pause.join().unwrap();
        let elapsed = started.elapsed();
        assert!(result.is_err(), "the pause stops the helper");
        assert!(
            elapsed < std::time::Duration::from_secs(25),
            "stopped promptly: {elapsed:?}"
        );
        let bytes: u64 = walk(&cache);
        assert!(
            bytes > 1_000_000,
            "the helper downloaded from the mirror: {bytes} bytes"
        );
    }

    /// Runs the real Silero VAD through the shim. Downloads only its model (about
    /// 1.4 MB). Run with `QWEN3_CACHE_DIR=<scratch dir outside /private> cargo test
    /// --lib -- --ignored --nocapture vad_detects`; needs macOS `say` and ffmpeg.
    #[test]
    #[ignore = "downloads the VAD model and synthesizes speech"]
    fn vad_detects_speech_and_loads_its_model_once() {
        assert!(std::env::var_os("QWEN3_CACHE_DIR").is_some());
        let work = tempfile::tempdir().unwrap();
        let (speech, silence) = (
            work.path().join("speech.wav"),
            work.path().join("quiet.wav"),
        );
        let aiff = work.path().join("speech.aiff");
        let run = |command: &mut std::process::Command| {
            let status = command.status().unwrap();
            assert!(status.success(), "{command:?}");
        };
        run(std::process::Command::new("say")
            .arg("-o")
            .arg(&aiff)
            .arg("The blue notebook contains seven pages. Water freezes at zero degrees."));
        for (args, output) in [
            (vec!["-i".into(), aiff.display().to_string()], &speech),
            (
                ["-f", "lavfi", "-i", "anullsrc=r=16000:cl=mono", "-t", "3"]
                    .map(str::to_owned)
                    .to_vec(),
                &silence,
            ),
        ] {
            run(std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args(args)
                .args(["-ar", "16000", "-ac", "1"])
                .arg(output));
        }

        let vad_part = &crate::models::status::APPLE_VAD;
        let mut files: Vec<String> = vad_part.files.iter().map(|f| (*f).to_owned()).collect();
        files.extend(vad_part.bundles.iter().map(|b| format!("{b}/**")));
        let manifest =
            CString::new(serde_json::json!([{"repo": vad_part.repo, "files": files}]).to_string())
                .unwrap();
        let model = CString::new("qwen3-0.6b").unwrap();
        let status = unsafe { ffi::c2m_asr_prepare_cache(model.as_ptr(), manifest.as_ptr()) };
        finish_transfer();
        assert_eq!(status, 0, "{}", last_error());

        let started = Instant::now();
        assert_eq!(unsafe { ffi::c2m_vad_load() }, 0, "{}", last_error());
        let load = started.elapsed();
        // Each call after the load only has inference left to do.
        let timed = |wav: &Path| {
            let started = Instant::now();
            let segments = vad(wav, 0.25, 0.35).unwrap();
            (segments, started.elapsed())
        };
        let (found, first) = timed(&speech);
        let (again, second) = timed(&speech);
        let (none, third) = timed(&silence);
        println!("load {load:?}; detect {first:?}, {second:?}, {third:?}; segments {found:?}");
        assert!(!found.is_empty(), "speech was detected");
        assert!(found.iter().all(|(start, end)| end > start));
        assert_eq!(found, again, "state does not leak between files");
        assert!(none.is_empty(), "silence has no speech: {none:?}");
        assert_eq!(unsafe { ffi::c2m_vad_load() }, 0, "{}", last_error());
    }

    fn walk(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| {
                        let path = entry.path();
                        if path.is_dir() {
                            walk(&path)
                        } else {
                            entry.metadata().map_or(0, |m| m.len())
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
}
