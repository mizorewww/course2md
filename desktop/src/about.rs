//! Product identity and build provenance, kept separate from operational settings.
use super::*;
use crate::settings_ui::settings_chip;
use crate::theme::*;

impl Desktop {
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
            .flex_wrap()
            .gap(px(16.))
            .text_color(color(INK))
            .child(img("images/course2md.png").size(px(56.)).flex_shrink_0())
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rems(200. / 14.))
                    .gap_1()
                    .child(
                        accessible_text("about-name", "course2md")
                            .role(Role::Heading)
                            .text_size(TEXT_TITLE)
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        accessible_text("about-build", version)
                            .text_size(TEXT_SETTINGS_AUX)
                            .text_color(color(GRAY)),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .flex_wrap()
                    .child(
                        quiet("about-project")
                            .settings_text()
                            .text_color(color(ACCENT))
                            .icon(icons::external_link())
                            .label(course2md::tr!("项目主页"))
                            .on_click(|_, _, cx| {
                                cx.open_url("https://github.com/mizorewww/course2md")
                            }),
                    )
                    .child(
                        quiet("about-issue")
                            .settings_text()
                            .text_color(color(ACCENT))
                            .icon(icons::edit())
                            .label(course2md::tr!("发送反馈"))
                            .on_click(|_, _, cx| {
                                cx.open_url("https://github.com/mizorewww/course2md/issues")
                            }),
                    ),
            )
            .into_any_element()
    }

    /// The licenses the app ships under, each opening its text.
    pub fn about_licenses(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_2()
            .flex_wrap()
            .justify_end()
            .child(
                settings_chip(outline_pill("about-license", cx).settings_text())
                    .icon(icons::code())
                    .label("course2md · MIT")
                    .on_click(|_, _, cx| {
                        cx.open_url("https://github.com/mizorewww/course2md/blob/main/LICENSE")
                    }),
            )
            .child(
                settings_chip(outline_pill("about-lucide-license", cx).settings_text())
                    .icon(icons::code())
                    .label("Lucide · ISC")
                    .on_click(|_, _, cx| {
                        cx.open_url("https://github.com/lucide-icons/lucide/blob/main/LICENSE")
                    }),
            )
            .child(
                settings_chip(outline_pill("about-icons-license", cx).settings_text())
                    .icon(icons::code())
                    .label("Material Icons · Apache 2.0")
                    .on_click(|_, _, cx| {
                        cx.open_url(
                            "https://github.com/google/material-design-icons/blob/master/LICENSE",
                        )
                    }),
            )
            .into_any_element()
    }
}
