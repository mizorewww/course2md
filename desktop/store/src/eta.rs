//! The one remaining-time estimate every measured stage shows: downloads, scene
//! scanning, screenshots, transcription and the AI steps alike.
//!
//! A stage reports its count unevenly: frames in bursts as the decoder catches up,
//! AI batches whenever an answer lands, bytes as the network delivers them. Timing
//! the rest by the last report would make the estimate jump with every burst, and
//! timing it by the average since the start would make it climb between reports
//! and never forget a slow start. Instead:
//!
//! - the rate is measured over a sliding window: the last [`WINDOW`] of reports,
//!   and never fewer than [`WINDOW_REPORTS`] of them, so work that reports rarely
//!   (an AI batch every half minute) is still averaged over several reports;
//! - nothing is timed before the work has run [`WARMUP_TIME`] and done a caller's
//!   minimum count (one batch per concurrent request, a wave of downloaded ranges);
//! - the estimate shown counts down by itself between reports, is reconsidered at
//!   most every [`REFRESH`], and moves only when the new estimate differs from it
//!   by more than [`TOLERANCE`] of it plus [`SLACK`] (then halfway), so ordinary
//!   fluctuation never moves it up and down;
//! - it is worded in steps that grow with it ([`label`]): 5 s under a minute, then
//!   10 s, 30 s, a minute and 5 minutes;
//! - a countdown that runs out while the work goes on shows nothing rather than a
//!   number that is no longer true, and work that resumes after [`STALL`] is
//!   timed afresh;
//! - work read back from an earlier run (AI answers from their receipts) is
//!   neither speed nor left to do: callers leave it out of the counts.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back the rate looks.
const WINDOW: Duration = Duration::from_secs(30);
/// The fewest reports with progress the rate spans, however old.
const WINDOW_REPORTS: usize = 8;
/// A silence after which work that resumes is timed afresh. Apple model
/// ranges land a wave at a time, half a minute apart on a slow link.
const STALL: Duration = Duration::from_secs(120);
/// Reports closer together than this are one point of the window.
const POINT_SPACING: Duration = Duration::from_millis(250);
/// How long work must have run in its sample before it is timed at all.
const WARMUP_TIME: Duration = Duration::from_secs(3);
/// The shown estimate is reconsidered at most this often.
const REFRESH: Duration = Duration::from_secs(1);
/// The share of the shown estimate a new one may differ by and leave it alone…
const TOLERANCE: f64 = 0.15;
/// …plus this many seconds, so a short estimate does not twitch either.
const SLACK: f64 = 5.0;
/// How far a shown estimate above a lower one within the tolerance moves
/// toward it at each reconsideration.
const SETTLE: f64 = 0.2;

/// A stage's remaining-time estimate, fed with its reports in order.
#[derive(Clone, Debug, Default)]
pub struct Eta {
    /// Reports that moved the count, oldest first: when, and the count then.
    /// Until the window is full the oldest is the sample's origin.
    points: VecDeque<(Instant, u64)>,
    /// The estimate in seconds as of an instant; it counts down from there.
    shown: Option<(Instant, f64)>,
}

