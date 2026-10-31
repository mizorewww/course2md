//! Layout helpers on the old theme that only `onboarding.rs`, `storage_ui.rs` and
//! `model_diagnostics.rs` still call. Settings itself is on the kit; delete this file
//! when those pages have moved.
use crate::theme::*;
use gpui::{prelude::*, *};
use gpui_kit::component::*;

/// A settings section: a titled group of tiles.
pub(crate) type SettingsGroup = TileGroup;

pub(crate) fn field_label(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, value)
        .text_size(TEXT_SETTINGS_BODY)
        .font_weight(FontWeight::SEMIBOLD)
}

/// Status rows inside an existing card: label on the shared axis, value trailing.
/// Do not wrap these in `settings_row`; that would give each item its own surface.
pub(crate) fn settings_status_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl IntoElement,
) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(CONTROL_HEIGHT)
        .items_center()
        .gap_4()
        .child(field_label(id, label).flex_1().min_w_0().max_w_full())
        .child(
            h_flex()
                .flex_shrink_0()
                .items_center()
                .justify_end()
                .child(value),
        )
}

/// One setting's content box inside a settings card; the card owns the surface.
pub(crate) fn setting_surface() -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .px(SETTING_ITEM_INSET)
        .py(rems(16. / 14.))
        .gap_2()
}

/// Diagnostic facts as on the canvas: an accent heading over one inset block of
/// label/value rows separated by hairlines. `after` holds actions that follow
/// the block rather than belong to a row.
#[derive(gpui::IntoElement)]
pub(crate) struct DetailGroup {
    id: ElementId,
    icon: Icon,
    title: &'static str,
    rows: Vec<AnyElement>,
    after: Vec<AnyElement>,
}

pub(crate) fn settings_detail_group(
    id: impl Into<ElementId>,
    icon: Icon,
    title: &'static str,
) -> DetailGroup {
    DetailGroup {
        id: id.into(),
        icon,
        title,
        rows: Vec::new(),
        after: Vec::new(),
    }
}

impl DetailGroup {
    /// One fact: a quiet label on a common ruler and its value.
    pub(crate) fn row(
        mut self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        value: impl IntoElement,
    ) -> Self {
        self.rows.push(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_wrap()
                .items_center()
                .gap_x_4()
                .gap_y_1()
                .child(
                    accessible_text(id, label)
                        .flex_basis(rems(180. / 14.))
                        .flex_shrink_0()
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(GRAY)),
                )
                .child(
                    div()
                        .flex_1()
                        .flex_basis(rems(200. / 14.))
                        .min_w_0()
                        .text_size(TEXT_SETTINGS_AUX)
                        .child(value),
                )
                .into_any_element(),
        );
        self
    }

    pub(crate) fn after(mut self, element: impl IntoElement) -> Self {
        self.after.push(element.into_any_element());
        self
    }
}

impl ParentElement for DetailGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.rows.extend(elements);
    }
}

impl RenderOnce for DetailGroup {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        // Label/value rows separated by hairlines, as on the canvas.
        let mut block = v_flex().w_full().min_w_0();
        for (index, row) in self.rows.into_iter().enumerate() {
            block = block.child(
                div()
                    .w_full()
                    .min_w_0()
                    .min_h(rems(48. / 14.))
                    .py(rems(8. / 14.))
                    .flex()
                    .items_center()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(color(HAIRLINE))
                    })
                    .child(row),
            );
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(section_heading(self.id, self.icon, self.title))
            .child(block)
            .children(self.after)
    }
}

/// The leading icon circle of a resource row (an account, a service, a
/// library location). It, the gap after it and the rows' insets are all in
/// rems, so the text ruler holds at every text scale.
pub(crate) const RESOURCE_ICON: Rems = rems(48. / 14.);
/// Between a resource's icon and its text.
pub(crate) const RESOURCE_GAP: Rems = rems(16. / 14.);

/// A resource's tinted icon circle.
pub(crate) fn resource_icon(icon: Icon, tint: Tint) -> Div {
    tinted_icon(icon, tint, RESOURCE_ICON)
}

/// The plus of an 「添加…」 tile, in the resource rows' icon column.
pub(crate) fn add_slot() -> Div {
    h_flex()
        .size(RESOURCE_ICON)
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .text_color(color(INK))
        .child(crate::icons::plus().size(rems(24. / 14.)))
}

/// How far a resource row's ⋯ button reaches past the row's trailing inset:
/// its round target ends 12px from the tile's edge, as on the canvas.
const RESOURCE_MENU_OUTSET: f32 = (20. - 12.) / 14.;

/// The ⋯ button at the end of a resource row (an account, a saved service,
/// a library location). Callers attach its label, tooltip and menu.
pub(crate) fn resource_menu(
    id: impl Into<ElementId>,
    cx: &App,
) -> gpui_kit::component::button::Button {
    icon_button(id, CONTROL_HEIGHT, false, cx)
        .mr(rems(-RESOURCE_MENU_OUTSET))
        .icon(crate::icons::more_vertical())
}

/// Diagnostic values begin on a common reading axis. They can wrap and grow
/// vertically without moving short values to the far edge of the window.
pub(crate) fn settings_detail_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    value: impl IntoElement,
) -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(rems(28. / 14.))
        .items_start()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .child(
            field_label(id, label)
                .min_w(rems(96. / 14.))
                .max_w_full()
                .flex_shrink_0()
                .whitespace_normal(),
        )
        .child(
            v_flex()
                .flex_1()
                .flex_basis(rems(240. / 14.))
                .min_w_0()
                .items_start()
                .child(value),
        )
}

pub(crate) fn settings_value(
    id: impl Into<ElementId>,
    value: impl Into<SharedString>,
) -> Stateful<Div> {
    accessible_text(id, value)
        .w_full()
        .min_w_0()
        .whitespace_normal()
        .text_size(TEXT_SETTINGS_BODY)
        .text_color(color(INK))
}
