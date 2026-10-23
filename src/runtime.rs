//! 子进程与本地服务的共用原语（llama-server / NPU worker / ffmpeg）：
//! - `ManagedChild`：Drop 保证 kill+wait，任何错误路径（含 `?` 早退）不泄漏进程
//! - `run_bounded`：短命令的超时与输出捕获
//! - `wait_ready`：健康轮询期间监视子进程，秒退立即报错而不是等满超时
//! - `retry_backoff` / `interruptible_sleep`：可取消的重试退避

use anyhow::{Context, Result};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 获取互斥锁；若锁已中毒（某线程在持锁期间 panic），直接取走数据继续用。
/// 语义：本进程自愿接受中毒锁——所有持锁临界区只做内存内状态更新，
/// 数据本身不会被 panic 路径写坏，传播 PoisonError 反而会让无关线程陪葬。
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Runs an async function from the CLI's synchronous entry points (main,
/// wizard) on one process-wide runtime, created on first use, so a command
/// that loops over notes does not start a worker pool per call.
pub fn block_on<F: std::future::Future>(f: F) -> Result<F::Output> {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime.block_on(f));
    }
    let runtime = tokio::runtime::Runtime::new()?;
    Ok(RUNTIME.get_or_init(|| runtime).block_on(f))
}

/// kill-on-drop 的子进程句柄。
pub struct ManagedChild {
    child: Child,
    name: &'static str,
}

