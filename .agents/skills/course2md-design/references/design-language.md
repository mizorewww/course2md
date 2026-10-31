# The design language

What makes the app read as itself, on every page and palette. A page that loses one of these has lost part of the product's character, even if it is correct Material. Each is a kit capability, so nothing here is painted by hand. `docs/split/DESIGN-RESTORE.md` has the before-and-after, element by element, while that directory exists.

## Color carries meaning, quietly

- **Tinted circles mark what a thing is.** Settings categories, accounts, services, library locations, diagnosis results and first-use choices lead with a hue circle: `tinted_icon`, or `ListLeading::Tinted` in a list item, 40 in navigation and tiles and 48 (`ui::RESOURCE_ICON`) on a resource row. Selected, the circle turns to the surface.
- **Families by place:** Appearance orange, generation blue, services teal, storage amber, application green; Bilibili pink, AI purple, speech teal, library amber; a task that waits for the person `Danger`, or amber when merely paused. `TintFamily` harmonizes each toward the palette, so never reach for a fixed hue, and never map a category onto a container role: those are one accent in the source palettes.
- **One color per region.** The ground, the surface and ink stay neutral so the main action and the selection are the only saturated things: neutral app-bar destinations and back buttons, neutral unselected segments, outlined buttons with a primary label.
- **Warnings are warm, failures red.** A partial note is a warning, not an error.

## Shape is generous

- The hero field and search bars are pills. Panels and cards that stand alone take `RADIUS_HERO` (28); note cards and tiles `TILE_RADIUS` (24); their inset covers `RADIUS_LARGE` (16); figures `RADIUS_LARGE_INCREASED` (20); form fields 12 (`TextFieldShape::Rounded`).
- Note cards inset their cover on the surface. A cover never runs to the card's edge, and a card's commands sit on its title row, not over the image.
- Dialogs are on the surface (`.container(ColorRole::SURFACE)`), not the kit's default gray.

## Hierarchy comes from surfaces, then hairlines

- One subject is one surface. The processing options are a single card; the note list and recent notes are single cards whose rows are parted by a hairline inset past the cover. Connected tiles are for peer settings, not for parts of one thing.
- Page titles are `HeadlineMedium` (`ui::page_title`), on the same line as the settings title.
- Rows hold what they are about: a status chip at the row's end, an add row's plus in the icon column with no chevron.
- Facts are chips, not a sentence: note facts as neutral chips, diagnosis and task steps as `status_chip_outlined`.

## The moments that carry the product

- **Home:** the hero title, the hero field with 「预览视频」 labeled, the drop hint with its icon, the video card with a medium 开始转换.
- **Reading a source:** the card's own skeleton, so the result replaces it in place.
- **Reader:** connected view pills on the page ground, flat contents, a tonal 从此处观看 at each section.
- **First use:** the numbered `Stepper` and engine tiles, each engine visible with its state.

## How to keep it

When a screen looks plainer than it did, the migration is not finished. Compare builds side by side by eye before accepting a change. If a look needs something the kit lacks, add it there first: one PR per concern, generic, with its roles working in every palette, tested and in the gallery.
