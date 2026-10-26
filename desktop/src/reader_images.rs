//! Screenshots the reader shows, decoded off the UI thread at the size they
//! are drawn and kept in a bounded cache.
//!
//! Lecture screenshots are full video frames (1920×1080 or larger). Handing
//! their paths to `img` decodes every one at full size, keeps the decoded
//! bitmap and its GPU texture for the rest of the session, and minifies a
//! 1920px texture into a 300px card without filtering, which shimmers on
//! slide text. Here the gallery asks for a thumbnail of the width it draws,
//! the viewer for the original, and the least recently drawn images are
//! released (bitmap and texture) once the cache exceeds its budget.
use crate::Desktop;
use gpui::{App, AppContext as _, Context, RenderImage};
use image::imageops::FilterType;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

/// Decoded bytes kept for thumbnails and originals together.
const BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// Decodes running at once, at most this many; the rest wait, newest
/// request first.
const MAX_IN_FLIGHT: usize = 4;
/// How long a newly decoded image counts as arriving, for its fade-in.
const FRESH_FOR: Duration = Duration::from_millis(400);
/// Thumbnail widths are rounded up to this step so small resizes reuse them.
const WIDTH_STEP: u32 = 256;

/// The width an image is decoded at: a thumbnail bucket, or the original.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Size {
    Thumbnail(u32),
    Original,
}

impl Size {
    /// The thumbnail bucket for an image drawn `pixels` device pixels wide.
    pub(crate) fn thumbnail(pixels: f32) -> Self {
        let pixels = pixels.max(1.).ceil() as u32;
        Self::Thumbnail(pixels.div_ceil(WIDTH_STEP).max(1) * WIDTH_STEP)
    }
}

type Key = (PathBuf, Size);

/// What a caller can draw now.
#[derive(Clone)]
pub(crate) enum Loaded {
    Pending,
    /// The image, and whether it has only just arrived.
    Ready(Arc<RenderImage>, bool),
    Failed,
}

enum Entry {
    /// Waiting for a decode slot; `tick` is the last draw that wanted it.
    Queued {
        tick: u64,
    },
    Decoding,
    Ready {
        image: Arc<RenderImage>,
        bytes: usize,
        tick: u64,
        loaded: Instant,
    },
    Failed,
}

pub(crate) struct ReaderImages {
    entries: HashMap<Key, Entry>,
    queue: VecDeque<Key>,
    in_flight: usize,
    bytes: usize,
    /// Advanced once per reader frame; entries remember when they were drawn.
    tick: u64,
    /// Bumped by `clear` so decodes started for an earlier note are dropped.
    generation: u64,
    /// Screenshots the open viewer shows or is about to; never evicted.
    pinned: Vec<PathBuf>,
}

impl Default for ReaderImages {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: 0,
            bytes: 0,
            tick: 1,
            generation: 0,
            pinned: Vec::new(),
        }
    }
}

/// Finds the cache inside the desktop from async completions.
pub(crate) type Locate = fn(&mut Desktop) -> &mut ReaderImages;

impl ReaderImages {
    /// Marks the start of a reader frame.
    pub(crate) fn next_frame(&mut self) {
        self.tick += 1;
    }

    /// The image at `size`, starting its decode when it is not cached.
    pub(crate) fn get(
        &mut self,
        path: &Path,
        size: Size,
        locate: Locate,
        cx: &mut Context<Desktop>,
    ) -> Loaded {
        let tick = self.tick;
        let key = (path.to_path_buf(), size);
        match self.entries.get_mut(&key) {
            Some(Entry::Ready {
                image,
                tick: used,
                loaded,
                ..
            }) => {
                *used = tick;
                return Loaded::Ready(image.clone(), loaded.elapsed() < FRESH_FOR);
            }
            Some(Entry::Failed) => return Loaded::Failed,
            Some(Entry::Decoding) => return Loaded::Pending,
            Some(Entry::Queued { tick: wanted }) => {
                *wanted = tick;
                // Newest wishes are decoded first.
                if let Some(position) = self.queue.iter().position(|queued| *queued == key) {
                    self.queue.remove(position);
                }
                self.queue.push_back(key);
            }
            None => {
                self.entries.insert(key.clone(), Entry::Queued { tick });
                self.queue.push_back(key);
            }
        }
        self.pump(locate, cx);
        Loaded::Pending
    }

