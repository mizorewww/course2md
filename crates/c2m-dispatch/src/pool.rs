//! Work that stops at the task's control: bounded retries, cancellable waits, a worker pool.

use crate::dispatch::{Failure, TaskControl};
use crate::runtime::lock_recover;
use anyhow::Result;
use std::time::{Duration, Instant};

/// How a request that failed for a passing reason is sent again: at most
/// `max_attempts` sends, waiting `base`, then twice as long each time up to `cap`,
/// plus up to [`JITTER`] so concurrent requests do not retry in step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base: Duration,
    pub cap: Duration,
    /// Follow what the service says: wait for its `Retry-After` (giving up past
    /// [`RETRY_AFTER_LIMIT`]) and send again after a 408. Off, only the backoff
    /// counts and a 408 ends the retries, as before either existed.
    pub follows_service: bool,
    /// The first wait after the service said it is busy (HTTP 429 or a server
    /// error), doubling from there up to `cap`: a rate limit or an overloaded
    /// gateway takes longer to clear than a dropped connection.
    pub busy_base: Duration,
    /// The most sends for a request the service refused only for now (HTTP 429
    /// or 503), when more than `max_attempts`: it was not processed, so another
    /// try costs nothing but time.
    pub busy_attempts: u32,
}

/// The random part of every wait.
pub const JITTER: Duration = Duration::from_millis(500);

/// The longest `Retry-After` a retry waits for. A service that asks for longer is
/// not going to answer this run; the request fails and can be retried later.
pub const RETRY_AFTER_LIMIT: Duration = Duration::from_secs(60);

impl RetryPolicy {
    /// One request and two more after 1 s and 2 s.
    pub const DEFAULT: Self = Self {
        max_attempts: 3,
        base: Duration::from_secs(1),
        cap: Duration::from_secs(64),
        follows_service: false,
        busy_base: Duration::from_secs(1),
        busy_attempts: 0,
    };

    /// The wait after the `attempt`-th failure, with `jitter` in `0.0..1.0` of [`JITTER`].
    pub fn delay(&self, attempt: u32, jitter: f64) -> Duration {
        self.delay_from(self.base, attempt, jitter)
    }

    fn delay_from(&self, base: Duration, attempt: u32, jitter: f64) -> Duration {
        let doubled = base.saturating_mul(1 << attempt.saturating_sub(1).min(16));
        doubled.min(self.cap) + JITTER.mul_f64(jitter.clamp(0.0, 1.0))
    }

    /// How many sends `failure` may have in all.
    pub fn attempts_for(&self, failure: &Failure) -> u32 {
        if matches!(failure.status, Some(429 | 503)) {
            self.max_attempts.max(self.busy_attempts)
        } else {
            self.max_attempts
        }
    }

    /// The wait before sending again after `failure`, or None when the service
    /// asked for longer than [`RETRY_AFTER_LIMIT`].
    pub fn wait_after(&self, attempt: u32, failure: &Failure, jitter: f64) -> Option<Duration> {
        let busy = failure
            .status
            .is_some_and(|status| status == 429 || (500..600).contains(&status));
        let base = if busy { self.busy_base } else { self.base };
        let backoff = self.delay_from(base, attempt, jitter);
        if !self.follows_service {
            return (failure.status != Some(408)).then_some(backoff);
        }
        match failure.retry_after {
            Some(asked) if asked > RETRY_AFTER_LIMIT => None,
            Some(asked) => Some(asked.max(backoff)),
            None => Some(backoff),
        }
    }
}

fn random_fraction() -> f64 {
    let mut random = [0; 4];
    let _ = getrandom::fill(&mut random);
    f64::from(u32::from_le_bytes(random)) / (f64::from(u32::MAX) + 1.0)
}

/// 第 `attempt` 次失败后的退避时长：1s、2s、4s… 指数增长 + 0~500ms 抖动，
/// 避免并发请求同步重试。
pub fn retry_backoff(attempt: u32) -> Duration {
    RetryPolicy::DEFAULT.delay(attempt, random_fraction())
}

/// Bounded retry for one service request. Retryable failures (confirmed unsent,
/// rate limited, a free service's lost response) back off between attempts and
/// the wait can be cancelled; every other failure returns at once.
pub fn retry<T>(
    control: &TaskControl,
    max_attempts: u32,
    attempt_once: impl FnMut() -> std::result::Result<T, Failure>,
) -> Result<T> {
    retry_with(
        control,
        &RetryPolicy {
            max_attempts,
            ..RetryPolicy::DEFAULT
        },
        attempt_once,
    )
}

