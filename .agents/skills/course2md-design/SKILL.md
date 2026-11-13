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

**A migration keeps the existing look.** Moving a page to another component, or to the kit, changes how it is built, not how it reads. A distinctive look the kit lacks (color, shape, hierarchy, spacing, emphasis) is added to the kit as a generic capability, and the page uses it; it is neither flattened to the kit's default nor hand-painted in the page. A change of component by meaning keeps the look it replaces. Before calling a migration done, compare the old and new build side by side by eye. See [the design language](references/design-language.md).

## Read the reference that controls the decision

- For every UI change, read [the design language](references/design-language.md), [the shared system](references/system.md) and [layout, type and icons](references/layout-and-type.md). Establish the screen's alignment axes and information hierarchy before changing spacing.
- When implementing on the kit (choosing a component, adding a symbol, a timer), read [kit usage](references/kit-usage.md).
- For interactions, asynchronous work, scrolling or animation, read [states and motion](references/states-and-motion.md).
- For first use, conversion, navigation or recovery, read [task decisions and continuity](references/interaction.md).
- For preferences, services, models and diagnostics, also read [settings composition](references/settings.md).
- Before a whole-product review, read [composition decisions](references/composition-review.md). Start with the user's task and information worth showing; a geometrically tidy screen may still need to be rejected.

## Product decisions

The task is video → readable note. Preview comes before conversion: keep the link input visible, show the video card immediately below it, and let only an explicit “开始转换” action submit work. Enter and “预览” read or refresh metadata without creating a conversion intent. Keep the preview on the workbench; no separate planning page is needed. Processing options have one home, Settings: every conversion, including a queued, retried or resumed task, runs with the settings current when its work starts, so a change in Settings takes effect for the next work that runs. There are no per-video or per-task copies of options, no saved plan to show, and nothing on the workbench that edits options for one video. A usable result opens directly when the person is still following that task; a separate completion page must not stand between the task and the note. Background completion must not steal someone's current location. Ask only for an unresolved user decision or actionable obstacle.

Home is one bounded column (760px) at every width, in all palettes, with preview confirmation: a centered hero title, the hero link field (the kit's `TextField::hero`: a 72dp surface pill, ringed when focused or filled, holding its labeled tonal preview action), then the video card, then 正在处理 and 最近的笔记. The video card stands alone as one surface: the preview (cover, title, source, what its text comes from, obstacles), or the current input's own task in its place (running status, attention or result). Its actions that belong together (更换视频 with 开始转换) wrap as one trailing group. The card's text-source chip describes the video's own input: which of its subtitle tracks to use, 重新读取字幕 and, for a local video, 选择字幕文件. Whether subtitles or speech recognition are used is Settings' 文字来源, which the chip shows but never overrides; set to speech recognition, it is information, not a choice. A video reopened on Home from a waiting task says beside 开始转换 that converting takes that task's place, and offers to continue it instead. Pasting another link, or dropping or choosing another file (the local drop zone stays above a running task), starts the next input. A video recognized on this computer shows its model status on the card before submission. Recent notes are one surface with 16:9 covers carrying their duration, parted by hairlines inset past the cover. Keep actual errors, missing models and absent services visible on the card, with a way to the setting or service that fixes them, instead of copying fictional ready states from the reference. A note takes its video's name and goes to the default library, uncategorized; renaming and filing happen in the library.

Across pages: an app bar with the course2md mark and name on the left and neutral destinations 笔记库 / 任务 (only when tasks exist) / 设置 with icon and label on the right, tonal when current; settings replaces it with its own navigation pane (back button, title, search, categories). Page titles are text only; section titles sit above segmented lists, one item per thing, with content that belongs to an item kept inside it. Keep explanatory text to consequences, obstacles and live status; do not add help that restates a label or a nearby control.

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