impl ManagedChild {
    pub fn spawn(name: &'static str, cmd: &mut Command) -> Result<Self> {
        let child = cmd
            .spawn()
            .with_context(|| crate::tr!("无法启动 {} / Could not start {}", name, name))?;
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

    /// 轮询等待进程退出，最多 `timeout`；Err = 超时（进程仍由 Drop 兜底 kill+wait）。
    pub fn wait_within(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let t0 = Instant::now();
        loop {
            if let Some(status) = self.try_wait() {
                return Ok(status);
            }
            anyhow::ensure!(
                t0.elapsed() < timeout,
                "{} 未在 {:.1}s 内退出 / did not exit within the time limit",
                self.name,
                timeout.as_secs_f64()
            );
            std::thread::sleep(WAIT_POLL_INTERVAL);
        }
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
/// wait_within 的轮询间隔：50ms 足够及时，又不至于空转 CPU
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

impl StderrTail {
    pub fn tail(&self) -> String {
        self.lines.lock().map(|v| v.join("\n")).unwrap_or_default()
    }

    /// 给错误附上 stderr 尾部作为 context（尾部为空则原样返回）。
    /// 收拢 asr.rs / npu.rs 同构的「失败时附加子进程 stderr 尾部」骨架。
    pub fn attach(&self, e: anyhow::Error, label: &str) -> anyhow::Error {
        let tail = self.tail();
        if tail.is_empty() {
            e
        } else {
            e.context(format!("{label}:\n{tail}"))
        }
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

/// 有界命令输出（stdout/stderr 经临时文件捕获，不受 pipe 缓冲与 GUI stdin 影响）。
pub struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// 短命令执行：stdin 关闭（ffmpeg 等工具在 GUI/管道 stdin 上会挂死）、
/// 超时强制 kill（ManagedChild 兜底）、输出经临时文件捕获（防 pipe 写满再超时）。
pub fn run_bounded(
    name: &'static str,
    cmd: &mut Command,
    timeout: Duration,
) -> Result<BoundedOutput> {
    use std::io::{Read, Seek};
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    cmd.stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = ManagedChild::spawn(name, cmd)?;
    let status = child.wait_within(timeout).map_err(|_| {
        anyhow::anyhow!(
            "{name} 超过 {0:.0} 秒未完成，已终止 / {name} did not finish within {0:.0}s and was stopped",
            timeout.as_secs_f64()
        )
    })?;
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = String::new();
    let mut err = String::new();
    stdout.take(64 * 1024 * 1024).read_to_string(&mut out)?;
    stderr.take(64 * 1024 * 1024).read_to_string(&mut err)?;
    Ok(BoundedOutput {
        status,
        stdout: out,
        stderr: err,
    })
}

/// 轮询 `{base}/health` 直到成功；子进程中途退出立即失败（不等满超时）。
/// `expect_body` 非空时还要求响应体包含该子串——防止端口被无关服务
/// 抢占（free_port 存在 TOCTOU 窗口）而误判就绪。
/// `control` 非空时每轮轮询前调用：返回 Err 表示外部要求中止（暂停/取消），
/// 立即报错返回而不等满超时；子进程由 ManagedChild 的 Drop 兜底 kill。
pub fn wait_ready(
    base: &str,
    timeout: Duration,
    child: &mut ManagedChild,
    expect_status: Option<&str>,
    control: Option<&dyn Fn() -> Result<()>>,
) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    loop {
        if let Some(check) = control {
            // 暂停/取消原样上抛：调用方要据此区分「用户中止」与「启动失败」
            check()?;
        }
        if let Some(st) = child.try_wait() {
            anyhow::bail!(
                "{} 启动失败（{st}） / {} exited during startup ({st})",
                child.name(),
                child.name()
            );
        }
        if t0.elapsed() > timeout {
            anyhow::bail!(
                "{} 启动超时（{:.0} 秒） / {} startup timed out after {:.0}s",
                child.name(),
                timeout.as_secs_f64(),
                child.name(),
                timeout.as_secs_f64()
            );
        }
        if let Ok(resp) = ureq::get(&url).timeout(Duration::from_secs(2)).call() {
            match expect_status {
                None => return Ok(()),
                Some(expected) => {
                    if resp
                        .into_json::<serde_json::Value>()
                        .is_ok_and(|body| body["status"] == expected)
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

/// 进程级抖动序列：与纳秒异或打散，避免并发请求同步重试（不引入 rand）。
static JITTER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 第 `attempt` 次失败后的退避时长：1s、2s、4s… 指数增长 + 0~500ms 抖动。
pub fn retry_backoff(attempt: u32) -> Duration {
    let base = 1_u64 << attempt.saturating_sub(1).min(6);
    let seq = JITTER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let jitter_ns = (nanos ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) % 500_000_000;
    Duration::from_nanos(base * 1_000_000_000 + jitter_ns)
}

/// Bounded retry for one service request. Retryable failures (confirmed unsent,
/// rate limited, a free service's lost response) back off between attempts and
/// the wait can be cancelled; every other failure returns at once.
pub fn retry<T>(
    control: &crate::dispatch::TaskControl,
    max_attempts: u32,
    mut attempt_once: impl FnMut() -> std::result::Result<T, crate::dispatch::Failure>,
) -> Result<T> {
    let mut attempt = 1;
    loop {
        control.check()?;
        match attempt_once() {
            Ok(value) => return Ok(value),
            Err(failure) if failure.retryable && attempt < max_attempts => {
                let wait = retry_backoff(attempt);
                tracing::warn!(
                    attempt,
                    of = max_attempts,
                    ?wait,
                    "请求失败，稍后重试 / Request failed; retrying shortly: {}",
                    failure.message
                );
                interruptible_sleep(control, wait)?;
                attempt += 1;
            }
            Err(failure) => return Err(failure.into()),
        }
    }
}

/// 可取消的等待：分段小睡并轮询任务控制文件，暂停/取消立即返回其错误。
pub fn interruptible_sleep(control: &crate::dispatch::TaskControl, wait: Duration) -> Result<()> {
    let deadline = Instant::now() + wait;
    loop {
        control.check()?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(Duration::from_millis(200)));
    }
}

/// 无重定向的 HTTP agent：整体超时参数化，内置 `.redirects(0)`。
///
/// 安全纪律（与 dispatch.rs 请求账本注释一致）：请求经 3xx 重定向后失败，
/// 无法证明服务端「未提交」——可能已产生费用/副作用，因此凡走账本的调用
/// 必须禁止跟随重定向。这里在构造处统一内置，不靠每个调用点自觉。
/// 需要 connect/read 细分超时的场景（fetch.rs 元数据、models.rs 大文件下载）
/// 形态不合，保留各自 AgentBuilder，不套用本 helper。
pub fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .redirects(0)
        .build()
}

/// 在 PATH 上查找可执行文件。
/// 用 `which` crate 而非手写 PATH 遍历：正确处理 Windows PATHEXT
/// （PATHEXT 含 `.bat`/`.cmd` 等）。
pub fn which(cmd: &str) -> Option<PathBuf> {
    which::which(cmd).ok()
}

/// Own an OS lock separately from the lifetime of duplicated file descriptors.
pub struct FileLock {
    file: std::fs::File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Unix flock locks belong to the open file description. A concurrently
        // forked child may still hold that description until exec, so closing
        // our descriptor alone need not release the lock at the end of its scope.
        let _ = self.file.unlock();
    }
}

/// Hold an OS file lock until the returned guard is dropped. The lock file
/// remains in place so another process cannot open a different inode at its path.
pub fn lock_file(path: &Path) -> Result<FileLock> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock().with_context(|| {
        crate::tr!("目录正被另一任务使用：{}。请等待任务结束", path.display()).to_string()
    })?;
    Ok(FileLock { file })
}

/// 让 OS 分配一个空闲端口。
/// 注意 TOCTOU：返回后到真正 bind 之间端口可能被抢占，
/// 调用方需用 `wait_ready` 的 `/health` status 校验兜底。
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// 临时工作目录（chunk / worker 脚本等中间产物）：创建失败立即报错，
/// Drop 时尽力清理整个目录。目录名带 tag 与随机后缀，避免并发实例冲突。
pub struct TempWorkDir {
    dir: tempfile::TempDir,
}

impl TempWorkDir {
    pub fn new(tag: &str) -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix(&format!("course2md-{tag}-"))
            .tempdir()
            .context(crate::tr!(
                "无法创建临时工作目录 / Could not create a temporary working directory"
            ))?;
        Ok(Self { dir })
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Runs `work` for every item on a bounded number of OS threads, preserving
/// input order. Workers stop taking new items after the first error, but every
/// started item is joined so confirmed side effects can still be saved.
pub fn map_pool<T, R>(
    items: Vec<T>,
    workers: usize,
    control: &crate::dispatch::TaskControl,
    work: impl Fn(T) -> Result<R> + Sync,
) -> Result<Vec<R>>
where
    T: Send,
    R: Send,
{
    let total = items.len();
    let queue = std::sync::Mutex::new(items.into_iter().enumerate());
    let results: std::sync::Mutex<Vec<Option<R>>> =
        std::sync::Mutex::new(std::iter::repeat_with(|| None).take(total).collect());
    let first_error = std::sync::Mutex::new(None::<anyhow::Error>);
    std::thread::scope(|scope| {
        for _ in 0..workers.max(1).min(total.max(1)) {
            scope.spawn(|| {
                loop {
                    if let Err(error) = control.check() {
                        let mut guard = lock_recover(&first_error);
                        if guard.is_none() {
                            *guard = Some(error);
                        }
                        break;
                    }
                    let next = lock_recover(&queue).next();
                    let Some((index, item)) = next else { break };
                    if lock_recover(&first_error).is_some() {
                        break;
                    }
                    match work(item) {
                        Ok(value) => lock_recover(&results)[index] = Some(value),
                        Err(error) => {
                            let mut guard = lock_recover(&first_error);
                            if guard.is_none() {
                                *guard = Some(error);
                            }
                            break;
                        }
                    }
                }
            });
        }
    });
    if let Some(error) = lock_recover(&first_error).take() {
        return Err(error);
    }
    Ok(lock_recover(&results)
        .iter_mut()
        .map(|value| value.take().expect("worker filled every index"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_lock_rejects_a_second_writer_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let first = lock_file(&path).unwrap();
        assert!(lock_file(&path).is_err());
        drop(first);
        assert!(lock_file(&path).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn workspace_lock_releases_while_an_inherited_descriptor_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let first = lock_file(&path).unwrap();
        // dup and fork both retain the same open file description. Keeping a
        // duplicate makes the fork-before-exec window deterministic, without
        // forking the multithreaded test runner or depending on scheduler timing.
        let inherited = first.file.try_clone().unwrap();
        assert!(lock_file(&path).is_err());
        drop(first);
        let second = lock_file(&path).expect("scope exit must release the original lock");
        drop(inherited);
        assert!(
            lock_file(&path).is_err(),
            "the new owner must keep its lock"
        );
        drop(second);
        assert!(lock_file(&path).is_ok());
    }

    /// 取消任务后，模型加载（最长 300s）不得继续空等：control 返回 Err 立即中止。
    #[cfg(unix)]
    #[test]
    fn wait_ready_aborts_when_control_hook_fails() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let mut child = ManagedChild::spawn("sleep", &mut cmd).unwrap();
        let t0 = Instant::now();
        // 永远不就绪的端口 + 60s 超时；control 第二轮即报错，应远早于超时返回
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = wait_ready(
            "http://127.0.0.1:9",
            Duration::from_secs(60),
            &mut child,
            None,
            Some(&|| {
                if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1 {
                    anyhow::bail!("测试取消 / test cancellation")
                } else {
                    Ok(())
                }
            }),
        );
        let err = result.expect_err("control 报错必须中止 wait_ready");
        assert_eq!(
            err.to_string(),
            "测试取消 / test cancellation",
            "控制错误须原样返回"
        );
        assert!(t0.elapsed() < Duration::from_secs(10), "取消不应等到超时");
    }

    #[test]
    fn retry_backoff_grows_exponentially_with_bounded_jitter() {
        for (attempt, base) in [(1, 1.0), (2, 2.0), (3, 4.0)] {
            let wait = retry_backoff(attempt).as_secs_f64();
            assert!((base..base + 0.5).contains(&wait), "{attempt}: {wait}");
        }
    }

    /// 错误路径/panic unwind 也必须 kill+wait 子进程（孤儿 llama-server 会占住 GPU 设备）。
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
