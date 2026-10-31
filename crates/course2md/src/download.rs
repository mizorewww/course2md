//! Model downloads for this application: `model-fetch` does the transfer; this finds
//! aria2, drives the progress bar and words what happened.

use anyhow::Result;
use model_fetch::{Cancel, Error, Event, Failure, Host, Phase, Progress};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::dispatch::TaskControl;
use crate::progress::{self, Bar, Stage, Unit};

pub use model_fetch::Remote;

/// Terminal bar for model downloads; Apple model preparation uses it too.
pub(crate) const MODEL_BAR: &str =
    "{spinner:.green} {msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({eta})";

/// Downloads the files of one model preparation. Nothing is resolved until a file
/// is actually missing: choosing hosts may announce itself or wait on the network.
#[derive(Default)]
pub struct Downloader(OnceLock<model_fetch::Downloader>);

impl Downloader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Downloads `remote` to `dest` unless a complete copy is already there.
    /// `.part` keeps the bytes received so far, so a later run resumes.
    pub fn fetch(&self, remote: &Remote, dest: &Path, control: &TaskControl) -> Result<()> {
        let downloader = self.0.get_or_init(|| {
            model_fetch::Downloader::new(crate::hub::endpoints().hosts.clone(), find_aria2())
        });
        let stage = Stage::ModelFile(remote.file.to_owned());
        progress::start(stage.clone());
        let bar = Bar::new(stage.clone(), remote.size)
            .with_unit(Unit::Bytes)
            .with_template(MODEL_BAR);
        let report = Report { remote, bar: &bar };
        match downloader.fetch(remote, dest, control, &report) {
            Ok(()) => {
                bar.finish();
                progress::done(stage);
                Ok(())
            }
            Err(Error::AllHostsFailed(failures)) => {
                bar.finish();
                let failures: Vec<_> = failures
                    .iter()
                    .map(|failed| format!("{}: {}", failed.host, failure_text(&failed.failure)))
                    .collect();
                anyhow::bail!(
                    "{}\n{}",
                    crate::tr!("所有下载地址均失败 / Every download host failed"),
                    failures.join("\n")
                )
            }
            Err(Error::Io(error)) => Err(error.into()),
            Err(Error::Cancelled) => Err(control.check().err().unwrap_or_else(|| {
                anyhow::anyhow!(crate::tr!("模型下载已停止 / Model download stopped"))
            })),
        }
    }
}

impl Cancel for TaskControl {
    fn is_cancelled(&self) -> bool {
        self.check().is_err()
    }
}

/// aria2c beside the executable (desktop packages) or on PATH.
fn find_aria2() -> Option<PathBuf> {
    if std::env::var_os("COURSE2MD_NO_ARIA2").is_some() {
        return None;
    }
    crate::tools::locate("aria2c").map(|tool| tool.path)
}

struct Report<'a> {
    remote: &'a Remote<'a>,
    bar: &'a Bar,
}

impl Progress for Report<'_> {
    fn event(&self, event: Event<'_>) {
        let file = self.remote.file;
        match event {
            Event::Phase { phase, host } => {
                let name = host.map(Host::name).unwrap_or_default();
                // "下载 Qwen3-ASR-1.7B-Q8_0.gguf · modelscope.cn" for the terminal bar.
                let (phase, human) = match phase {
                    Phase::Connect => (
                        progress::Phase::Connect,
                        crate::tr!("连接 {} · {}", name, file),
                    ),
                    Phase::Retry => (
                        progress::Phase::Retry,
                        crate::tr!("重新连接 {} · {}", name, file),
                    ),
                    Phase::Verify => (progress::Phase::Verify, crate::tr!("校验 {}", file)),
                    Phase::Download => (progress::Phase::Download, format!("{file} · {name}")),
                };
                self.bar.set_phase(phase, &name, human);
            }
            Event::Resumed { bytes } => self.bar.resume_at(bytes),
            Event::Position { bytes } => self.bar.set_position(bytes),
            Event::Request { url, from } => tracing::info!(url, from, "download"),
            Event::Interrupted {
                host,
                received,
                failure,
            } => tracing::warn!(
                file,
                host = %host.name(),
                received,
                "下载中断，正在续传 / Download interrupted; resuming: {}",
                failure_text(failure)
            ),
            Event::HostFailed { host, failure } => tracing::warn!(
                file,
                host = %host.name(),
                "模型下载失败，换下一个地址 / Model download failed; trying the next host: {}",
                failure_text(failure)
            ),
            Event::ChecksumMismatch { expected, actual } => {
                tracing::warn!(file, expected, actual, "checksum mismatch")
            }
            Event::Aria2Unavailable { reason } => tracing::warn!(
                "aria2 无法使用，改用内置下载 / aria2 unavailable; using the built-in downloader: {reason}"
            ),
            Event::ManifestFailed { error } => tracing::warn!(
                file,
                "模型清单未能写入 / Could not write the model manifest: {error}"
            ),
            Event::Completed { host } => tracing::info!(file, host = %host.name(), "downloaded"),
        }
    }
}

