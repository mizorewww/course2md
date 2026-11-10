//! What the app puts on the clipboard. Every copy goes through [`write`], so a
//! capture run records it instead of replacing the person's clipboard.
//!
//! Images go on the clipboard as PNG. GPUI writes one to the macOS pasteboard as
//! `public.png` and to the Windows clipboard as the registered `PNG` format; its
//! Linux clipboards (X11 and Wayland) carry text only, so there the app offers the
//! image's path instead (see [`IMAGES`]).
use gpui::{App, ClipboardItem, Image, ImageFormat};
use std::{io::Cursor, path::Path};

/// Whether this platform's clipboard can hold an image.
pub(crate) const IMAGES: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Puts `item` on the clipboard. A capture run (`COURSE2MD_CAPTURE`) writes it to
/// the file `COURSE2MD_CAPTURE_CLIPBOARD` names, when set, and leaves the system's
/// clipboard alone.
pub(crate) fn write(item: ClipboardItem, cx: &mut App) {
    #[cfg(feature = "capture")]
    if std::env::var_os("COURSE2MD_CAPTURE").is_some() {
        record(&item);
        return;
    }
    cx.write_to_clipboard(item);
}

/// Under capture, the text copies the kit makes itself (selected note text, a
/// text field's Copy and Cut) are taken over before they reach its handlers,
/// which write to the system clipboard directly: by the Copy and Cut key
/// bindings in any window, and by a Copy or Cut action dispatched in the app's
/// own view. Selected note text is recorded as [`write`] records a copy; a copy
/// or cut in a text field is logged and dropped, so it changes nothing.
#[cfg(feature = "capture")]
pub(crate) fn intercept_text_copies(cx: &mut App) {
    cx.intercept_keystrokes(|event, window, cx| {
        let copies = window
            .possible_bindings_for_input(std::slice::from_ref(&event.keystroke))
            .iter()
            .any(|binding| is_text_copy(binding.action().as_any()));
        if copies {
            record_text_copy(window, cx);
            cx.stop_propagation();
        }
    })
    .detach();
}

/// The app view's root under capture: Copy and Cut dispatched anywhere in it (as
/// a menu does, without a key) are taken over as [`intercept_text_copies`] says.
pub(crate) fn intercept_view_copies(root: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    #[cfg(feature = "capture")]
    if std::env::var_os("COURSE2MD_CAPTURE").is_some() {
        use gpui::InteractiveElement as _;
        return root
            .capture_action(|_: &gpui_base::input::Copy, window, cx| {
                record_text_copy(window, cx);
                cx.stop_propagation();
            })
            .capture_action(|_: &gpui_base::input::Cut, window, cx| {
                record_text_copy(window, cx);
                cx.stop_propagation();
            });
    }
    root
}

/// Whether `action` is the kit's Copy or Cut.
#[cfg(feature = "capture")]
fn is_text_copy(action: &dyn std::any::Any) -> bool {
    action.is::<gpui_base::input::Copy>() || action.is::<gpui_base::input::Cut>()
}

/// A capture run's text copy: the selected note text, recorded; anything else
/// (a text field's selection, which only the field can read) logged and dropped.
#[cfg(feature = "capture")]
pub(crate) fn record_text_copy(window: &mut gpui::Window, cx: &mut App) {
    let text = gpui_base::TextSelection::selected_text(window, cx);
    if text.trim().is_empty() {
        eprintln!("capture: text copy with no note text selected (not copied)");
    } else {
        write(ClipboardItem::new_string(text), cx);
    }
}

/// A capture run's would-be clipboard write: an image's bytes or the text, in the
/// file `COURSE2MD_CAPTURE_CLIPBOARD` names.
#[cfg(feature = "capture")]
fn record(item: &ClipboardItem) {
    use gpui::ClipboardEntry;
    let (kind, bytes) = match item.entries().first() {
        Some(ClipboardEntry::Image(image)) => (
            format!("image {}", image.format.mime_type()),
            image.bytes().to_vec(),
        ),
        _ => (
            "text".to_owned(),
            item.text().unwrap_or_default().into_bytes(),
        ),
    };
    match std::env::var_os("COURSE2MD_CAPTURE_CLIPBOARD") {
        Some(path) => match std::fs::write(&path, &bytes) {
            Ok(()) => eprintln!(
                "capture: clipboard {kind}, {} bytes, recorded in {} (not copied)",
                bytes.len(),
                Path::new(&path).display()
            ),
            Err(error) => eprintln!("capture: clipboard {kind}: {error}"),
        },
        None => eprintln!(
            "capture: clipboard {kind}, {} bytes (not copied)",
            bytes.len()
        ),
    }
}

