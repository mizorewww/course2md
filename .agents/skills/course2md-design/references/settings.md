# Settings composition

Read with [the shared system](system.md), [layout and type](layout-and-type.md) and [states and motion](states-and-motion.md).

## Scope and continuity

Use [task decisions and continuity](interaction.md) for settings reached during another task. Keep infrequently changed defaults here and current-task choices near their effects. A missing required service or unavailable model should lead to the relevant repair and back to the originating work, not require users to reconstruct a path through settings categories.

State whether a change applies to future work or the current input. Reuse saved service configuration; immutable versions protect submitted tasks internally and belong in details when they explain a discrepancy. Do not make ordinary service selection a version-management workflow. Preserve unfinished input during temporary navigation, and distinguish canceling an editor from publishing its values.

Model setup should communicate the decision that matters: the recommended usable option, any relevant download, and why someone might change it. Hardware and model identifiers remain available for explicit configuration and diagnosis. Opening settings must not be a prerequisite for ordinary conversion when usable defaults already exist.

## Layout

Use a leading navigation pane when the current window and text scale leave a useful content pane; otherwise use a compact category control. The pane holds a back button with the 设置 title, a settings search field and the categories as list items (icon, title, one-line summary), grouped by relationship, the current one selected. Keep navigation outside content scrolling and keep tab/arrow-key behavior.

Within the content pane (at most 760px), give each group a section title above a segmented list, one item per setting. Begin with meaningful groups such as “关于” or “运行检查”. Do not repeat the category name directly above them. A form group has one label/control alignment system: its outer edges align with other groups. Do not constrain a selector to an unrelated subcolumn while allowing its help to span the pane.

A complete setting contains its label, value, explanation and related action. Use the same item boundary for all of them. Supporting text uses the information icon within that boundary; do not wrap every sentence in a second full-width panel. Input editors use top labels, and model/service/location entries use resource layouts; they are not all the same two-column form.

The content pane scrolls independently of navigation. Dialog titles/actions and external-request consequences stay available when their form body scrolls.

## Appearance

Appearance mode and the independently saved light/dark palettes are different choices. Show stable light and dark preset previews; open a clearly titled palette chooser containing actual previews. The number of available palettes must not move unrelated settings when the appearance mode changes. Preserve all ten palettes and all existing persisted values.

Use the same label/control row for mode, font size and the motion switch. A chooser calculates columns from its actual inner width and the current label size. Long palette names remain readable. Immediate changes and persistence failure must be visible in the chooser; failure must not silently close it or claim the new choice is saved.

## Application and diagnostics

Group product identity/version, related actions and licenses in reading order. Keep actions near their information. Use compact capability/status rows; place program, model and device details in distinct subordinate groups when expanded. Be explicit about the difference between an available runtime and a model that has actually been loaded. Do not show irrelevant platform internals at the same level as the user's next action.

## Native evidence

Check the whole initial screen before scrolling, then expanded diagnostic/service/model states, in normal, narrow and wide windows at default and enlarged text. Selection movement and theme-color interpolation should read as one response; avoid category/page re-entry fades and changing grid height on the same click.
