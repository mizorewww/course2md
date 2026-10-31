//! The compositions pages need that `gpui_kit_material` has no component for, and
//! nothing else. Look in the kit and its gallery first; `docs/split/MIGRATION.md` maps
//! every old helper to its replacement. Nothing here may merely rename a kit component.
use crate::symbols;
use gpui::{prelude::*, *};
use gpui_base::{active_focus_trap, h_flex, v_flex};
use gpui_kit_material::{color::ColorRole, icons::Icon, prelude::*, tokens::shape::OuterCorners};
use std::{rc::Rc, sync::Arc};

fn child_id(id: &ElementId, name: impl Into<SharedString>) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), name.into())
}

/// One status line: `parts` separated by " · ", each a flex child, so a line breaks
/// between parts and never starts with a separator, and a part longer than the
/// line wraps inside itself. The kit has no text run with these break opportunities;
/// the text's size and color come from the caller.
///
/// ```ignore
/// ui::status_line("task-stage", "360.0 MB / 1.56 GB · 速度 12 MB/s")
///     .text_size(TEXT_AUX)
///     .text_color(color(ColorRole::ON_SURFACE_VARIANT))
/// ```
pub fn status_line(id: impl Into<ElementId>, text: &str) -> Div {
    let id = id.into();
    let mut parts = text.split(" · ").peekable();
    let mut line = div()
        .flex()
        .flex_wrap()
        .min_w_0()
        .max_w_full()
        .gap_x(rem(4.));
    let mut index = 0usize;
    while let Some(part) = parts.next() {
        let part: SharedString = if parts.peek().is_some() {
            format!("{part} ·").into()
        } else {
            part.to_owned().into()
        };
        line = line.child(
            accessible_text(child_id(&id, index.to_string()), part)
                .min_w_0()
                .max_w_full()
                .whitespace_normal(),
        );
        index += 1;
    }
    line
}

/// Reports an open menu or dropdown's state from `window.use_keyed_state`: the kit's
/// `MenuPopup` and `ExposedDropdown` are controlled, and these sites keep nothing else.
pub fn keyed_open(
    id: &ElementId,
    window: &mut Window,
    cx: &mut App,
) -> (bool, impl Fn(&bool, &mut Window, &mut App) + 'static) {
    let state = window.use_keyed_state(child_id(id, "open"), cx, |_, _| false);
    let open = *state.read(cx);
    (open, move |next: &bool, _: &mut Window, cx: &mut App| {
        state.update(cx, |state, cx| {
            *state = *next;
            cx.notify();
        })
    })
}

/// A `MenuPopup` that remembers its own open state.
///
/// ```ignore
/// ui::menu("note-menu", IconButton::new("note-more", symbols::more_vert(), label), {
///     MaterialMenu::new("note-menu-items").item(
///         MaterialMenuItem::new(course2md::tr!("重命名"))
///             .leading(Icon::from(symbols::edit()))
///             .on_click(cx.listener(|this, _, window, cx| this.rename(window, cx))),
///     )
/// }, window, cx)
/// ```
pub fn menu(
    id: impl Into<ElementId>,
    trigger: impl IntoElement,
    menu: MaterialMenu,
    window: &mut Window,
    cx: &mut App,
) -> MenuPopup {
    let id = id.into();
    let (open, set_open) = keyed_open(&id, window, cx);
    MenuPopup::new(id, trigger, menu)
        .open(open)
        .on_open_change(set_open)
}

/// A menu trigger's click handler: a pointer press already reached the popup, a key
/// did not. `id` is the one given to [`menu`].
///
/// ```ignore
/// IconButton::new("note-more", symbols::more_vert(), label)
///     .on_click(ui::menu_key_toggle(&"note-menu".into(), window, cx))
/// ```
pub fn menu_key_toggle(
    id: &ElementId,
    window: &mut Window,
    cx: &mut App,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let (open, set_open) = keyed_open(id, window, cx);
    move |event, window, cx| {
        if event.is_keyboard() {
            set_open(&!open, window, cx);
        }
    }
}

/// The items of a menu that picks one value, the current one checked.
///
/// ```ignore
/// let items = ui::choice_items(
///     [(Mode::Fast, course2md::tr!("快速")), (Mode::Full, course2md::tr!("完整"))],
///     &self.mode,
///     cx.listener(|this, mode: &Mode, _, cx| this.set_mode(*mode, cx)),
/// );
/// ```
pub fn choice_items<T: PartialEq + 'static>(
    options: impl IntoIterator<Item = (T, impl Into<SharedString>)>,
    current: &T,
    on_pick: impl Fn(&T, &mut Window, &mut App) + 'static,
) -> impl Iterator<Item = MaterialMenuItem> {
    let on_pick = Rc::new(on_pick);
    let items: Vec<_> = options
        .into_iter()
        .map(|(value, label)| {
            let on_pick = on_pick.clone();
            MaterialMenuItem::new(label)
                .selected(value == *current)
                .on_click(move |_, window, cx| on_pick(&value, window, cx))
        })
        .collect();
    items.into_iter()
}

