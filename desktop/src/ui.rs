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

enum Control {
    /// The whole row toggles, as `ListItem` does with a trailing switch.
    Switch(bool, SwitchChange),
    /// A row whose value opens this menu.
    Menu(SharedString, MaterialMenu),
    /// A control at the row's trailing edge; it moves under the label when both don't fit.
    Inline(AnyElement),
    /// A wide control under the label: a button group, a text field.
    Below(AnyElement),
}

/// One row of a [`field_table`].
pub struct Field {
    id: SharedString,
    label: SharedString,
    hint: Option<SharedString>,
    disabled: bool,
    control: Control,
}

impl Field {
    fn new(id: impl Into<SharedString>, label: impl Into<SharedString>, control: Control) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            hint: None,
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

    fn render(self, outer: OuterCorners, window: &mut Window, cx: &mut App) -> AnyElement {
        let scale = TileScale::Regular;
        let Self {
            id,
            label,
            hint,
            disabled,
            control,
        } = self;
        let text_id = ElementId::Name(format!("{id}-label").into());
        match control {
            Control::Switch(on, on_change) => ListItem::new(ElementId::Name(id), label)
                .when_some(hint, ListItem::supporting)
                .trailing(ListTrailing::Switch(on))
                .container(ColorRole::SURFACE)
                .segment(outer)
                .disabled(disabled)
                .on_click(move |_, window, cx| on_change(&!on, window, cx))
                .into_any_element(),
            Control::Menu(value, items) => {
                let value = Some(match hint {
                    Some(hint) => format!("{value} · {hint}").into(),
                    None => value,
                });
                let trigger = scale
                    .button(ElementId::Name(id.clone()), outer)
                    .accessibility_label(label.clone())
                    .disabled(disabled)
                    .child(
                        scale
                            .row()
                            .child(scale.text(text_id, label, value))
                            .child(tile_chevron()),
                    );
                let popup = ElementId::Name(format!("{id}-menu").into());
                menu(popup, trigger, items, window, cx)
                    .anchor(Anchor::TopRight)
                    .into_any_element()
            }
            Control::Inline(control) => scale
                .surface(outer)
                .child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .min_w_0()
                        .gap(scale.gap())
                        .px(ITEM_INSET)
                        .py(rem(12.))
                        .child(
                            scale
                                .text(text_id, label, hint)
                                .flex_basis(rem(200.))
                                .min_h(rem(40.))
                                .justify_center(),
                        )
                        .child(div().max_w_full().child(control)),
                )
                .into_any_element(),
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
    }
}

#[derive(gpui::IntoElement)]
pub struct FieldTable {
    id: SharedString,
    title: SharedString,
    fields: Vec<Field>,
}

