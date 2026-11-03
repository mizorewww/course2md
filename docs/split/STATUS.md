# Split status

Update this file in the same commit as the work it records.

**Current phase:** Wave 4. The acceptance-round-1 fixes, the reduction chunks and the `desktop-core` split are
merged into `split/workspace`; acceptance round 2 is next. Every `DESIGN-RESTORE.md` restore that waited on the kit
is in, through kit PRs #16 to #27. Open: U2 (reader resize, unmeasured: load stayed high), U21 (not reproduced),
and kit #28 (a menu opens above its field when there is no room below), ready for the owner.

**Dependency form:** git, `gpui-kit-material` `main` at `1bc3ea7`, in `[workspace.dependencies]` and all six
`[patch]` entries of the root `Cargo.toml`. No temporary pin. Kit #28 to #30 are in.

## Metrics

`python3 docs/split/metrics.py` at `e31032b1` (3.0.0-alpha.1): **69,361** production Rust lines against the 78,518
baseline (-9,157, -11.7%); the current script reads 77,921 at `3db4a88`, so -8,560, -11.0% like for like. Engine
20,807 across `crates/`; desktop 48,554 (`desktop` 30,027, `prefs` 4,486, `store` 6,455, `tasks` 4,338, `reader-core`
994, `accounts` 496, `ui` 1,758). The figure rose from 66,768 at `edf140d` with the round-2 fixes and the thin-wrapper
work (decision 9), which the owner accepted costs lines (decision 13).

Secondary metric (decision 13): the merged `gpui-kit-material` PRs #1 to #30 add **6,364** net Rust lines
(`gh pr view <n> -R mizorewww/gpui-kit-material --json files`, `.rs` additions minus deletions): 3,191 in tests and
the gallery (`tests/`, `gallery/`, `*_tests.rs`) and **3,173** elsewhere (#1 to #27: 2,833 and 2,844; #28: 321 and
133; #29: 16 and 191; #30: 21 and 5). The 3,173 still holds any inline `#[cfg(test)]` module. `model-fetch`
(1,242), `slide-detect` (596) and `subtitle-parse` (711) hold **2,549** production lines at their pinned revisions
(`metrics.py` on each). Across all the owner's repositories the net reduction is 9,157 − 2,549 − 3,173 = **3,435**
lines (−4.4% of the baseline), beside the −11.7% inside this repository.

The reduction was re-estimated by reading every desktop file over 400 lines and every engine crate (finding P1):
about 3,500 lines looked removable with every feature kept. Done by chunk, the reductions came to engine and
process helpers -140, tasks -174, reader -103, library and storage -188, settings and setup -175, import -15,
async idiom -40: -835, about a quarter of the estimate. Most rows were not duplication once read (different
wording, look, retry policy or cancellation), or saved nothing after formatting. What remains is feature logic.
Under the no-drop rule this is the reachable figure; see `REDUCTION.md` § Working target. The thin-wrapper work
since added lines back (entity plumbing, events, `ShellFacts`).

| Reading | Commit | Lines |
| --- | --- | ---: |
| Baseline | `3db4a88` | 78,518 (77,921 by the current script) |
| Wave 1 end | `0a13c9f` | 74,176 (74,038 by the current script) |
| Waves 2 and 3 | `29cdd24` | 61,992 (current script) |
| Acceptance round 1 | `d89579d` | 63,001 |
| Fixes and logins | `5863f22` | 67,456 |
| Reduction chunks, crate split | `edf140d` | 66,768 |
| 3.0.0-alpha.1: round-2 fixes, thin wrapper | `e31032b1` | 69,361 |

## P2: the thin wrapper (decision 9, finding Q2)

Branch `split/thin-wrapper`, first step of P2-b. The binary moved logic out and the reader became an entity:

- **Out of the binary.** The reader's export (the suggested name, refusing any note-version folder, a temporary
  folder beside the target and the rename) is `desktop_reader_core::reader::{export_file_name, export_version}`.
  What a task run sends and records is `desktop_tasks::run`: the engine request and control file, the start's
  library checks, the record of a failed start, a run and its end, filing a published note, the repair
  configuration and the uncertain requests. The app keeps when each happens: `claim_start` (A3), held tasks (A2),
  the poll (B1), model consent (B5), resend authorization and the paid-request guards are unchanged. The folder
  context of a note or the next note is `desktop_store::records::Records::folder_context`.
- **Shared entities.** `Entity<Records>` (`desktop_store::records`) holds the workspace record and the library
  scan, which were two `Desktop` fields; `Entity<NoteExports>` holds exports for the reader and the library rows.
- **ReaderPage.** `Entity<ReaderPage>` holds the reader's state and `OpenedNote`, renders itself, reads
  `Records` and `NoteExports`, and emits `ReaderEvent` (navigate, open task, reprocess, repair service, message,
  record error, viewer opened). Desktop subscribes, keeps opening a note (its conversion guards and the draft),
  and draws the reader's title bar from `ReaderPage::title_bar`.
- **Verified.** Captures of the capture build before (`5ad1305b`) and after, paper and ink, of home, tasks,
  library, settings (appearance, storage), the reader (top, `~shots`, `~shot:1`, `~end`, `dialog:image`, find
  with next and close, version menu and switching to the other version, export menu, scroll, back, a narrow
  window with the contents over the note): every pair is pixel-identical. Reading positions saved after a scroll
  and restored on the next open are the same. The export to a file goes through the native save panel, which the
  harness cannot drive; `export_version` is tested on a published fixture instead.
