//! 长驻子进程生命周期管理（llama-server / NPU worker）。
//!
//! 统一三件此前散落在 asr.rs / npu.rs 的重复实现：
//! - `ManagedChild`：Drop 保证 kill+wait，任何错误路径（含 `?` 早退）不泄漏进程
//! - `wait_ready`：健康轮询期间监视子进程，秒退立即报错而不是傻等 300s 超时
//! - `which` / `free_port` / `TempWorkDir`：单一实现

use anyhow::{Context, Result};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

/// kill-on-drop 的子进程句柄。
pub struct ManagedChild {
    child: Child,
    name: &'static str,
}

impl ManagedChild {
    pub fn spawn(name: &'static str, cmd: &mut Command) -> Result<Self> {
        let child = cmd.spawn().with_context(|| format!("启动 {name} 失败"))?;
        Ok(Self { child, name })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// 取出 piped 的 stderr 句柄（配合 [`drain_stderr`] 使用）。
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 非阻塞收割；Some = 已退出。
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait()
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // 双 kill 无害（对已退出进程返回 Err 被忽略）
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 后台读取子进程 stderr：避免 pipe 写满阻塞子进程；保留尾部若干行
/// 供失败诊断；verbose(debug) 时逐行转发到 tracing。
/// 返回的尾部缓存与会话同寿命，随时可读取。
#[derive(Clone, Default)]
pub struct StderrTail {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

const STDERR_TAIL_MAX: usize = 100;

impl StderrTail {
    pub fn tail(&self) -> String {
        self.lines.lock().map(|v| v.join("\n")).unwrap_or_default()
    }
}

/// 为一个已 spawn 的 stderr pipe 启动 drain 线程。
/// `target` 是 tracing target（如 "llama_server" / "npu_worker"），用于区分日志来源。
pub fn drain_stderr(stderr: std::process::ChildStderr, target: &'static str) -> StderrTail {
    use std::io::BufRead;
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let shared = lines.clone();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            // tracing 的 target: 只接受常量，来源名放消息前缀
            tracing::debug!("[{target}] {line}");
            if let Ok(mut v) = shared.lock()
                && !line.trim().is_empty()
            {
                v.push(line);
                let overflow = v.len().saturating_sub(STDERR_TAIL_MAX);
                if overflow > 0 {
                    v.drain(..overflow);
                }
            }
        }
    });
    StderrTail { lines }
}

/// 轮询 `{base}/health` 直到成功；子进程中途退出立即失败（不等满超时）。
/// `expect_body` 非空时还要求响应体包含该子串——防止端口被无关服务
/// 抢占（free_port 存在 TOCTOU 窗口）而误判就绪。
pub fn wait_ready(
    base: &str,
    timeout: Duration,
    child: &mut ManagedChild,
    expect_body: Option<&str>,
) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    loop {
        if let Some(st) = child.try_wait() {
            anyhow::bail!(
                "{} 启动过程中已退出（{st}），详见其 stderr 输出",
                child.name()
            );
        }
        if t0.elapsed() > timeout {
            anyhow::bail!("{} 启动超时（{:.0}s）", child.name(), timeout.as_secs_f64());
        }
        if let Ok(resp) = ureq::get(&url).timeout(Duration::from_secs(2)).call() {
            match expect_body {
                None => return Ok(()),
                Some(needle) => {
                    if let Ok(body) = resp.into_string()
                        && body.contains(needle)
                    {
                        return Ok(());
                    }
                    // 端口上跑着别的服务：继续等（真正要起的服务可能还在绑端口）
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 在 PATH 上查找可执行文件（Windows 自动尝试 .exe 后缀）。
pub fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![cmd.to_string(), format!("{cmd}.exe")]
    } else {
        vec![cmd.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// 让 OS 分配一个空闲端口。
/// 注意 TOCTOU：返回后到真正 bind 之间端口可能被抢占，
/// 调用方需用 `wait_ready` 的响应体校验兜底。
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// 临时工作目录（chunk / worker 脚本等中间产物）：创建失败立即报错，
/// Drop 时尽力清理整个目录。目录名带 tag 与 pid 区分来源与并发实例。
pub struct TempWorkDir {
    path: PathBuf,
}

impl TempWorkDir {
    pub fn new(tag: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("course2md-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("创建临时目录 {}", path.display()))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempWorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_common_tools() {
        // CI 环境保证 cargo 存在；本测试机器必有 shell 基础工具
        #[cfg(unix)]
        let probe = "ls";
        #[cfg(not(unix))]
        let probe = "cmd";
        assert!(which(probe).is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn free_port_returns_bindable() {
        let p = free_port().unwrap();
        // 立即再绑同一端口不一定成功（TOCTOU），但必须是合法端口值
        assert!(p > 0);
    }

    #[test]
    fn temp_workdir_created_and_cleaned() {
        let path;
        {
            let d = TempWorkDir::new("test").unwrap();
            path = d.path().to_path_buf();
            assert!(path.is_dir());
            std::fs::write(path.join("x"), b"y").unwrap();
        }
        assert!(!path.exists(), "Drop 必须清理整个目录");
    }

    /// issue #12：错误路径/panic unwind 也必须 kill+wait llama-server——
    /// 孤儿进程占着 /dev/kfd 会加剧 ROCm 核显问题。这里验证 Drop 的兜底语义。
    #[cfg(unix)]
    #[test]
    fn managed_child_drop_kills_and_reaps() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let child = ManagedChild::spawn("sleep", &mut cmd).unwrap();
        let pid = child.id().to_string();
        drop(child);
        // kill -0 仅探测存在性：Drop 已 kill+wait，进程应不复存在（zombie 也被收割）
        let st = Command::new("kill").args(["-0", &pid]).status().unwrap();
        assert!(!st.success(), "pid {pid} 应已被 Drop 清理");
    }
}
