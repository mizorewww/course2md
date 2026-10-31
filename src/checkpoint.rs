//! ASR checkpoint：逐 chunk 追加写 asr.jsonl，重跑时按精确时间边界跳过已完成段。
//!
//! 语义：
//! - **运行身份**：`.asr_identity` 记录 (schema 版本, provider, 模型, max_speech)。
//!   身份不一致的旧进度（换模型/换后端/换切分长度）整体作废并重算，
//!   杜绝「换模型后 resume 混用两个模型的转写」。
//! - **空结果也算完成**：成功但无语音的 chunk 记录为 `text: ""`，
//!   否则静音段每次断点续跑都会重复 ASR。
//! - **写盘失败不标记完成**：record() 返回 Result，只有落盘成功才计入 done。
//! - **损坏策略**：最后一行允许是崩溃残留的半截 JSON（容忍恢复）；
//!   中间任何损坏行都是硬错误（静默跳过会导致转写内容悄悄缺失）。
//! - **不兼容进度归档**：不复用进度时移动旧 checkpoint 到 .previous-asr，
//!   否则旧记录会与新记录叠加，之后 resume 会输出双份文本。
//! - 全部完成后原子写 `.asr_done` 标记，重跑完全跳过 ASR。

use crate::timeline::TranscriptEvent;
use crate::tr;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

/// checkpoint 身份的 schema 版本：仅当 checkpoint 的格式/语义变化时才 bump。
/// 与 course2md 发布版本（CARGO_PKG_VERSION）解耦——patch 版本升级不应作废全部 ASR 进度。
/// 注意：bump 会让所有旧 checkpoint 失效一次（身份不匹配 → 整体作废重算）。
const CHECKPOINT_SCHEMA_VERSION: u32 = 2;

/// 一条 ASR 记录的身份：决定 checkpoint 能否被复用。
/// 任何影响转写内容语义的字段变化都必须使旧进度作废。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsrIdentity {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    /// 语音分段上限（秒）：决定 chunk 边界，即 checkpoint 的 key
    pub max_speech: f32,
}

impl AsrIdentity {
    pub fn new(provider: &str, model: &str, max_speech: f32) -> Self {
        Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            provider: provider.to_string(),
            model: model.to_string(),
            max_speech,
        }
    }
}

pub struct Checkpoint {
    path: PathBuf,
    done_path: PathBuf,
    file: Option<std::fs::File>,
    done: HashSet<(u64, u64)>,
    events: Vec<TranscriptEvent>,
}

/// f64 → u64 bit 模式（可哈希；时间来自同一 JSON round-trip，位级一致）
fn key(start: f64, end: f64) -> (u64, u64) {
    (start.to_bits(), end.to_bits())
}

/// Every recorded chunk counts as done (an empty chunk is a completed
/// silent span), while only chunks with text are kept as events.
fn split_done(events: Vec<TranscriptEvent>) -> (HashSet<(u64, u64)>, Vec<TranscriptEvent>) {
    let done = events.iter().map(|e| key(e.start, e.end)).collect();
    let readable = events
        .into_iter()
        .filter(|e| !e.text.trim().is_empty())
        .collect();
    (done, readable)
}

impl Checkpoint {
    /// Read completed chunks for a failed task's material preview without opening a
    /// writer, repairing a truncated tail, or marking the transcription complete.
    pub fn saved_events(out_dir: &Path) -> Result<Vec<TranscriptEvent>> {
        let (_, mut events) = split_done(Self::load_events(&out_dir.join("asr.jsonl"))?.0);
        events.sort_by(|a, b| a.start.total_cmp(&b.start));
        Ok(events)
    }