type SwitchChange = Rc<dyn Fn(&bool, &mut Window, &mut App)>;
type RowClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

enum Control {
    Switch(bool, SwitchChange),
    /// A row whose value opens this menu.
    Menu(SharedString, MaterialMenu),
    /// A control at the row's trailing edge; it moves under the label when both don't fit.
    Inline(AnyElement),
    /// A wide control under the label: a button group, a text field.
    Below(AnyElement),
    /// A row that goes somewhere or adds something.
    Link(RowClick),
    /// A row that shows or hides its detail in place.
    Disclosure(bool, RowClick, Div),
    /// Content with the row's inset and no label of its own.
    Content(AnyElement),
    /// A row that draws its own surface, given its corners.
    Tile(Box<dyn FnOnce(OuterCorners) -> AnyElement>),
}

/// One row of a [`field_table`].
pub struct Field {
    id: SharedString,
    label: SharedString,
    hint: Option<SharedString>,
    icon: Option<Icon>,
    disabled: bool,
    control: Control,
}

impl Field {
    fn new(id: impl Into<SharedString>, label: impl Into<SharedString>, control: Control) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            hint: None,
            icon: None,
            disabled: false,
            control,
        }
    }

    pub fn switch(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        on: bool,
        on_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self::new(id, label, Control::Switch(on, Rc::new(on_change)))
    }

    pub fn menu(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
        menu: MaterialMenu,
    ) -> Self {
        Self::new(id, label, Control::Menu(value.into(), menu))
    }

    pub fn inline(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> Self {
        Self::new(id, label, Control::Inline(control.into_any_element()))
    }

    pub fn below(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        control: impl IntoElement,
    ) -> Self {
        Self::new(id, label, Control::Below(control.into_any_element()))
    }

    pub fn link(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self::new(id, label, Control::Link(Box::new(on_click)))
    }

    /// `detail` stays in the tree while closed, so closing can play.
    pub fn disclosure(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        open: bool,
        on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        detail: Div,
    ) -> Self {
        Self::new(
            id,
            label,
            Control::Disclosure(open, Box::new(on_toggle), detail),
        )
    }

    pub fn content(id: impl Into<SharedString>, content: impl IntoElement) -> Self {
        Self::new(id, "", Control::Content(content.into_any_element()))
    }

    pub fn tile(
        id: impl Into<SharedString>,
        build: impl FnOnce(OuterCorners) -> AnyElement + 'static,
    ) -> Self {
        Self::new(id, "", Control::Tile(Box::new(build)))
    }

    /// A leading icon for a menu or link row.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Supporting text under the label; empty means none.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        let hint = hint.into();
        self.hint = (!hint.is_empty()).then_some(hint);
        self
    }

    /// Dims a switch or menu row. An inline or below control disables itself.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Text and a control on one line while both fit; flex wrap moves the control below.
    fn inline_row(outer: OuterCorners, text: Div, control: impl IntoElement) -> AnyElement {
        let scale = TileScale::Regular;
        scale
            .surface(outer)
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .min_w_0()
                    .gap(scale.gap())
                    .px(ITEM_INSET)
                    .py(rem(12.))
                    .child(text.flex_basis(rem(200.)).min_h(rem(40.)).justify_center())
                    .child(div().max_w_full().child(control)),
            )
            .into_any_element()
    }

    fn render(self, outer: OuterCorners, window: &mut Window, cx: &mut App) -> AnyElement {
        let scale = TileScale::Regular;
        let Self {
            id,
            label,
            hint,
            icon,
            disabled,
            control,
        } = self;
        let text_id = ElementId::Name(format!("{id}-label").into());
        match control {
            Control::Switch(on, on_change) => Self::inline_row(
                outer,
                scale.text(text_id, label.clone(), hint),
                MaterialSwitch::new(ElementId::Name(id), label)
                    .checked(on)
                    .disabled(disabled)
                    .on_change(move |on, window, cx| on_change(on, window, cx)),
            ),
            Control::Menu(value, items) => {
                let value = Some(match hint {
                    Some(hint) => format!("{value} · {hint}").into(),
                    None => value,
                });
                let popup = ElementId::Name(format!("{id}-menu").into());
                let trigger = scale
                    .button(ElementId::Name(id.clone()), outer)
                    .accessibility_label(label.clone())
                    .disabled(disabled)
                    .on_click(menu_key_toggle(&popup, window, cx))
                    .child(
                        scale
                            .row()
                            .children(icon)
                            .child(scale.text(text_id, label, value))
                            .child(tile_chevron()),
                    );
                menu(popup, trigger, items, window, cx)
                    .anchor(Anchor::TopRight)
                    .into_any_element()
            }
            Control::Inline(control) => {
                Self::inline_row(outer, scale.text(text_id, label, hint), control)
            }
            Control::Below(control) => scale
                .surface(outer)
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap(rem(12.))
                        .px(ITEM_INSET)
                        .py(rem(16.))
                        .child(scale.text(text_id, label, hint))
                        .child(control),
                )
                .into_any_element(),
            Control::Link(on_click) => scale
                .button(ElementId::Name(id), outer)
                .accessibility_label(label.clone())
                .disabled(disabled)
                .on_click(on_click)
                .child(
                    scale
                        .row()
                        .children(icon)
                        .child(scale.text(text_id, label, hint))
                        .child(tile_chevron()),
                )
                .into_any_element(),
            Control::Disclosure(open, on_toggle, detail) => {
                let detail_id = ElementId::Name(format!("{id}-detail").into());
                let button_corners = [outer[0], outer[1], outer[2] && !open, outer[3] && !open];
                scale
                    .surface(outer)
                    .child(
                        scale
                            .button(ElementId::Name(id), button_corners)
                            .accessibility_label(label.clone())
                            .on_click(on_toggle)
                            .child(
                                scale
                                    .row()
                                    .child(scale.text(text_id, label, hint))
                                    .child(disclosure_chevron(open)),
                            ),
                    )
                    .child(gpui_kit_material::motion::disclosure(
                        detail_id,
                        open,
                        detail.w_full().min_w_0().px(ITEM_INSET).pb(rem(16.)),
                    ))
                    .into_any_element()
            }
            Control::Content(content) => scale
                .surface(outer)
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .px(ITEM_INSET)
                        .py(rem(16.))
                        .child(content),
                )
                .into_any_element(),
            Control::Tile(build) => build(outer),
        }
    }
}

