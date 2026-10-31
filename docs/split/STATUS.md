# Split status

Update this file in the same commit as the work it records.

**Current phase:** Wave 1 (see `PLAN.md` § Phases). P0, P1, P2 are finished; D12 goes with the kit scrollbar, T7 with `split/tooling`.
**Dependency form:** git, kit `a5acb0c`, six `[patch]` entries. Windows and Linux depend on the desktop workflow run (dispatched on `split/workspace`).

## Done

- P0: branch `split/workspace`, kit worktree `course2md-integration`, documents, scaffold residue removed.
- P1: the desktop builds and runs on the `gpui-kit-material` stack: `gpui-kit` facade, `gpui-base`,
  `gpui-kit-material`, the kit's `[patch]` tables, `gpui-fast`, toolchain 1.99.1. Source pinning
  (`sources.py`, `sources.lock.json`, `patches/`, `.deps`) is gone.

## Patch triage

Superseded by `REDUCTION.md` § Kit audit: the kit carries no patch for this app.

## P1 verification

On kit merge `03b38ec` (which includes `origin/main` at `c6a1476`), Rust 1.99.1, Xcode 27.2 beta for Metal.

- `desktop`: `cargo fmt --check`; `cargo test` 303 passed, 3 ignored (they need yt-dlp, ffmpeg or network);
  `cargo clippy --all-targets --no-deps -- -D warnings`; `cargo check --features capture,performance --all-targets`.
- Kit, affected packages only, on the earlier merge `f836c92` (`03b38ec` does not touch the text system):
  `cargo test -p gpui-pre --lib text_system` 56, `-p gpui-fast --lib line_break` 15, `--lib text_system` 41.
  `gpui-base` is not a workspace member, so `a_disabled_input_leaves_the_tab_order` passed with it added as
  a member temporarily. `cargo clippy -p gpui-pre -p gpui-fast --lib` ran before the merges, with no findings.
  The kit's full `cargo test --workspace` was not run, to save disk.
- Native run, fixture library and isolated `HOME`/`XDG_*`: the normal build stayed alive with no panic until
  stopped; the capture build exited 0. Looked at PNGs of home, library, reader, settings, rename dialog, image viewer and overflow
  menu; tasks was captured but not opened. Driven with simulated input: reader
  find (1/2, 2/2, highlights, scroll to match, close), inline formulas, text selection, wheel scroll, Escape
  closing the viewer. A real reader line shows `11 分 39 秒` moving down whole and no line opening with punctuation.
- Not exercised: a real conversion (no engine binary beside the app, so home shows its "cannot run" notice,
  as it should), Windows and Linux builds, and the old stack for comparison.

## Next action

Merge wave 1 branches as they report, then start wave 2.

## Decisions made during the work

- **macOS redraw fallback not ported.** The kit restarts the display link on occlusion and screen
  changes, and a drawable-prefetch waker recovers skipped frames, so a dirty window draws when it
  becomes visible again. Its `frame_waker` acts only with vsync off, so a link that is registered
  but not ticking while the window is visible has no recovery. That is a theoretical gap: nothing
  in the app needs frames while occluded (`on_next_frame` is only used for focus and scroll
  restoration, which can wait), and the fallback would draw synchronously on the main thread, against
  the kit's rule. Port it only if a frozen window is ever observed.
- **`gpui-kit-material` is on with `default-features = false`** (`serde`, `gpui-fast`) until P5 uses it.

## Foundation

Shell, theme, symbols and `ui.rs` are on the kit; pages move in wave 2 by `MIGRATION.md`. Worked example: `about.rs`.

- One paint: old tokens read the kit's roles. Kit's `Theme` is still written by `theme.rs`, through the kit's
  Base-only `apply_scheme`, because the bridge's `advance_component_theme` would fight it every frame. Wave 3
  switches to `init_component_theme`/`advance_component_theme` when `theme.rs` goes.
- Symbols: `build.rs` renders `desktop/assets/symbols.txt`; `embedded-symbols` stays off.
- Lucide SVGs are gone. `assets/outline/LICENSE` and `assets/material/LICENSE` stay until `package.py` stops copying them (`split/tooling`).

## Debts for the Material work

Visual only unless noted.

- Old pages still have button labels at Kit's sizes through `theme::ButtonLabelSize`, Kit's default menus, and Kit's default switches, until they move.
- `NoteText` ignores `TextSelectionEvent::TouchSelectionChanged`, so it draws no touch handles.
- Modal dialogs no longer mark the page behind them hidden to assistive technology (`ModalBackground` went, per the audit; GPUI has no attribute for it on a div). Keyboard focus stays trapped.
- The macOS "always show scrollbars" preference shows the bar on hover: Kit maps not-auto-hide to `ScrollbarMode::Hover`.
- `COURSE2MD_CAPTURE_CHROME` draws the other platform's captions, but content that measures the bar (the reader) still reserves this platform's width: `CaptionButtons::trailing_for_style` is not public.

## Open problems

- **Stale worktree records.** `../zed/.git/worktrees` and `../gpui-component/.git/worktrees` still list
  entries for the removed `desktop/.deps` worktrees. Left alone, as they are sibling repositories;
  `git worktree prune` there clears them if the owner wants.
