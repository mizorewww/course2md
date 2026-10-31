//! Reader identities, source links, search and display timestamps.
use std::{
    collections::BTreeMap,
    ops::Range,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceTarget {
    Web(String),
    Local(PathBuf),
}

pub fn source_target(raw: &str, local: bool) -> Option<SourceTarget> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(url) = url::Url::parse(raw) {
        if url.scheme() == "file" {
            return url.to_file_path().ok().map(SourceTarget::Local);
        }
        if !local && matches!(url.scheme(), "https" | "http") && url.host_str().is_some() {
            return Some(SourceTarget::Web(url.into()));
        }
    }
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(SourceTarget::Local(path))
}

/// Only these source platforms promise a seek URL; arbitrary sites and local players do not.
pub fn seek_url(target: &SourceTarget, seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds < 0. {
        return None;
    }
    let SourceTarget::Web(raw) = target else {
        return None;
    };
    let mut url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let supported = ["youtube.com", "youtu.be", "bilibili.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")));
    if !supported {
        return None;
    }
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "t" && key != "start")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs)
        .append_pair("t", &(seconds.floor() as u64).to_string());
    url.set_fragment(None);
    Some(url.into())
}

/// Prefer the registered new location even while the old recovery copy still exists.
pub fn relocated_source(path: &Path, locations: &[(PathBuf, Vec<PathBuf>)]) -> PathBuf {
    let mut candidates = Vec::new();
    for (current, previous) in locations {
        for old in previous {
            if let Ok(relative) = path.strip_prefix(old) {
                candidates.push((old.components().count(), current.join(relative)));
            } else if let (Ok(path), Ok(old)) = (path.canonicalize(), old.canonicalize())
                && let Ok(relative) = path.strip_prefix(&old)
            {
                candidates.push((old.components().count(), current.join(relative)));
            }
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, path)| path)
        .unwrap_or_else(|| path.to_path_buf())
}

pub fn nearest_time(
    times: impl IntoIterator<Item = (usize, Option<f64>)>,
    target: f64,
) -> Option<(usize, bool)> {
    if !target.is_finite() {
        return None;
    }
    times
        .into_iter()
        .filter_map(|(index, seconds)| {
            seconds
                .filter(|s| s.is_finite())
                .map(|seconds| (index, (seconds - target).abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(index, distance)| (index, distance < 0.05))
}

/// Map case-folded matches back to valid UTF-8 ranges, including expanding lowercase chars.
pub struct PreparedText {
    pub lower: String,
    pub mapping: Vec<(usize, usize)>,
}

pub fn prepare_text(text: &str) -> PreparedText {
    let mut lower = String::new();
    let mut mapping = Vec::new();
    for (start, character) in text.char_indices() {
        for lower_char in character.to_lowercase() {
            let count = lower_char.len_utf8();
            lower.push(lower_char);
            mapping.extend(std::iter::repeat_n(
                (start, start + character.len_utf8()),
                count,
            ));
        }
    }
    PreparedText { lower, mapping }
}

pub fn text_matches_prepared(prepared: &PreparedText, query: &str) -> Vec<Range<usize>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    prepared
        .lower
        .match_indices(&query)
        .filter_map(|(start, value)| {
            Some(prepared.mapping.get(start)?.0..prepared.mapping.get(start + value.len() - 1)?.1)
        })
        .collect()
}

pub fn text_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    text_matches_prepared(&prepare_text(text), query)
}

/// A contents list describes topics. Frame boundaries remain in the article,
/// but a timestamp alone does not become a chapter in the reading rail.
pub fn content_outline(
    headings: &[(usize, String, Option<f64>, bool)],
    topics: &[(f64, String)],
) -> Vec<(usize, String, Option<f64>)> {
    let mut named = BTreeMap::new();
    for (index, label, seconds, timestamp_only) in headings {
        if !label.trim().is_empty() && !timestamp_only {
            named.insert(*index, (label.clone(), *seconds));
        }
    }
    for (seconds, title) in topics.iter().filter(|(_, title)| !title.trim().is_empty()) {
        if let Some((index, _)) = nearest_time(
            headings
                .iter()
                .map(|(index, _, seconds, _)| (*index, *seconds)),
            *seconds,
        ) {
            // The entry opens this section, whose displayed time may differ
            // slightly from the summary's proposed topic boundary.
            let displayed_time = headings
                .iter()
                .find(|(heading, _, _, _)| *heading == index)
                .and_then(|(_, _, seconds, _)| *seconds);
            named.insert(index, (title.clone(), displayed_time));
        }
    }
    if named.len() < 2 {
        return Vec::new();
    }
    named
        .into_iter()
        .map(|(index, (label, seconds))| (index, label, seconds))
        .collect()
}

/// Calendar date in local time, without slicing a formatted timestamp.
pub fn date_local(milliseconds: u64) -> String {
    use chrono::Datelike;
    let instant = i64::try_from(milliseconds)
        .ok()
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis);
    let Some(instant) = instant.filter(|instant| (1..=9999).contains(&instant.year())) else {
        return "-".into();
    };
    instant
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d")
        .to_string()
}

/// Display an immutable UTC instant using the system timezone's rules for that
/// date, rather than applying today's offset to every historical timestamp.
pub fn timestamp_local(milliseconds: u64) -> String {
    timestamp_in_timezone(milliseconds, &chrono::Local)
}