impl Eta {
    /// Forgets everything measured: the work started again (a retry, a new
    /// download phase), and what came before says nothing about its pace.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Takes the report of `current` of `total` at `at`, for work whose sample
    /// began at `origin` (when, and the count then: work done before it, such as
    /// a resumed file or cached results, is not throughput). Nothing is timed
    /// before `warmup` units of this sample are done.
    pub fn observe(
        &mut self,
        at: Instant,
        origin: (Instant, u64),
        current: u64,
        total: u64,
        warmup: u64,
    ) {
        if total == 0 || current >= total {
            self.shown = None;
            return;
        }
        let (start, base) = origin;
        if self.points.is_empty() {
            self.points.push_back(origin);
        }
        let &(last_time, last_count) = self.points.back().unwrap_or(&origin);
        if current < last_count {
            self.reset();
            return;
        }
        if current > last_count
            && self.points.len() > 1
            && at.saturating_duration_since(last_time) >= self.stall()
        {
            // Work that stalled for two minutes or more (three of its usual gaps
            // between reports, for work that reports rarely) starts measuring
            // again from here: the stall says nothing about its pace now.
            self.points.clear();
            self.points.push_back((at, current));
            self.shown = None;
            return;
        }
        if current > last_count {
            // The newest point moves forward until it is a spacing past the one
            // before it, then stays and a new one starts: compared with the
            // newest instead, frequent reports would keep replacing it and the
            // window would never fill.
            let len = self.points.len();
            let previous = (len > 1).then(|| self.points[len - 2].0);
            if previous
                .is_some_and(|time| last_time.saturating_duration_since(time) < POINT_SPACING)
            {
                self.points.pop_back();
            }
            self.points.push_back((at, current));
        }
        while self.points.len() > WINDOW_REPORTS + 1
            && self
                .points
                .get(1)
                .is_some_and(|(time, _)| at.saturating_duration_since(*time) >= WINDOW)
        {
            self.points.pop_front();
        }
        if current.saturating_sub(base) < warmup.max(1)
            || at.saturating_duration_since(start) < WARMUP_TIME
        {
            return;
        }
        let Some(&(from, counted)) = self.points.front() else {
            return;
        };
        let span = at.saturating_duration_since(from).as_secs_f64();
        let rate = current.saturating_sub(counted) as f64 / span;
        if !(rate > 0. && rate.is_finite()) {
            return;
        }
        let estimate = (total - current) as f64 / rate;
        // A countdown that ran out is not shown and is not a starting point.
        let shown = self.shown.filter(|(then, value)| {
            *value - at.saturating_duration_since(*then).as_secs_f64() > 0.
        });
        self.shown = match shown {
            None => Some((at, estimate)),
            Some((then, value)) => {
                let since = at.saturating_duration_since(then);
                if since < REFRESH {
                    return;
                }
                let showing = (value - since.as_secs_f64()).max(0.);
                let gap = estimate - showing;
                // A longer estimate must differ by twice as much: a number
                // that goes up is the one that reads as jumping.
                let tolerance = if gap > 0. { 2. } else { 1. } * (TOLERANCE * showing + SLACK);
                if gap.abs() > tolerance {
                    Some((at, showing + gap / 2.))
                } else if gap < 0. {
                    // Within the tolerance it only ever closes in from above, a
                    // fifth of the way each time, so it settles on the estimate
                    // without going up.
                    Some((at, showing + gap * SETTLE))
                } else {
                    return;
                }
            }
        };
    }

    /// How long a silence is a stall: [`STALL`], or three times the usual gap
    /// between reports when work reports more rarely than that (one AI batch
    /// every few minutes on a local model).
    fn stall(&self) -> Duration {
        let mut gaps: Vec<Duration> = self
            .points
            .iter()
            .zip(self.points.iter().skip(1))
            .map(|((earlier, _), (later, _))| later.saturating_duration_since(*earlier))
            .collect();
        if gaps.is_empty() {
            return STALL;
        }
        gaps.sort();
        STALL.max(gaps[gaps.len() / 2] * 3)
    }

    /// The remaining time as of `at`, while the countdown has not run out.
    pub fn remaining(&self, at: Instant) -> Option<Duration> {
        let (then, value) = self.shown?;
        let left = value - at.saturating_duration_since(then).as_secs_f64();
        (left > 0.).then(|| Duration::from_secs_f64(left))
    }

    /// [`Self::remaining`] in words.
    pub fn label(&self, at: Instant) -> Option<String> {
        self.remaining(at).map(label)
    }
}

