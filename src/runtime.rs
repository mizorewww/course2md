//! 长驻子进程生命周期管理（llama-server / NPU worker）。
//!
//! 统一三件此前散落在 asr.rs / npu.rs 的重复实现：
//! - `ManagedChild`：Drop 保证 kill+wait，任何错误路径（含 `?` 早退）不泄漏进程
//! - `wait_ready`：健康轮询期间监视子进程，秒退立即报错而不是傻等 300s 超时
//! - `which` / `free_port`：单一实现

use anyhow::{Context, Result};
use std::net::TcpListener;
use std::path::PathBuf;
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
    fn try_status(&mut self) -> Option<ExitStatus> {
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
pub struct StderrTail {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

const STDERR_TAIL_MAX: usize = 100;

impl StderrTail {
    pub fn tail(&self) -> String {
        self.lines.lock().map(|v| v.join("\n")).unwrap_or_default()
    }
}

impl Clone for StderrTail {
    fn clone(&self) -> Self {
        Self {
            lines: self.lines.clone(),
        }
    }
}

impl Default for StderrTail {
    fn default() -> Self {
        Self {
            lines: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

/// 为一个已 spawn 的 stderr pipe 启动 drain 线程。
pub fn drain_stderr(stderr: std::process::ChildStderr) -> StderrTail {
    use std::io::BufRead;
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let shared = lines.clone();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            tracing::debug!(target: "llama_server", "{line}");
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
pub fn wait_ready(base: &str, timeout: Duration, child: &mut ManagedChild) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    loop {
        if let Some(st) = child.try_status() {
            anyhow::bail!(
                "{} 启动过程中已退出（{st}），详见其 stderr 输出",
                child.name()
            );
        }
        if t0.elapsed() > timeout {
            anyhow::bail!("{} 启动超时（{:.0}s）", child.name(), timeout.as_secs_f64());
        }
        if ureq::get(&url)
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 私有工具目录：自动安装的外部工具落在这里（免 root、免 PATH 配置、可整目录删除）。
/// `COURSE2MD_TOOLS_DIR` 可整体重定向（离线/企业管控/Nix 用户自供依赖）。
pub fn tools_dir() -> PathBuf {
    if let Ok(d) = std::env::var("COURSE2MD_TOOLS_DIR")
        && !d.trim().is_empty()
    {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("course2md").join("bin");
    }
    #[cfg(windows)]
    {
        if let Some(d) = std::env::var_os("APPDATA") {
            return PathBuf::from(d).join("course2md").join("bin");
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("share").join("course2md").join("bin")
}

/// 在 PATH 上查找可执行文件；私有工具目录优先于系统 PATH。
/// 除平铺布局外，还会检查 `tools_dir/<cmd>/<cmd>`（带自带动态库的子目录布局，
/// 如 llama-server 需要 `$ORIGIN` 下的 .so/.dylib/.dll）。
pub fn which(cmd: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![cmd.to_string(), format!("{cmd}.exe")]
    } else {
        vec![cmd.to_string()]
    };
    // 1) 私有工具目录（自动安装的优先，先于系统同名物）
    let td = tools_dir();
    for dir in [td.clone(), td.join(cmd)] {
        if dir.is_dir()
            && let Some(hit) = names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
        {
            return Some(hit);
        }
    }
    // 2) 系统 PATH
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

/// 进程启动时把私有工具目录前置到 PATH。
/// 两个用途：兜底任何未经 [`which`] 的裸 `Command::new`；让子进程链
/// （yt-dlp 内部调用 ffmpeg/ffprobe 等）也能看到自动安装的工具。
///
/// # Safety
/// 只能在 main 启动早期、任何线程创建之前调用一次（edition 2024 下
/// `set_var` 是 unsafe：并发读 PATH 的线程存在时修改是 UB）。
pub unsafe fn prepend_tools_to_path() {
    let td = tools_dir();
    let cur = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<_> = std::env::split_paths(&cur).collect();
    if paths.first() == Some(&td) {
        return;
    }
    paths.insert(0, td);
    if let Ok(joined) = std::env::join_paths(&paths) {
        // SAFETY: 见函数文档——启动早期单线程调用一次。
        unsafe { std::env::set_var("PATH", joined) };
    }
}

/// 让 OS 分配一个空闲端口。
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
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
}