    /// 在 out_dir 下打开（或按 resume/身份决定作废重开）checkpoint。
    pub fn open(out_dir: &Path, resume: bool, identity: &AsrIdentity) -> Result<Self> {
        let path = out_dir.join("asr.jsonl");
        let done_path = out_dir.join(".asr_done");
        let identity_path = out_dir.join(".asr_identity");
        let mut cp = Checkpoint {
            path: path.clone(),
            done_path: done_path.clone(),
            file: None,
            done: HashSet::new(),
            events: vec![],
        };

        let identity_matches = Self::stored_identity(&identity_path).map(|stored| {
            let ok = stored.as_ref() == Some(identity);
            if !ok {
                match stored {
                    Some(old) => tracing::info!(
                        old = ?old, new = ?identity,
                        "asr checkpoint 身份不匹配（模型/后端/版本已变化），旧进度作废重算 / asr checkpoint identity mismatch (model/backend/version changed); old progress discarded and recomputed"
                    ),
                    None => tracing::info!(
                        "asr checkpoint 无身份标记，旧进度作废重算 / asr checkpoint has no identity marker; old progress discarded and recomputed"
                    ),
                }
            }
            ok
        });

        let usable = resume && identity_matches?;
        if !usable {
            // 不复用：清掉全部旧进度（否则旧记录会叠加进本次结果）
            Self::clear(&path, &done_path, &identity_path)?;
        } else {
            let (events, clean) = Self::load_events(&path)?;
            tracing::info!(n = events.len(), "asr checkpoint resumed");
            // 崩溃残留的半截行会和新记录拼成一行中间损坏 JSON，下次 resume 直接硬错误
            // （自我污染）：追加前把文件换成完整的行。
            if !clean {
                let mut lines = String::new();
                for event in &events {
                    lines.push_str(&serde_json::to_string(event)?);
                    lines.push('\n');
                }
                atomic_write(&path, lines.as_bytes())?;
            }
            (cp.done, cp.events) = split_done(events);
        }
        // 记录本次身份（原子写；同时也作为「清档后」的新标记）
        let id = serde_json::to_string(identity)?;
        atomic_write(&identity_path, id.as_bytes())?;
        Ok(cp)
    }

    /// 磁盘上存的身份；文件不存在时返回 None。
    fn stored_identity(path: &Path) -> Result<Option<AsrIdentity>> {
        if !path.is_file() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(path).with_context(|| {
            tr!(
                "读取 {} / Failed to read {}",
                path.display(),
                path.display()
            )
        })?;
        Ok(Some(serde_json::from_str(&s).context(tr!(
            "识别进度记录已损坏 / The transcription progress record is damaged"
        ))?))
    }

    fn clear(path: &Path, done_path: &Path, identity_path: &Path) -> Result<()> {
        let old = [path, done_path, identity_path]
            .into_iter()
            .filter(|p| p.exists())
            .collect::<Vec<_>>();
        if old.is_empty() {
            return Ok(());
        }
        let history = path
            .parent()
            .unwrap_or(Path::new("."))
            .join(".previous-asr");
        std::fs::create_dir_all(&history)?;
        let archive = tempfile::Builder::new()
            .prefix("checkpoint-")
            .tempdir_in(&history)?
            .keep();
        for file in old {
            std::fs::rename(file, archive.join(file.file_name().unwrap())).with_context(|| {
                tr!(
                    "无法保留旧识别进度 {} / Cannot preserve previous transcription progress {}",
                    file.display(),
                    file.display()
                )
            })?;
        }
        Ok(())
    }

    /// 读取事件行，并告知文件是否可直接追加。最后一行允许半截（崩溃残留）；
    /// 中间损坏 → 硬错误。同一 (start,end) 只保留首条（防御历史文件中的重复行）。
    fn load_events(path: &Path) -> Result<(Vec<TranscriptEvent>, bool)> {
        if !path.is_file() {
            return Ok((vec![], true));
        }
        let read_failed = || {
            tr!(
                "读取 {} / Failed to read {}",
                path.display(),
                path.display()
            )
        };
        let bytes = std::fs::read(path).with_context(read_failed)?;
        let size = bytes.len();
        let content = complete_text(bytes).with_context(read_failed)?;
        let clean = content.len() == size && (content.is_empty() || content.ends_with('\n'));
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let last_idx = lines.len().saturating_sub(1);
        let mut out: Vec<TranscriptEvent> = vec![];
        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        for (i, line) in lines.iter().enumerate() {
            match serde_json::from_str::<TranscriptEvent>(line) {
                Ok(ev) => {
                    if seen.insert(key(ev.start, ev.end)) {
                        out.push(ev);
                    } else if i != last_idx {
                        tracing::warn!(
                            line = i + 1,
                            "重复的识别检查点已忽略 / Ignored duplicate transcription checkpoint"
                        );
                    }
                }
                // 崩溃时最后一行可能只写了一半：容忍
                Err(e) if i == last_idx && !content.ends_with('\n') => {
                    tracing::debug!("checkpoint 末行解析失败（按崩溃残留忽略）：{e}");
                }
                Err(e) => anyhow::bail!(
                    "识别进度文件第 {1} 行已损坏，请重新识别 / Line {1} of the transcription progress file is damaged; transcribe again ({0}): {e}",
                    path.display(),
                    i + 1
                ),
            }
        }
        Ok((out, clean))
    }

