# Layout, type and icons

Use this for any native page or shared labels, inputs, selectors and list rows. The ruler names and numbers below are **course2md choices**. Their official basis is the downloaded [layout basics](material/offline/android-layout.md), [grids and units](material/offline/android-grids.md), [Material 3 type roles](material/offline/android-material3.md) and [Material Symbols guide](material/offline/google-symbols.md), plus the inspected [M3 grids](material/excerpts/m3-grids.md), [type](material/excerpts/m3-typography.md) and [icons](material/excerpts/m3-icons.md) pages.

## Establish rulers before spacing

Official basis: M3 uses global alignment lines for margins, titles and content. Its grid adapts to available space; proximity and consistent sizing communicate relationships. Its type guidance distinguishes glyphs, line boxes, padding and baselines. Treat these as different measurements.

Write down a small set of rulers for the actual content pane, not the outer window. A screenshot with no identifiable common rulers needs a composition change before pixel tuning.

| Ruler | What aligns to it |
| --- | --- |
| Pane leading/trailing edge | Common outer bounds of title region, form groups, lists and information panels |
| Leading icon slot | Left edge and width of icons in comparable labels or list rows |
| Label/text start | Text after a leading icon; supporting text for the same item begins here |
| Control start / end | Consistent control column for comparable settings rows, within the form grid |
| Content start below title | Top of the first meaningful group across related pages or changing steps |
| Row centerline / first text line | Single-line control centers on the row; a multiline label's icon aligns with its first line |

For an icon/label row, text starts at `row leading + icon slot width + icon/label gap`. Reuse that formula for headings, descriptions and comparable rows. A missing icon does not justify another unexplained subcolumn. A heading without an icon may align to the group's text start when it labels those rows; document that relationship.

Reserve common action/value slots across a collection. A short title must not pull a trailing action left. A long title wraps within its text column before colliding with the action. Set the text region's actual shrink constraint; never hide an incorrect width calculation with clipping.

### A form grid is not two distant edges

Bound the whole setting unit to a useful content width. Inside it, align trailing switches and associated actions to the item edge; keep the label and explanation together. For choices that need more space, allow the value region to reflow below its label. Input editors can use top labels. A global fixed label width or control cap is not an alternative to understanding the content type.

If `usable width < label minimum + gap + control minimum`, put the control under the label at the same text ruler. Recompute from current font measurement. Do not create an arbitrary 520px subcolumn or breakpoint that ignores 200% text. A selector's required width includes icons, labels, gaps, segment padding, outer track padding and borders.

Groups have a closer internal rhythm than their external separation. Start with 8px inside icon/label clusters, 12–16px between related rows, and 24–32px between meaningful groups. These are project starting tokens; adjust the shared role when content requires it. Do not separate an explanation from its action to fill spare height.

Cards hold independent subjects or entry points. A setting may own a surface containing its label, value, effect and action; a section around such items usually needs only heading and spacing. Information panels grow to fit a short explanation. Within a setting surface, use the information icon and supporting text without another frame. A screen crowded by help may need shorter help or progressive disclosure, not smaller type.

## Type and weight