/// A section of settings rows: its title and one connected tile per [`Field`]. Rows
/// mix controls, and `ListTrailing` holds no custom element, so a `segmented_list`
/// alone cannot express this; the rows' ruler stays here instead of in each page.
///
/// ```ignore
/// ui::field_table("note-settings", course2md::tr!("笔记"), [
///     ui::Field::switch("summary", course2md::tr!("生成摘要"), on, change).hint(hint),
///     ui::Field::menu("language", course2md::tr!("语言"), current, language_menu),
///     ui::Field::below("mode", course2md::tr!("模式"), mode_group),
/// ])
/// ```
pub fn field_table(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    fields: impl IntoIterator<Item = Field>,
) -> FieldTable {
    FieldTable {
        id: id.into(),
        title: title.into(),
        fields: fields.into_iter().collect(),
        reveal: None,
    }
}

#[derive(gpui::IntoElement)]
pub struct FieldTable {
    id: SharedString,
    title: SharedString,
    fields: Vec<Field>,
    reveal: Option<ScrollHandle>,
}

impl FieldTable {
    /// A row taking keyboard focus scrolls into view in `scroll`.
    pub fn reveal_in(mut self, scroll: ScrollHandle) -> Self {
        self.reveal = Some(scroll);
        self
    }
}

impl RenderOnce for FieldTable {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The group hands each tile its corners only when it lays them out, and
        // a row's `render` needs the window: keep the rows' corners here.
        let count = self.fields.len();
        let mut group = TileGroup::new(self.id, self.title);
        for (index, field) in self.fields.into_iter().enumerate() {
            let reveal_id = ElementId::Name(format!("{}-reveal", field.id).into());
            let row = field.render(list_corners(index, count), window, cx);
            let row = match self.reveal.clone() {
                Some(scroll) => RevealFocus::new(reveal_id, row, scroll).into_any_element(),
                None => row,
            };
            group = group.tile(move |_| row);
        }
        group
    }
}

/// What background work is doing, for [`async_status`]. The state machine stays
/// with its page; only the rendering of its states is shared.
pub enum AsyncStatus {
    Idle,
    Busy(SharedString),
    Success(SharedString),
    Warning(SharedString),
    Failed(SharedString),
}