fn timestamp_in_timezone<Tz: chrono::TimeZone>(milliseconds: u64, timezone: &Tz) -> String {
    use chrono::Datelike;

    let display = || {
        let instant = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
            i64::try_from(milliseconds).ok()?,
        )?;
        // Keep ordinary display years bounded before adding a timezone offset;
        // malformed persisted values must not overflow or invent a nearby date.
        if !(1..=9999).contains(&instant.year()) {
            return None;
        }
        let local = instant.with_timezone(timezone).naive_local();
        if !(1..=9999).contains(&local.year()) {
            return None;
        }
        Some(local.format("%Y-%m-%d %H:%M").to_string())
    };
    display().unwrap_or_else(|| course2md::tr!("时间未知").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contents_use_real_topics_instead_of_every_frame_timestamp() {
        let headings = vec![
            (0, "摘要".into(), None, false),
            (2, "00:06".into(), Some(6.), true),
            (5, "00:16".into(), Some(16.), true),
            (8, "00:27".into(), Some(27.), true),
            (11, "00:40".into(), Some(40.), true),
        ];
        assert!(content_outline(&headings, &[]).is_empty());
        assert_eq!(
            content_outline(
                &headings,
                &[(6., "问题由来".into()), (39., "讨论与回应".into())]
            ),
            vec![
                (0, "摘要".into(), None),
                (2, "问题由来".into(), Some(6.)),
                (11, "讨论与回应".into(), Some(40.)),
            ]
        );
        let authored = vec![
            (0, "第一章".into(), None, false),
            (5, "第二章".into(), None, false),
        ];
        assert_eq!(
            content_outline(&authored, &[]),
            authored
                .iter()
                .map(|(index, label, seconds, _)| (*index, label.clone(), *seconds))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn source_links_do_not_invent_local_or_unknown_seek_support() {
        assert!(source_target("", false).is_none());
        assert!(source_target("javascript:alert(1)", false).is_none());
        let directory = tempfile::tempdir().unwrap();
        let video = directory.path().join("video.mp4");
        let local = source_target(video.to_str().unwrap(), true).unwrap();
        assert!(seek_url(&local, 12.).is_none());
        let unknown = source_target("https://video.example/watch", false).unwrap();
        assert!(seek_url(&unknown, 12.).is_none());
        let web = source_target("https://www.bilibili.com/video/BV123?p=2&t=4#old", false).unwrap();
        let url = seek_url(&web, 90.).unwrap();
        assert!(url.contains("p=2") && url.contains("t=90") && !url.contains("old"));
    }
    #[test]
    fn move_uses_new_root_even_when_old_copy_is_kept() {
        let locations = vec![(PathBuf::from("/new"), vec![PathBuf::from("/old")])];
        assert_eq!(
            relocated_source(Path::new("/old/video.mp4"), &locations),
            PathBuf::from("/new/video.mp4")
        );
        assert_eq!(
            relocated_source(Path::new("/outside/video.mp4"), &locations),
            PathBuf::from("/outside/video.mp4")
        );
    }
    #[test]
    fn nearest_time_uses_known_data_and_reports_approximate_matches() {
        assert_eq!(
            nearest_time([(0, None), (1, Some(10.)), (2, Some(20.))], 19.),
            Some((2, false))
        );
        assert_eq!(
            nearest_time([(0, None), (1, Some(0.))], 0.),
            Some((1, true))
        );
        assert_eq!(nearest_time([(0, None)], 0.), None);
    }
    #[test]
    fn search_preserves_utf8_and_finds_multiple_real_matches() {
        let value = "中文 AI 与 ai，İstanbul";
        let ranges = text_matches(value, "ai");
        assert_eq!(
            ranges.iter().map(|r| &value[r.clone()]).collect::<Vec<_>>(),
            ["AI", "ai"]
        );
        let range = text_matches(value, "i̇").remove(0);
        assert_eq!(&value[range], "İ");
        assert!(text_matches(value, "").is_empty());
    }
    #[test]
    fn local_display_rolls_dates_across_year_and_leap_day_boundaries() {
        let singapore = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let pacific = chrono::FixedOffset::west_opt(8 * 3600).unwrap();
        let milliseconds = |value: &str| {
            chrono::DateTime::parse_from_rfc3339(value)
                .unwrap()
                .timestamp_millis() as u64
        };
        assert_eq!(
            timestamp_in_timezone(milliseconds("2023-12-31T20:30:00Z"), &singapore),
            "2024-01-01 04:30"
        );
        assert_eq!(
            timestamp_in_timezone(milliseconds("2024-02-28T20:30:00Z"), &singapore),
            "2024-02-29 04:30"
        );
        assert_eq!(timestamp_in_timezone(0, &pacific), "1969-12-31 16:00");
        assert_eq!(
            timestamp_in_timezone(milliseconds("2024-03-01T00:15:00Z"), &pacific),
            "2024-02-29 16:15"
        );
    }
    #[test]
    fn local_display_preserves_non_hour_offsets_and_rejects_invalid_dates() {
        let nepal = chrono::FixedOffset::east_opt(5 * 3600 + 45 * 60).unwrap();
        assert_eq!(timestamp_in_timezone(0, &nepal), "1970-01-01 05:45");
        assert_eq!(timestamp_in_timezone(u64::MAX, &nepal), "时间未知");
        assert_eq!(timestamp_in_timezone(253402300799999, &nepal), "时间未知");
    }
}
