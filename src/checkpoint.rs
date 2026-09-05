//! ASR checkpoint：逐 chunk 追加写 asr.jsonl，重跑时按精确时间边界跳过已完成段。
//!
//! v2 语义（1.0 起）：
//! - **运行身份**：`.asr_identity` 记录 (schema 版本, provider, 模型, max_speech)。
//!   身份不一致的旧进度（换模型/换后端/换切分长度）整体作废并重算，
//!   杜绝「换模型后 resume 混用两个模型的转写」。
//! - **空结果也算完成**：成功但无语音的 chunk 记录为 `text: ""`，
//!   否则静音段每次断点续跑都会重复 ASR。
//! - **写盘失败不标记完成**：record() 返回 Result，只有落盘成功才计入 done。
//! - **损坏策略**：最后一行允许是崩溃残留的半截 JSON（容忍恢复）；
//!   中间任何损坏行都是硬错误（静默跳过会导致转写内容悄悄缺失）。
//! - **`--no-resume` 清档**：不复用进度时删除旧 checkpoint，
//!   否则旧记录会与新记录叠加，之后 resume 会输出双份文本。
//! - 全部完成后原子写 `.asr_done` 标记，重跑完全跳过 ASR。

use crate::timeline::TranscriptEvent;
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

/// load_events 的结果：解析出的事件 + 供 append 前修复文件尾部的信息。
struct LoadedEvents {
    events: Vec<TranscriptEvent>,
    /// 文件的安全前缀长度（最后一个完整 `\n` 之后；合法末行无尾换行时 = 文件全长）。
    /// 残行（崩溃残留的半截 JSON）不计入。
    valid_len: u64,
    /// 末行合法但缺尾换行（手改文件）：追加前必须先补一个 `\n`。
    needs_newline: bool,
}

