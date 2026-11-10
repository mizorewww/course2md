//! What the app puts on the clipboard. Every copy goes through [`write`], so a
//! capture run records it instead of replacing the person's clipboard.
use gpui::{App, ClipboardItem};

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

/// A capture run's would-be clipboard write: an image's bytes or the text, in the
/// file `COURSE2MD_CAPTURE_CLIPBOARD` names.
#[cfg(feature = "capture")]
fn record(item: &ClipboardItem) {
    use gpui::ClipboardEntry;
    use std::path::Path;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn copies_reach_the_apps_clipboard(cx: &mut gpui::TestAppContext) {
        // The test platform's clipboard, never the system's.
        let item = ClipboardItem::new_string("https://example.com/video".into());
        cx.update(|cx| write(item.clone(), cx));
        assert_eq!(cx.read_from_clipboard(), Some(item));
    }
}
