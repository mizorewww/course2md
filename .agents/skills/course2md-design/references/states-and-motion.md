# States, motion and scrolling

Use for shared controls, asynchronous work, progress, overlays and scroll containers. Read [the source notes](material-sources.md#states-and-motion) and the complete licensed [motion](material/offline/android-motion.md), [ripple](material/offline/web-ripple.md) and [progress](material/offline/android-progress.md) documentation as relevant. Recipes and durations below are **course2md choices** unless explicitly identified as official values.

## Persistent meaning and transient feedback

Official basis: M3 puts a state overlay between container and content. Selection coexists with hover, focus and press. Only one transient state layer applies at once. Current published opacities are hover 8%, focus 10%, press 10% and drag 16%; older 12% focus/press summaries are not this captured guidance.

Determine persistent meaning first, then feedback:

| Layer | Responsibility |
| --- | --- |
| Base / selected container | Durable choice or navigation location |
| One state overlay | Immediate hover, focus, press or drag feedback in the current content's semantic color |
| Content | Stationary icon and label with the appropriate on-container color |
| Focus indicator where needed | Keyboard location, visible within the clipping contract |

For combined pointer states, choose one overlay, for example drag → press → focus → hover. Do not sum four alpha values. Preserve a keyboard focus indicator when it carries additional meaning. Disabled controls perform no action and do not replay hover/press motion. Apply feedback to the actionable child, not its entire non-interactive toolbar.

Selected hover must not replace a tonal base with an unselected background. Selection persists as focus moves or the pointer leaves. Pointer entry, press and release do not change text width or move neighbors. Inspect selected + hover, selected + press and selected + keyboard focus separately in the native app.

Painted bounds include ring, border and overlay. Keep them visible by using consistent inset paint or a shared reserved gutter. Do not remove focus to fix clipping or give each page's edge control a unique spacer.

## Motion has an object and a reason

Official basis: M3 separates spatial movement from color/opacity effects; effects do not overshoot. It provides a functional Standard scheme for utilitarian products. Transitions still use easing/duration, with coherent direction, stable content and clean fades. Common transitions should not move every child independently.

Before animating, state what changed and what remains a landmark. Use a shared motion role, not a page-local curve/timer. These are project starting recipes, not official token mappings:

| Intent | Animate | Starting range | Stable elements |
| --- | --- | --- | --- |
| Hover/press | One low-emphasis state layer | Immediate | Bounds, text and icon |
| Choice changes | Selected option's shape and color | Fast spatial spring (0.9 / 1400) | Labels, segment widths and neighboring layout |
| Theme changes | Semantic color roles | Slow effects spring (1 / 800) | Text geometry, chooser height and page location |
| Expand detail / page region arrives | One content region's reveal/extent or offset, its opacity separately | Default spatial (0.9 / 700) + default effects (1 / 1600) | Group heading and initial reading anchor |
| Show/dismiss overlay | Overlay as one unit | About 160ms; dismissal may be shorter | Background location and initiating control |
| Active process | Real loading/progress indicator | While its process is active | Input context, state text and actions |

The springs are Material's Standard motion scheme tokens (damping / stiffness, from [Material Android motion](material/offline/android-motion.md)), implemented once in `motion.rs` (`Spring`, `FAST_SPATIAL`, `DEFAULT_SPATIAL`, `DEFAULT_EFFECTS`, `SLOW_EFFECTS`); spatial springs move and reshape, effects springs fade color and opacity and never overshoot. A matching constant does not prove good motion. Selectors and gestures retarget from their current presentation value immediately. Do not finish an obsolete animation, queue a new one or restart from an old endpoint after a rapid click.

Use one principal animated response per intent. A theme click must not also re-enter/fade the whole page, move every label and resize the preset grid. Ordinary category navigation retains stable composition with restrained feedback. A disclosure stays subordinate to its heading; do not vertically recenter the whole panel when detail height changes.

Avoid overlapping partly transparent paragraphs during replacement. Keep fades short and orientation clear. Any spatial transition has one coherent direction. Keep progress motion within its status region.

With existing reduced motion, show settled state directly, suppress decorative movement and keep activity text truthful. Never delay an action until animation finishes. Verify retargeting, exit during animation and interrupted asynchronous work.

## Conversion states and result access

| State | User sees / can do | Transition |
| --- | --- | --- |
| Empty input | One clear source input and primary import action | No empty document-management task |
| Reading preview | Input stays visible; show metadata/card directly beneath it, loading status and cancellation | Stop at preview; never submit without confirmation |
| Preview ready | Video card, editable options and an explicit “开始转换” | Submit only on that action |
| Confirmed prerequisites | Keep the confirmed task context and actionable obstacles visible | Continue the confirmed intent once checks succeed |
| Decision needed | Exact obstacle/consequence, current values and a direct choice/repair | Resume the original intent with its data intact |
| Processing | Real phase and one primary loading/progress treatment | Permit background continuation and navigation |
| Partial / failed | Existing input/results, specific reason and scoped recovery | Retry failed work when supported |
| Complete | Open the usable note when still following this task; otherwise update its status quietly | No planning approval or completion landing page before reading |

These states stay on the workbench. Preview and conversion are distinct user actions; alternate source routes and restored drafts must preserve that boundary. Internal plan details and completion history must not add extra pages.

Determinate progress represents actual measured completion, never a timer or equal fractions assigned to unequal backend stages. Without a meaningful denominator, show indeterminate progress and the real phase. Switch when measurements become available without resetting surrounding content.

Use one indicator per grouped operation. Independently started operations may have their own. Keep each process consistent throughout the app. Linear progress belongs to its loading container; circular progress belongs where content is loading or inside the acknowledged action. Long work does not require a blocking wait screen. Do not invent percentages, durations, ETA, hardware availability or successful persistence.

## Scrollbars

This is the user's requested **desktop project behavior**, not a claim about M3 mobile scrollbars.

- Show while the region scrolls, the pointer is over its scrollable area/gutter, or the thumb is dragged. Keep visible through inertia and active dragging even when the pointer leaves.
- When interaction ends, fade after a short shared idle grace period. Avoid flicker between wheel events. Re-entry cancels pending hiding; dragging retains its hit target.
- Honor the existing macOS preference for always-visible scrollbars when exposed by the app/platform. Reduced motion can make visibility changes immediate.
- Overlay within a consistent gutter. Visibility changes must not alter content width, text wrapping, control positions or scroll offset. Shared clearance prevents covering trailing text/actions.
- Hide when there is no scrollable extent. Keep ordinary wheel, trackpad and keyboard behavior and a usable thumb target. Distinguish scrolling content from fixed navigation/dialog actions.

Review idle, hovered, wheel-active, inertial, thumb-dragged, post-idle and no-overflow states. Record actual interactions; an invisible thumb in one still image does not demonstrate correct behavior.

## Dialogs and contextual help

Official basis: M3 dialogs interrupt for one consequential task. If a dialog scrolls, title/actions remain available and its background does not scroll with it. Critical information must not exist only in a tooltip.

State the consequence specifically, group fields/errors and provide clear confirm/cancel actions. A scrolling service editor keeps its action and external-request consequence visible. Dismissal restores the initiating context without publishing an unfinished edit. See [business invariants](interaction.md#business-invariants).

Information panels explain in place. They can expand into structured detail without becoming a modal workflow. Share their type, icon, surface and spacing roles across first use, workbench and settings.