fn failure_text(failure: &Failure) -> String {
    match failure {
        Failure::Request(message) => {
            format!(
                "{}: {message}",
                crate::tr!("模型请求失败 / Model request failed")
            )
        }
        Failure::Status { code, body } => format!("HTTP {code}: {body}"),
        Failure::Incomplete { expected, received } => format!(
            "下载不完整 / Incomplete download (expected {expected} bytes, received {received})"
        ),
        Failure::Stalled => crate::tr!("一分钟没有收到数据 / No data for a minute").into(),
        Failure::ChecksumMismatch => crate::tr!("文件校验不符 / Checksum mismatch").into(),
        Failure::Aria2 { code, message } => format!("aria2 {code}: {message}"),
        Failure::Aria2Exited { status, stderr } => format!("aria2c {status}: {stderr}"),
        Failure::Aria2Rpc(message) => format!("aria2 RPC: {message}"),
        Failure::Io(error) => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha256(body: &[u8]) -> String {
        Sha256::digest(body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn failures_use_the_application_wording() {
        assert_eq!(
            failure_text(&Failure::Stalled),
            crate::tr!("一分钟没有收到数据 / No data for a minute")
        );
        assert_eq!(
            failure_text(&Failure::ChecksumMismatch),
            crate::tr!("文件校验不符 / Checksum mismatch")
        );
        assert_eq!(
            failure_text(&Failure::Status {
                code: 503,
                body: "busy".into()
            }),
            "HTTP 503: busy"
        );
        assert!(
            failure_text(&Failure::Incomplete {
                expected: 5,
                received: 3
            })
            .contains("expected 5 bytes, received 3")
        );
    }

    #[test]
    fn a_transfer_through_the_application_reports_and_verifies() {
        let hash = sha256(b"12345");
        let remote = Remote {
            repo: "test/repo",
            file: "synthetic.bin",
            size: 5,
            sha256: &hash,
        };
        let (base, server) = crate::test_http::serve(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n12345".into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("synthetic.bin");
        let downloader = Downloader::new();
        downloader
            .0
            .set(model_fetch::Downloader::new(vec![Host::Hub(base)], None))
            .unwrap();
        downloader
            .fetch(&remote, &dest, &TaskControl::default())
            .unwrap();
        let requests = server.join().unwrap();
        assert!(
            requests[0]
                .head
                .contains("/test/repo/resolve/main/synthetic.bin")
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"12345");
    }

    #[test]
    fn every_failed_host_is_listed_under_one_message() {
        let remote = Remote {
            repo: "test/repo",
            file: "synthetic.bin",
            size: 5,
            sha256: "00",
        };
        let (base, server) = crate::test_http::serve(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\ngone".into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let downloader = Downloader::new();
        downloader
            .0
            .set(model_fetch::Downloader::new(vec![Host::Hub(base)], None))
            .unwrap();
        let message = downloader
            .fetch(&remote, &root.path().join("f"), &TaskControl::default())
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(message.starts_with(crate::tr!(
            "所有下载地址均失败 / Every download host failed"
        )));
        assert!(message.ends_with("127.0.0.1: HTTP 404: gone"), "{message}");
    }
}
