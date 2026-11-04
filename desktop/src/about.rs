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
    // Each license is a document to open, not a value to choose: one link per text,
    // named in full, wrapping as the row narrows (V3).
    let licenses = h_flex()
        .w_full()
        .min_w_0()
        .flex_wrap()
        .gap(rem(8.))
        .children(
            LICENSES
                .into_iter()
                .enumerate()
                .map(|(index, (name, url))| {
                    material_button(("about-license", index), ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
                        .icon(symbols::open_in_new())
                        .child(name)
                        .on_click(move |_, _, cx| cx.open_url(url))
                }),
        );
    vec![
        ui::Field::content("about-identity", identity()),
        ui::Field::tile("about-links", |outer| {
            let scale = TileScale::Regular;
            let link = |id: &'static str,
                        label: &'static str,
                        icon: gpui_kit_material::icons::SymbolIcon,
                        corners,
                        url: &'static str| {
                // The two tiles share the row at every width and text size: they
                // shrink together and a long label wraps inside its tile, so the
                // connected corners stay where they belong.
                scale
                    .button(id, corners)
                    .flex_1()
                    .flex_basis(rems(0.))
                    .min_w_0()
                    .accessibility_label(label)
                    .on_click(move |_, _, cx| cx.open_url(url))
                    .child(
                        scale
                            .row()
                            .min_w_0()
                            .child(
                                Icon::from(icon)
                                    .size(rem(24.))
                                    .color(color(ColorRole::ON_SURFACE_VARIANT)),
                            )
                            .child(
                                scale
                                    .label(SharedString::from(format!("{id}-label")), label)
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal(),
                            ),
                    )
            };
            h_flex()
                .w_full()
                .min_w_0()
                .gap(TILE_GAP)
                .child(link(
                    "about-project",
                    course2md::tr!("项目主页"),
                    symbols::open_in_new(),
                    [outer[0], false, false, outer[3]],
                    "https://github.com/mizorewww/course2md",
                ))
                .child(link(
                    "about-issue",
                    course2md::tr!("发送反馈"),
                    symbols::chat_bubble(),
                    [false, outer[1], outer[2], false],
                    "https://github.com/mizorewww/course2md/issues",
                ))
                .into_any_element()
        }),
        ui::Field::below("about-license", course2md::tr!("开源许可"), licenses),
        last,
    ]
}
