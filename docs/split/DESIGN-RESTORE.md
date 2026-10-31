# Design restore audit

What wave 2 changed against the app before the migration, page by page, and whether each look comes
back. The rule is `PLAN.md` decision 16: a migration keeps the look; a look the kit lacks is added to the
kit, and the page uses it. Decision 14 may still change a control where the old one meant the wrong thing,
and then its look stays.

- **Baseline:** `3db4a88`, built with `--features capture` at its locked GPUI sources.
- **Current:** `split/workspace` at `0283304`, same feature, kit `a22271e`.
- **Conditions:** 1280×780, Paper (light) and Ink (dark), Simplified Chinese, isolated `HOME`/`XDG_*`, one fixture copied
  per run: three real notes, one partial note, one folder and two services, made by the baseline app and engine against synthetic
  tools and a local fake AI service. Both engines were built with `COURSE2MD_NO_APPLE=1`, so the Apple model reads as missing in both.
- **Images:** `$SHOTS/{b,c}/{light,dark}/<name>.png`, where `b` is baseline, `c` current and
  `$SHOTS=/private/tmp/claude-501/-Users-aac6fef-Developer-course2md/efbdb7fd-15b6-4010-9790-15ca1d457be9/scratchpad/design-restore/shots`.
  Each row names the image; code locations are baseline `desktop/src` at `3db4a88`. The images are working
  evidence in scratch and are not committed.
- **Compared by eye:** every pair in light, a sample in dark. Dark showed nothing light did not. Current's dialogs were retaken after a stuck confirm dialog spoiled the
  first set. One note was enriched by hand with a summary and six sections (`reader-rich*`), for both builds.

## Result

| Verdict | Items |
| --- | --- |
| `restore` | 70 |
| `keep new` | 18 |
| Total | 88 |

Grep of the baseline for the same things, which cross-checked the rows rather than counted them: `tinted_icon`/`Tint::` 24 uses outside `theme.rs`, `RADIUS_*`,
`rounded_full`, shadows and gradients about 150 uses, semantic status colors 123 uses in page files. They resolve into the rows
below, mostly through the shared items G3 to G9.

## Why the kit needs work

`Tint` exists in the kit, but it maps blue, teal, pink and purple onto `PRIMARY_CONTAINER`, `SECONDARY_CONTAINER`,
`TERTIARY_CONTAINER` and `TERTIARY_FIXED`, and for every source palette (`Scheme::from_palette`) the first three are the same
accent tint. A straight swap would have painted the settings navigation one color. So a tonal icon is not a matter of calling
`tinted_icon`; the families need their own roles.

