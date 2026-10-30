# Layout, type and icons

Use this for any native page. Metrics come from the kit's `tokens`; this file covers how to arrange them.

## Establish rulers before spacing

Write down a small set of rulers for the actual content pane, not the outer window. A screenshot with no identifiable common rulers needs a composition change before pixel tuning.

| Ruler | What aligns to it |
| --- | --- |
| Pane leading/trailing edge | Common outer bounds of title region, form groups, lists and information panels |
| Leading icon slot | Left edge and width of icons in comparable labels or list rows |
| Label/text start | Text after a leading icon; supporting text for the same item begins here |
| Control start / end | Consistent control column for comparable settings rows |
| Content start below title | Top of the first meaningful group across related pages or changing steps |
| Row centerline / first text line | Single-line control centers on the row; a multiline label's icon aligns with its first line |

Reserve common action/value slots across a collection. A short title must not pull a trailing action left. A long title wraps within its text column before colliding with the action. Set the text region's actual shrink constraint; never hide an incorrect width calculation with clipping.

### A form grid is not two distant edges

Bound the whole setting unit to a useful content width. Inside it, align trailing switches and associated actions to the item edge; keep the label and explanation together. For choices that need more space, let the value reflow below its label. Input editors can use top labels.

If `usable width < label minimum + gap + control minimum`, put the control under the label at the same text ruler, computed from current font measurement. Do not create a fixed subcolumn or breakpoint that ignores 200% text.

Groups have a closer internal rhythm than their external separation. Do not separate an explanation from its action to fill spare height.

Cards hold independent subjects or entry points. A section around such items usually needs only a heading and spacing. A screen crowded by help may need shorter help or progressive disclosure, not smaller type.

## Type and weight

Use the kit's `TypeRole`s, not local sizes. Geometry that shares a ruler with text uses `typography::rem`, so it follows the text scale.

| Use | Role | Icon treatment |
| --- | --- | --- |
| Hero title | Display | Text only |
| Page heading | Headline (`page_heading`) | Text only; the app bar or settings navigation marks the destination |
| Home section heading | Title Large | None |
| Section title above a group | `section_title` | None |
| Action, navigation, main field label | The component's own label role | Meaningful icon |
| Field value, ordinary UI body | Body Medium (settings: Body Large) | No compulsory decorative icon |
| Necessary metadata, support | Body Small (settings: Body Medium) | Status icon only when it communicates state |
| Note-reading body | Body Large, adapted to the reader's scale | Preserve semantic Markdown formatting; no icon per paragraph |

Emphasis applies to the scan and action layer. It does not bold filenames, values, every subtitle, information-panel prose or a generated note. Short headings and labels have no terminal period; explanatory sentences remain normal prose.

Measure Chinese, mixed Chinese/Latin, numbers, descenders and long filenames with the actual font and fallback; a Chinese fallback cannot be judged as Latin Roboto. Do not crop glyphs, apply negative line height or invent per-page vertical offsets. If the kit's line box does not fit, fix it in the kit.

### Text measurement

Distinguish four things when reviewing a control: layout bounds (parent space, padding, shrink limits), line box, glyph ink and baseline (including fallback metrics), and painted bounds (container, selection, overlay, focus).

For multiline text, place the icon with the first line and flow remaining lines below that line's text start. Do not center a small icon halfway down a three-line explanation.

Character count × font size is not a valid width for shaped mixed-script labels. Use tabular figures for changing counters and progress when supported, and preserve their slot width: percentage updates must not move neighboring status text.

### Icons

All interface icons are Material Symbols through the kit's `icons`: `IconName` for its aliases, `SymbolIcon` for the rest of the catalog. Do not add icon files, another icon family or a local renderer. Choose icons recognizable beside their label; compare a narrow icon, a circle and a wide icon beside the same label in the native app. Their slots agree; their ink need not occupy identical shapes.

## Explanation and reading

Optional explanation goes in a quiet information panel or discoverable detail beside the relevant choice. A short panel has one information icon, regular body text and natural height. Several paragraphs or technical output belong in structured expanded detail. Do not repeat a label as the panel title.

Tooltips label icon-only controls or add optional context; they never contain the only explanation of a dangerous action, required setting, failure or current value. Modals ask for necessary decisions. Keep material consequences and errors beside the initiating action even while a form scrolls.

The reader is a content surface. Use a bounded prose column, ordinary paragraph weight, useful paragraph spacing and distinct heading levels. As a starting point, test roughly 35–50 Chinese characters or 60–85 Latin characters per line instead of full-window prose. At enlarged type, keep the column within its pane and reflow without shrinking the chosen font.

## Native review evidence

Record the pane width, text scale, chosen rulers and actual deviations. Compare the title, first group, representative rows, main action, expanded information panel and focused edge control, with a long mixed-script value. Repeat at narrow/default/wide windows and offered text scales, recording whether rows wrap, stack or change pane arrangement and whether actions stay associated with their content. “No overlap” is not a complete layout review.