/// The picture at `path` as a PNG clipboard image. A PNG file is copied as it is;
/// any other format the reader shows (JPEG, WebP) is decoded and encoded as PNG,
/// which every clipboard reader accepts. Run it off the UI thread.
pub(crate) fn png_image(path: &Path) -> anyhow::Result<ClipboardItem> {
    let bytes = std::fs::read(path)?;
    let reader = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
    let png = if reader.format() == Some(image::ImageFormat::Png) {
        bytes
    } else {
        let decoded = reader.decode()?;
        let mut png = Vec::new();
        decoded.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
        png
    };
    Ok(ClipboardItem::new_image(&Image::from_bytes(
        ImageFormat::Png,
        png,
    )))
}

/// The latest of a series of asynchronous copies. Each copy takes a number when
/// it starts; when it finishes, only the latest one may still reach the
/// clipboard, so a slow encode cannot overwrite what a later copy put there.
#[derive(Default)]
pub(crate) struct Latest(u64);

impl Latest {
    /// Starts a copy, superseding every earlier one, and returns its number.
    pub(crate) fn begin(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }

    /// Whether the copy numbered `copy` is still the latest.
    pub(crate) fn is_current(&self, copy: u64) -> bool {
        self.0 == copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::ClipboardEntry;

    fn png_of(item: &ClipboardItem) -> &Image {
        match item.entries() {
            [ClipboardEntry::Image(image)] => image,
            other => panic!("expected one image entry, got {other:?}"),
        }
    }

    #[test]
    fn a_jpeg_screenshot_is_copied_as_a_png_of_the_same_picture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slide_0001.jpg");
        image::RgbImage::from_pixel(64, 36, image::Rgb([200, 40, 40]))
            .save(&path)
            .unwrap();
        let item = png_image(&path).unwrap();
        let image = png_of(&item);
        assert_eq!(image.format, ImageFormat::Png);
        assert!(image.bytes().starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = image::load_from_memory_with_format(image.bytes(), image::ImageFormat::Png)
            .unwrap()
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 36));
        // JPEG is lossy; the colour survives within a few levels.
        let [r, g, b] = decoded.get_pixel(32, 18).0;
        assert!(r > 180 && g < 70 && b < 70, "{r} {g} {b}");
        // Nothing but the image: a text reader finds no stray string.
        assert_eq!(item.text(), None);
    }

    #[test]
    fn a_png_screenshot_is_copied_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        // Named as a JPEG: the content, not the extension, decides.
        let path = dir.path().join("frame.jpg");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let item = png_image(&path).unwrap();
        assert_eq!(png_of(&item).bytes(), std::fs::read(&path).unwrap());
    }

    #[test]
    fn a_missing_or_unreadable_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(png_image(&dir.path().join("missing.jpg")).is_err());
        let broken = dir.path().join("broken.jpg");
        std::fs::write(&broken, b"not an image").unwrap();
        assert!(png_image(&broken).is_err());
    }

    #[test]
    fn only_the_latest_copy_may_finish() {
        let mut latest = Latest::default();
        let slow = latest.begin();
        let fast = latest.begin();
        // The later copy finishes first and is kept; the earlier one, arriving
        // after it, is dropped.
        assert!(latest.is_current(fast));
        assert!(!latest.is_current(slow));
        // Leaving the note supersedes the copy in flight too.
        latest.begin();
        assert!(!latest.is_current(fast));
    }

    #[gpui::test]
    fn copies_reach_the_apps_clipboard(cx: &mut gpui::TestAppContext) {
        // The test platform's clipboard, never the system's.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");
        image::RgbaImage::new(4, 4).save(&path).unwrap();
        let item = png_image(&path).unwrap();
        cx.update(|cx| write(item.clone(), cx));
        assert_eq!(cx.read_from_clipboard(), Some(item));
    }
}
