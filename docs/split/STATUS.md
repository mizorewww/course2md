# Split status

Update this file in the same commit as the work it records.

**Current phase:** P1 Patches and deps is finished.
**Dependency form:** path (flip to git rev before acceptance). CI cannot build the desktop until then.

## Done

- P0: branch `split/workspace`, kit worktree `course2md-integration`, documents, scaffold residue removed.
- P1: the desktop builds and runs on the `gpui-kit-material` stack: `gpui-kit` facade, `gpui-base`,
  `gpui-kit-material`, the kit's `[patch]` tables, `gpui-fast`, toolchain 1.99.1. Source pinning
  (`sources.py`, `sources.lock.json`, `patches/`, `.deps`) is gone.

## Patch triage

| Patch | Result | Why |
| --- | --- | --- |
| `zed-cjk-line-break` | Ported, both backends | `gpui-fast` had the punctuation rules, `gpui-pre` had none. Both now also keep no-break spaces and word joiners together, including glyphless joiners in shaped text, and a forced break carries the previous character. `vendor/gpui-pre/cjk-line-break-patch.md` |
| `component-input-accessibility` | Ported in part | Disabled input leaves the Tab order (`gpui-base`). Kit 0.8.1's `InputAccessibility` has the role, label and value. Text runs and selection for screen readers are not ported; the owner excluded specialist accessibility work |
| `zed-input-accessibility` | Dropped | Only served the text-run part above |
| `zed-macos-window` | Not ported | See the decision below |
| `zed-metal-frame-lifetime` | Already in the kit | `draw_with_status` has a per-draw autorelease pool |
| `zed-metal-present-stages` | Dropped | Superseded by `presentation_wait` and GPU timestamps; `performance.rs` now reports those |
| `component-tooltip-lifecycle` | Dropped | Kit 0.8.1's `WindowState` dismisses a removed trigger's tooltip and keeps a pressed trigger's dismissed. Not covered: its dismissal on any keystroke |
| `component-selectable-text-highlights` | Dropped | Unused: the app paints through its own `NoteText` element |
| `component-button-text-size` | Dropped, debt | `MaterialButton` in P5 |
| `component-menu-style` | Dropped, debt | `MaterialMenu` in P5 |
| `component-switch-geometry` | Dropped, debt | `MaterialSwitch` in P5 |

## Next action

Finish P2, then P3. Before P5, read the debts below.

## Decisions made during the work

- **macOS redraw fallback not ported.** The kit restarts the display link on occlusion and screen
  changes, and a drawable-prefetch waker recovers skipped frames, so a dirty window draws when it
  becomes visible again. Its `frame_waker` acts only with vsync off, so a link that is registered
  but not ticking while the window is visible has no recovery. That is a theoretical gap: nothing
  in the app needs frames while occluded (`on_next_frame` is only used for focus and scroll
  restoration, which can wait), and the fallback would draw synchronously on the main thread, against
  the kit's rule. Port it only if a frozen window is ever observed.
- **`gpui-kit-material` is on with `default-features = false`** (`serde`, `gpui-fast`) until P5 uses it.
- **Kit changes go upstream as one PR per concern** (plan decision 11): `05632a9` wrapping, `e016136`
  disabled inputs.

## Debts for P5

Visual only; the behaviours are verified below.

- Button labels asking for 15-16 px render at Kit's 1rem (14 px) through `theme::ButtonLabelSize`, and
  that takes Kit's `Large` size, so a button that sets no padding of its own gets `Large` padding.
  13 px labels render at Kit's 12.25 px. Kit's `Button` has no label size override.
- Menus use Kit's default look: no 48 px rows, tonal checked rows, 16 px container or entrance.
- Danger menu rows are `theme::danger_menu_item`, an element row with no menu-item label of its own.
- Switches are Kit's default size with no thumb icons.
- `NoteText` ignores `TextSelectionEvent::TouchSelectionChanged`, so it draws no touch handles.

## Open problems

- **Binary size for P5.** `gpui-kit-material`'s default `embedded-symbols` embeds about 52 MB (three
  variable fonts and the SVG pack in `gpui-material-symbols`). It has no subset feature. Recommendation:
  keep the feature off and add an app-owned `AssetSource` that holds only the symbols the app uses,
  since `Asset::provide` exists for runtime supply and `icons::Assets::with` takes a fallback source.
  Check that this covers Kit's translated default icons before relying on it.
- **Find field keys (not from this port).** With focus in the reader's find field, `cmd-g` and Escape
  do nothing: the field renders in the top bar, outside the `CourseReader` key context on `reader-page`.
  With focus in the reader both work. Found by reading the tree and running it; the old stack was not
  rerun, but this structure did not change. Belongs in P2 or P5.
- **Stale worktree records.** `../zed/.git/worktrees` and `../gpui-component/.git/worktrees` still list
  entries for the removed `desktop/.deps` worktrees. Left alone, as they are sibling repositories;
  `git worktree prune` there clears them if the owner wants.
