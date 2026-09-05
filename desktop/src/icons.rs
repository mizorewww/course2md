//! Embedded Material Icons, including the component library's standard icon paths.
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

pub struct Assets;
const ICONS: &[(&str, &[u8])] = &[
    (
        "icons/book-open.svg",
        include_bytes!("../assets/material/book-open.svg"),
    ),
    (
        "icons/plus.svg",
        include_bytes!("../assets/material/plus.svg"),
    ),
    (
        "icons/task.svg",
        include_bytes!("../assets/material/task.svg"),
    ),
    (
        "icons/refresh.svg",
        include_bytes!("../assets/material/refresh.svg"),
    ),
    (
        "icons/settings.svg",
        include_bytes!("../assets/material/settings.svg"),
    ),
    (
        "icons/folder.svg",
        include_bytes!("../assets/material/folder.svg"),
    ),
    (
        "icons/folder-open.svg",
        include_bytes!("../assets/material/folder-open.svg"),
    ),
    (
        "icons/arrow-left.svg",
        include_bytes!("../assets/material/arrow-left.svg"),
    ),
    (
        "icons/chevron-up.svg",
        include_bytes!("../assets/material/chevron-up.svg"),
    ),
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/material/chevron-down.svg"),
    ),
    (
        "icons/chevron-left.svg",
        include_bytes!("../assets/material/chevron-left.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/material/chevron-right.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/material/close.svg"),
    ),
    (
        "icons/circle-x.svg",
        include_bytes!("../assets/material/circle-x.svg"),
    ),
    (
        "icons/file.svg",
        include_bytes!("../assets/material/file.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../assets/material/check.svg"),
    ),
    (
        "icons/eye.svg",
        include_bytes!("../assets/material/eye.svg"),
    ),
    (
        "icons/eye-off.svg",
        include_bytes!("../assets/material/eye-off.svg"),
    ),
    (
        "icons/minus.svg",
        include_bytes!("../assets/material/minus.svg"),
    ),
    (
        "icons/external-link.svg",
        include_bytes!("../assets/material/external-link.svg"),
    ),
    (
        "icons/circle-check.svg",
        include_bytes!("../assets/material/circle-check.svg"),
    ),
    (
        "icons/info.svg",
        include_bytes!("../assets/material/info.svg"),
    ),
    (
        "icons/triangle-alert.svg",
        include_bytes!("../assets/material/triangle-alert.svg"),
    ),
    (
        "icons/window-close.svg",
        include_bytes!("../assets/material/window-close.svg"),
    ),
    (
        "icons/window-maximize.svg",
        include_bytes!("../assets/material/window-maximize.svg"),
    ),
    (
        "icons/window-minimize.svg",
        include_bytes!("../assets/material/window-minimize.svg"),
    ),
    (
        "icons/window-restore.svg",
        include_bytes!("../assets/material/window-restore.svg"),
    ),
    (
        "icons/loader.svg",
        include_bytes!("../assets/material/loader.svg"),
    ),
    (
        "icons/loader-circle.svg",
        include_bytes!("../assets/material/loader-circle.svg"),
    ),
];
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit_assets::Assets.load(path)
    }
    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(
            ICONS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
pub fn task() -> gpui_component::Icon {
    gpui_component::Icon::default().path("icons/task.svg")
}
pub fn refresh() -> gpui_component::Icon {
    gpui_component::Icon::default().path("icons/refresh.svg")
}