    /// Whether decoding `path` at `size` has failed.
    pub(crate) fn failed(&self, path: &Path, size: Size) -> bool {
        matches!(
            self.entries.get(&(path.to_path_buf(), size)),
            Some(Entry::Failed)
        )
    }

    /// A ready image at any size, preferring the original, without decoding.
    pub(crate) fn any_ready(&mut self, path: &Path) -> Option<Arc<RenderImage>> {
        let tick = self.tick;
        let mut best: Option<(&mut Entry, u32)> = None;
        for ((candidate, size), entry) in self.entries.iter_mut() {
            if candidate != path || !matches!(entry, Entry::Ready { .. }) {
                continue;
            }
            let rank = match size {
                Size::Original => u32::MAX,
                Size::Thumbnail(width) => *width,
            };
            if best.as_ref().is_none_or(|(_, best)| rank > *best) {
                best = Some((entry, rank));
            }
        }
        match best {
            Some((
                Entry::Ready {
                    image, tick: used, ..
                },
                _,
            )) => {
                *used = tick;
                Some(image.clone())
            }
            _ => None,
        }
    }

    /// Keeps the viewer's screenshots while it shows them; an empty list
    /// lets them go when the viewer closes.
    pub(crate) fn pin(&mut self, paths: Vec<PathBuf>) {
        self.pinned = paths;
    }

    /// Releases everything, as when another note opens.
    pub(crate) fn clear(&mut self, cx: &mut App) {
        for (_, entry) in self.entries.drain() {
            if let Entry::Ready { image, .. } = entry {
                cx.drop_image(image, None);
            }
        }
        self.queue.clear();
        self.pinned.clear();
        self.bytes = 0;
        self.generation += 1;
        // Decodes in flight finish into the void; their slots free then.
    }

    fn pump(&mut self, locate: Locate, cx: &mut Context<Desktop>) {
        let slots = std::thread::available_parallelism()
            .map_or(2, |cores| cores.get() / 2)
            .clamp(2, MAX_IN_FLIGHT);
        while self.in_flight < slots {
            let Some(key) = self.queue.pop_back() else {
                return;
            };
            // A wish from frames ago belongs to a card scrolled away.
            if let Some(Entry::Queued { tick }) = self.entries.get(&key)
                && *tick + 2 < self.tick
            {
                self.entries.remove(&key);
                continue;
            }
            self.entries.insert(key.clone(), Entry::Decoding);
            self.in_flight += 1;
            let generation = self.generation;
            let decode = cx.background_spawn({
                let (path, size) = key.clone();
                async move { decode(&path, size) }
            });
            cx.spawn(async move |this, cx| {
                let result = decode.await;
                let _ = this.update(cx, |desktop, cx| {
                    let images = locate(desktop);
                    images.finish(key, generation, result, cx);
                    images.pump(locate, cx);
                    desktop.reader_image_loaded(cx);
                });
            })
            .detach();
        }
    }

    fn finish(&mut self, key: Key, generation: u64, result: Option<RenderImage>, cx: &mut App) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if generation != self.generation {
            // Decoded for a note `clear` has since released.
            return;
        }
        let entry = match result {
            Some(image) => {
                let bytes = image.as_bytes(0).map_or(0, <[u8]>::len);
                self.bytes += bytes;
                Entry::Ready {
                    image: Arc::new(image),
                    bytes,
                    tick: self.tick,
                    loaded: Instant::now(),
                }
            }
            None => Entry::Failed,
        };
        self.entries.insert(key, entry);
        self.evict(cx);
    }

    /// Drops the least recently drawn images until the cache fits its
    /// budget, never one drawn in the current or previous frame.
    fn evict(&mut self, cx: &mut App) {
        while self.bytes > BUDGET_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter_map(|(key, entry)| match entry {
                    Entry::Ready { tick, .. }
                        if *tick + 1 < self.tick && !self.pinned.contains(&key.0) =>
                    {
                        Some((key, *tick))
                    }
                    _ => None,
                })
                .min_by_key(|(_, tick)| *tick)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else {
                return;
            };
            if let Some(Entry::Ready { image, bytes, .. }) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(bytes);
                cx.drop_image(image, None);
            }
        }
    }

    #[cfg(test)]
    fn ready_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| matches!(entry, Entry::Ready { .. }))
            .count()
    }
}

