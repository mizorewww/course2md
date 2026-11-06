# Using the kit

How a page reaches `gpui-kit-material`. Use `gpui_kit_material::prelude::*`; colors come from `theme::color(ColorRole::…)`, icons from `symbols::*`, text from `i18n/catalog.tsv` through `course2md::tr!`. For anything not covered here, in order: drop the need, compose kit components, and only then add a composition to `desktop/ui/src/ui.rs`, whose module doc says why each one exists. `desktop/src/about.rs` is a small worked example.

## Adding a symbol

Add the name to `desktop/assets/symbols.txt` ([catalog](https://m3symbols.mizore.blog)), then call `symbols::<name>()`. The build renders each listed name, so a name outside the catalog fails the build and an unlisted symbol does not compile. Append ` fill` to the line if the page draws `.filled(true)`. A debug build prints any `material-symbols/` path it could not load.

## Choose by intent

Pick the component by what the person is doing, not by which control stood there before. Change the control type or the layout when another one means the thing better; features stay. See the kit's gallery (`cargo run -p gpui-kit-material-gallery` in the kit repository) and `components/mod.rs`.

| The person is | Kit choice | Not |
| --- | --- | --- |
| Choosing one of 2 to 5 short options, seen together | `ConnectedButtonGroup` | A dropdown hiding so few |
| Choosing one of many, or options that are only text | `ExposedDropdown` | A row of buttons |
| Choosing one of many, with icons or supporting text | `ui::Field::menu` (an `ExposedDropdown` with the caller's `MaterialMenu`), with `ui::choice_items` | |
| Choosing one of a few that each need an explanation | `ChoiceTile`s, or `ListItem` with `ListLeading::Radio` | A button group with a hint elsewhere |
| Choosing several | `ChipSet` / `FilterChip`, or `ListItem` with a checkbox | Toggled buttons |
| Turning one thing on or off | `MaterialSwitch`; in settings `ui::Field::switch` | Two-option button group |
| Narrowing what a list shows | `FilterChip`, `SearchField` | A `ConnectedButtonGroup` unless it is a view of the same items |
| Switching among peer views of one subject | `MaterialTabs` | |
| Going to another place in the app | The app bar, or a selected `ListItem` in a pane | Tabs |
| Going forward: the region's one main action | `MaterialButton` `Filled`, icon and verb | |
| Taking a weighted secondary action | `Tonal`; same size `Outlined` beside a main action | Another `Filled` |
| Acting on the thing beside it | `Text`; `IconButton` with `MaterialTooltip` (`ui::tip_button`) if the icon says it | A full-width button |
| Several actions on one thing | `ui::menu` behind an `IconButton`, or `SplitButton`; on a right press `MaterialContextMenu` | A row of quiet buttons |
| Opening a place or detail from a row | `ListItem` with `.on_click` and a trailing arrow | A button in the row |
| Showing or hiding part of an item in place | `TileScale::button` with `disclosure_chevron` and `motion::disclosure` | A new page |
| Entering text | `TextField` with a label; `SearchField` to search | A bare input with a separate label |
| Reading what something is | `status_chip` beside it, `Badge` for a count | Colored text alone |
| Watching one operation run | `ui::async_status`, `LinearProgress`, `CircularProgress`, `LoadingIndicator` | A chip |
| Told a consequence or obstacle | `supporting_info` in a boundary, `notice_banner` alone | A dialog |
| Told something passing | `Snackbar` | A banner that needs dismissing |
| Deciding something consequential | `ui::Confirm`, `MaterialDialog` | A tooltip, a banner |
| Browsing independent subjects | `MaterialCard`, `segmented_list` of `ListItem` when they are rows | Cards around every setting |

## Accepted differences

Do not work around these:

- A button is the kit's size (`.button_size(ButtonSize::…)`), not an exact label size.
- Menus take `MaterialMenu::style` and `density`, not per-item tweaks.
- `ExposedDropdown` builds its menu from strings. Options with icons or supporting text pass the caller's `MaterialMenu` through `.menu`, as `ui::Field::menu` does.
- `ListTrailing` holds an icon, a checkbox, a radio or a switch, never a custom element. A row with another control is a `ui::Field`, or `list_tile` with `TileScale`.
- A danger item is a red leading icon (`ColorRole::ERROR`) plus the confirmation it already asks.
- `use ColorRole::*` is fine in a file that paints a lot.

## Timers that are not animation

Motion is the kit's: a page writes no curve, duration or `Animation::new`. These durations are not motion and stay in the app:

- `FRESH_FOR` (`desktop/src/reader_images.rs`): how long a newly decoded image counts as arriving, for its fade-in.
- `PROGRESS_INTERVAL` (`desktop/src/storage_ui.rs`): how often a library move reports progress to the view.
- `READING_POSITION_SETTLE` (`desktop/src/reader_ui.rs`): how long scrolling rests before the reader records a reading position. Restoring a position waits for the content to lay out, not for a timer.