/// The one way a check, probe or load reports itself: a status icon or spinner in the
/// label's icon slot, the message, and `retry` once work is not running. Nothing is
/// drawn while idle. `status_chip` labels a thing; this narrates an operation.
///
/// ```ignore
/// ui::async_status("ollama-status", &status, Some(
///     material_button("ollama-retry", ButtonVariant::Text)
///         .icon(symbols::refresh())
///         .child(course2md::tr!("重试"))
///         .on_click(cx.listener(|this, _, _, cx| this.ollama_refresh(cx)))
///         .into_any_element(),
/// ), cx)
/// ```
pub fn async_status(
    id: impl Into<ElementId>,
    status: &AsyncStatus,
    retry: Option<AnyElement>,
    cx: &App,
) -> Option<Div> {
    let id = id.into();
    let icon = |icon: gpui_kit_material::icons::SymbolIcon, role| {
        Icon::from(icon)
            .size(ICON_SLOT)
            .color(color(role))
            .into_any_element()
    };
    let text_id = child_id(&id, "text");
    let (slot, text, retry) = match status {
        AsyncStatus::Idle => return None,
        AsyncStatus::Busy(message) => (
            icon_spinner(child_id(&id, "spinner"), message.clone(), ICON_SLOT, cx),
            accessible_text(text_id, message.clone()).role(Role::Status),
            None,
        ),
        AsyncStatus::Success(message) => (
            icon(symbols::check_circle(), ColorRole::SUCCESS),
            accessible_text(text_id, message.clone()).role(Role::Status),
            retry,
        ),
        AsyncStatus::Warning(message) => (
            icon(symbols::warning(), ColorRole::WARNING),
            accessible_text(text_id, message.clone()).role(Role::Status),
            retry,
        ),
        AsyncStatus::Failed(message) => (
            icon(symbols::error(), ColorRole::ERROR),
            error_text(text_id, message.clone()),
            retry,
        ),
    };
    Some(
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap_x(ICON_GAP)
            .gap_y(rem(4.))
            .child(slot)
            .child(
                text.flex_1()
                    .flex_basis(rem(160.))
                    .min_w_0()
                    .whitespace_normal(),
            )
            .children(retry),
    )
}

/// A question before a consequential action. Cancel is named too: a verb that says
/// what stays ("保留笔记") reads better than a generic one.
pub struct Confirm {
    title: SharedString,
    cancel: SharedString,
    confirm: SharedString,
    danger: bool,
    text: Vec<String>,
}

impl Confirm {
    pub fn new(
        title: impl Into<SharedString>,
        cancel: impl Into<SharedString>,
        confirm: impl Into<SharedString>,
    ) -> Self {
        Self {
            title: title.into(),
            cancel: cancel.into(),
            confirm: confirm.into(),
            danger: false,
            text: Vec::new(),
        }
    }

    /// Paints the confirming action in the error role, with a trash icon.
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// One paragraph of the consequence.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text.push(text.into());
        self
    }

    /// Asks. `on_confirm` runs if the person accepts; dismissing does nothing.
    /// The dialog itself is drawn by the shell through [`confirm_host`].
    ///
    /// ```ignore
    /// ui::Confirm::new(course2md::tr!("删除笔记"), course2md::tr!("保留笔记"), label)
    ///     .danger()
    ///     .text(consequence)
    ///     .ask(cx.listener(|this, _, window, cx| this.delete_note(window, cx)), window, cx);
    /// ```
    pub fn ask(
        self,
        on_confirm: impl Fn(&(), &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut App,
    ) {
        cx.set_global(ConfirmSlot(Some((self, Rc::new(on_confirm))), true));
        window.refresh();
    }
}

type OnConfirm = Rc<dyn Fn(&(), &mut Window, &mut App)>;

/// The question being asked. It outlives the answer, so the dialog can play its exit.
#[derive(Default)]
struct ConfirmSlot(Option<(Confirm, OnConfirm)>, bool);
impl Global for ConfirmSlot {}

fn close_confirm(window: &mut Window, cx: &mut App) {
    cx.default_global::<ConfirmSlot>().1 = false;
    window.refresh();
}

