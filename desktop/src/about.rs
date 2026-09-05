//! Product identity and build provenance, kept separate from operational settings.
use super::*;
use crate::theme::{INK, LINE, MUTED};
use gpui_component::button::Button;

impl Desktop {
    pub fn about_page(&self, _: &mut Context<Self>) -> AnyElement {
        let commit = env!("COURSE2MD_DESKTOP_COMMIT");
        v_flex()
            .w_full()
            .max_w(px(760.))
            .gap_6()
            .text_color(rgb(INK))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("course2md"),
                    )
                    .child(div().text_color(rgb(MUTED)).child("把课程整理成笔记。")),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(about_detail("版本", env!("CARGO_PKG_VERSION")))
                    .when(!commit.is_empty(), |view| {
                        view.child(about_detail("构建", commit))
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        Button::new("about-project")
                            .h(px(32.)).min_h(px(32.))
                            .label("项目主页")
                            .icon(IconName::ExternalLink)
                            .on_click(|_, _, cx| {
                                cx.open_url("https://github.com/mizorewww/course2md");
                            }),
                    )
                    .child(
                        Button::new("about-issue")
                            .h(px(32.)).min_h(px(32.))
                            .label("反馈问题")
                            .icon(IconName::ExternalLink)
                            .on_click(|_, _, cx| {
                                cx.open_url("https://github.com/mizorewww/course2md/issues");
                            }),
                    ),
            )
            .child(
                v_flex()
                    .pt_6()
                    .gap_3()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("开源许可"))
                    .child(
                        h_flex()
                            .gap_3()
                            .flex_wrap()
                            .child(
                                Button::new("about-license")
                                    .h(px(32.)).min_h(px(32.))
                                    .label("course2md · MIT")
                            .icon(IconName::ExternalLink)
                                    .on_click(|_, _, cx| {
                                        cx.open_url("https://github.com/mizorewww/course2md/blob/main/LICENSE");
                                    }),
                            )
                            .child(
                                Button::new("about-icons-license")
                                    .h(px(32.)).min_h(px(32.))
                                    .label("Material Icons · Apache 2.0")
                            .icon(IconName::ExternalLink)
                                    .on_click(|_, _, cx| {
                                        cx.open_url("https://github.com/google/material-design-icons/blob/master/LICENSE");
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn about_detail(label: &'static str, value: &'static str) -> Div {
    h_flex()
        .gap_4()
        .child(
            div()
                .w(px(64.))
                .flex_shrink_0()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(div().font_weight(FontWeight::MEDIUM).child(value))
}
