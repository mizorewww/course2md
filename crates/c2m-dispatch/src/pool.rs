//! Work that stops at the task's control: bounded retries, cancellable waits, a worker pool.

use crate::dispatch::{Failure, TaskControl};
use crate::runtime::lock_recover;
use anyhow::Result;
use std::time::{Duration, Instant};

/// 第 `attempt` 次失败后的退避时长：1s、2s、4s… 指数增长 + 0~500ms 抖动，
/// 避免并发请求同步重试。
pub fn retry_backoff(attempt: u32) -> Duration {
    let base = 1_u64 << attempt.saturating_sub(1).min(6);
    let mut random = [0; 4];
    let _ = getrandom::fill(&mut random);
    let jitter_ns = u64::from(u32::from_le_bytes(random)) % 500_000_000;
    Duration::from_nanos(base * 1_000_000_000 + jitter_ns)
}

/// Bounded retry for one service request. Retryable failures (confirmed unsent,
/// rate limited, a free service's lost response) back off between attempts and
/// the wait can be cancelled; every other failure returns at once.
pub fn retry<T>(
    control: &TaskControl,
    max_attempts: u32,
    mut attempt_once: impl FnMut() -> std::result::Result<T, Failure>,
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
pub fn interruptible_sleep(control: &TaskControl, wait: Duration) -> Result<()> {
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

/// Runs `work` for every item on a bounded number of OS threads, preserving
/// input order. Workers stop taking new items after the first error, but every
/// started item is joined so confirmed side effects can still be saved. A single
/// worker runs on the calling thread.
pub fn map_pool<T, R>(
    items: Vec<T>,
    workers: usize,
    control: &TaskControl,
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
    let keep_first = |error| {
        lock_recover(&first_error).get_or_insert(error);
    };
    let worker = || {
        loop {
            if let Err(error) = control.check() {
                keep_first(error);
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
                    keep_first(error);
                    break;
                }
            }
        }
    };
    match workers.max(1).min(total.max(1)) {
        1 => worker(),
        count => std::thread::scope(|scope| {
            for _ in 0..count {
                scope.spawn(worker);
            }
        }),
    }
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
    fn retry_backoff_grows_exponentially_with_bounded_jitter() {
        for (attempt, base) in [(1, 1.0), (2, 2.0), (3, 4.0)] {
            let wait = retry_backoff(attempt).as_secs_f64();
            assert!((base..base + 0.5).contains(&wait), "{attempt}: {wait}");
        }
    }
}
