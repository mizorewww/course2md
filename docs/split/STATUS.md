# Split status

Update this file in the same commit as the work it records.

**Current phase:** Wave 4. The acceptance-round-1 fixes, the reduction chunks and the `desktop-core` split are
merged into `split/workspace`; acceptance round 2 is next. Every `DESIGN-RESTORE.md` restore that waited on the kit
is in, through kit PRs #16 to #27. Open: U2 (reader resize, unmeasured: load stayed high), U21 (not reproduced),
and kit #28 (a menu opens above its field when there is no room below), ready for the owner.

**Dependency form:** git, `gpui-kit-material` `main` at `53fa0fd`, in `[workspace.dependencies]` and all six
`[patch]` entries of the root `Cargo.toml`. No temporary pin. Repin when #28 merges.

## Metrics

`python3 docs/split/metrics.py` at `edf140d`: **66,768** production Rust lines against the 78,518 baseline
(-11,750, -15.0%); the current script reads 77,921 at `3db4a88`, so -14.3% like for like. Engine 19,901 across
`crates/`; desktop 46,867 (`desktop` 29,451, `prefs` 4,454, `store` 6,112, `tasks` 3,653, `reader-core` 946,
`accounts` 493, `ui` 1,758).

Secondary metric (decision 13): the merged `gpui-kit-material` PRs #1 to #27 add **5,677** net Rust lines
(`gh pr view <n> --json files`, `.rs` files only): 2,833 in tests and the gallery (`tests/`, `gallery/`, `*_tests.rs`)
and 2,844 elsewhere. The 2,844 still holds any inline `#[cfg(test)]` module. #28, open, would add 454 (321 of them
tests). `model-fetch`, `slide-detect` and `subtitle-parse` hold 2,343. Across all the owner's repositories the
net reduction is about 11,750 − 2,343 − 2,844 = **6,563** lines (−8.4% of the baseline), beside the −15.0% inside this
repository.

The reduction was re-estimated by reading every desktop file over 400 lines and every engine crate (finding P1):
about 3,500 lines looked removable with every feature kept. Done by chunk, the reductions came to engine and
process helpers -140, tasks -174, reader -103, library and storage -188, settings and setup -175, import -15,
async idiom -40: -835, about a quarter of the estimate. Most rows were not duplication once read (different
wording, look, retry policy or cancellation), or saved nothing after formatting. What remains is feature logic.
Under the no-drop rule this is the reachable figure; see `REDUCTION.md` § Working target.

| Reading | Commit | Lines |
| --- | --- | ---: |
| Baseline | `3db4a88` | 78,518 (77,921 by the current script) |
| Wave 1 end | `0a13c9f` | 74,176 (74,038 by the current script) |
| Waves 2 and 3 | `29cdd24` | 61,992 (current script) |
| Acceptance round 1 | `d89579d` | 63,001 |
| Fixes and logins | `5863f22` | 67,456 |
| Reduction chunks, crate split | `edf140d` | 66,768 |

## Next action

Acceptance round 2 with fresh reviewers (UI, app, CLI, plan), per `PLAN.md` § Acceptance; repin to the kit `main`
that has #28 when it merges.

## Owner questions

Each saves a little and changes behaviour, so it waits for the owner; the default is to keep things as they are.

- Stop reading schema-1 records and task records from before `events.jsonl` (about -115 to -175).
- Run the CLI pipeline through `execution::Request` (about -60): an interrupted CLI run from before the upgrade
  would start over instead of resuming.
- Whether an image cache belongs in the kit (recommendation: no).

## Decisions made during the work

- **macOS redraw fallback not ported.** The kit restarts the display link on occlusion and screen
  changes, and a drawable-prefetch waker recovers skipped frames, so a dirty window draws when it
  becomes visible again. Its `frame_waker` acts only with vsync off, so a link that is registered
  but not ticking while the window is visible has no recovery. That is a theoretical gap: nothing
  in the app needs frames while occluded (`on_next_frame` is only used for focus and scroll
  restoration, which can wait), and the fallback would draw synchronously on the main thread, against
  the kit's rule. Port it only if a frozen window is ever observed.
- The decisions on `Tree`, `syn`, `keyring`, `toml_edit`, `apple-asr-sys` and lever 10's desktop half are in
  `PLAN.md` § Decisions.

## Debts for the Material work

- Modal dialogs no longer mark the page behind them hidden to assistive technology (`ModalBackground` went, per
  the audit; GPUI has no attribute for it on a div). Keyboard focus stays trapped.
- `COURSE2MD_CAPTURE_CHROME` draws the other platform's captions, but content that measures the bar (the reader)
  still reserves this platform's width: `CaptionButtons::trailing_for_style` is not public.

## Open problems

- **Stale worktree records.** `../gpui-component/.git/worktrees` still lists entries for the removed
  `desktop/.deps` worktrees. Left alone, as it is a sibling repository; `git worktree prune` there clears them
  if the owner wants.
- **Leftover kit branches.** `course2md-design` and `course2md-integration` hold commits that `main` has no
  equivalent of (`git cherry`), so they stay until the owner confirms they are no longer wanted.