impl RenderOnce for FieldTable {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The group hands each tile its corners only when it lays them out, and
        // a row's `render` needs the window: keep the rows' corners here.
        let count = self.fields.len();
        let mut group = TileGroup::new(self.id, self.title);
        for (index, field) in self.fields.into_iter().enumerate() {
            let row = field.render(list_corners(index, count), window, cx);
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
    let icon = |symbol: SymbolIconRole| {
        let (icon, role) = match symbol {
            SymbolIconRole::Success => (symbols::check_circle(), ColorRole::SUCCESS),
            SymbolIconRole::Warning => (symbols::warning(), ColorRole::WARNING),
            SymbolIconRole::Failed => (symbols::error(), ColorRole::ERROR),
        };
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
            icon(SymbolIconRole::Success),
            accessible_text(text_id, message.clone()).role(Role::Status),
            retry,
        ),
        AsyncStatus::Warning(message) => (
            icon(SymbolIconRole::Warning),
            accessible_text(text_id, message.clone()).role(Role::Status),
            retry,
        ),
        AsyncStatus::Failed(message) => (
            icon(SymbolIconRole::Failed),
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

enum SymbolIconRole {
    Success,
    Warning,
    Failed,
}

/// A question before a consequential action. Cancel is named too: a verb that says
/// what stays ("保留笔记") reads better than a generic one.
pub struct Confirm {
    title: SharedString,
    cancel: SharedString,
    confirm: SharedString,
    danger: bool,
    text: Vec<SharedString>,
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
    pub fn text(mut self, text: impl Into<SharedString>) -> Self {
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

/// The question being asked. It outlives the answer, so the dialog can play its exit.
#[derive(Default)]
struct ConfirmSlot(
    Option<(Confirm, Rc<dyn Fn(&(), &mut Window, &mut App)>)>,
    bool,
);
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
            .content(
                v_flex()
                    .gap(rem(8.))
                    .text_color(color(ColorRole::ON_SURFACE_VARIANT))
                    .children(confirm.text.iter().enumerate().map(|(index, text)| {
                        accessible_text(("confirm-text", index), text.clone()).whitespace_normal()
                    })),
            )
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

/// A download's quantity, labeled speed and time remaining, on one stable block so
/// the numbers do not hide behind a single sentence.
pub fn transfer_status(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    metrics: &crate::activity::TransferMetrics,
    fraction: Option<f32>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    use crate::activity::TransferEta;
    let id = id.into();
    let line = |name: &'static str, text: SharedString, role, size| {
        accessible_text(child_id(&id, name), text)
            .text_size(size)
            .text_color(color(role))
    };
    let meter = |name: &'static str, label: &'static str, value: &str| {
        v_flex()
            .min_w(rem(98.))
            .gap(rem(4.))
            .child(line(
                label,
                label.into(),
                ColorRole::ON_SURFACE_VARIANT,
                TEXT_AUX,
            ))
            .child(
                line(
                    name,
                    value.to_owned().into(),
                    ColorRole::ON_SURFACE,
                    TEXT_BODY,
                )
                .font_weight(FontWeight::SEMIBOLD),
            )
    };
    let remaining = match &metrics.eta {
        Some(TransferEta::Remaining(value)) => Some(value),
        _ => None,
    };
    let eta_note = match &metrics.eta {
        Some(TransferEta::Note(value)) => Some(value),
        _ => None,
    };
    let notes = [
        ("eta-note", eta_note),
        ("note", metrics.note.as_ref()),
        ("aside", metrics.aside.as_ref()),
        ("source", metrics.source.as_ref()),
    ];
    v_flex()
        .w_full()
        .min_w_0()
        .gap(rem(8.))
        .child(
            line("title", title.into(), ColorRole::ON_SURFACE, TEXT_BODY)
                .font_weight(FontWeight::SEMIBOLD),
        )
        .when(!metrics.quantity.is_empty(), |view| {
            view.child(line(
                "quantity",
                metrics.quantity.clone().into(),
                ColorRole::ON_SURFACE,
                TEXT_BODY,
            ))
        })
        .when(metrics.speed.is_some() || remaining.is_some(), |view| {
            view.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .gap(rem(24.))
                    .when_some(metrics.speed.as_ref(), |row, speed| {
                        row.child(meter("speed", course2md::tr!("速度"), speed))
                    })
                    .when_some(remaining, |row, value| {
                        row.child(meter("eta", course2md::tr!("预计剩余"), value))
                    }),
            )
        })
        .children(notes.into_iter().filter_map(|(name, text)| {
            let text = text?.clone().into();
            Some(line(name, text, ColorRole::ON_SURFACE_VARIANT, TEXT_AUX))
        }))
        .when_some(fraction, |view, fraction| {
            view.child(progress(child_id(&id, "bar"), fraction, window, cx))
        })
}

/// A 16:9 video cover. Without an image the tonal frame stays, so rows keep their
/// rhythm while covers load; callers place their placeholder icon in it and give it a width.
pub fn cover(radius: Pixels, image: Option<std::path::PathBuf>) -> Div {
    div()
        .aspect_ratio(16. / 9.)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius)
        .overflow_hidden()
        .bg(color(ColorRole::SECONDARY_CONTAINER))
        .when_some(image, |slot, image| {
            slot.child(
                img(image)
                    .size_full()
                    .rounded(radius)
                    .object_fit(ObjectFit::Cover),
            )
        })
}

/// What follows the pointer while a note, folder or queued task is dragged.
pub fn drag_chip(icon: impl Into<Icon>, label: SharedString) -> Div {
    h_flex()
        .max_w(rem(336.))
        .gap(rem(8.))
        .px(rem(12.))
        .py(rem(8.))
        .rounded(RADIUS_SMALL)
        .bg(color(ColorRole::SURFACE))
        .shadow(gpui_kit_material::tokens::elevation::level3())
        .text_size(TEXT_BODY)
        .font_weight(FontWeight::SEMIBOLD)
        .child(
            icon.into()
                .size(ICON_SLOT)
                .color(color(ColorRole::ON_SURFACE_VARIANT)),
        )
        .child(div().min_w_0().truncate().child(label))
}

/// A fact beside its label: labels share a bounded column, values a stable start, and
/// a value that does not fit wraps under its label.
pub fn detail_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon: impl IntoElement,
    value: impl IntoElement,
) -> Div {
    div()
        .flex()
        .flex_wrap()
        .w_full()
        .min_w_0()
        .min_h(rem(32.))
        .items_center()
        .gap_x(rem(12.))
        .gap_y(rem(2.))
        .child(
            semantic_label(id, label, icon)
                .w(rem(160.))
                .max_w_full()
                .flex_shrink_0(),
        )
        .child(
            div()
                .flex_1()
                .flex_basis(rem(200.))
                .min_w_0()
                .max_w_full()
                .font_weight(FontWeight::NORMAL)
                .child(value),
        )
}

/// Whether a dialog holds the window, Kit's or the kit's: app-wide shortcuts wait.
pub fn modal_open(window: &mut Window, cx: &mut App) -> bool {
    use gpui_kit::component::WindowExt as _;
    window.has_active_dialog(cx) || active_focus_trap(window, cx).is_some()
}