## Shared

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| G1 | App bar destinations | Ink label and outline icon, tonal when selected. `home-empty-noopts`, `theme.rs:1673` | Primary-colored label and icon | restore | Navigation competed with the page's action. Text button in a neutral tone (kit) |
| G2 | Wordmark | Regular weight. `views.rs` brand | Medium | restore | Quieter identity, app-side |
| G3 | Outlined button | Visible control outline, primary label. `theme.rs:1818` | Faint outline, gray label. `reader`, `set-services` | restore | It read as disabled. Accent tone for `Outlined` (kit) |
| G4 | Quiet inline action | Ink label beside primary text buttons. `theme.rs:1899`, `home` | All primary | restore | Two levels of emphasis became one. Neutral tone for `Text` (kit) |
| G5 | Back and tool icon buttons | Neutral `SURFACE_HIGH` circle. `theme.rs:1855`, `set-appearance` | Secondary-container circle | restore | Neutral tone for `IconButton` (kit) |
| G6 | Connected group, unselected | Neutral gray beside the primary fill. `choice_group.rs`, `set-generation` | Tonal blue | restore | The selected item stood alone. Neutral option on `ConnectedButtonGroup` (kit) |
| G7 | Switch | 52×32 with an × in the off thumb and a check on. `theme.rs:2229`, `set-generation-2` | Plain thumb | restore | `MaterialSwitch` has `SwitchIcons`; `ListTrailing::Switch` cannot ask for them (kit) |
| G8 | Form fields | Outlined at radius 12, label always floating, placeholder shown. `theme.rs:1427`, `svc-dialog` | Radius 4, label resting inside | restore | Shape option on `TextField` (kit), `.float_label` and `.placeholder` at call sites |
| G9 | Dialog surface | `SURFACE`, white in Paper. `theme.rs:1976`, `dlg-storage` | Untinted light gray, `SURFACE_CONTAINER_HIGH` of a source palette | restore | The gray matched nothing else on screen. Container option on `MaterialDialog` (kit) |
| G10 | Search field | Settings: surface with hairline. Library: neutral fill. `theme.rs:1499`, `settings`, `library` | Pale untinted gray on both | restore | Style option on `SearchField` (kit) |
| G11 | Row titles in forms | Regular weight. `set-appearance` | Medium, on every row and card title | restore | Emphasis belongs to the scan layer per the skill; app-side, `ui::Field` and card titles |
| G12 | Icon choice | Books (library), database (storage), document (notes), ring notebook, TV | Newspaper-like stack, server rack, article | restore | Within Material Symbols: pick the nearest form, app-side `symbols.txt` |
| G13 | Icon family | Lucide | Material Symbols | keep new | Decision 1 |
| G13a | Icon stroke | Fine, round-ended line. `icons.rs` | Weight 400 Outlined: heavier, square ends | restore | Compared by eye: the stroke is a large part of the old character. Rounded style and a lighter weight within Material Symbols, kit `icons` |
| G14 | Typeface | System UI | Roboto with CJK fallback | keep new | Decision 1; the kit's type |
| G15 | Dialog placement | Near the top | Centered | keep new | The kit places dialogs; plainly consistent with Material |
| G16 | Motion | `motion.rs` | The kit's | keep new | Decision 15 |
| G17 | Page titles | 30/36. `theme.rs:427`, `library` | 24/32 | restore | Hierarchy; a larger `TypeRole` or a role option on `page_heading` |
| G18 | Corner scale | Cards 16, panels 28, settings tiles 24. `theme.rs:273` | Cards 12, tiles 24, panels 24 | restore | Roomy corners are the old character; kit radius tokens stay, pages pick the larger token through card options |
| G19 | Scrim | 28% light, 58% dark. `theme.rs` | Flat 32% | keep new | Material's value; compared by eye in both modes |
| G20 | Dialog width | 448 default | 312 default | restore | `.width(px)` or `DialogSize` at call sites |
| G21 | Menu rows | 48, radius 10 | 44 | keep new | The kit's menu; no distinct look lost, and the behavior PRs on the kit own this component |

## Home

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| H0 | Hero title | 40/48. `import_ui.rs`, `home` | 36/44 | restore | App-side role |
| H1 | Link field | 72 tall pill on `SURFACE`, 2px ring primary when focused or filled, error ring on failure, no resting outline, no floating label. `import_ui.rs:1125`, `home-empty-noopts` | 56 tall outlined rectangle with a notched label. `import_ui.rs:1026` today | restore | The page's signature. Hero `TextField` (kit) |
| H2 | Preview action | Tonal pill with icon and 「预览视频」 inside the field. `home-preview` | Icon-only circle with a tooltip | restore | Trailing slot must hold a labeled button (kit) |
| H3 | Drop hint | Upload icon with the sentence, plain link 「选择文件」 | Icon moved onto the link | restore | App-side |
| H4 | Hero spacing | Title 20 above a 72 field; 24 to the hint | Title touches the label notch | restore | App-side; follows H1 |
| H5 | Options panel | One card: header, hairline, quiet section titles and content inside. `home-preview` | Header card, then separate cards with titles outside | restore | It is one thing, and the skill says so; app-side |
| H6 | Options group | 52 tall, hint with info icon below. `import_ui.rs` | 40 tall, hint as row support | restore | `ButtonSize::Medium`, app-side |
| H7 | Speech choice in options | Outlined field with label, value and trailing CPU. `home` | Row with chevron and menu | keep new | Decision 14: a row opening a menu holds the same state and reflows better; the field radius restored by G8 where a field remains |
| H8 | Video card | Cover radius 18 with 16 inset, filled movie glyph, duration chip, larger actions. `import_ui.rs:3247`, `home-preview` | Radius from `ListLeading`, outline glyph | restore | App-side |
| H9 | Start action | `ButtonSize::Medium` filled, with `Outlined` 更换视频 beside it. `home-preview` | Small | restore | The region's main action lost its weight; app-side |
| H10 | Reading state | Skeleton of the card: cover block and two bars. `import_ui.rs:1226`, `home-reading` | One bar | restore | The result replaced it in place; app-side |
| H11 | Running card | Cover, title, stage, thin progress in the text column, actions in a row. `home-running` | Adds step chips and second lines; bar at full width | restore | Tasks owns the steps; app-side |
| H12 | Submitted options | Flat read-only rows in the panel with a primary-plus row 「设置下一个视频」. `home` | That action moved into the card, options into segments | restore | The skill keeps it visible in the panel |
| H13 | Recent notes | One card, hairlines inset past the cover, duration on the cover, chevron. `course_library.rs:2729`, `home-empty-noopts` | Segmented cards, kebab, no duration on cover | restore | App-side |
| H14 | Partial task card | No cover | Placeholder cover | keep new | Same geometry as every other card state, so nothing jumps |
| H15 | Task error line | Ink | Error color | keep new | Plainly more correct |