/// Draws the current [`Confirm`]. Rendered once, by the shell.
pub fn confirm_host(cx: &App) -> Option<AnyElement> {
    let ConfirmSlot(Some((confirm, on_confirm)), open) = cx.try_global::<ConfirmSlot>()? else {
        return None;
    };
    let on_confirm = on_confirm.clone();
    let accept = material_button("confirm-accept", ButtonVariant::Filled)
        .icon(if confirm.danger {
            symbols::delete()
        } else {
            symbols::check()
        })
        .child(confirm.confirm.clone())
        .when(confirm.danger, |button| {
            button
                .bg(color(ColorRole::ERROR))
                .text_color(color(ColorRole::ON_ERROR))
        })
        .on_click(move |_, window, cx| {
            close_confirm(window, cx);
            on_confirm(&(), window, cx);
        });
    Some(
        MaterialDialog::new("confirm")
            .headline(confirm.title.clone())
            .when(!confirm.text.is_empty(), |dialog| {
                dialog.supporting_text(confirm.text.join("\n"))
            })
            .action(
                material_button("confirm-cancel", ButtonVariant::Text)
                    .child(confirm.cancel.clone())
                    .on_click(|_, window, cx| close_confirm(window, cx)),
            )
            .action(accept)
            .modal(*open)
            .on_dismiss(close_confirm)
            .into_any_element(),
    )
}

/// A download or preparation: its title, what is known so far as one status line,
/// and a bar once the total is known. The kit's progress has no such label block.
pub fn transfer_status(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    metrics: &crate::activity::TransferMetrics,
    fraction: Option<f32>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let id = id.into();
    v_flex()
        .w_full()
        .min_w_0()
        .gap(rem(8.))
        .child(
            accessible_text(child_id(&id, "title"), title)
                .text_size(TEXT_BODY)
                .font_weight(FontWeight::MEDIUM)
                .text_color(color(ColorRole::ON_SURFACE)),
        )
        .child(
            status_line(child_id(&id, "detail"), &metrics.summary())
                .text_size(TEXT_AUX)
                .text_color(color(ColorRole::ON_SURFACE_VARIANT)),
        )
        .when_some(fraction, |view, fraction| {
            view.child(progress(child_id(&id, "bar"), fraction, window, cx))
        })
}

/// Whether a dialog holds the window, Kit's or the kit's: app-wide shortcuts wait.
pub fn modal_open(window: &mut Window, cx: &mut App) -> bool {
    use gpui_kit::component::WindowExt as _;
    window.has_active_dialog(cx) || active_focus_trap(window, cx).is_some()
}

#[cfg(test)]
mod tests {
    use super::{
        AsyncStatus, Confirm, Field, async_status, confirm_host, field_table, status_line,
    };
    use gpui::{
        Context, InteractiveElement, IntoElement, ParentElement, Pixels, Render, Styled,
        TestAppContext, VisualTestContext, Window, div, px,
    };
    use gpui_kit_material::prelude::MaterialMenu;

    struct Line(Pixels);

    impl Render for Line {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(
                div()
                    .debug_selector(|| "wrapper".into())
                    .w(self.0)
                    .child(status_line(
                        "line",
                        "360.0 MB / 1.56 GB · 速度 12 MB/s · 预计剩余 1 分 50 秒",
                    )),
            )
        }
    }

    /// Parts wrap as whole children, so a line never starts with a separator.
    #[gpui::test]
    fn a_status_line_breaks_between_its_parts(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| Line(px(1000.)));
        let height = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            let bounds = cx.debug_bounds("wrapper").expect("the line");
            bounds.size.height
        };
        let wide = height(cx);
        view.update_in(cx, |view, _, cx| {
            view.0 = px(120.);
            cx.notify();
        });
        let narrow = height(cx);
        assert!(narrow >= wide * 3., "{wide:?} then {narrow:?}");
    }

    struct Rows;

    impl Render for Rows {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(600.))
                .child(field_table(
                    "section",
                    "Section",
                    [
                        Field::switch("switch", "Switch", true, |_, _, _| {}).hint("Hint"),
                        Field::menu("menu", "Menu", "Value", MaterialMenu::new("items")),
                        Field::inline("inline", "Inline", div().child("control")),
                        Field::below("below", "Below", div().child("control")),
                    ],
                ))
                .children(async_status("idle", &AsyncStatus::Idle, None, cx))
                .children(async_status(
                    "failed",
                    &AsyncStatus::Failed("No".into()),
                    None,
                    cx,
                ))
                .children(confirm_host(cx))
        }
    }

    /// Every primitive draws, and a question shows until it is answered.
    #[gpui::test]
    fn the_primitives_draw_and_a_confirmation_appears(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            gpui_kit_material::init(cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| Rows);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(confirm_host(cx).is_none());
            Confirm::new("Delete", "Keep", "Delete")
                .danger()
                .text("Consequence")
                .ask(|_, _, _| {}, window, cx);
            assert!(confirm_host(cx).is_some());
            window.draw(cx).clear(cx);
        });
    }
}