/// Decodes `path` and scales it to `size` with a filter that keeps slide text
/// legible, as the BGRA frame GPUI uploads.
fn decode(path: &Path, size: Size) -> Option<RenderImage> {
    let image = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let image = match size {
        Size::Thumbnail(width) if width < image.width() => {
            let height = ((image.height() as u64 * width as u64) / image.width() as u64).max(1);
            image.resize_exact(width, height as u32, FilterType::Triangle)
        }
        _ => image,
    };
    let mut data = image.into_rgba8();
    for pixel in data.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some(RenderImage::new([image::Frame::new(data)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_widths_round_up_to_shared_buckets() {
        assert_eq!(Size::thumbnail(1.), Size::Thumbnail(256));
        assert_eq!(Size::thumbnail(256.), Size::Thumbnail(256));
        assert_eq!(Size::thumbnail(257.), Size::Thumbnail(512));
        assert_eq!(Size::thumbnail(780.5), Size::Thumbnail(1024));
    }

    #[test]
    fn decoding_scales_to_the_bucket_and_keeps_the_aspect_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");
        image::RgbaImage::from_pixel(1920, 1080, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let thumb = decode(&path, Size::Thumbnail(512)).unwrap();
        assert_eq!(thumb.size(0).width.0, 512);
        assert_eq!(thumb.size(0).height.0, 288);
        // BGRA, as GPUI uploads it.
        assert_eq!(&thumb.as_bytes(0).unwrap()[..4], &[30, 20, 10, 255]);
        let original = decode(&path, Size::Original).unwrap();
        assert_eq!(original.size(0).width.0, 1920);
        // A bucket wider than the image keeps its pixels.
        assert_eq!(
            decode(&path, Size::Thumbnail(4096))
                .unwrap()
                .size(0)
                .width
                .0,
            1920
        );
        assert!(decode(&dir.path().join("missing.png"), Size::Original).is_none());
        // A JPEG cut before its image data cannot be decoded.
        let jpeg = dir.path().join("frame.jpg");
        image::RgbImage::from_pixel(64, 36, image::Rgb([200, 100, 50]))
            .save(&jpeg)
            .unwrap();
        let bytes = std::fs::read(&jpeg).unwrap();
        let scan = bytes.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
        std::fs::write(&jpeg, &bytes[..scan]).unwrap();
        assert!(decode(&jpeg, Size::thumbnail(256.)).is_none());
    }

    #[gpui::test]
    fn eviction_keeps_recent_images_and_releases_old_ones(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut images = ReaderImages::default();
            let frame = || {
                RenderImage::new([image::Frame::new(image::RgbaImage::new(
                    4096, 4096, // 64 MiB each
                ))])
            };
            for index in 0..6 {
                images.next_frame();
                images.next_frame();
                images.in_flight += 1;
                images.finish(
                    (PathBuf::from(format!("{index}.jpg")), Size::Original),
                    images.generation,
                    Some(frame()),
                    cx,
                );
            }
            assert!(images.bytes <= BUDGET_BYTES);
            assert_eq!(images.ready_count(), 4);
            assert!(images.any_ready(Path::new("5.jpg")).is_some());
            assert!(images.any_ready(Path::new("0.jpg")).is_none());
            images.clear(cx);
            assert_eq!(images.ready_count(), 0);
            assert_eq!(images.bytes, 0);
        });
    }
}