## Library

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| L1 | Sidebar items | 44 tall pill. `folder_tree.rs:491`, `library` | 32 tall | restore | App-side density on `ListItem`, radius from the kit |
| L2 | 「文件夹」 label | Quiet section label | Missing | restore | App-side |
| L3 | Note card | `SURFACE` card radius 16 with 8 inset cover at its own radius, text on surface. `course_library.rs:2270`, `library` | Filled gray card, cover to the edge, bold title | restore | The grid read as gray blocks. Surface variant on `MaterialCard` (kit) |
| L4 | Card menu | Kebab on the title row | Kebab on a circle over the cover | restore | App-side, follows L3 |
| L5 | List view | One card with inset hairlines, status chip trailing. `course_library.rs:2186`, `library-list` | Segments with gaps, chip under the metadata | restore | App-side |
| L6 | Folder grouping toggle | Outlined with folder icon; tonal with check when on | Faint outline, no icon | restore | G3 plus icon |
| L7 | Import action | Tonal, medium, radius 16 | Tonal pill | restore | `ButtonSize::Medium` with `ButtonShape::Square` |
| L8 | Group header | Plain: chevron, name, count beside it. `library-group` | Hover bar, count at the far edge | restore | App-side |
| L9 | Card metadata | Folder and date | Adds uploader and duration | keep new | Useful and fits; no look lost |

## Tasks

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| T1 | Attention lead | Tinted circle with a state icon. `task_ui.rs:2033`, `tasks` | Cover placeholder | restore | The state is the point of this card; `tinted_icon` in `ERROR` or `AMBER` tone |
| T2 | Attention actions | Left, on the text ruler | Right edge | restore | App-side |
| T3 | Step chips | Neutral with check, active tonal with spinner. `task_ui.rs:23`, `tasks-running` | Green success chips | restore | Green steps read as many results; `outlined_status_chip`-like neutral (kit has it) |
| T4 | Running title | 「正在生成《…》」 with chip at the end | Title alone, chip below | restore | App-side |
| T6 | Task card | Radius 28 with roomy padding. `task_ui.rs:2248` | Tile | restore | App-side, card option |
| T5 | Count chips | Text only | With a status icon | keep new | Status role in the system table |

## Reader

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| R1 | Note attention | Warning tone, radius 28, page level above the view switch. `reader_ui.rs:4881`, `reader` | Error tone inside the note | restore | A partial note isn't an error; app-side `notice_banner` kind |
| R2 | View switch | Connected pills, left, primary fill. `reader_ui.rs:1699`, `reader` | Full-width underlined tabs on a white band | restore | The band read as a second title bar. `ConnectedButtonGroup` is allowed for views of the same item |
| R3 | Note metadata | Tonal chips with icon | Plain line | restore | App-side, the kit's neutral status chip |
| R4 | Timestamp | Tonal pill 「▶ 00:00 从此处观看」. `reader_ui.rs:5661` | Small text and glyph | restore | Depends on `TextView`'s block support, see the report |
| R5 | Note card height | Hugs the note | Stretches to the window | restore | App-side |
| R6 | Reader bar | 56 tall, breadcrumb and title centered. `reader_ui.rs:4321` | Crowded to the top | restore | App-side |
| R8 | Summary | Tinted panel with sparkle icon and title. `reader-rich` | Plain headings in the note | restore | The first thing to read; needs a block style from `TextView` |
| R9 | Contents | Flat pills, no surface. `reader_ui.rs:1590`, `reader-rich` | Segmented white list | restore | App-side |
| R10 | Section cards | A card per section. `reader_ui.rs:1374` | One surface | keep new | `TextView` as one document gives selection and find across sections |
| R11 | Body and figures | 17, figures radius 20. `reader-rich` | 16, radius 12 | restore | Reading comfort; `TextView` style options |
| R12 | Screenshot gallery | Surface card, inset cover, actions over the image. `reader_ui.rs:5308`, `reader-rich-shots` | Filled gray card, cover to the edge | restore | Same as L3 |
| R13 | Image viewer | Rounded modal on scrim, outlined tools. `reader-rich-viewer` | Whole window, quiet text buttons | restore | App-side; `MaterialDialog` wide |
| R7 | Empty screenshots | Card, icon, title, action | Same | keep new | Equivalent |

