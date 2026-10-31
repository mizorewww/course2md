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

## Debts for the Material work

Visual only; the behaviours are verified below.

- Button labels asking for 15-16 px render at Kit's 1rem (14 px) through `theme::ButtonLabelSize`, and
  that takes Kit's `Large` size, so a button that sets no padding of its own gets `Large` padding.
  13 px labels render at Kit's 12.25 px. Kit's `Button` has no label size override.
- Menus use Kit's default look: no 48 px rows, tonal checked rows, 16 px container or entrance.
- Danger menu rows are `theme::danger_menu_item`, an element row with no menu-item label of its own.
- Switches are Kit's default size with no thumb icons.
- `NoteText` ignores `TextSelectionEvent::TouchSelectionChanged`, so it draws no touch handles.

## Open problems

- **Binary size.** `gpui-kit-material`'s default `embedded-symbols` embeds about 52 MB (three
  variable fonts and the SVG pack in `gpui-material-symbols`). It has no subset feature. Recommendation:
  keep the feature off and add an app-owned `AssetSource` that holds only the symbols the app uses,
  since `Asset::provide` exists for runtime supply and `icons::Assets::with` takes a fallback source.
  Check that this covers Kit's translated default icons before relying on it.
- **Stale worktree records.** `../zed/.git/worktrees` and `../gpui-component/.git/worktrees` still list
  entries for the removed `desktop/.deps` worktrees. Left alone, as they are sibling repositories;
  `git worktree prune` there clears them if the owner wants.