- **Rendering (step 1).** Every change Desktop makes to `Records` or the reader goes through `Change::change`,
  which notifies the entity; a running job's events update the record quietly and notify it at the coalesced
  repaint, as Desktop itself was. The reader is drawn `cached` in its old column, and its title bar is a view of
  its own (`ReaderBar`); the shell no longer reads or observes the reader on the reader page. Capture builds count
  builds per simulated input, with a frame after each step (gpui-fast, retention on): find's next match
  Desktop 0 / reader 4; a decoded screenshot in the viewer (1.5 s) Desktop 0 / reader 13; one wheel step
  Desktop 2 / reader 9, three steps 6 / 13 (new list rows ask for layout, and gpui-fast builds the view around a
  view that does). Before, the reader was Desktop's own render, so each of those reader builds was a Desktop
  build. Stale state: with `COURSE2MD_CAPTURE_RETAINED` (no window refresh before a shot), a note with failed
  screenshots whose 重新生成截图 creates a follow-up task while the reader shows (the retry leaves, the record
  changes) and the whole suite match the before build. The before build itself varies between runs (formula
  raster, the viewer's zoom-out state, scrollbar fade, a few others); every after shot equals a before shot from
  one of three runs, except two paper shots whose differing regions are each one of those variations.
- **AccountsView (step 2).** `Entity<AccountsView>` (`desktop/src/account_ui.rs`) holds the Bilibili account, its
  login fields and the YouTube connection, and draws their rows and dialogs for settings, first use and the
  workbench. It emits `AccountsEvent` (open a dialog, signed in, connected); Desktop decides which failed source
  preview a dialog may retry when it opens, and retries it only while that preview is still shown and still needs
  it, as before. Codex and Ollama stay with the service forms. Captures of home, settings › services, first use's
  account step, every login dialog state and every YouTube dialog state, paper and ink, match the before build
  except spinner frames and a 2 px caret column; the retained-mode run matches likewise.
- **Numbers.** Binary `desktop/src` 30,212 → 30,104 lines (`wc -l`), `Desktop` 44 → 43 fields, `impl Desktop` in
  23 → 22 files; `metrics.py` 67,603 → 68,162 (+559: `run.rs` and `records.rs`, their tests excluded, and the
  entity plumbing). After step 1: 30,216 lines, 44 fields (`reader_bar`), 22 files, 68,274 (the build counter
  and retained captures are about 60 of the +112). After step 2: 30,345 lines, 42 fields, 19 files, 68,403.
- **Work (P2-c, branch `split/thin-wrapper2`, step 1).** `Entity<Work>` replaces five `Desktop` fields (`work`,
  `transient_task_result`, `draft_deadline` and the wake channel's two ends). `desktop_tasks::job::Work` (was
  `WorkState`) also holds the finished task's announcement, the draft deadline and the wake channel, and computes its
  part of the poll deadline, the event batch, a task job's and a model job's install and the job's exit; tests
  `deadlines_come_from_the_draft_the_announcement_and_quitting`, `a_running_job_ticks_and_its_exit_names_its_task`.
  The run orchestration in `task_runner.rs` reads and writes it through the entity; `claim_start` (A3), held tasks
  (A2), the poll (B1), model consent (B5), resend authorization and the paid-request guards are unchanged.
  Verified before (`ab18df27`) and after with the capture build, paper and ink, normal and retained: home, tasks
  (scrolled, details), library (cards, list, grouped, search), settings (generation, storage), first use's model step
  are pixel-identical (storage differs only in the fixture's folder name when runs share no fixture; identical when
  they do). Behaviour, fake AI server on loopback, 25 s or 40 s per request: a local video submitted, a second task
  queued, pause all, resume all, cancel — the same screens except the running clock and progress bar, 1 proofread +
  1 summary request in both builds; the app `kill -9`ed during proofreading with the worker left running, relaunched:
  held (no pause), then continued from the worker's answer to complete, identical screens, 1 proofread + 1 summary in
  both builds, no resend.
- **Numbers.** Before P2-c (`ab18df27`): 30,345 lines, 42 fields, 19 files, `metrics.py` 68,699. After step 1:
  30,362 lines, 38 fields, 19 files, 68,835 (`job.rs` +136: what moved out of the binary plus the entity's methods;
  the binary's `cx` threading adds back what it lost).
- **TasksPage (step 2).** `Entity<TasksPage>` replaces `task_list`: it renders the task page (drawn with the window,
  not cached) and the task cards of the workbench and the library rows' menus, observes `Records` and `Work`, and
  emits `TasksEvent`; running an action, pause or resume all, moving a waiting task and opening a task's note are
  `Desktop` methods in `task_runner.rs`, so `task_ui.rs` has no `impl Desktop`. Opening a row asks the window to
  select the task and only then opens it, as before (test `a_row_opens_through_the_window_and_folds_on_its_own`).
  Captures (before `ab18df27`, paper and ink, normal and retained) of the step-1 suite plus the task page's details
  opened and closed, its ⋯ and right-click menus, the bottom of the queue, history, and back from the library:
  identical to the before build (one paper run had one pixel differ in a scrolled shot; rerun, identical). The
  behaviour runs of step 1 again: the same screens apart from the clock and progress bars, 1 proofread + 1 summary
  request in both scenarios, no resend.
- **Numbers.** After step 2: 30,616 lines, 38 fields, 18 files, `metrics.py` 69,045; the binary
  grew by the page's struct, its event enum and handler, and `ShellFacts`.
- **Next.** The library page as an entity reading `Records`, then `Prefs`, which would let `ShellFacts` go.

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
