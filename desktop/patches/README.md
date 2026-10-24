# Reviewed GPUI source patches

`scripts/sources.py` checks each patch against the source revision, then applies it to the generated `.deps` worktrees; developer checkouts are never patched. Unrecognized edits are rejected without changing any files. When upgrading the locked revisions, review upstream changes before adjusting a patch, and never fix an app build by editing `.deps`.

| Patch | Purpose |
| --- | --- |
| `zed-input-accessibility.patch` | Accessibility-only focus delegate for controls whose semantic frame and editor are separate elements; adds no keyboard target or Tab stop. |
| `component-input-accessibility.patch` | Connects each input's semantic frame to the editor focus, exposes text runs and selection, reports read-only/disabled state, skips disabled inputs in Tab order; password values stay hidden. |
| `zed-macos-window.patch` | Forwards window focus to AccessKit and adds an AppKit redraw fallback (32 ms after the last request or completed callback) for when the display link stops making progress. |
| `component-tooltip-lifecycle.patch` | Dismisses a window's tooltip before navigation can remove its trigger and cancels delayed tooltips. |
| `component-switch-geometry.patch` | Optional absolute/rem switch track height with proportional thumb and inset, plus the opt-in `reference_geometry()` (52×32 track) and `thumb_icons()` (Material 3 switch with icons: 24 px thumbs, outline-colored unchecked thumb). Default sizes are unchanged. |
| `component-button-text-size.patch` | Opt-in `label_text_size` for button labels. |
| `component-menu-style.patch` | Opt-in global `MenuStyle` for popup, dropdown and context menus: container and row geometry, one highlight layer, tonal checked rows with a trailing check, danger items, hairline separators, group labels and a fade-in entrance. Without the global, menus are unchanged. |
| `component-selectable-text-highlights.patch` | Opt-in `highlights` style runs and an `on_layout` measurement callback for `SelectableText`, used by reader find; runs outside the current selection skip collecting the selected text. |
| `zed-cjk-line-break.patch` | Text wrapping never starts a line with full-width closing punctuation such as `，`, `。` or `）` (UAX #14 no-break-before classes for Chinese and Japanese). |
| `zed-metal-frame-lifetime.patch` | One autorelease pool per Metal frame so drawables are returned promptly. |
| `zed-metal-present-stages.patch` | Opt-in profiler spans for each Metal presentation stage; compiled out of non-profiler builds. |

Validate after changing a patch:

```sh
python3 -m unittest discover -s desktop/scripts -p test_sources.py
python3 desktop/scripts/sources.py --locked
cargo check --manifest-path desktop/Cargo.toml
```