    /// 该 chunk 是否已完成（可跳过）。空文本（静音）chunk 同样算完成。
    pub fn is_done(&self, start: f64, end: f64) -> bool {
        self.done.contains(&key(start, end))
    }

    /// 已完成且有文本的事件（含 resume 加载的历史 + 本次新增）。
    /// 空文本 chunk 是静音标记，不进时间线。
    #[cfg(test)]
    pub fn events(&self) -> &[TranscriptEvent] {
        &self.events
    }

    /// events() 的按 start 升序排序副本（历史 + 本次混合时顺序不保证）。
    pub fn sorted_events(&self) -> Vec<TranscriptEvent> {
        let mut all = self.events.clone();
        all.sort_by(|a, b| a.start.total_cmp(&b.start));
        all
    }

    /// 记录一个完成的 chunk（append + flush）。写盘失败返回 Err 且不标记完成。
    /// `text` 允许为空串：表示「成功识别且确认无语音」。
    pub fn record(&mut self, start: f64, end: f64, text: &str) -> Result<()> {
        let ev = TranscriptEvent {
            start,
            end,
            text: text.to_string(),
            raw: None,
            translation: None,
        };
        let failed = || {
            tr!(
                "写 checkpoint {} / Failed to write checkpoint {}",
                self.path.display(),
                self.path.display()
            )
        };
        if self.file.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir).with_context(failed)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .with_context(failed)?;
            self.file = Some(file);
        }
        if let Some(mut file) = self.file.as_ref() {
            writeln!(file, "{}", serde_json::to_string(&ev)?).with_context(failed)?;
            file.flush().with_context(failed)?;
        }
        // 只有落盘成功才更新内存状态
        self.done.insert(key(start, end));
        if !text.trim().is_empty() {
            self.events.push(ev);
        }
        Ok(())
    }

    /// 全部完成：原子写标记（后续重跑直接跳过 ASR）。
    pub fn finish(&mut self) -> Result<()> {
        atomic_write(&self.done_path, b"done\n")?;
        self.file = None;
        Ok(())
    }
}

