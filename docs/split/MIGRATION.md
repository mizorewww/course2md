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

## `ui.rs`

| Item | Use it for |
| --- | --- |
| `field_table`, `Field` | A settings section whose rows mix switches, menus and other controls |
| `AsyncStatus`, `async_status` | One check, probe or load narrating itself. The state machine stays in the page |
| `menu`, `keyed_open` | `MenuPopup` or `ExposedDropdown` that keeps nothing but its open state |
| `choice_items` | Menu items that pick one value |
| `Confirm`, `modal_open` | A consequential question; whether a dialog holds the window |
| `status_line` | Text with " · " between its parts, breaking only between parts |
| `transfer_status` | A download's quantity, speed and time remaining |
| `cover`, `drag_chip`, `detail_row` | 16:9 cover with its placeholder; what follows the pointer; a fact beside its label |

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
| `control`, `primary_pill` | `material_button(id, ButtonVariant::Filled)` |
| `tonal_pill` | `ButtonVariant::Tonal` |
| `outline_pill` | `ButtonVariant::Outlined` |
| `text_button`, `quiet` | `ButtonVariant::Text` |
| `danger_pill` | `ui::Confirm::danger`; an error action outside a dialog is `Text` with `.text_color(color(ERROR))` |
| `disabled_when` | `.disabled(…)` |
| `icon_button`, `input_action` | `IconButton::new(id, icon, label)`, with `.variant(IconButtonVariant::Tonal)`, `.button_size(…)`; wrap in `MaterialTooltip::plain(id, label).trigger(…)` |
| `filter_chip` | `FilterChip` |
| `chip` | `chip`, or `AssistChip` |
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
| `card_surface` | `card(CardVariant::…)`, or `MaterialCard` when it acts |
| `selection_card` | `MaterialCard::new(id, variant).state(…)` with `.on_click` |
| `drag_chip`, `cover_frame`, `cover_slot`, `detail_row` | `ui::drag_chip`, `ui::cover`, `ui::detail_row` |

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
| `SingleChoiceGroup`, `SelectField`, `MenuBuilder` | See below |

## `motion.rs`

Everything is `gpui_kit_material::motion::*` except the components named here.

| Old | Replacement |
| --- | --- |
| `Spring`, `FAST_SPATIAL`, `DEFAULT_SPATIAL`, `DEFAULT_EFFECTS`, `SLOW_EFFECTS`, `Spring::duration`, `::position`, `::ease` | `MotionToken`, `motion::curve(token)`, `motion::token_value` |
| `enter`, `state_enter`, `page_enter`, `value`, `selection_value`, `disclosure` | Same names |
| `spinner(id, cx)`, `icon_spinner(id, size, cx)` | `spinner(id, label, cx)`, `icon_spinner(id, label, size, cx)` in components: they now take a label |
| `indeterminate_progress`, `progress`, `linear_progress` | Same names in components, or `LinearProgress`, `LoadingIndicator` |
| `transfer_status` | `ui::transfer_status` |

## `palettes.rs`

Now a re-export; the persisted form is unchanged. `Appearance`, `PaletteId`, `ThemePreferences`, `Palette` are
`gpui_kit_material::color::*`, `ALL` is `color::palette::ALL`. Use `Scheme::from_palette(id)` for a preview, not `id.colors()`.

## `choice_group.rs`

| Old | Replacement |
| --- | --- |
| `SingleChoiceGroup::new`, `.options`, `.selected`, `.full_width`, `.segmented`, `.height`, `.label_text_size`, `.focus_handles`, `.icon`, `.reveal_in`, `.disabled`, `.on_change` | `ConnectedButtonGroup`, same method names except `.on_change` → `.on_activate`; prefer `.button_size` to `.height` and `.label_text_size` |
| `.tabs` | `MaterialTabs` |

## `select_field.rs`

| Old | Replacement |
| --- | --- |
| `SelectField` | `ExposedDropdown::new(id, label).options(…).selected(…)`, opened with `ui::keyed_open`; with icons, `ui::menu` |
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