Official basis: Material distinguishes display/headline/title/body/label roles and combines ordinary and emphasized styles for hierarchy. Long passages use body typography. The current M3 page requires attention to language-dependent line height; a Chinese fallback font cannot be measured as Latin Roboto. See [source notes](material-sources.md#typography-and-icons).

At 100% interface scale, use these project roles. Specify **size, line height and real font weight together** in shared primitives. An outer `text_size` does not establish a component's internal label style.

| Role | Size / line box | Weight | Icon treatment |
| --- | --- | --- | --- |
| Page heading | 30 / 36px (hero 40 / 48px) | Regular, about 400 | Text only; the app bar or settings navigation marks the destination |
| Section title above a group | 14 / 20px, secondary text color | Medium, about 500 | None; 20px inset to the tiles' text |
| Action, navigation, main field label | 14–15 / 22px (settings tiles: 16–17px) | Medium, about 500 | Meaningful icon plus 8px gap |
| Field value / ordinary UI body | 14 / 22px (settings: 16px) | Regular, about 400 | No compulsory decorative icon |
| Necessary metadata / support | 12 / 18px (settings: 14px) | Regular, about 400 | Status icon only when it communicates state |
| Note-reading body | Start at 16 / 26px, adapt to the reader's scale | Regular, about 400 | Preserve semantic Markdown formatting; no icon per paragraph |

“Icon + bold label” applies to the UI's scan and action layer. It does not bold filenames, values, every subtitle, information-panel prose or a generated note. Never manufacture importance by emphasizing everything. Short headings/labels have no terminal period; explanatory sentences remain normal prose.

Use the actual font face and fallback. Measure Chinese, mixed Chinese/Latin, numbers, descenders and long filenames. If glyphs exceed the proposed line box, increase the line box or control height for that role. Do not crop glyphs, apply negative line height or invent per-page vertical offsets.

### Text measurement contract

Distinguish four rectangles/lines when reviewing a control:

1. **Layout bounds:** parent space, padding, border and shrink limits.
2. **Line box:** line-height region used for spacing and centering.
3. **Glyph ink and baseline:** shaped text, ascent/descent and fallback metrics.
4. **Painted bounds:** container, selection, pointer overlay and focus treatment.

For single-line controls, center the line box and icon box on the control centerline, then inspect optical alignment in the native app. For multiline text, place the icon with the first line and flow remaining lines below that line's text start. Do not center a small icon halfway down a three-line explanation.

Center the **whole icon/label cluster** within a pill or segment. Centering text in a flex child with an icon outside it produces a visibly off-center control. Reserve a selected icon's slot or use a stable icon so selection does not make the label jump.

Use real text measurement. Character count × font size is not a valid width for shaped mixed-script labels. Use tabular figures for changing counters/progress when supported and preserve their slot width. Percentage updates must not move neighboring status text.

### Icons need optical alignment

Shared navigation, actions and settings use the Lucide outline icons in `desktop/assets/outline`, resolved centrally in `desktop/src/icons.rs`; brand marks and meanings without an outline match keep their existing assets. Settings navigation stays at 14px while settings content uses the larger sizes in the type table.

The Material icon source allows 20dp optical icons and a 40dp target for mouse/keyboard layouts. That supports the project's compact desktop baseline, not a universal mobile target rule. Keep the current Material family consistent. Existing legacy Material Icons are not automatically variable Material Symbols; do not pretend they expose weight/grade/optical-size axes.

Use a shared 20px icon slot for normal 14px UI labels, judged as a component cluster. For inline glyph-style Symbols, M3 describes matching size/weight and lowering the symbol baseline by roughly 11.5% of text size. Apply that only to an inline-symbol renderer with relevant metrics. An SVG centered by flex must not also receive that baseline correction. Shared optical adjustments need native checks with multiple shapes.

Choose recognizable icons with optical weight that fits medium-weight labels. Do not simulate boldness by scaling the bounding box or mixing stroke families. Compare a narrow icon, a circle and a wide icon beside the same label. Their slots agree; their ink need not occupy identical shapes.

## Explanation and reading

Optional explanation goes in a quiet information panel or discoverable detail beside the relevant choice. A short panel has one information icon, regular body text and natural height. Add a heading only when useful. Several paragraphs or technical output belong in structured expanded detail. Do not repeat a label as the panel title.

Tooltips label icon-only controls or add optional context; they never contain the only explanation of a dangerous action, required setting, failure or current value. Modals ask for necessary decisions. Keep material consequences and errors beside the initiating action even while a form scrolls.

The reader is a content surface. Use a bounded prose column, ordinary paragraph weight, useful paragraph spacing and distinct heading levels. As a project starting point, test roughly 35–50 Chinese characters or 60–85 Latin characters per line instead of full-window prose. Measure real content; mixed script, lists and code differ. At enlarged type, keep the column within its pane and reflow without shrinking the chosen font.

## Native review evidence

Record the pane width/text scale, chosen rulers and actual deviations. Compare the title, first group, representative rows, main action, expanded information panel and focused edge control. Include a long mixed-script value. A useful capture shows the complete grouping and its geometry, not just an isolated component.

Repeat at narrow/default/wide windows and offered text scales. Record whether rows wrap, stack or change pane arrangement and whether actions remain associated with their content. “No overlap” is not a complete layout review.
