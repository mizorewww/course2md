# Task decisions and continuity

Use this reference when designing first use, conversion, result access, configuration handoffs or recovery. It describes decision criteria and business constraints; it does not prescribe a fixed screen sequence or tutorial length.

## Assign work to the right party

The user supplies the content and intent. The app should detect metadata, choose supported local capabilities, reuse established preferences, propose a name and destination, and advance routine processing. A step does not need a separate user command merely because it has a separate backend function.

Show a visible preview before conversion. Keep the link input present above the video card. Enter, preview, local file selection, candidate selection, source retries and login return only prepare or refresh the preview; they must not queue conversion. Show an explicit “开始转换” action with the video preview. Only that action (or an explicitly labeled batch/new-version command) establishes submission intent. After this confirmation, asynchronous prerequisites may continue the requested conversion. Do not add a separate planning or completion page. Open a usable note directly when the person is still following the submitted task; background completion must not take over navigation.

Ask when the app cannot determine the intended video, when an explicit choice cannot be honored, or when continuing changes a material consequence such as sending content to a different service, repeating a possibly billed request, replacing content, or moving/deleting user files. State the consequence and offer a direct way forward. Keep these decisions distinct from ordinary status feedback.

“Automatic” delegates a choice within its stated scope. For example, an automatic local text route can try usable subtitles and then local speech recognition when subtitles are unavailable. A subtitle-only request must not silently become speech recognition, and a local request must not silently become a cloud request. A failed auxiliary probe need not block an otherwise viable task; a failed source identity or access check may do so.

## Defaults and first use

Choose useful defaults before adding a setting or an explanation. Configure reusable capabilities once; expose per-task overrides only when they help the current intent. Clearly distinguish changes for future work from changes to the current input. Do not require users to understand service revision IDs or task serialization to make that distinction.

Use first-use guidance to help people reach value with real content. It can combine a concise introduction, relevant capability choices and contextual help; choose the form from the actual task. Allow skipping, retain completed choices, and provide a way to revisit guidance without repeating it on every launch. Download or account setup should be relevant to the chosen capability, with truthful status and an opportunity to stop. An existing working configuration should not be erased or subjected to another compulsory setup flow.

Present primary choices at the level of the user's outcome or processing location. Recommend a supported default and explain what an automatic choice will actually use. Put implementation choices beneath the relevant route rather than presenting automatic policy, hardware backends and external services as equal decisions. Keep an existing explicit choice visible and authoritative. Model alternatives need concise, supported quality/resource tradeoffs; names and parameter counts alone do not explain which to choose, and unmeasured speed or quality claims are not guidance.

Give a short guide a content-appropriate panel, with a consistent starting position for its heading and progression within the same window. Let content take its natural height and scroll when needed; do not vertically recenter every different-height step or use spare window height to separate related content from its actions. Keep the action group adjacent to the task. An optional step's action should make clear when it proceeds without completing that setup.

Use meaningful icons and emphasized text for the main choices and actions. Keep explanation in a quiet information panel or optional detail, with regular body typography. A title, a short primary choice and a nearby action should establish the flow before optional prose is expanded. Do not hide a consequential service choice, current setting or actionable obstacle merely to shorten the page.

Keep a material consequence, such as sending a test request to an external service, with the action that initiates it. It must remain visible when that action is available, including when the form body scrolls. Explanatory text elsewhere in the step does not establish that relationship.

## Continue, interrupt and return

An accepted request remains an intent while the app checks its prerequisites. Normal completion of a check should advance that intent. Keep cancellation, source replacement and late asynchronous responses tied to the correct input and request; a stale response must not resume abandoned work.

Temporary configuration or login should preserve the originating input, task and reading position and make continuation discoverable. Navigation is not cancellation. Use an explicit cancel/discard action for an unfinished edit when losing it matters; do not publish a private edit merely to preserve navigation continuity.

A completion action should reach the result it names. Processing history and technical logs remain available, but should not be mandatory intermediates before reading an available note. Keep partial results readable and offer recovery for the failed part. Status updates should not steal navigation from someone working elsewhere.

## Business invariants

- Submitted tasks retain their captured source, processing choices and service configuration; updating defaults must not rewrite queued or running work.
- Names, storage associations, notes and existing versions survive failed saves and canceled repairs. A failed save must not claim persistence.
- Repeating an uncertain external request requires a clear user decision. Automatic recovery must not duplicate a possibly completed or billed request.
- A label such as “only this note” has a real scope boundary. Define what happens on a new source, retry, navigation and restart; do not let a temporary choice silently become a permanent default or vice versa.
- Fresh setup, a configured repeat task and a restored interrupted task are distinct states. Review each without assuming evidence for one proves the others.

## Evidence

Judge a flow with an actual native action trace, including the decisions made, extra navigation, changed data and reached result. Screenshots establish visible state; source inspection establishes implementation paths. Neither alone proves that first-time users can complete the task or that a repeat run reuses configuration.