/// The file's text. A crash can stop a write inside a multi-byte character, so
/// bytes that are not UTF-8 are a torn tail when they lie after the last newline,
/// and that line is dropped here; anywhere earlier they are damage.
fn complete_text(bytes: Vec<u8>) -> Result<String> {
    let error = match String::from_utf8(bytes) {
        Ok(text) => return Ok(text),
        Err(error) => error,
    };
    let bytes = error.as_bytes();
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    Ok(std::str::from_utf8(&bytes[..complete])?.to_owned())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(model: &str) -> AsrIdentity {
        AsrIdentity::new("gpu", model, 20.0)
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("c2m-cp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn failed_task_preview_reads_completed_chunks_without_mutating_a_truncated_tail() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"{\"start\":2.0,\"end\":3.0,\"text\":\"second\"}\n{\"start\":0.0,\"end\":1.0,\"text\":\"first\"}\n{\"start\":";
        let path = dir.path().join("asr.jsonl");
        std::fs::write(&path, bytes).unwrap();
        let events = Checkpoint::saved_events(dir.path()).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text, "first");
        assert_eq!(events[1].text, "second");
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert!(!dir.path().join(".asr_done").exists());
    }

    #[test]
    fn a_tail_torn_inside_a_character_is_only_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        Checkpoint::open(dir.path(), true, &identity("qwen3")).unwrap();
        let path = dir.path().join("asr.jsonl");
        let whole = "{\"start\":0.0,\"end\":1.0,\"text\":\"第一句\"}\n";
        // The crash stopped two bytes into the three bytes of `二`.
        let mut torn = whole.as_bytes().to_vec();
        let unfinished = "{\"start\":1.0,\"end\":2.0,\"text\":\"第二句";
        torn.extend_from_slice(&unfinished.as_bytes()[..unfinished.len() - 4]);
        assert!(std::str::from_utf8(&torn).is_err());
        std::fs::write(&path, &torn).unwrap();

        // Reading a failed task's progress leaves the file alone.
        assert_eq!(Checkpoint::saved_events(dir.path()).unwrap().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), torn);

        let mut checkpoint = Checkpoint::open(dir.path(), true, &identity("qwen3")).unwrap();
        assert!(checkpoint.is_done(0.0, 1.0));
        assert!(!checkpoint.is_done(1.0, 2.0));
        checkpoint.record(1.0, 2.0, "第二句").unwrap();
        drop(checkpoint);
        assert_eq!(
            Checkpoint::saved_events(dir.path()).unwrap()[1].text,
            "第二句"
        );

        // Invalid bytes before the last line are damage, not a crash at the end.
        let mut damaged = b"{\"start\":0.0,\"text\":\"\xff\"}\n".to_vec();
        damaged.extend_from_slice(whole.as_bytes());
        std::fs::write(&path, damaged).unwrap();
        assert!(Checkpoint::open(dir.path(), true, &identity("qwen3")).is_err());
    }

    #[test]
    fn complete_corrupt_line_is_not_treated_as_truncation() {
        let dir = tempfile::tempdir().unwrap();
        Checkpoint::open(dir.path(), true, &identity("qwen3")).unwrap();
        std::fs::write(dir.path().join("asr.jsonl"), b"garbage\n").unwrap();
        assert!(Checkpoint::open(dir.path(), true, &identity("qwen3")).is_err());
    }

    #[test]
    fn atomic_write_replaces_existing_file_without_touching_sibling_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(path.with_extension("tmp"), b"unrelated").unwrap();
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            std::fs::read(path.with_extension("tmp")).unwrap(),
            b"unrelated"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600, "配置与凭据仅本人可读");
            let note = dir.path().join("course.md");
            atomic_write_shared(&note, b"note").unwrap();
            assert_eq!(mode(&note), 0o644, "笔记文件可被其他程序读取");
        }
    }

    #[test]
    fn empty_chunk_is_recorded_and_skipped_on_resume() {
        let d = tmpdir("empty");
        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        cp.record(0.0, 2.0, "hello").unwrap();
        cp.record(2.0, 4.0, "").unwrap(); // 静音 chunk
        cp.finish().unwrap();
        drop(cp);

        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert!(cp.is_done(0.0, 2.0));
        assert!(cp.is_done(2.0, 4.0), "空文本 chunk 也必须视为已完成");
        assert_eq!(cp.events().len(), 1, "空文本不进时间线");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn identity_mismatch_discards_old_progress() {
        let d = tmpdir("ident");
        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        cp.record(0.0, 2.0, "旧模型的结果").unwrap();
        drop(cp); // 未 finish：模拟中断

        let cp = Checkpoint::open(&d, true, &identity("whisper")).unwrap();
        assert!(!cp.is_done(0.0, 2.0), "换模型后旧 chunk 必须作废");
        assert!(cp.events().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_resume_clears_old_files() {
        let d = tmpdir("clear");
        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        cp.record(0.0, 2.0, "x").unwrap();
        cp.finish().unwrap();
        assert!(d.join(".asr_done").is_file());

        // --no-resume：旧进度必须被清掉，而不是叠加
        let mut cp = Checkpoint::open(&d, false, &identity("qwen3")).unwrap();
        assert!(cp.events().is_empty() && cp.done.is_empty());
        cp.record(0.0, 2.0, "y").unwrap();
        drop(cp);

        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1, "重复运行不得产生双份记录");
        assert_eq!(cp.events()[0].text, "y");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn checkpoint_without_identity_is_discarded() {
        let d = tmpdir("legacy");
        std::fs::write(
            d.join("asr.jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&TranscriptEvent {
                    start: 0.0,
                    end: 1.0,
                    text: "无身份标记".into(),
                    raw: None,
                    translation: None,
                })
                .unwrap()
            ),
        )
        .unwrap();
        std::fs::write(d.join(".asr_done"), b"done\n").unwrap();

        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert!(cp.events().is_empty(), "无身份标记的进度必须作废");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn middle_corruption_is_hard_error() {
        let ev = |s: f64, t: &str| {
            serde_json::to_string(&TranscriptEvent {
                start: s,
                end: s + 1.0,
                text: t.into(),
                raw: None,
                translation: None,
            })
            .unwrap()
        };
        // 先建立身份（模拟一次正常 open），再植入损坏内容
        let d = tmpdir("mid");
        Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        std::fs::write(
            d.join("asr.jsonl"),
            format!("{}\n garbage \n{}\n", ev(0.0, "a"), ev(1.0, "b")),
        )
        .unwrap();
        // 中间损坏 → 硬错误（与之相对：末行半截可恢复，见下方截断测试）
        assert!(Checkpoint::open(&d, true, &identity("qwen3")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn truncated_tail_is_cut_before_resume_appends() {
        // 末行半截 → resume 后继续 record，不得把新行拼在残行后
        // （否则形成中间损坏行，下次 resume 硬错误，进度自我污染）。
        let d = tmpdir("tailwrite");
        let ev = |s: f64, t: &str| {
            serde_json::to_string(&TranscriptEvent {
                start: s,
                end: s + 1.0,
                text: t.into(),
                raw: None,
                translation: None,
            })
            .unwrap()
        };
        Checkpoint::open(&d, true, &identity("qwen3")).unwrap(); // 建立身份
        std::fs::write(
            d.join("asr.jsonl"),
            format!("{}\n{{\"start\":1.", ev(0.0, "a")),
        )
        .unwrap();

        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1);
        assert!(!cp.is_done(1.0, 2.0), "末行半截不应被计入完成");
        cp.record(2.0, 3.0, "新记录").unwrap();
        drop(cp);

        // 重新打开：完整读回两条记录，且没有中间损坏行硬错误
        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 2, "残行截断后新旧记录都应完整读回");
        assert_eq!(cp.events()[0].text, "a");
        assert_eq!(cp.events()[1].text, "新记录");
        assert!(cp.is_done(2.0, 3.0));
        let _ = std::fs::remove_dir_all(&d);

        // 变体：残行就是第一条记录（文件内没有任何完整行），同样先截断再追加
        let d = tmpdir("tailfirst");
        Checkpoint::open(&d, true, &identity("qwen3")).unwrap(); // 建立身份
        std::fs::write(d.join("asr.jsonl"), b"{\"start\":").unwrap();
        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert!(cp.events().is_empty());
        cp.record(1.0, 2.0, "recovered").unwrap();
        drop(cp);
        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1);
        assert_eq!(cp.events()[0].text, "recovered");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn valid_tail_without_newline_gets_separator_before_append() {
        // 手改文件：末行是合法 JSON 但缺尾换行 → 追加前补换行，不得拼行
        let d = tmpdir("nonl");
        let l = serde_json::to_string(&TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "手改".into(),
            raw: None,
            translation: None,
        })
        .unwrap();
        Checkpoint::open(&d, true, &identity("qwen3")).unwrap(); // 建立身份
        std::fs::write(d.join("asr.jsonl"), &l).unwrap(); // 无尾换行

        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1, "合法末行不应被截掉");
        cp.record(2.0, 3.0, "追加").unwrap();
        drop(cp);

        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 2);
        assert_eq!(cp.events()[1].text, "追加");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unwritable_out_dir_fails_loudly() {
        // out_dir 本身是个普通文件 → checkpoint 完全不可用，open 必须报错
        let f = std::env::temp_dir().join(format!("c2m-cp-file-{}", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        assert!(Checkpoint::open(&f, false, &identity("qwen3")).is_err());
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn record_write_failure_does_not_mark_done() {
        // 先正常打开，再破坏写入路径：把 asr.jsonl 所在目录换成只读场景较繁琐，
        // 这里直接验证语义：record 失败 → done 不含该 chunk。
        // 构造：打开后删除目录，record 无法建文件 → Err 且不标记。
        let d = tmpdir("wfail");
        let mut cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
        std::fs::write(&d, b"x").unwrap(); // 目录换成普通文件 → 无法重建写入路径
        assert!(cp.record(0.0, 1.0, "t").is_err(), "写盘失败必须报错");
        assert!(!cp.is_done(0.0, 1.0), "写盘失败不得标记完成");
        let _ = std::fs::remove_file(&d);
    }

    #[test]
    fn duplicate_lines_keep_first() {
        let d = tmpdir("dup");
        let _ = Checkpoint::open(&d, true, &identity("qwen3")).unwrap(); // 建立身份
        let l = serde_json::to_string(&TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "first".into(),
            raw: None,
            translation: None,
        })
        .unwrap();
        let l2 = serde_json::to_string(&TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "second".into(),
            raw: None,
            translation: None,
        })
        .unwrap();
        std::fs::write(d.join("asr.jsonl"), format!("{l}\n{l2}\n")).unwrap();
        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1);
        assert_eq!(cp.events()[0].text, "first");
        let _ = std::fs::remove_dir_all(&d);
    }
}
