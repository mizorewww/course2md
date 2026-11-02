# Split status

Update this file in the same commit as the work it records.

**Current phase:** Wave 4, acceptance fixes. Round 1 of acceptance (`ACCEPTANCE.md`) failed in all four
reviews; the engine crates, the desktop crates, the design restore, the Bilibili password and SMS logins
(Geetest) and the YouTube browser login, `engine-fixes` (C1–C14), `desktop-logic` (A2–A14) and the first
`desktop-ui` batch are merged into `split/workspace`. Open: the rest of `desktop-ui` (U1, U2, U4, U7, U10 and
`DESIGN-RESTORE.md` § Not finished), and the plan blockers P1 (reduction) and P2 (thin wrapper).

**Dependency form:** git, `gpui-kit-material` `main` at `79bce30`, in `[workspace.dependencies]` and all six
`[patch]` entries of the root `Cargo.toml`. No temporary pin. Kit `main` has since merged #18, #20 and #21,
which U1, U4/G17 and U10/L1 wait for, so the next repin takes them; #19 and #22 to #27 are open.

## Metrics

`python3 docs/split/metrics.py` at `5863f22`: **67,456** production Rust lines, against the 78,518 baseline
(-11,062, -14.1%). The baseline was counted with an earlier script; the current one reads
77,921 at `3db4a88`, so -10,465 (-13.4%). Desktop 47,530 (`desktop` 29,912, `desktop/core` 15,848, `desktop/ui` 1,770); engine 19,926
across `crates/`.

Secondary metric (decision 13), lines this project put into repositories the owner maintains:

- `gpui-kit-material`: merged PRs #1 to #18, #20 and #21 add 2,537 net Rust lines outside `tests/`, examples
  and the gallery (in-file test modules included, so this is an upper bound). The open PRs #19 and #22 to #27
  would add 1,412 more.
- `model-fetch` 1,080, `slide-detect` 596, `subtitle-parse` 667 production lines at the pinned revisions: 2,343.

Primary plus secondary: 72,336, so the true deletion is about 5,590 lines (-7.2%) like for like. Since round 1 of acceptance
(`d89579d`, 63,001) the count rose by 4,455: the Bilibili and YouTube logins 3,329, `engine-fixes` 380,
`desktop-logic` 383, `desktop-ui` 347. The working target in `REDUCTION.md` (about 46,700) has not been
re-estimated (finding P1).

| Reading | Commit | Lines |
| --- | --- | ---: |
| Baseline | `3db4a88` | 78,518 (77,921 by the current script) |
| Wave 1 end | `0a13c9f` | 74,176 (74,038 by the current script) |
| Waves 2 and 3 | `29cdd24` | 61,992 (current script) |
| Acceptance round 1 | `d89579d` | 63,001 |
| Acceptance fixes so far | `5863f22` | 67,456 |

## Next action

Finish `desktop-ui`: repin the kit to a `main` that has #18, #20 and #21 and take U1, U4 and U10; the rest
waits on #19 and #22 to #27. Then decide P1 and P2 with the owner (re-estimate the reduction; split the
`Desktop` impl and `desktop-core`), and run a fresh acceptance round with new reviewers.

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
