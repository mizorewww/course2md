//! Where a video plays, apart from which video it is. An extractor answers with
//! a video's canonical page, which drops the playlist a link opened it in; some
//! videos (a course's, for one) play only inside that playlist. The identity
//! still comes from the extractor's ID, so these parameters never change it.

/// The parameters of a YouTube link that say where the video plays. Tracking
/// (`si`, `pp`, `feature`) and the time are not among them.
const YOUTUBE_PLAYBACK: [&str; 2] = ["list", "index"];

fn is_youtube(url: &url::Url) -> bool {
    url.host_str().is_some_and(|host| {
        let host = host.to_ascii_lowercase();
        ["youtube.com", "youtu.be", "youtube-nocookie.com"]
            .iter()
            .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    })
}

/// The video a YouTube link names: `watch?v=`, `youtu.be/ID`, `shorts/ID`,
/// `live/ID` or `embed/ID`.
fn youtube_video(url: &url::Url) -> Option<String> {
    if !is_youtube(url) {
        return None;
    }
    if url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("youtu.be"))
    {
        return url
            .path_segments()?
            .next()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
    }
    let mut segments = url.path_segments()?;
    match segments.next()? {
        "watch" => url
            .query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, id)| id.into_owned()),
        "shorts" | "live" | "embed" | "v" => segments.next().map(str::to_owned),
        _ => None,
    }
    .filter(|id| !id.is_empty())
}

/// A YouTube link to one video, perhaps opened from a playlist. Reading it as a
/// single video takes `--no-playlist`, or yt-dlp lists the playlist instead; a
/// Bilibili link is left alone, where that flag would quietly pick part 1.
pub fn names_youtube_video(link: &str) -> bool {
    url::Url::parse(link).is_ok_and(|url| youtube_video(&url).is_some())
}

/// `pairs` appended to `target`'s query, each replacing one of the same name.
fn with_pairs(target: &str, pairs: Vec<(String, String)>) -> String {
    if pairs.is_empty() {
        return target.to_owned();
    }
    let Ok(mut url) = url::Url::parse(target) else {
        return target.to_owned();
    };
    let kept: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| !pairs.iter().any(|(name, _)| name == key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(kept).extend_pairs(pairs);
    url.into()
}

/// `concrete`, the extractor's page for video `id`, with the playlist the person's
/// `input` opened that same video in. Any other input leaves it as it is.
pub(crate) fn video_url(concrete: &str, input: &str, id: &str) -> String {
    let Ok(input) = url::Url::parse(input) else {
        return concrete.to_owned();
    };
    let same_video = url::Url::parse(concrete).is_ok_and(|url| is_youtube(&url))
        && youtube_video(&input).is_some_and(|video| video == id);
    if !same_video {
        return concrete.to_owned();
    }
    let pairs = input
        .query_pairs()
        .filter(|(key, value)| YOUTUBE_PLAYBACK.contains(&key.as_ref()) && !value.is_empty())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    with_pairs(concrete, pairs)
}

/// `entry`, one video of the playlist `input` lists, inside that playlist. Its
/// position is the playlist's to know, so a pasted `index` is not carried over.
pub(crate) fn entry_url(entry: &str, input: &str) -> String {
    let (Ok(input), Ok(parsed)) = (url::Url::parse(input), url::Url::parse(entry)) else {
        return entry.to_owned();
    };
    if !is_youtube(&input) || youtube_video(&parsed).is_none() {
        return entry.to_owned();
    }
    let list = input
        .query_pairs()
        .find(|(key, value)| key == "list" && !value.is_empty())
        .map(|(_, value)| value.into_owned());
    match list {
        Some(list) => with_pairs(entry, vec![("list".into(), list)]),
        None => entry.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_keeps_the_playlist_it_was_opened_in() {
        let canonical = "https://www.youtube.com/watch?v=abc123";
        for (input, expected) in [
            (
                "https://www.youtube.com/watch?v=abc123&list=PLx&index=3&si=track&pp=xyz&t=40",
                "https://www.youtube.com/watch?v=abc123&list=PLx&index=3",
            ),
            (
                "https://youtu.be/abc123?list=PLx&si=track",
                "https://www.youtube.com/watch?v=abc123&list=PLx",
            ),
            (
                "https://m.youtube.com/watch?list=PLx&v=abc123",
                "https://www.youtube.com/watch?v=abc123&list=PLx",
            ),
            (
                "https://www.youtube.com/shorts/abc123?feature=share",
                canonical,
            ),
            ("https://www.youtube.com/watch?v=abc123", canonical),
            // Another video's playlist is not this one's.
            ("https://www.youtube.com/watch?v=other&list=PLx", canonical),
            ("not a url", canonical),
        ] {
            assert_eq!(video_url(canonical, input, "abc123"), expected, "{input}");
        }
    }

    #[test]
    fn only_a_youtube_video_is_read_without_its_playlist() {
        for link in [
            "https://www.youtube.com/watch?v=abc123&list=PLx&index=2",
            "https://youtu.be/abc123?list=PLx",
            "https://www.youtube.com/shorts/abc123",
        ] {
            assert!(names_youtube_video(link), "{link}");
        }
        for link in [
            "https://www.youtube.com/playlist?list=PLx",
            "https://www.youtube.com/@teacher/videos",
            "https://www.bilibili.com/video/BV1xx",
            "/videos/lecture.mp4",
        ] {
            assert!(!names_youtube_video(link), "{link}");
        }
    }

    #[test]
    fn other_platforms_keep_the_extractors_page() {
        let bilibili = "https://www.bilibili.com/video/BV1xx?p=2";
        assert_eq!(
            video_url(
                bilibili,
                "https://www.bilibili.com/video/BV1xx?p=2&list=1&spm_id_from=a",
                "BV1xx_p2"
            ),
            bilibili
        );
        assert_eq!(
            entry_url(
                "https://www.bilibili.com/video/BV1xx?p=3",
                "https://www.bilibili.com/video/BV1xx?list=1"
            ),
            "https://www.bilibili.com/video/BV1xx?p=3"
        );
    }

    #[test]
    fn a_playlists_entries_play_inside_it() {
        for input in [
            "https://www.youtube.com/playlist?list=PLx",
            "https://www.youtube.com/watch?v=first&list=PLx&index=1",
        ] {
            assert_eq!(
                entry_url("https://www.youtube.com/watch?v=abc123", input),
                "https://www.youtube.com/watch?v=abc123&list=PLx",
                "{input}"
            );
        }
        // A channel's videos are no playlist; an entry that is no video stays as it is.
        assert_eq!(
            entry_url(
                "https://www.youtube.com/watch?v=abc123",
                "https://www.youtube.com/@teacher/videos"
            ),
            "https://www.youtube.com/watch?v=abc123"
        );
        assert_eq!(
            entry_url(
                "https://www.youtube.com/playlist?list=PLy",
                "https://www.youtube.com/playlist?list=PLx"
            ),
            "https://www.youtube.com/playlist?list=PLy"
        );
    }
}