## Settings

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| S1 | Navigation icons and tiles | 72 tall tiles, 40 tinted circles: orange, blue, teal, amber, green; surface when selected. `settings_ui.rs:1288`, `set-services` | Gray icons | restore | Named by the owner. `ListLeading::Tinted`, `TintFamily` (kit) |
| S2 | Resource rows | 48 tinted circles: Bilibili pink, AI purple, speech teal, library amber. `settings_ui.rs:516`, `set-services`, `set-storage` | 24 gray icons | restore | Same |
| S3 | Add rows | Plus in the icon column, no chevron. `settings_ui.rs:525` | Small plus, chevron | restore | App-side |
| S4 | Login button | Bilibili mark, primary label, outlined | Generic arrow icon, gray | restore | Brand mark is allowed by the skill |
| S5 | Open location | Tonal 「打开位置」 | Icon only | restore | App-side |
| S6 | Library path | Monospace. `storage_ui.rs:1859` | Roboto | restore | Paths are data; app-side |
| S7 | Diagnostics summary | Tinted circle, title, count, row of outlined status chips, recheck. `settings_ui.rs:2996`, `set-application` | Rows of label and chip | restore | The verdict at a glance. Tinted plus `outlined_status_chip` |
| S8 | About links | Two side by side. `about.rs`, `set-application-2` | Stacked rows with chevrons | restore | App-side |
| S9 | Page title | On the 「设置」 title's line | 12 lower | restore | Alignment, app-side |
| S10 | Theme cards | Overline role above the palette name. `set-appearance` | Role as title, name as support | restore | App-side |
| S11 | Palette picker | Four across, wide dialog, selected caption tonal with ring. `appearance_ui.rs:149`, `dlg-palette` | Three across, selected caption gray | restore | `.width` and role, app-side |
| S12 | Recognition mode | Row with 「自动」 chip. `set-generation` | Two-option group | keep new | Decision 14: two short peers |
| S13 | AI service choice | Outlined field with icon. `set-generation-2` | Row and menu | keep new | Same as H7 |
| S14 | Service editor | Inline page with breadcrumb. `svc-ai` | Dialog | keep new | The baseline used the same form in a dialog already (`svc-dialog`); one form, one place |
| S15 | Service type | Dropdown with description. `svc-dialog` | Radio tiles | keep new | Decision 14: few options that each need an explanation; retains the description |

## First use

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| O1 | Progress | Numbered stepper with labels and connecting hairlines. `onboarding.rs:2579`, `ob` | 「第 1 步，共 4 步」 and a bar | restore | Shows where the person is going. `Stepper` (kit) |
| O2 | Engine choice | Radio tiles with tinted icons, tones per engine. `onboarding.rs:1214`, `ob` | Rows opening menus | restore | Decision 14's own table: few, each with an explanation |
| O3 | Account and model cards | Tinted pink and purple. `onboarding.rs:1490`, `ob-account`, `ob-model` | Gray | restore | S2 |
| O4 | AI step switches | Leading icon and switch with thumb icon. `ob-ai` | Plain | restore | G7 |
| O5 | Recovery rows | One row with a warning | Same text, more rows | keep new | No look lost |

## Dialogs

| # | Element | Baseline | Current | Verdict | Reason and kit need |
| --- | --- | --- | --- | --- | --- |
| D1 | Rename | Label above, radius 12 field. `dlg-rename` | Notched label, radius 4 | restore | G8 |
| D2 | Confirm, login, storage | White, radius 28 | Gray, same form | restore | G9 |