impl Checkpoint {
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
                        "asr checkpoint 身份不匹配（模型/后端/版本已变化），旧进度作废重算"
                    ),
                    None => tracing::info!(
                        "asr checkpoint 无身份标记（1.0 前的旧格式），旧进度作废重算"
                    ),
                }
            }
            ok
        });

        let usable = resume && identity_matches.unwrap_or(false);
        if !usable {
            // 不复用：清掉全部旧进度（否则旧记录会叠加进本次结果）
            Self::clear(&path, &done_path, &identity_path)?;
        } else if done_path.is_file() {
            let events = Self::load_events(&path)?.events;
            cp.done = events.iter().map(|e| key(e.start, e.end)).collect();
            cp.events = events
                .into_iter()
                .filter(|e| !e.text.trim().is_empty())
                .collect();
            tracing::info!(n = cp.events.len(), "asr checkpoint complete, reusing");
        } else {
            let loaded = Self::load_events(&path)?;
            if path.is_file() {
                tracing::info!(n = loaded.events.len(), "asr checkpoint resumed (partial)");
                cp.done = loaded.events.iter().map(|e| key(e.start, e.end)).collect();
                cp.events = loaded
                    .events
                    .into_iter()
                    .filter(|e| !e.text.trim().is_empty())
                    .collect();
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    // 保留既有内容（partial resume），显式不 truncate
                    .truncate(false)
                    .open(&path)
                    .with_context(|| format!("打开 checkpoint {}", path.display()))?;
                let file_len = f.metadata().map(|m| m.len()).unwrap_or(0);
                if loaded.valid_len < file_len {
                    // 打开写入句柄前把文件截断到最后一个完整行：
                    // 否则崩溃残留的半截行会和新记录拼成一行中间损坏 JSON，
                    // 下次 resume 直接硬错误（自我污染）。
                    // 注意：不能用 append-only 句柄做 set_len——Windows 上
                    // FILE_APPEND_DATA 不含写长度权限，会 Access denied。
                    f.set_len(loaded.valid_len)
                        .with_context(|| format!("截断 checkpoint 残行 {}", path.display()))?;
                } else if loaded.needs_newline {
                    // 末行是合法 JSON 但缺尾换行（手改文件）：先补换行再追加
                    use std::io::Seek as _;
                    f.seek(std::io::SeekFrom::End(0))?;
                    f.write_all(b"\n")
                        .with_context(|| format!("补换行 {}", path.display()))?;
                }
                // 非 append 句柄：后续 record 前定位到文件尾（单写者进程，一次即可）
                use std::io::Seek as _;
                f.seek(std::io::SeekFrom::End(0))
                    .with_context(|| format!("seek checkpoint {}", path.display()))?;
                cp.file = Some(f);
            }
        }
        // 记录本次身份（原子写；同时也作为「清档后」的新标记）
        let id = serde_json::to_string(identity)?;
        atomic_write(&identity_path, id.as_bytes())?;
        Ok(cp)
    }

    /// 磁盘上存的身份；文件不存在（1.0 前旧 checkpoint）返回 None。
    fn stored_identity(path: &Path) -> Result<Option<AsrIdentity>> {
        if !path.is_file() {
            return Ok(None);
        }
        let s =
            std::fs::read_to_string(path).with_context(|| format!("读取 {}", path.display()))?;
        Ok(Some(
            serde_json::from_str(&s).context("checkpoint 身份文件损坏")?,
        ))
    }

    fn clear(path: &Path, done_path: &Path, identity_path: &Path) -> Result<()> {
        for p in [path, done_path] {
            if p.exists() && std::fs::remove_file(p).is_err() {
                anyhow::bail!("无法清除旧 checkpoint {}", p.display());
            }
        }
        let _ = std::fs::remove_file(identity_path);
        Ok(())
    }

    /// 读取事件行。最后一行允许半截（崩溃残留）；中间损坏 → 硬错误。
    /// 同一 (start,end) 只保留首条（防御历史文件中的重复行）。
    fn load_events(path: &Path) -> Result<LoadedEvents> {
        if !path.is_file() {
            return Ok(LoadedEvents {
                events: vec![],
                valid_len: 0,
                needs_newline: false,
            });
        }
        let content =
            std::fs::read_to_string(path).with_context(|| format!("读取 {}", path.display()))?;
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let last_idx = lines.len().saturating_sub(1);
        let mut out: Vec<TranscriptEvent> = vec![];
        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        for (i, line) in lines.iter().enumerate() {
            match serde_json::from_str::<TranscriptEvent>(line) {
                Ok(ev) => {
                    let k = key(ev.start, ev.end);
                    if seen.insert(k) {
                        out.push(ev);
                    } else if i != last_idx {
                        tracing::warn!(line = i + 1, "checkpoint 存在重复 chunk 记录，仅保留首条");
                    }
                }
                Err(e) => {
                    if i == last_idx && !content.ends_with('\n') {
                        // 崩溃时最后一行可能只写了一半：容忍
                        tracing::debug!("checkpoint 末行解析失败（按崩溃残留忽略）：{e}");
                    } else {
                        anyhow::bail!(
                            "checkpoint {} 第 {} 行损坏（非末行，不能静默跳过，否则转写内容缺失）：{e}",
                            path.display(),
                            i + 1
                        );
                    }
                }
            }
        }
        // 尾部状态：决定 resume 后 append 前是否需要截断/补换行
        let last_nl = content.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let tail = &content[last_nl..];
        let (valid_len, needs_newline) = if tail.trim().is_empty() {
            (content.len() as u64, false) // 以换行结尾（或空文件）：直接可追加
        } else if serde_json::from_str::<TranscriptEvent>(tail.trim()).is_ok() {
            (content.len() as u64, true) // 合法末行但无尾换行
        } else {
            (last_nl as u64, false) // 半截残行：截到最后一个完整行
        };
        Ok(LoadedEvents {
            events: out,
            valid_len,
            needs_newline,
        })
    }

    /// 该 chunk 是否已完成（可跳过）。空文本（静音）chunk 同样算完成。
    pub fn is_done(&self, start: f64, end: f64) -> bool {
        self.done.contains(&key(start, end))
    }

    /// 已完成且有文本的事件（含 resume 加载的历史 + 本次新增）。
    /// 空文本 chunk 是静音标记，不进时间线。
    pub fn events(&self) -> &[TranscriptEvent] {
        &self.events
    }

    /// 记录一个完成的 chunk（append + flush）。写盘失败返回 Err 且不标记完成。
    /// `text` 允许为空串：表示「成功识别且确认无语音」。
    pub fn record(&mut self, start: f64, end: f64, text: &str) -> Result<()> {
        let ev = TranscriptEvent {
            start,
            end,
            text: text.to_string(),
            raw: None,
        };
        if self.file.is_none() {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("创建 checkpoint 目录 {}", dir.display()))?;
            }
            self.file = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .with_context(|| format!("打开 checkpoint {}", self.path.display()))?,
            );
        }
        let line = serde_json::to_string(&ev)?;
        if let Some(f) = &mut self.file {
            writeln!(f, "{line}")
                .with_context(|| format!("写 checkpoint {}", self.path.display()))?;
            f.flush()
                .with_context(|| format!("flush checkpoint {}", self.path.display()))?;
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

/// 小文件原子写：tmp → fsync → rename，避免崩溃留下半截文件。
/// 保证级别：崩溃安全（文件本体已 fsync 后才 rename）；
/// 不含掉电场景下父目录的 fsync（极端掉电时目录项本身可能未落盘）。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("创建目录 {}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)
        .with_context(|| format!("写 {}", path.display()))?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .with_context(|| format!("替换 {}", path.display()))?;
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
    fn first_truncated_record_is_repaired_before_append() {
        let dir = tempfile::tempdir().unwrap();
        Checkpoint::open(dir.path(), true, &identity("qwen3")).unwrap();
        std::fs::write(dir.path().join("asr.jsonl"), b"{\"start\":").unwrap();
        let mut cp = Checkpoint::open(dir.path(), true, &identity("qwen3")).unwrap();
        cp.record(1.0, 2.0, "recovered").unwrap();
        drop(cp);
        let cp = Checkpoint::open(dir.path(), true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events()[0].text, "recovered");
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
        assert_eq!(std::fs::read(path.with_extension("tmp")).unwrap(), b"unrelated");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
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
    fn legacy_checkpoint_without_identity_is_discarded() {
        let d = tmpdir("legacy");
        std::fs::write(
            d.join("asr.jsonl"),
            format!(
                "{}\n",
                serde_json::to_string(&TranscriptEvent {
                    start: 0.0,
                    end: 1.0,
                    text: "旧格式".into(),
                    raw: None,
                })
                .unwrap()
            ),
        )
        .unwrap();
        std::fs::write(d.join(".asr_done"), b"done\n").unwrap();

        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert!(cp.events().is_empty(), "无身份标记的 1.0 前进度必须作废");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn middle_corruption_is_hard_error_but_truncated_tail_recovers() {
        let ev = |s: f64, t: &str| {
            serde_json::to_string(&TranscriptEvent {
                start: s,
                end: s + 1.0,
                text: t.into(),
                raw: None,
            })
            .unwrap()
        };
        // 先建立身份（模拟一次正常 open），再植入损坏内容
        let establish = |d: &Path| Checkpoint::open(d, true, &identity("qwen3")).unwrap();

        // 中间损坏 → 硬错误
        let d = tmpdir("mid");
        establish(&d);
        std::fs::write(
            d.join("asr.jsonl"),
            format!("{}\n garbage \n{}\n", ev(0.0, "a"), ev(1.0, "b")),
        )
        .unwrap();
        assert!(Checkpoint::open(&d, true, &identity("qwen3")).is_err());
        let _ = std::fs::remove_dir_all(&d);

        // 末行半截 → 容忍并恢复前面的记录
        let d = tmpdir("tail");
        establish(&d);
        std::fs::write(
            d.join("asr.jsonl"),
            format!("{}\n{{\"start\":1.", ev(0.0, "a")),
        )
        .unwrap();
        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1);
        assert!(!cp.is_done(1.0, 2.0), "末行半截不应被计入完成");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn truncated_tail_is_cut_before_resume_appends() {
        // B2 回归：末行半截 → resume 后继续 record，不得把新行拼在残行后
        // （否则形成中间损坏行，下次 resume 硬错误，进度自我污染）。
        let d = tmpdir("tailwrite");
        let ev = |s: f64, t: &str| {
            serde_json::to_string(&TranscriptEvent {
                start: s,
                end: s + 1.0,
                text: t.into(),
                raw: None,
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
        cp.record(2.0, 3.0, "新记录").unwrap();
        drop(cp);

        // 重新打开：完整读回两条记录，且没有中间损坏行硬错误
        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 2, "残行截断后新旧记录都应完整读回");
        assert_eq!(cp.events()[0].text, "a");
        assert_eq!(cp.events()[1].text, "新记录");
        assert!(cp.is_done(2.0, 3.0));
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
        })
        .unwrap();
        let l2 = serde_json::to_string(&TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "second".into(),
            raw: None,
        })
        .unwrap();
        std::fs::write(d.join("asr.jsonl"), format!("{l}\n{l2}\n")).unwrap();
        let cp = Checkpoint::open(&d, true, &identity("qwen3")).unwrap();
        assert_eq!(cp.events().len(), 1);
        assert_eq!(cp.events()[0].text, "first");
        let _ = std::fs::remove_dir_all(&d);
    }
}
