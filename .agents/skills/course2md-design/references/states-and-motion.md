# States, motion and scrolling

Use for asynchronous work, progress, overlays and scroll containers. The kit's components paint their own state layers and play their own motion; this file covers what the app decides.

## Persistent meaning and transient feedback

Selection is durable; hover, focus and press are transient and sit over it. Selected hover must not replace the selected surface, and selection persists as focus moves or the pointer leaves. Pointer entry, press and release do not change text width or move neighbors. Disabled controls perform no action. Apply feedback to the actionable child, not its entire non-interactive toolbar. Inspect selected + hover, selected + press and selected + keyboard focus separately in the native app.

## Motion has an object and a reason

Before animating, state what changed and what remains a landmark. Use the kit's `motion`, never a page-local curve or timer. The app does not tune curves or durations.

| Intent | Animate | Kit | Stable elements |
| --- | --- | --- | --- |
| Hover, press | The component's state layer | The component | Bounds, text and icon |
| Choice changes | The selected option | `ConnectedButtonGroup`, `motion::selection_value` | Labels, segment widths and neighboring layout |
| Theme changes | Color roles | `theme::advance_component_theme` | Text geometry, chooser height and page location |
| Expand detail | One content region | `motion::disclosure` | Group heading and initial reading anchor |
| A page region arrives | One region | `motion::page_enter`, `state_enter` | Navigation and reading anchor |
| Show or dismiss an overlay | The overlay as one unit | The component | Background location and initiating control |
| Active process | A real indicator | Progress components | Input context, state text and actions |

Use one principal animated response per intent. A theme click must not also re-enter the whole page, move every label and resize the preset grid. Ordinary category navigation retains stable composition. A disclosure stays subordinate to its heading; do not vertically recenter the whole panel when detail height changes. Keep progress motion within its status region. Avoid overlapping partly transparent paragraphs during replacement.

With reduced motion, show settled state directly and keep activity text truthful. Never delay an action until animation finishes. Verify retargeting after a rapid click, exit during animation and interrupted asynchronous work.

## Conversion states and result access

| State | User sees / can do | Transition |
| --- | --- | --- |
| Empty input | One clear source input and primary import action | No empty document-management task |
| Reading preview | Input stays visible; show metadata/card directly beneath it, loading status and cancellation | Stop at preview; never submit without confirmation |
| Preview ready | Video card, editable options and an explicit “开始转换” | Submit only on that action |
| Confirmed prerequisites | Keep the confirmed task context and actionable obstacles visible | Continue the confirmed intent once checks succeed |
| Decision needed | Exact obstacle/consequence, current values and a direct choice/repair | Resume the original intent with its data intact |
| Processing | The task replaces the video card with its real phase and one loading/progress treatment; the options become its read-only plan with 设置下一个视频 | Permit background continuation and navigation; the next video starts from saved defaults |
| Partial / failed | Existing input/results, specific reason and scoped recovery | Retry failed work when supported |
| Complete | Open the usable note when still following this task; otherwise update its status quietly | No planning approval or completion landing page before reading |

These states stay on the workbench. Preview and conversion are distinct user actions; alternate source routes and restored drafts must preserve that boundary. Internal plan details and completion history must not add extra pages.

Determinate progress represents actual measured completion, never a timer or equal fractions assigned to unequal backend stages. Without a meaningful denominator, show indeterminate progress and the real phase. Switch when measurements become available without resetting surrounding content.

Use one indicator per grouped operation. Independently started operations may have their own. Linear progress belongs to its loading container; circular progress belongs where content is loading or inside the acknowledged action. Long work does not require a blocking wait screen. Do not invent percentages, durations, ETA, hardware availability or successful persistence.

## Scrollbars

This is the user's requested desktop behavior.

- Show while the region scrolls, the pointer is over its scrollable area/gutter, or the thumb is dragged. Keep visible through inertia and active dragging even when the pointer leaves.
- When interaction ends, fade after a short shared idle grace period. Avoid flicker between wheel events. Re-entry cancels pending hiding; dragging retains its hit target.
- Honor the existing macOS preference for always-visible scrollbars when exposed by the app/platform. Reduced motion can make visibility changes immediate.
- Overlay within a consistent gutter. Visibility changes must not alter content width, text wrapping, control positions or scroll offset. Shared clearance prevents covering trailing text/actions.
- Hide when there is no scrollable extent. Keep ordinary wheel, trackpad and keyboard behavior and a usable thumb target. Distinguish scrolling content from fixed navigation/dialog actions.

Review idle, hovered, wheel-active, inertial, thumb-dragged, post-idle and no-overflow states. Record actual interactions; an invisible thumb in one still image does not demonstrate correct behavior.

## Dialogs and contextual help

A dialog interrupts for one consequential task. If it scrolls, title and actions remain available and its background does not scroll with it. Critical information must not exist only in a tooltip.

State the consequence specifically, group fields/errors and provide clear confirm/cancel actions. A scrolling service editor keeps its action and external-request consequence visible. Dismissal restores the initiating context without publishing an unfinished edit. See [business invariants](interaction.md#business-invariants).

Information panels explain in place. They can expand into structured detail without becoming a modal workflow.
