//! Product identity and build provenance, kept separate from operational settings.
use super::*;
use crate::settings_ui::tile_button;
use crate::theme::*;
use gpui_component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};

/// The licenses the app ships under and where each text lives.
const LICENSES: [(&str, &str); 3] = [
    (
        "course2md · MIT",
        "https://github.com/mizorewww/course2md/blob/main/LICENSE",
    ),
    (
        "Lucide · ISC",
        "https://github.com/lucide-icons/lucide/blob/main/LICENSE",
    ),
    (
        "Material Icons · Apache 2.0",
        "https://github.com/google/material-design-icons/blob/master/LICENSE",
    ),
];

impl Desktop {
    /// The identity tile: app icon, name, version and build.
    pub fn about_page(&self) -> AnyElement {
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
            .gap(px(16.))
            .p(px(20.))
            .child(img("images/course2md.png").size(px(56.)).flex_shrink_0())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(2. / 14.))
                    .child(
                        accessible_text("about-name", "course2md")
                            .role(Role::Heading)
                            .text_size(rems(20. / 14.))
                            .line_height(rems(28. / 14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color(INK)),
                    )
                    .child(tile_support("about-build", version)),
            )
            .into_any_element()
    }

    /// A link tile of the 关于 group (项目主页, 发送反馈).
    pub fn about_link(
        &self,
        id: &'static str,
        icon: Icon,
        label: &'static str,
        url: &'static str,
        outer: [bool; 4],
        cx: &App,
    ) -> AnyElement {
        tile_button(id, outer, cx)
            .gap(rems(1.))
            .accessibility_label(label)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(rems(1.))
                    .child(icon.size(rems(20. / 14.)).text_color(color(GRAY)))
                    .child(tile_label(SharedString::from(format!("{id}-label")), label)),
            )
            .on_click(move |_, _, cx| cx.open_url(url))
            .into_any_element()
    }

    /// 开源许可: the licenses the app ships under; the row opens a menu of
    /// their texts.
    pub fn about_licenses(&self, outer: [bool; 4], cx: &App) -> AnyElement {
        let summary = LICENSES
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(course2md::tr!("、"));
        tile_button("about-license", outer, cx)
            .min_h(rems(76. / 14.))
            .accessibility_label(course2md::tr!("开源许可"))
            .child(tile_text(
                "about-license-title",
                course2md::tr!("开源许可"),
                Some(summary.into()),
            ))
            .child(tile_chevron())
            .dropdown_menu_with_anchor(
                Anchor::TopRight,
                |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    LICENSES.into_iter().fold(menu, |menu, (name, url)| {
                        menu.item(
                            PopupMenuItem::new(name)
                                .icon(icons::external_link())
                                .on_click(move |_, _, cx| cx.open_url(url)),
                        )
                    })
                },
            )
            .into_any_element()
    }
}