/// A remaining time in the step its size calls for, rounded up so it never
/// reaches zero early: "15 秒", "1 分 40 秒", "4 分 30 秒", "12 分钟", "1 小时 05 分".
pub fn label(remaining: Duration) -> String {
    let seconds = remaining.as_secs_f64();
    let step = match seconds {
        ..60. => 5.,
        ..180. => 10.,
        ..600. => 30.,
        ..3600. => 60.,
        _ => 300.,
    };
    let rounded = ((seconds / step).ceil() * step).max(step) as u64;
    if (600..3600).contains(&rounded) {
        c2m_i18n::tr!("{} 分钟", rounded / 60)
    } else {
        crate::activity::duration_label(rounded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator, so a noisy run is the same every time.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// What a person sees, read every `read` from the start, for reports
    /// `(seconds, count)` of `total`, with the true remaining time at each read.
    fn watch(reports: &[(f64, u64)], total: u64, warmup: u64) -> Vec<(f64, Option<u64>, f64)> {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let finish = reports.last().unwrap().0;
        let mut eta = Eta::default();
        let mut next = 0;
        let mut seen = Vec::new();
        let mut time = 0.;
        while time < finish {
            while next < reports.len() && reports[next].0 <= time {
                let (when, count) = reports[next];
                eta.observe(at(when), (start, 0), count, total, warmup);
                next += 1;
            }
            let shown = eta.remaining(at(time)).map(|left| {
                // The value as worded, in seconds.
                let seconds = left.as_secs_f64();
                let step = match seconds {
                    ..60. => 5.,
                    ..180. => 10.,
                    ..600. => 30.,
                    ..3600. => 60.,
                    _ => 300.,
                };
                ((seconds / step).ceil() * step) as u64
            });
            seen.push((time, shown, finish - time));
            time += 0.1;
        }
        seen
    }

    /// How often the shown value changes, how often it goes up, and its worst
    /// error against the truth (relative, or against a minute when less is
    /// left) from `from` of the work on.
    fn jitter(seen: &[(f64, Option<u64>, f64)], from: f64) -> (usize, usize, f64) {
        let shown: Vec<_> = seen
            .iter()
            .filter_map(|(t, s, truth)| Some((*t, (*s)?, *truth)))
            .collect();
        let changes = shown.windows(2).filter(|w| w[0].1 != w[1].1).count();
        let rises = shown.windows(2).filter(|w| w[1].1 > w[0].1).count();
        let total = seen.last().map_or(0., |(t, ..)| *t);
        let error = shown
            .iter()
            .filter(|(t, ..)| *t > total * from && *t < total * 0.95)
            .map(|(_, s, truth)| (*s as f64 - truth).abs() / truth.max(60.))
            .fold(0., f64::max);
        (changes, rises, error)
    }

    #[test]
    fn a_noisy_scan_counts_down_steadily() {
        // A scan of 3,000 frames in 300 s: frames arrive in uneven bursts every
        // 50–350 ms, with a two-second pause every 25 s while a candidate is saved.
        let mut noise = Noise(7);
        let (mut time, mut count, mut reports) = (0., 0u64, Vec::new());
        while count < 3_000 {
            time += 0.05 + 0.3 * noise.next();
            if (time % 25.) < 2. {
                continue;
            }
            count = (count + (1. + 6. * noise.next()) as u64).min(3_000);
            reports.push((time, count));
        }
        let seen = watch(&reports, 3_000, 1);
        // Nothing before there is enough to go on.
        assert!(
            seen.iter()
                .take_while(|(t, ..)| *t < 3.)
                .all(|(_, s, _)| s.is_none())
        );
        assert!(seen.iter().any(|(t, s, _)| *t < 6. && s.is_some()));
        let (changes, rises, error) = jitter(&seen, 0.1);
        let seconds = seen.len() / 10;
        // It changes at most about once per step, never faster than once a second.
        assert!(changes <= seconds / 2, "{changes} changes in {seconds} s");
        assert!(rises <= 3, "{rises} rises");
        assert!(error < 0.3, "error {error}");
    }

    #[test]
    fn concurrent_ai_batches_are_timed_from_the_first_round() {
        // 40 batches, four at a time, each answered in 20–40 s.
        let mut noise = Noise(11);
        let mut lanes = [0f64; 4];
        let mut done = Vec::new();
        for _ in 0..40 {
            let lane = (0..4)
                .min_by(|a, b| lanes[*a].total_cmp(&lanes[*b]))
                .unwrap();
            lanes[lane] += 20. + 20. * noise.next();
            done.push(lanes[lane]);
        }
        done.sort_by(f64::total_cmp);
        let reports: Vec<(f64, u64)> = done
            .iter()
            .enumerate()
            .map(|(i, t)| (*t, i as u64 + 1))
            .collect();
        let seen = watch(&reports, 40, 4);
        // Not before the first round of four is answered.
        let first = seen.iter().find(|(_, s, _)| s.is_some()).unwrap().0;
        assert!(first >= done[3], "{first} before {}", done[3]);
        let (_, rises, error) = jitter(&seen, 1. / 3.);
        assert!(rises <= 1, "{rises} rises");
        assert!(error < 0.25, "error {error}");
    }

    #[test]
    fn a_steady_download_is_not_moved_by_small_fluctuations() {
        // 100 MB at 1 MB/s ± 20%, reported every 200 ms.
        let mut noise = Noise(3);
        let mut reports = Vec::new();
        let mut count = 0u64;
        for tick in 1..=500 {
            count += (200_000. * (0.8 + 0.4 * noise.next())) as u64;
            reports.push((tick as f64 * 0.2, count.min(100_000_000)));
        }
        let total = count;
        let seen = watch(&reports, total, 1);
        let (_, rises, error) = jitter(&seen, 0.1);
        assert_eq!(rises, 0, "a steady download only counts down");
        assert!(error < 0.2, "error {error}");
    }

    #[test]
    fn a_real_slowdown_is_followed_and_a_stall_shows_nothing() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let mut eta = Eta::default();
        // 10 units a second for 20 s of 1,000.
        for second in 1..=20 {
            eta.observe(at(second as f64), (start, 0), second * 10, 1_000, 1);
        }
        let before = eta.remaining(at(20.)).unwrap().as_secs_f64();
        assert!((75. ..85.).contains(&before), "{before}");
        // Then 2 a second: the estimate follows within a minute.
        for second in 21..=80 {
            eta.observe(
                at(second as f64),
                (start, 0),
                200 + (second - 20) * 2,
                1_000,
                1,
            );
        }
        let after = eta.remaining(at(80.)).unwrap().as_secs_f64();
        let truth = (1_000. - 320.) / 2.;
        assert!((after - truth).abs() / truth < 0.3, "{after} vs {truth}");
        // No report for longer than the estimate: nothing is shown.
        assert!(eta.remaining(at(80. + after + 1.)).is_none());
        // Done, or a retry: no estimate.
        eta.observe(at(500.), (start, 0), 1_000, 1_000, 1);
        assert!(eta.remaining(at(500.)).is_none());
    }

    /// A scan reporting every 100 ms that slows down from 100 to 20 frames a
    /// second: the estimate follows the new pace within the window, rather than
    /// the average since the start.
    #[test]
    fn frequent_reports_keep_a_window_that_follows_a_slowdown() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let total = 50_000;
        let mut eta = Eta::default();
        let mut count = 0;
        for tick in 1..=6_000 {
            let seconds = f64::from(tick) / 10.;
            count += if seconds <= 300. { 10 } else { 2 };
            eta.observe(at(seconds), (start, 0), count, total, 1);
        }
        // At 600 s: 36,000 done, 14,000 left at 20 a second.
        let truth = (total - count) as f64 / 20.;
        let left = eta.remaining(at(600.)).unwrap().as_secs_f64();
        assert!((left - truth).abs() / truth < 0.2, "{left} vs {truth}");
        assert!(eta.points.len() <= 200, "{} points", eta.points.len());
    }

    /// Work that resumes after a stall longer than the window is timed afresh,
    /// at its new pace, not from half of an old countdown.
    #[test]
    fn work_resuming_after_a_stall_is_timed_afresh() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let mut eta = Eta::default();
        for second in 1..=20 {
            eta.observe(at(second as f64), (start, 0), second * 10, 1_000, 1);
        }
        // Nothing for two minutes, then 10 a second again.
        for second in 140..=150 {
            let count = 200 + (second - 139) * 10;
            eta.observe(at(second as f64), (start, 0), count, 1_000, 1);
        }
        let left = eta.remaining(at(150.)).unwrap().as_secs_f64();
        let truth = (1_000. - 310.) / 10.;
        assert!((left - truth).abs() / truth < 0.2, "{left} vs {truth}");
    }

    /// One batch every two and a half minutes (a local model, one at a time) is
    /// its pace, not a stall: the estimate appears and follows it.
    #[test]
    fn work_that_reports_every_few_minutes_is_timed() {
        let start = Instant::now();
        let at = |seconds: f64| start + Duration::from_secs_f64(seconds);
        let mut eta = Eta::default();
        for batch in 1..=4u64 {
            eta.observe(at(150. * batch as f64), (start, 0), batch, 12, 1);
        }
        let left = eta.remaining(at(600.)).unwrap().as_secs_f64();
        let truth = 8. * 150.;
        assert!((left - truth).abs() / truth < 0.2, "{left} vs {truth}");
    }

    #[test]
    fn labels_round_up_in_steps_that_grow() {
        let label = |seconds| label(Duration::from_secs_f64(seconds));
        assert_eq!(label(0.4), "5 秒");
        assert_eq!(label(12.), "15 秒");
        assert_eq!(label(58.), "1 分 00 秒");
        assert_eq!(label(61.), "1 分 10 秒");
        assert_eq!(label(170.), "2 分 50 秒");
        assert_eq!(label(181.), "3 分 30 秒");
        assert_eq!(label(599.), "10 分钟");
        assert_eq!(label(725.), "13 分钟");
        assert_eq!(label(3599.), "1 小时 00 分");
        assert_eq!(label(3900.), "1 小时 05 分");
    }
}
