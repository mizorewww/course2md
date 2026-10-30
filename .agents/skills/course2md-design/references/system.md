# Shared design system

Which kit component carries each role, and how to judge a whole screen. Names are from `gpui_kit_material::components`; the kit's rustdoc and gallery have their variants and sizes.

Choose the content pane and its title, icon, text and control rulers before selecting cards or spacing ([layout and type](layout-and-type.md#establish-rulers-before-spacing)). A whole screen needs a primary task, necessary information and subordinate configuration. A short form should not become a decorative card grid.

## Roles

| Role | Composition | Kit |
| --- | --- | --- |
| Main action | Icon + short verb; one per active region | `MaterialButton`, `Filled` |
| Weighted secondary action | | `MaterialButton`, `Tonal` |
| Secondary action | Same size as the main action beside it | `MaterialButton`, `Outlined` |
| Inline action | Beside the information it acts on | `MaterialButton`, `Text` |
| Icon action | Tooltip names it | `IconButton`, `MaterialTooltip` |
| Peer selection | Short related options; independent commands are not single-choice | `ConnectedButtonGroup` |
| Grouped items | Section title above, one item per thing | `section_title`, `segmented_list` of `ListItem` |
| Preference | Label on the left, control on the right; the control moves below when it does not fit | `ListItem` trailing `Switch`, or a control beneath it |
| Peer navigation | | `MaterialTabs` |
| App navigation | App bar, or a pane where settings owns the page | `TopAppBar`, `PaneScaffold`, selected `ListItem` |
| Independent subject | One topic and its actions | `MaterialCard` |
| Data entry | Visible label, errors below | `TextField`, `ExposedDropdown`, `SearchField` |
| Status | Icon and short text, adjacent to the thing described | `status_chip`, `Badge` |
| Progress | See [states and motion](states-and-motion.md#conversion-states-and-result-access) | `LinearProgress`, `CircularProgress`, `LoadingIndicator` |
| Explanation | Short regular-weight prose, natural height | `supporting_info` inside a boundary, `info_callout` or `notice_banner` alone |
| Interruption | One consequential decision | `MaterialDialog` |
| Menu, transient message | | `MaterialMenu`, `Snackbar` |
| Window | macOS keeps native traffic lights; the app draws captions only where it owns the title bar | `TitleBar`, `CaptionButtons` |

For a meaning the table misses, look through the kit's components and gallery before writing anything. Platform brand marks identify a source and are not interface icons.

## Color and painted bounds

Pages paint semantic `ColorRole`s through `theme::color`, never a stored palette color. Body and supporting text establish the reading hierarchy; primary distinguishes actions and selection; status colors identify status. If a role reads wrong in one palette, correct the kit's mapping.

A control's geometry includes the paint added by selection, hover, press and focus. In a clipped or scrolling container, leave room for outward paint so the complete state stays visible at the content edges. Fix this in the container relationship, not by hiding focus or adding an exception around each field. Review edge controls in their actual native states.

## Whole-screen comparisons

For a product flow, start with [task decisions and continuity](interaction.md): establish whether the user can reach the intended result and return from interruptions before assessing composition. An orderly set of redundant steps is still a poor interaction.

Inspect the workbench, library, tasks, reader and settings side by side at default size. The forward action should attract more attention than navigation. Related controls must align even when their label lengths differ. Empty screens should not look like disabled populated screens. Expanded details should add a structured explanation rather than reveal an undifferentiated text dump.

Compare the initial and enlarged-type layouts: the latter is a new layout constraint, not simply a magnified screenshot.

For every new wrapper, ask what relationship it communicates. Remove it if spacing and a single heading already express that relationship. Do not remove a genuinely useful boundary solely to reduce the number of borders.
