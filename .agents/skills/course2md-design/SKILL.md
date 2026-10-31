---
name: course2md-design
description: Design, implement and independently review course2md's native task flows, first use, pages, wording, themes and motion on gpui-kit-material. Not for CLI-only or conversion-engine work.
---

# course2md desktop design

This is the project's design authority. The app is pure Material 3 on [`gpui-kit-material`](https://github.com/mizorewww/gpui-kit-material). This skill describes the target, even where the app has not reached it yet. The user's current instructions take precedence. Rejected native screens are evidence of problems, never a template to reproduce or an acceptance result. The Material canvas “course2md Material 重设计” (claude.ai artifact CkLqb68g6mjYR1PJFLxvhQ) is a reference for composition; where its geometry differs from the kit, the kit wins.

## Who owns what

**The kit owns appearance:** color roles and palettes (`color`, `theme`), shape, type, layout metrics, elevation and state layers (`tokens`), motion (`motion`), Material Symbols (`icons`) and controls (`components`). Read its `AGENTS.md`, `docs/ARCHITECTURE.md` and rustdoc, and run its gallery to see a component's variants. It records the Material source of each value, so this skill does not.

**The app owns the product:** task flows, what each page shows and in what order, wording, content widths, and macOS window, menu and keyboard conventions.

A page does not define colors, radii, control sizes, curves or icon files, and does not hand-build a control the kit has. For anything the app seems to need from below, in order: drop the need (change the wording, layout or behavior); write it with existing kit and GPUI Kit APIs; only if those are genuinely awkward, make it generic and propose it to the kit. No feature is dropped: behavior may get simpler, every feature stays.

## Read the reference that controls the decision

- For every UI change, read [the shared system](references/system.md) and [layout, type and icons](references/layout-and-type.md). Establish the screen's alignment axes and information hierarchy before changing spacing.
- For interactions, asynchronous work, scrolling or animation, read [states and motion](references/states-and-motion.md).
- For first use, conversion, navigation or recovery, read [task decisions and continuity](references/interaction.md).
- For preferences, services, models and diagnostics, also read [settings composition](references/settings.md).
- Before a whole-product review, read [composition decisions](references/composition-review.md). Start with the user's task and information worth showing; a geometrically tidy screen may still need to be rejected.

## Product decisions

The task is video → readable note. Preview comes before conversion: keep the link input visible, show the video card immediately below it, and let only an explicit “开始转换” action submit work. Enter and “预览” read or refresh metadata without creating a conversion intent. Keep preview and options on the workbench; no separate planning page is needed. A usable result opens directly when the person is still following that task; a separate completion page must not stand between the task and the note. Background completion must not steal someone's current location. Ask only for an unresolved user decision or actionable obstacle.

Home is one bounded column (760px) at every width, in all palettes, with preview confirmation: a centered hero title, a prominent link field that holds its tonal preview action, then the video unit, then 正在处理 and 最近的笔记. The video unit is the video card (the preview, or the current input's own task in its place: running status, attention or result) with the processing options panel connected to its bottom; with no video the panel stands alone and says it applies to the next video. Its header row (处理选项, what it applies to, a chevron) is the one control that shows or hides the options; the choice persists, and with a preview loaded the header stays visible without scrolling at 1280×780. Card actions that belong together (更换视频 with 开始转换) wrap as one trailing group. Never place the options beside the recent notes or anywhere they could read as belonging to something other than that video. While a submitted task owns the input, the panel shows that task's saved plan read-only and always keeps 设置下一个视频 visible; it never shows editable controls for submitted work. Recent notes are one list with 16:9 covers. Inside the options panel, quiet section titles head each group: a connected button group with its mode hint (the video card, not the hint, states what this video will use), and label-left/switch-right list items. Keep actual errors, missing models and absent services visible instead of copying fictional ready states from the reference.

Across pages: an app bar with the course2md mark and name on the left and 笔记库 / 任务 (only when tasks exist) / 设置 with icon and label on the right; settings replaces it with its own navigation pane (back button, title, search, categories). Page titles are text only; section titles sit above segmented lists, one item per thing, with content that belongs to an item kept inside it. Keep explanatory text to consequences, obstacles and live status; do not add help that restates a label or a nearby control.

Use a meaningful icon plus label for navigation destinations, actions and preference labels. Values, metadata, explanatory sentences and long-form note text retain ordinary body typography. Do not attach an icon to every sentence or make an entire form, error paragraph or note bold.

Keep explanation inside the same setting or resource boundary as its subject, without a second full-width frame. Standalone guidance can have its own low-emphasis panel. Essential field labels, the current selection, actionable errors and the consequence of an action remain visible with that action. A tooltip is supplementary help; a modal dialog is a necessary interruption. These are different roles.

Scrollbars respond to scrolling, hover and dragging, then fade after interaction ends, while honoring the system preference for always-visible scrollbars. Their appearance must not change content width or move text. See [the scrollbar contract](references/states-and-motion.md#scrollbars).

Keep choices with their effects. The app detects metadata, reuses established configuration and supplies supported defaults. Explicit source/model/service choices remain authoritative. First use is skippable and revisitable and helps complete a real task; optional services, appearance and diagnostics are not prerequisites for ordinary conversion.

## Implement

Choose a component by what the user is doing and what it means in Material, not by what a control used to be. Motion comes from the kit's `motion` and component defaults; a page doesn't tune its own curves or durations.

When a kit component looks wrong, find out whether the page misuses it or the kit is wrong, then fix it at that level and inspect every affected caller. A component is not shared in practice when callers replace its inner padding, font or selected state.

Measure actual text, icon, container and painted state bounds. Parent width and the current 100/125/150/200% text scale determine layout; character-count widths, silent clipping and fixed-height multiline surfaces do not. Reflow before related elements collide. Preserve ordinary keyboard operation.

Preserve all palettes and separate light/dark selections, through the kit's `color::palette` and `ThemePreferences`. Previews use the same scheme as the app. A failed save keeps a recovery action and must not claim persistence. Do not perform filesystem or network work during rendering. Labels come from the catalog through `tr!`; the kit ships no strings.

## Review and finish

For a whole-product UI change, use independent subagents to review every affected page and its loading, empty, failure and completion states, including shared-component callers and overlays. Give reviewers the original rejected screens, current native captures and this skill. Do not give them a list of claimed fixes to rubber-stamp. Record concrete findings and how each was resolved; a previous review does not establish a new pass. A reviewer who implements a region must not be its only final reviewer. The main agent owns the final design judgment, including contradictions between regions.

Follow a fresh task and a repeat task through the running native app to a usable note. Check temporary settings/login navigation, return, retry, cancellation and late responses. Then review complete-screen hierarchy and grouping, followed by alignment, actual line/icon geometry, states and motion. Compilation, tests, screenshot dimensions and a duration constant are not design acceptance.

Verify native normal/narrow/wide windows, light/dark appearance, every offered palette and type scale. Inspect resting, hovered, pressed and keyboard-focused selected controls; active and idle scrolling; entering and settling motion; failure and completion. Distinguish screenshot evidence, source inspection and runtime observation, and state any unvisited cases. The user excluded specialist accessibility audits: preserve ordinary keyboard behavior and existing preferences without creating a separate accessibility project.

Use isolated fixtures for failing saves, generation and destructive flows. Test changed behavior meaningfully, keep atomic Git commits by concern, and finish with candid evidence and a runnable reviewed preview. Do not alter release tags or replace published assets during an ordinary UI change.
