# Moving a page to the kit

For page agents in wave 2. Design authority: `.agents/skills/course2md-design/SKILL.md`. Use
`gpui_kit_material::prelude::*`. **Worked example: `desktop/src/about.rs`** (with its rows in
`settings_ui.rs::application_settings_page`); it imports nothing from `theme::*`.

Order of preference for anything not in these tables: drop the need; compose kit components; only
then `desktop/src/ui.rs`. Do not add to `theme.rs`. Text comes from `i18n/catalog.tsv` in four
languages, through `course2md::tr!`.

## What the foundation already did

- One paint: `theme::color(CANVAS)` and `gpui_kit_material::theme::color(ColorRole::SURFACE_CONTAINER)`
  are the same value, so old and new code can share a screen in any order.
- Kit's `Theme` colors stay written by `theme.rs` until wave 3 removes it. Wave 3 replaces
  `theme::apply_preference` and `apply_scale` in `views.rs` with `init_component_theme` at startup
  and `advance_component_theme` before Root's render. Render must not touch Kit's `Theme` after that.
- Every old `icons::*` already draws a Material Symbol. New code calls `symbols::*`.
- Text is Roboto with the system's CJK fallback.

## Adding a symbol

Add the name to `desktop/assets/symbols.txt` ([catalog](https://m3symbols.mizore.blog)), then call
`symbols::<name>()`. A name outside the catalog fails the build. Mark the line ` fill` if you draw
`.filled(true)`. A debug build prints any `material-symbols/` path it could not load. `symbols.rs` has examples.

## Choose by intent

**A page agent picks the component by what the person is doing, not by which old widget stood there.**
A one-to-one swap of a wrong control is not acceptable: change the control type or the layout when another
one means the thing better. Features stay. Old widgets were used for several purposes, so the tables further
down name a replacement only where an old item has one meaning. Reference: the kit's Gallery
(`cargo run -p gpui-kit-material --example gallery` in the kit repository) and `components/mod.rs`.

| The person is | Kit choice | Not |
| --- | --- | --- |
| Choosing one of 2 to 5 short options, seen together | `ConnectedButtonGroup` | A dropdown hiding so few |
| Choosing one of many, or options that are only text | `ExposedDropdown` | A row of buttons |
| Choosing one of many, with icons or supporting text | `ui::Field::menu`, or `ui::menu` with `ui::choice_items` | |
| Choosing one of a few that each need an explanation | `ChoiceTile`s, or `ListItem` with `ListLeading::Radio` | A button group with a hint elsewhere |
| Choosing several | `ChipSet` / `FilterChip`, or `ListItem` with a checkbox | Toggled buttons |
| Turning one thing on or off | `MaterialSwitch`; in settings `ui::Field::switch` | Two-option button group |
| Narrowing what a list shows | `FilterChip`, `SearchField` | A `ConnectedButtonGroup` unless it is a view of the same items |
| Switching among peer views of one subject | `MaterialTabs` | |
| Going to another place in the app | The app bar, or a selected `ListItem` in a pane | Tabs |
| Going forward: the region's one main action | `MaterialButton` `Filled`, icon and verb | |
| Taking a weighted secondary action | `Tonal`; same size `Outlined` beside a main action | Another `Filled` |
| Acting on the thing beside it | `Text`; `IconButton` with `MaterialTooltip` if the icon says it | A full-width button |
| Several actions on one thing | `MenuPopup` behind an `IconButton`, or `SplitButton` | A row of quiet buttons |
| Opening a place or detail from a row | `ListItem` with `.on_click` and a trailing arrow | A button in the row |
| Showing or hiding part of an item in place | `TileScale::button` with `disclosure_chevron` and `motion::disclosure` | A new page |
| Entering text | `TextField` with a label; `SearchField` to search | A bare input with a separate label |
| Reading what something is | `status_chip` beside it, `Badge` for a count | Colored text alone |
| Watching one operation run | `ui::async_status`, `LinearProgress`, `CircularProgress`, `LoadingIndicator` | A chip |
| Told a consequence or obstacle | `supporting_info` in a boundary, `notice_banner` alone | A dialog |
| Told something passing | `Snackbar` | A banner that needs dismissing |
| Deciding something consequential | `ui::Confirm`, `MaterialDialog` | A tooltip, a banner |
| Browsing independent subjects | `MaterialCard`, `segmented_list` of `ListItem` when they are rows | Cards around every setting |

## Motion

All animation is the kit's: `gpui_kit_material::motion` and what components do on their own. A page writes no
curve, duration or `Animation::new`. The kit's default scheme is Expressive where `motion.rs` used Standard
tokens, so springs are a little livelier; do not set a scheme per page.

| Old animation | Kit |
| --- | --- |
| `Spring` and its constants | `MotionToken`, `motion::curve`; the palette blend is `theme::apply_scheme` |
| `motion::enter`, `state_enter`, `page_enter` | Same names: same fade and rise, the kit's scale-aware distance |
| `motion::value`, `selection_value` (`home-hero-top`, palette cards, `ChoiceTile`) | Same names |
| `motion::disclosure` | Same name |
| `spinner`, `icon_spinner` | Same names in components, now with a label. Prefer `MaterialButton::loading` inside a button |
| `progress`, `linear_progress`, `indeterminate_progress` | Same names in components, or `LinearProgress` |
| `transfer_status` | `ui::transfer_status` |
| `choice_group`'s selection fill | `ConnectedButtonGroup`'s own morph; `MaterialTabs`' indicator |
| `theme::selection_card`, `ChoiceTile`, tile hover and press blends | `MaterialCard`, kit `ChoiceTile`, `RingedButton`, `Ripple` state layers |
| The scrollbar's fade | Kit's `ScrollbarMotion` |

Everything the app animates has a kit form, so no backport is proposed. Two verdicts:

- `performance::record_motion` traced the old helpers. The kit has no hook, so a performance build lists fewer
  motion events as pages move, and none from the shell now. Dropped; frame timings stay.
- Not animation, and they stay: the 800 ms wait in `reader_ui.rs` (restoring a position), `FRESH_FOR` in
  `reader_images.rs` and `PROGRESS_INTERVAL` in `storage_ui.rs`.

## `ui.rs`

| Item | Why the kit does not cover it |
| --- | --- |
| `field_table`, `Field` | A settings section whose rows mix controls; `ListTrailing` holds no custom element |
| `AsyncStatus`, `async_status` | One check, probe or load narrating itself. The state machine stays in the page |
| `menu`, `keyed_open` | `MenuPopup` and `ExposedDropdown` are controlled; these sites keep no other state |
| `choice_items` | Menu items that pick one value |
| `Confirm`, `modal_open` | `MaterialDialog` is an element in the tree; a question asked from a handler needs its one host |
| `status_line` | Text with " · " between parts, breaking only between parts |
| `transfer_status` | A download's title, status line and bar |

`task_actions` is not here: the selection is about tasks, not generic. Keep it in `task_ui.rs` as
one `fn task_actions(task, surface) -> Vec<TaskAction>` rendered to both buttons and menu items.

## Accepted differences

From `REDUCTION.md` § Kit audit. Do not work around these:

- `button_size` replaces `label_text_size`. A button is the kit's size, not an exact label size.
- `MaterialMenu::style`/`density` replaces `MenuStyle` tweaks; 12 `context_menu(` sites stay on Kit's `PopupMenu`.
- `ExposedDropdown` takes strings. Options with icons use `MenuPopup`.
- `ListTrailing` has no custom element. Select rows use `ui::Field::menu` or `list_tile` and `TileScale`.
- A danger row is a red `.leading` icon plus the existing confirmation; dismiss-tooltip-on-any-key is gone.
- `use ColorRole::*` is fine in a file that paints a lot.

## Lookup: names with one meaning

A row saying "by intent" means use the table above.

## `theme.rs`

### Colors

| Old token | `ColorRole` |
| --- | --- |
| `CANVAS` | `SURFACE_CONTAINER` |
| `SURFACE` | `SURFACE` |
| `SURFACE_HIGH` | `SURFACE_CONTAINER_HIGHEST` |
| `INSET` | `INSET` |
| `INK` | `ON_SURFACE` |
| `GRAY` | `ON_SURFACE_VARIANT` |
| `FAINT` | `FAINT` |
| `HAIRLINE` | `OUTLINE_VARIANT` |
| `CARD_LINE` | `CARD_LINE` |
| `CONTROL` | `OUTLINE` |
| `ACCENT` | `PRIMARY` |
| `ON_PRIMARY`, `SECONDARY_CONTAINER`, `ON_SECONDARY_CONTAINER` | same name |
| `SUCCESS`, `SUCCESS_BG`, `WARNING`, `WARNING_BG` | same name |
| `DANGER` | `ERROR` |
| `DANGER_BG` | `ERROR_CONTAINER` |
| `FIND_HIGHLIGHT`, `FIND_CURRENT`, `FIND_CURRENT_LINE`, `SELECTION`, `READING_SELECTION` | same name |

| Old | Replacement |
| --- | --- |
| `ColorToken`, `color` | `ColorRole`, `theme::color` |
| `blend`, `system_dark` | `gpui_kit_material::theme::blend`, `::system_dark` |
| `is_dark_paint` | `gpui_kit_material::theme::is_dark` |
| `apply_preference`, `apply_scale`, `init` | Shell only; see above |
| `image_scrim`, `on_image_scrim` | Keep as two local constants in the image viewer: the image is not themed |
| `caption_hover`, `caption_pressed`, `caption_close_hover`, `caption_close_pressed`, `on_caption_close` | Deleted with `window_controls.rs` once no caller is left; the kit's `TitleBar` paints captions |

### Geometry and type

| Old | Replacement |
| --- | --- |
| `RADIUS_PILL`, `RADIUS_SMALL`, `RADIUS_HERO`, `RADIUS_CARD` | Same names in `tokens::shape`. `RADIUS_CARD` is now 12 px; components supply their own |
| `FIELD_RADIUS` | Same name, now 4 px; use `TextField` instead of a radius |
| `SETTINGS_CARD_RADIUS`, `TILE_RADIUS`, `TILE_INNER_RADIUS`, `TILE_GAP` | `TILE_RADIUS`, `TILE_INNER_RADIUS`, `TILE_GAP` |
| `BUTTON_LARGE`, `BUTTON_MEDIUM` | `ButtonSize::Medium` (56 px), `ButtonSize::Small` (40 px) |
| `CONTROL_HEIGHT`, `ICON_SLOT`, `ICON_GAP`, `LABEL_INSET`, `COMPACT_WINDOW` | Same names in `tokens::layout` |
| `SETTING_ITEM_INSET` | `ITEM_INSET` |
| `SECTION_TITLE_GAP` | Same name; `TileGroup` applies it |
| `INLINE_SELECT_WIDTH`, `COLUMN`, `hero_top` | A product width or space: a constant in the page |
| `title_bar_height` | `views::title_bar_height` is the shell's; a page should not need it |
| `task_dialog_top`, `dialog_width`, `DIALOG_WIDTH`, `DIALOG_WIDE_WIDTH`, `DIALOG_MARGIN` | `MaterialDialog` places and sizes itself: `.size(DialogSize::…)` or `.width(px)` |
| `design_width`, `is_compact` | Same names in `tokens::layout` |
| `shaped_width` | Avoid it. Let flex wrap decide, or use `views::shaped_width` where a bar must choose a density |
| `IMAGE_DIALOG_MARGIN`, `_MAX_WIDTH`, `_MIN_WIDTH`, `_PADDING` | Constants in the image viewer |
| `TEXT_AUX`, `TEXT_BODY`, `TEXT_TITLE`, `TEXT_READER`, `TEXT_HEADLINE`, `TEXT_SETTINGS_BODY`, `TEXT_SETTINGS_AUX` | Same names in `tokens::typography`, or better a `TypeRole` |
| `QUIET_INSET` | Gone: `ButtonVariant::Text` has its own inset |
| `shadow_card`, `shadow_popover` | `tokens::elevation::level1`, `level3`; components carry their own |

### Buttons and fields

| Old | Replacement |
| --- | --- |
| `ButtonLabelSize::label_text_size`, `SettingsText::settings_text` | `.button_size(…)`; a setting's text comes from its row |
| `control`, `primary_pill`, `tonal_pill`, `outline_pill`, `text_button`, `quiet` | By intent: `material_button(id, variant)` |
| `danger_pill` | `ui::Confirm::danger`; an error action outside a dialog is `Text` with `.text_color(color(ERROR))` |
| `disabled_when` | `.disabled(…)` |
| `icon_button`, `input_action` | `IconButton::new(id, icon, label)`, with `.variant(IconButtonVariant::Tonal)`, `.button_size(…)`; wrap in `MaterialTooltip::plain(id, label).trigger(…)` |
| `filter_chip`, `chip` | By intent: `FilterChip`, `AssistChip` |
| `nav_action`, `nav_destination`, `nav_destination_width` | Gone with the shell; `MaterialButton` with `Tonal` when selected, or `MaterialTabs` for peers |
| `text_input`, `labeled_input`, `outlined_field_label` | `TextField::new(id, &state).label(…).outlined()` |
| `search_field` | `SearchField::new(&state).leading(…)` |
| `preference_switch`, `processing_switch` | `ListTrailing::Switch`, `ui::Field::switch`, or `MaterialSwitch` |
| `danger_menu_item` | `MaterialMenuItem` with `.leading(Icon::from(symbols::delete()).color(color(ERROR)))` |

### Surfaces and text

| Old | Replacement |
| --- | --- |
| `page_heading`, `section_title`, `section_heading`, `accessible_text`, `error_text`, `semantic_label`, `supporting_info`, `info_callout` | Same names in the kit. `section_heading`'s icon is a kit `Icon` |
| `notice_banner`, `NoticeKind` | Same names |
| `badge`, `BadgeKind`, `outlined_badge` | `status_chip`, `StatusKind`, `outlined_status_chip` |
| `card_surface`, `selection_card` | By intent: `MaterialCard`, `card(CardVariant::…)`, `ListItem` with a radio |
| `drag_chip` | `AssistChip` with `.elevated(true)` |
| `cover_frame`, `cover_slot` | `ListLeading::Video` in a row; a local 16:9 `div` with the image in a card |
| `detail_row` | `ListItem` with `.trailing_text`, or headline and `.supporting` |

### Tiles and dialogs

| Old | Replacement |
| --- | --- |
| `TileGroup`, `TileScale`, `tile`, `list_tile`, `tile_info`, `tile_chevron`, `disclosure_chevron`, `choice_radio`, `ChoiceTile`, `Tint`, `tinted_icon` | Same names in the kit. Prefer `ListItem`, `segmented_list` when a row is only text and a switch or icon |
| `tile_corners`, `list_corners` | Same names in `tokens::shape` |
| `TEXT_TILE_LABEL` | `TileScale::Regular.label_size()` |
| `tile_label`, `tile_support`, `tile_text`, `tile_row`, `tile_button` | `TileScale::Regular.label`, `.support`, `.text`, `.row`, `.button` |
| `tile_disclosure` | `TileScale::Regular.button(id, outer)` holding a `disclosure_chevron(open)`, with `motion::disclosure` below |
| `MaterialDialog` trait, `dialog_title`, `dialog_actions` | `MaterialDialog::new(id).headline(…).content(…).action(…).modal(open).on_dismiss(…)`, an element kept in the tree with `open` in the page's state |
| `confirm_dialog` | `ui::Confirm` |

## `motion.rs`

See [Motion](#motion). Every public item (`Spring`, `FAST_SPATIAL`, `DEFAULT_SPATIAL`, `DEFAULT_EFFECTS`, `Spring::duration`, `::position`, `::ease`, `enter`, `state_enter`, `page_enter`, `spinner`, `icon_spinner`, `indeterminate_progress`, `value`, `selection_value`, `progress`, `linear_progress`, `disclosure`) is covered there.

## `palettes.rs`

Now a re-export; the persisted form is unchanged. `Appearance`, `PaletteId`, `ThemePreferences`, `Palette` are
`gpui_kit_material::color::*`, `ALL` is `color::palette::ALL`. Use `Scheme::from_palette(id)` for a preview, not `id.colors()`.

## `choice_group.rs`, `select_field.rs`

By intent; these two carried most mismatched uses. A `SingleChoiceGroup` picking among many becomes a
dropdown; a `SelectField` over two options becomes a button group or a switch.

| Old | When that control is right |
| --- | --- |
| `SingleChoiceGroup` and its `.options`, `.selected`, `.full_width`, `.segmented`, `.focus_handles`, `.icon`, `.reveal_in`, `.disabled` | `ConnectedButtonGroup`, same method names |
| `.on_change` | `.on_activate` |
| `.height`, `.label_text_size` | `.button_size` |
| `.tabs` | `MaterialTabs` |
| `SelectField` | `ExposedDropdown::new(id, label).options(…).selected(…)`, opened with `ui::keyed_open` |
| `.detail`, `.trailing`, `.label`, `.icon`, `.disabled` | `.supporting_text`, an option's text, the dropdown's label, `.leading_icon`, `.disabled` |
| `MenuBuilder` | `MaterialMenu`. A `context_menu(` site keeps Kit's closure and `PopupMenu` |

## `focus_scroll.rs`

`FocusRing`, `RevealFocus` (`new`, `in_list`, `inline`) and `contain_scroll` have the same names in the kit.

## `scroll.rs`

| Old | Replacement |
| --- | --- |
| `vertical_scrollbar`, `vertical_scrollbar_for` | `Scrollbar::vertical(handle)`: policy comes from Kit's theme, which `Theme::sync_scrollbar_appearance` keeps with the system |
| `horizontal_scrollbar(handle, overflowing)` | `Scrollbar::horizontal(handle)`, adding `.mode(ScrollbarMode::Always)` when overflowing |

## `icons.rs`

`icons::x()` returns Kit's `Icon`; `symbols::y()` converts into the kit's. `Assets` is `symbols::Assets`.

| `icons::` | `symbols::` |
| --- | --- |
| `arrow_forward`, `article`, `check`, `check_circle`, `chevron_left`, `chevron_right`, `close`, `cloud`, `code`, `computer`, `content_copy`, `delete`, `download`, `edit`, `file_upload`, `fit_screen`, `folder`, `folder_open`, `history`, `image`, `inbox`, `info`, `link`, `list`, `login`, `logout`, `mic`, `movie`, `palette`, `pause`, `play_arrow`, `refresh`, `save`, `schedule`, `science`, `search`, `settings`, `storage`, `subtitles`, `summarize`, `task`, `tune`, `tv`, `warning`, `zoom_in`, `zoom_out` | same name |
| `arrow_left` | `arrow_back` |
| `arrow_down`, `arrow_up` | `arrow_downward`, `arrow_upward` |
| `auto_fix` | `auto_fix_high` |
| `book_open` | `menu_book` |
| `braces` | `data_object` |
| `chevron_down`, `chevron_up` | `keyboard_arrow_down`, `keyboard_arrow_up` |
| `cpu`, `microchip` | `memory` |
| `error` | `cancel` |
| `external_link` | `open_in_new` |
| `eye`, `eye_off` | `visibility`, `visibility_off` |
| `file_text` | `description` |
| `folder_input` | `drive_file_move` |
| `folder_plus` | `create_new_folder` |
| `globe` | `language` |
| `gpu` | `developer_board` |
| `grip_vertical` | `drag_indicator` |
| `layout_grid` | `grid_view` |
| `library` | `library_books` |
| `list_checks` | `checklist` |
| `loader_circle` | `progress_activity`; prefer `spinner` |
| `maximize` | `open_in_full` |
| `message_square` | `chat_bubble` |
| `moon`, `sun` | `dark_mode`, `light_mode` |
| `more_vertical` | `more_vert` |
| `notebook` | `book` |
| `play` | `play_arrow` |
| `plus` | `add` |
| `restart` | `restart_alt` |
| `server` | `dns` |
| `sparkles` | `auto_awesome` |
| `text_lines` | `notes` |
| `toc` | `format_list_bulleted` |
| `bilibili`, `bilibili_mark` | Stay `icons::bilibili`, moving to the source card with the brand asset |
| `caption_close`, `caption_maximize`, `caption_minimize`, `caption_restore` | Gone with `window_controls.rs` |
