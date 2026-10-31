//! Product identity and build provenance, kept separate from operational settings.
//! The worked example of a page on the kit: see docs/split/MIGRATION.md.
use crate::{symbols, ui};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*};

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

/// The identity row: app icon, name, version and build.
fn identity() -> Div {
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

/// The rows of the 关于 group, in reading order, with the page's own `last` row.
pub fn fields(last: ui::Field) -> Vec<ui::Field> {
    let summary = LICENSES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(course2md::tr!("、"));
    let licenses = LICENSES.into_iter().map(|(name, url)| {
        MaterialMenuItem::new(name)
            .leading(Icon::from(symbols::open_in_new()))
            .on_click(move |_, _, cx| cx.open_url(url))
    });
    vec![
        ui::Field::content("about-identity", identity()),
        ui::Field::link(
            "about-project",
            course2md::tr!("项目主页"),
            |_, _, cx| cx.open_url("https://github.com/mizorewww/course2md"),
        )
        .icon(symbols::open_in_new()),
        ui::Field::link("about-issue", course2md::tr!("发送反馈"), |_, _, cx| {
            cx.open_url("https://github.com/mizorewww/course2md/issues")
        })
        .icon(symbols::chat_bubble()),
        ui::Field::menu(
            "about-license",
            course2md::tr!("开源许可"),
            summary,
            MaterialMenu::new("about-license-items")
                .width(320.)
                .items(licenses),
        ),
        last,
    ]
}