/// [`retry`] by `policy`. A `Retry-After` the service sent is waited for when it
/// is longer than the backoff, and ends the retries when it is too long to wait.
pub fn retry_with<T>(
    control: &TaskControl,
    policy: &RetryPolicy,
    mut attempt_once: impl FnMut() -> std::result::Result<T, Failure>,
) -> Result<T> {
    let mut attempt = 1;
    loop {
        control.check()?;
        let failure = match attempt_once() {
            Ok(value) => return Ok(value),
            Err(failure) => failure,
        };
        let attempts = policy.attempts_for(&failure);
        let wait = (failure.retryable && attempt < attempts)
            .then(|| policy.wait_after(attempt, &failure, random_fraction()))
            .flatten();
        let Some(wait) = wait else {
            return Err(failure.into());
        };
        tracing::warn!(
            attempt,
            of = attempts,
            ?wait,
            "请求失败，稍后重试 / Request failed; retrying shortly: {}",
            failure.message
        );
        interruptible_sleep(control, wait)?;
        attempt += 1;
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
    fn backoff_doubles_from_the_base_up_to_the_cap() {
        let policy = RetryPolicy {
            max_attempts: 6,
            base: Duration::from_secs(2),
            cap: Duration::from_secs(10),
            follows_service: true,
            ..RetryPolicy::DEFAULT
        };
        let waits: Vec<_> = (1..=5).map(|n| policy.delay(n, 0.0).as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 10, 10]);
        // Jitter adds at most JITTER, also past the cap.
        assert_eq!(policy.delay(5, 1.0), Duration::from_secs(10) + JITTER);
        assert_eq!(policy.delay(1, 0.5), Duration::from_millis(2_250));
    }

    #[test]
    fn retry_after_is_waited_for_unless_it_is_too_long() {
        let policy = RetryPolicy {
            follows_service: true,
            ..RetryPolicy::DEFAULT
        };
        let limited = |after: Option<u64>| Failure {
            retry_after: after.map(Duration::from_secs),
            ..Failure::with_status(Some(429), true, "rate limited")
        };
        assert_eq!(
            policy.wait_after(1, &limited(Some(7)), 0.0),
            Some(Duration::from_secs(7))
        );
        // A shorter ask than the backoff keeps the backoff.
        assert_eq!(
            policy.wait_after(3, &limited(Some(1)), 0.0),
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            policy.wait_after(1, &limited(None), 0.0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(policy.wait_after(1, &limited(Some(3600)), 0.0), None);
        // Speech recognition keeps its old schedule: the backoff alone, no 408.
        let plain = RetryPolicy::DEFAULT;
        assert_eq!(
            plain.wait_after(1, &limited(Some(3600)), 0.0),
            Some(Duration::from_secs(1))
        );
        let timeout = Failure::with_status(Some(408), true, "timeout");
        assert_eq!(plain.wait_after(1, &timeout, 0.0), None);
        assert!(policy.wait_after(1, &timeout, 0.0).is_some());
    }

    #[test]
    fn only_retryable_failures_are_sent_again_and_only_up_to_the_limit() {
        let control = TaskControl::default();
        let policy = RetryPolicy {
            max_attempts: 3,
            base: Duration::ZERO,
            cap: Duration::ZERO,
            follows_service: true,
            ..RetryPolicy::DEFAULT
        };
        let calls = std::cell::Cell::new(0);
        let rejected = retry_with(&control, &policy, || -> std::result::Result<(), _> {
            calls.set(calls.get() + 1);
            Err(Failure::with_status(Some(401), false, "bad key"))
        });
        assert!(rejected.is_err());
        assert_eq!(calls.get(), 1, "an authentication failure is not retried");
        calls.set(0);
        let busy = retry_with(&control, &policy, || -> std::result::Result<(), _> {
            calls.set(calls.get() + 1);
            Err(Failure::with_status(Some(503), true, "busy"))
        });
        assert!(busy.is_err());
        assert_eq!(calls.get(), 3);
        calls.set(0);
        let recovered = retry_with(&control, &policy, || {
            calls.set(calls.get() + 1);
            if calls.get() < 2 {
                Err(Failure::with_status(Some(408), true, "timeout"))
            } else {
                Ok(calls.get())
            }
        });
        assert_eq!(recovered.unwrap(), 2);
    }

    #[test]
    fn a_busy_service_is_given_longer_and_a_rate_limit_more_tries() {
        let policy = RetryPolicy {
            max_attempts: 4,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(60),
            follows_service: true,
            busy_base: Duration::from_secs(5),
            busy_attempts: 6,
        };
        let failure = |status| Failure::with_status(Some(status), true, "x");
        let lost = Failure::with_status(None, true, "lost");
        let waits = |failure: &Failure| -> Vec<u64> {
            (1..=5)
                .map(|n| policy.wait_after(n, failure, 0.0).unwrap().as_secs())
                .collect()
        };
        assert_eq!(waits(&lost), [1, 2, 4, 8, 16]);
        assert_eq!(waits(&failure(429)), [5, 10, 20, 40, 60]);
        assert_eq!(waits(&failure(502)), [5, 10, 20, 40, 60]);
        assert_eq!(policy.attempts_for(&failure(429)), 6);
        assert_eq!(policy.attempts_for(&failure(503)), 6);
        assert_eq!(policy.attempts_for(&failure(500)), 4);
        assert_eq!(policy.attempts_for(&lost), 4);
        // Speech recognition keeps its schedule.
        let plain = RetryPolicy::DEFAULT;
        assert_eq!(
            plain.wait_after(1, &failure(429), 0.0),
            Some(Duration::from_secs(1))
        );
        assert_eq!(plain.attempts_for(&failure(429)), 3);
    }

    #[test]
    fn retry_backoff_grows_exponentially_with_bounded_jitter() {
        for (attempt, base) in [(1, 1.0), (2, 2.0), (3, 4.0)] {
            let wait = retry_backoff(attempt).as_secs_f64();
            assert!((base..base + 0.5).contains(&wait), "{attempt}: {wait}");
        }
    }
}
