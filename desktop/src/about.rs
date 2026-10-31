//! Product identity and build provenance, kept separate from operational settings.
//! The worked example of a page on the kit: see docs/split/MIGRATION.md.
use crate::{symbols, ui};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{
    color::ColorRole,
    icons::{Icon, SymbolIcon},
    prelude::*,
    tokens::shape::OuterCorners,
};

/// The licenses the app ships under and where each text lives.
const LICENSES: [(&str, &str); 3] = [
    (
        "course2md · MIT",
        "https://github.com/mizorewww/course2md/blob/main/LICENSE",
    ),
    (
        "Material Symbols · Apache 2.0",
        "https://github.com/google/material-design-icons/blob/master/LICENSE",
    ),
    (
        "Roboto · OFL 1.1",
        "https://github.com/googlefonts/roboto-3-classic/blob/main/OFL.txt",
    ),
];

/// The identity tile: app icon, name, version and build.
pub fn identity() -> Div {
    let commit = env!("COURSE2MD_DESKTOP_COMMIT");
    let version = if commit.is_empty() {
        env!("CARGO_PKG_VERSION").to_owned()
    } else {
        course2md::tr!("{} · 构建 {}", env!("CARGO_PKG_VERSION"), commit)
    };
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(rem(16.))
        .p(ITEM_INSET)
        .child(img("images/course2md.png").size(rem(56.)).flex_shrink_0())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(rem(2.))
                .child(
                    accessible_text("about-name", "course2md")
                        .role(Role::Heading)
                        .text_size(TEXT_TITLE)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color(ColorRole::ON_SURFACE)),
                )
                .child(TileScale::Regular.support("about-build", version)),
        )
}

/// A link tile of the 关于 group (项目主页, 发送反馈).
pub fn link(
    id: &'static str,
    icon: SymbolIcon,
    label: &'static str,
    url: &'static str,
    outer: OuterCorners,
) -> AnyElement {
    let scale = TileScale::Regular;
    scale
        .button(id, outer)
        .accessibility_label(label)
        .child(
            scale
                .row()
                .child(Icon::from(icon).color(color(ColorRole::ON_SURFACE_VARIANT)))
                .child(scale.label(SharedString::from(format!("{id}-label")), label)),
        )
        .on_click(move |_, _, cx| cx.open_url(url))
        .into_any_element()
}

/// 开源许可: the licenses the app ships under; the row opens a menu of their texts.
pub fn licenses(outer: OuterCorners, window: &mut Window, cx: &mut App) -> AnyElement {
    let scale = TileScale::Regular;
    let label = course2md::tr!("开源许可");
    let summary = LICENSES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(course2md::tr!("、"));
    let trigger = scale
        .button("about-license", outer)
        .accessibility_label(label)
        .child(
            scale
                .row()
                .child(scale.text("about-license-title", label, Some(summary.into())))
                .child(tile_chevron()),
        );
    let items = LICENSES.into_iter().map(|(name, url)| {
        MaterialMenuItem::new(name)
            .leading(Icon::from(symbols::open_in_new()))
            .on_click(move |_, _, cx| cx.open_url(url))
    });
    ui::menu(
        "about-license-menu",
        trigger,
        MaterialMenu::new("about-license-items").items(items),
        window,
        cx,
    )
    .anchor(Anchor::TopRight)
    .into_any_element()
}
