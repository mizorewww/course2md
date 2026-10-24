# Shared design system

This is the maintained course2md adaptation. Rules and dimensions in this file are project choices unless explicitly identified as official. Rejected native screens are regression evidence; they do not substitute for judging a complete new composition or establish design acceptance.

Read [layout, type and icons](layout-and-type.md) for the concrete ruler and measurement contract, and [states and motion](states-and-motion.md) for combined states, progress, scrolling and animation. The [official source guide](material-sources.md) links real downloaded content and explains which M3 content is available only as short quotations.

## Primary guidance and deliberate choices

Material's official layout guidance uses global alignment lines, consistent arrangements and proximity to signal hierarchy. Choose the content pane and its title, icon, text and control rulers before selecting cards or padding. A whole screen needs a primary task, necessary information and subordinate configuration. A short form should not become a decorative card grid. The detailed project contract is in [layout and type](layout-and-type.md#establish-rulers-before-spacing).

- [Material selection controls](https://m3.material.io/components/segmented-buttons/guidelines): related short options, consistent labels, rounded grouping, no wrapped segments, restrained width on large panes. The site now recommends connected button groups for Expressive users; course2md adopts the shared selection principles and a restrained capsule treatment, not an exact Expressive implementation.
- [Material button groups](https://m3.material.io/components/button-groups/overview): related controls form a coherent group. Reuse grouping and state treatment without giving independent commands single-choice behavior.
- [Material states](https://m3.material.io/foundations/interaction/states/overview): pointer feedback and focus add to the component's meaning. Keep selection present while these states are active.
- [Apple design principles](https://developer.apple.com/design/human-interface-guidelines/design-principles) and [layout](https://developer.apple.com/design/human-interface-guidelines/layout): judge the interface through purpose, hierarchy, alignment and grouping. Fewer inconsistent elements matter more than adding another decorative effect.
- [Apple motion](https://developer.apple.com/design/human-interface-guidelines/motion): use movement to explain changes while retaining context. A mechanical intermediate frame does not establish a natural transition.

## Shared roles

| Role | Composition | Implementation |
| --- | --- | --- |
| Main action | Filled accent, icon + short verb; one per active region | `primary_pill` (40/48/56px) |
| Weighted secondary action | Tonal secondary container | `tonal_pill` |
| Secondary action | Outline border with accent label; same shape and height | `outline_pill` |
| Inline action | Accent text button, or quiet gray label beside the information it acts on | `text_button` / `quiet` |
| Icon action | Round 40–48px target; tonal for back/reset | `icon_button` |
| Peer selection | Connected button group: 2px gaps, outer ends round, 8px inner corners; the selected option morphs into an accent pill | `SingleChoiceGroup` |
| Grouped items | Connected surface tiles on the surface-container ground: 24px outside corners, 6px inside, 2px gaps | `tile` / `list_tile` / `SettingsGroup` |
| Peer navigation | Same capsule geometry and state layers; tab semantics | `SingleChoiceGroup::tabs` |
| Sidebar | Leading label, shared tonal capsule, no external underline/side stripe | Vertical navigation variant |
| Data entry | Visible rounded field; label/value distinction, errors below | Single-line `Input`, shared control height |
| Preference | Left label, right control slot; move control below when necessary | `settings_row` |
| Detail | Group title, aligned label/value rows, natural long-value height | `settings_detail_group` / `settings_detail_row` |
| Status | Icon and short text together; adjacent to the thing described | `badge` |
| Information panel | Quiet tonal surface, one information icon, short regular-weight prose | Shared help treatment, natural height; optional explanation rather than a modal interruption |

The 40px control baseline, UI type scale, spacing steps, preview geometry and transition durations are project decisions. The complete [type table](layout-and-type.md#type-and-weight) defines size, line box, weight and icon roles. Main UI labels use meaningful icons and medium weight; values, explanatory prose and note-reading text stay regular. Do not present these as exact official requirements. Sizes scale with type; radii and decorative borders do not determine text measurement. Ordinary controls need at least `max(40 × scale, actual line-box height + top/bottom padding)`; multiline surfaces grow naturally.

## Theme roles and painted bounds

Map each source palette into application roles before sharing it with pages and component internals. Body and supporting text establish the reading hierarchy; accent distinguishes actions and selection; status colors identify status. A terminal or editor theme's foreground may need a documented adaptation for a document interface. Correct a failed role mapping centrally, preserve the stored palette identity and light/dark choices, and record retained upstream colors versus application adaptations in `desktop/assets/themes/SOURCES.md`. Previews use that same mapping.

A shared control's geometry includes its rendered label, icon, internal padding and the paint added by selection, hover, press and focus. Matching outer heights alone does not establish a common control. In a clipped or scrolling container, reserve room for outward paint or use a consistent inset treatment so the complete state remains visible at the content edges. Fix this contract in the shared control/container relationship, not by hiding focus or adding an exception around each affected field. Review edge controls in their actual native states as well as the resting layout.

Selection is a persistent base beneath one transient state overlay. Do not add multiple state opacities together or let hover replace the selected surface. Scrollbar appearance must not reflow its content. Use the [state contracts](states-and-motion.md) for implementation and native observation.

## Whole-screen comparisons

For a product flow, start with [task decisions and continuity](interaction.md): establish whether the user can reach the intended result and return from interruptions before assessing composition. An orderly set of redundant steps is still a poor interaction.

Inspect the workbench, library, tasks, reader and settings side by side at default size. The forward action should attract more attention than navigation. Related controls must align even when their label lengths differ. Empty screens should not look like disabled populated screens. Expanded details should add a structured explanation rather than reveal an undifferentiated text dump.

Measure a selector including track padding alongside an input and a button. Then click, hover and focus its selected item. Changing these states must preserve the same selection and avoid shifting nearby text. Compare the initial and enlarged-type layouts: the latter is a new layout constraint, not simply a magnified screenshot.

For every new wrapper, ask what relationship it communicates. Remove it if spacing and a single heading already express that relationship. Do not remove a genuinely useful boundary solely to reduce the number of borders.
