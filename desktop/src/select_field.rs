//! The one select trigger: an outlined field that opens a shared popup menu.
//! Inline rows show a single value; forms add a floating label, an icon tile
//! and a second line. While open the outline turns accent and the chevron flips.
use super::*;
use gpui::{prelude::*, *};
use gpui_kit::component::{
    Disableable as _, ElementExt as _, Icon, StyledExt as _,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    h_flex,
    menu::{DropdownMenu, PopupMenu},
    v_flex,
};
use std::rc::Rc;

/// One menu's contents, shared by a ⋯ button and a right-click on the same item.
pub type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// A menu is never narrower than this, even under a narrow trigger.
const MENU_MIN_WIDTH: Pixels = px(200.);

#[derive(gpui::IntoElement)]
pub struct SelectField {
    id: ElementId,
    value: SharedString,
    detail: Option<SharedString>,
    trailing: Option<SharedString>,
    label: Option<SharedString>,
    icon: Option<Icon>,
    disabled: bool,
    menu: MenuBuilder,
    style: StyleRefinement,
}

#[derive(Clone, Copy, Default)]
struct OpenState {
    open: bool,
    width: Pixels,
}

impl SelectField {
    pub fn new(
        id: impl Into<ElementId>,
        value: impl Into<SharedString>,
        menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.into(),
            detail: None,
            trailing: None,
            label: None,
            icon: None,
            disabled: false,
            menu: Rc::new(menu),
            style: StyleRefinement::default(),
        }
    }
    /// A second, quieter line under the value.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
    /// A short qualifier before the chevron, e.g. the resolved language.
    pub fn trailing(mut self, trailing: impl Into<SharedString>) -> Self {
        self.trailing = Some(trailing.into());
        self
    }
    /// A floating outlined-field label; also the field's accessible name.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    /// A leading icon tile for values that name a kind of thing.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Styled for SelectField {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for SelectField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(
            ElementId::from(SharedString::from(format!("select-field:{:?}", self.id))),
            cx,
            |_, _| OpenState::default(),
        );
        let OpenState { open, .. } = *state.read(cx);
        let open = open && !self.disabled;
        let ink = color(INK);
        let faded = |alpha| Rgba { a: alpha, ..ink };
        let (outline, chevron) = if self.disabled {
            (faded(0.12), faded(0.38))
        } else if open {
            (color(ACCENT), color(ACCENT))
        } else {
            (color(CONTROL), color(GRAY))
        };
        let has_icon = self.icon.is_some();
        let has_detail = self.detail.is_some();
        let tall = self.label.is_some() || self.detail.is_some() || self.icon.is_some();
        // A 2px open outline must not move the content: trade the extra pixel
        // for one pixel of padding.
        let border = if open { px(2.) } else { px(1.) };
        let rem = window.rem_size();
        let inset = |value: f32| rem * (value / 14.) - border;
        let name: SharedString = match &self.label {
            Some(label) => course2md::tr!("{}：{}", label, self.value).into(),
            None => self.value.clone(),
        };
        let value_color = if self.disabled { faded(0.38) } else { ink };
        let content = h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(rems(12. / 14.))
            .font_weight(FontWeight::NORMAL)
            .children(self.icon.map(|icon| {
                h_flex()
                    .flex_shrink_0()
                    .size(rems(32. / 14.))
                    .items_center()
                    .justify_center()
                    .rounded(px(10.))
                    .bg(color(SURFACE_HIGH))
                    .text_color(color(GRAY))
                    .child(icon.size(rems(18. / 14.)))
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .items_start()
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(if tall {
                                TEXT_SETTINGS_BODY
                            } else {
                                rems(15. / 14.)
                            })
                            .line_height(rems(22. / 14.))
                            .text_color(value_color)
                            .child(self.value.clone()),
                    )
                    .children(self.detail.map(|detail| {
                        div()
                            .w_full()
                            .truncate()
                            .text_size(TEXT_AUX)
                            .line_height(rems(16. / 14.))
                            .text_color(if self.disabled {
                                faded(0.38)
                            } else {
                                color(GRAY)
                            })
                            .child(detail)
                    })),
            )
            .children(self.trailing.map(|trailing| {
                div()
                    .flex_shrink_0()
                    .text_size(rems(13. / 14.))
                    .text_color(if self.disabled {
                        faded(0.38)
                    } else {
                        color(GRAY)
                    })
                    .child(trailing)
            }))
            .child(
                if open {
                    crate::icons::chevron_up()
                } else {
                    crate::icons::chevron_down()
                }
                .size(rems(20. / 14.))
                .flex_shrink_0()
                .text_color(chevron),
            );
        let menu = self.menu.clone();
        let measured = state.clone();
        let tracked = state.clone();
        let trigger = Button::new(self.id.clone())
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(faded(0.).into())
                    .foreground(ink.into())
                    .hover(faded(0.08).into())
                    // Open is shown by the outline; a fill would hide the icon tile.
                    .active(faded(0.).into())
                    .shadow(false),
            )
            .w_full()
            .h_auto()
            .min_h(rems(
                match (has_detail, tall) {
                    (true, _) => 60.,
                    (false, true) => 56.,
                    (false, false) => 48.,
                } / 14.,
            ))
            .py(if tall { inset(8.) } else { inset(0.) })
            .pl(inset(if has_icon { 12. } else { 16. }))
            .pr(inset(12.))
            .rounded(FIELD_RADIUS)
            .border(border)
            .border_color(outline)
            .justify_start()
            .disabled(self.disabled)
            .accessibility_label(name)
            .child(content)
            .dropdown_menu(move |popup, window, cx| {
                let width = measured.read(cx).width.max(MENU_MIN_WIDTH);
                menu(popup.min_w(width), window, cx)
            })
            .on_open_change(move |open, _, cx| {
                tracked.update(cx, |state, cx| {
                    state.open = *open;
                    cx.notify();
                })
            });
        let label_tone = if self.disabled {
            faded(0.38)
        } else if open {
            color(ACCENT)
        } else {
            color(GRAY)
        };
        let field = match self.label {
            Some(label) => outlined_field_label(
                SharedString::from(format!("{:?}-label", self.id)),
                label,
                TEXT_AUX,
                label_tone,
                trigger,
            ),
            None => div().w_full().min_w_0().child(trigger),
        };
        div()
            .w_full()
            .min_w_0()
            .refine_style(&self.style)
            .child(field)
            .on_prepaint(move |bounds, _, cx| {
                state.update(cx, |state, _| state.width = bounds.size.width);
            })
    }
}
