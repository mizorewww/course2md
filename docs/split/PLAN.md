# course2md split plan

Working plan for splitting course2md into crates and moving the desktop app to
pure Material on `gpui-kit-material`. `STATUS.md` holds progress and the next
action; `AUDIT.md` holds the findings this plan is based on. Delete this
directory when the project is accepted.

**Resuming after a context loss:** read this file, then `STATUS.md` and `ACCEPTANCE.md`, then run
`git log --oneline --first-parent main..split/workspace` here and
`gh pr list -R mizorewww/gpui-kit-material` for the kit PRs in flight.

## Owner decisions

1. The app becomes **pure Material**. Lucide and the app's own tile geometry go;
   `.agents/skills/course2md-design/SKILL.md` must be rewritten to say so.
2. Kit work happens on a **worktree**, never on the owner's `main` checkout. The owner merges.
3. GPUI Kit dependencies come from one pinned revision of `gpui-kit-material`, tracking its
   mainline. The `[patch]` table holds only what the build needs (six entries on macOS),
   superseding "match exactly" per decision 11; add entries only if CI shows a platform needs them.
4. Patches the kit already has are taken from the kit; do not port them twice.
5. Order: sync patches and deps, fix bugs, then the rest of this plan.
6. Develop with documents, atomic commits, frequent pushes.
7. `gh` may create **public** repositories for the split-out crates.
8. Acceptance: independent subagent reviews find no problems in UI, app and CLI,
   and find the result conforms to this plan.
9. The app should end up as a thin wrapper over many crates with low coupling and
   high cohesion. A crate boundary that needs many cross-crate reaches is wrong; move
   the boundary rather than widening the API.
10. Disk is tight. Do not keep several full `target` directories: share one
    `CARGO_TARGET_DIR` where possible, delete build output and worktrees once merged,
    and check `df -h` during long builds. Never delete anything in the owner's
    `gpui-kit-material` checkout or other sibling repositories.

11. **The goal is less code, not relocated code.** A need is presumed false until audited;
    the owner expects most are. For each thing the app seems to need from below, in order:
    (a) drop the need (change the app's wording, layout or behaviour); (b) write it the
    Kit-native way, with the existing `gpui-kit-material` and Kit APIs; (c) only if the kit's
    API is genuinely awkward, make it generic and open a PR **to `gpui-kit-material`** (or
    `gpui-material-symbols`, or a crate split out here), after independent review. One concern
    per PR, branched from that repository's `origin/main`, not the integration branch. The
    owner merges these.
12. **No new engine patches, and no PRs to Longbridge or Zed.** Nothing from this project
    is added to the kit's `vendor/` to serve the app. The upstream ends at
    `gpui-kit-material`. Removing patches from `vendor/` is welcome where the audit shows
    they aren't needed. Exception (owner, 2026-11-01): `TextView` block styling for the reader's
    summary panel goes into the kit's `vendor/gpui-base` by PR to `gpui-kit-material`.

13. **Code reduction targets (owner, 2026-10-31).** Componentization removes about 20%
    as a first target (adjustable if not achievable); removing wheels removes more;
    the eventual goal is **60-90% less code**.
    - Baseline at `3db4a88`: 78,518 production Rust lines (`src` 20,413, `desktop/src`
      58,105), plus 18,113 test lines, 2,423 script lines, 453 Swift lines.
    - Primary metric: production Rust lines in this repository, all workspace crates included.
    - Secondary metric, always reported beside it: lines added to repositories the owner
      maintains (kit, split-out crates). Moving code to one of those counts toward the
      primary metric but is visible here, so relocation isn't mistaken for deletion.
    - Record both in `STATUS.md` at each phase end.
    - **No feature is dropped (owner, 2026-10-31).** Behaviour may be simplified, but
      every feature stays. The working target is the largest reduction the audit finds
      achievable under that rule, whatever it comes to.

14. **Choose components again; don't map old to new (owner, 2026-10-31).** Many current control
    choices are wrong in meaning and UX. For each page element, ask what the user is doing and pick
    the Material component that means that, from the kit's catalog, even if it changes the control
    type, the layout or the number of controls. A one-to-one swap of a wrong control isn't done.
    Every feature stays. Record changed choices in the page agent's report.
15. **All motion is the kit's (owner, 2026-10-31).** Animation uses the kit's `motion` and its
    components' built-in motion. `motion.rs` and hand-tuned curves and durations go. An app animation
    the kit can't express and that is worth keeping is backported: generic, one PR to
    `gpui-kit-material`, reviewed, under decision 11(c). Otherwise it is dropped for the kit's.
16. **A migration keeps the look (owner, 2026-10-31).** Decision 14 picks the component; it doesn't
    make the page plainer. Distinctive design the app already has, such as coloured tonal icons and
    the large-radius link field, survives. A look the kit lacks is added to `gpui-kit-material`, and
    the page uses it; it is neither flattened to the kit default nor hand-painted in the page.
    `DESIGN-RESTORE.md` lists what wave 2 lost and how each item is restored.

## Decisions

Taken by reading the code, against decisions 11 and 13. Each states what would reopen it.

- **Library sidebar: the app's `folder_tree.rs`, not the kit's `Tree`.** The kit's `Tree`
  (`vendor/gpui-base/src/tree.rs`) is an unstyled `uniform_list` of `TreeItem`s whose expansion it owns,
  and a full click on a folder both selects and toggles it. The sidebar selects a place on click and
  toggles only by its arrow, mixes rows of different heights (all notes, a library heading, a section
  title, folders), and draws everything itself: the drag source and drop outline, the right-click menu,
  the tooltip for a cut name, the indent per level and the note counts. All of that would stay in the
  app's render callback; `Tree` would remove only the dozen lines that skip a closed folder's children,
  while adding a mirror of `organize::Library` into `TreeItem`s and changing what a click does. Reopen if
  the kit gains a Material tree with a separate disclosure target.
- **Catalog test: keep the hand scanner, no `syn`.** `crates/course2md/tests/i18n_catalog.rs` is test
  code, outside the primary metric. `syn` (or `proc-macro2`'s tokenizer) would replace the literal scanner
  (about 130 lines), but the test still needs its own code for what it checks beyond literals: joining a
  multi-line `///` help text into the one key clap looks up, and the `about`/`help` attributes. Reopen if
  the scanner misreads a literal.
- **Credentials: Keychain through `security-framework` on macOS, `keyring` elsewhere.** Consolidating on
  `keyring` 4 would save about 70 lines in `desktop/core/src/credentials.rs`, but the Keychain status code
  that every error message shows would have to be dug out of `keyring`'s platform error, and it must read items that 2.0.1's signed app wrote (the audit's
  caveat), which cannot be tested here without that signed build. Reopen when a signed build can test it.
- **No `toml_edit`.** `c2m-config` rewrites `config.toml` from its struct and keeps the previous file as
  `config.toml.bak`; the CLI's own template is written whole. `toml_edit` would keep a person's comments
  across `llm setup` and `--login ollama`, which no feature asks for, and adds code rather than removing it.
- **No `apple-asr-sys`.** The sherpa-onnx spike rejected one engine (`REDUCTION.md` § Decisions applied),
  so the Swift module stays. As a published crate it would only move `apple.rs` (614 lines, tied to
  `c2m-protocol`'s progress events and `tr!`), the Swift build in `c2m-asr/build.rs` and
  `native/apple-asr` to another repository: relocation, counted by the secondary metric, with no deletion.
  It is already isolated as part of `c2m-asr`.
- **Lever 10, desktop half: not done.** Its two items stay in this repository. The screenshot cache and
  viewer (`desktop/src/reader_images.rs`, 472 lines) is written against `Desktop` and GPUI's `RenderImage`;
  the form primitives became `desktop-ui` (`desktop/ui/src/ui.rs`), a workspace crate, after the looks
  they lacked went to the kit (PRs #8 to #14). Publishing either would be relocation. Open question for the
  owner: whether a sized, budgeted image cache belongs in `gpui-kit-material`; recommended no, until a
  second app needs it.

## Locations

| What | Where | Branch |
| --- | --- | --- |
| App | `/Users/aac6fef/Developer/course2md` | `split/workspace`; agents work in sibling worktrees on `split/<topic>` |
| Kit PR worktrees | `/Users/aac6fef/Developer/gpui-kit-material-<topic>` | One per PR, `design/<topic>` or `text/<topic>`, each from kit `origin/main` |
| Kit main (do not touch) | `/Users/aac6fef/Developer/gpui-kit-material` | `main` |
| Symbols (do not touch unless needed) | `/Users/aac6fef/Developer/gpui-material-symbols` | `main` |

Each kit worktree starts from `origin/main`, so the owner's unpushed local commits
are not published by pushing a PR branch; remove the worktree and its branch once the PR
merges. Do not push the app's `main` or tags, and do not change published releases.

## Target structure

Working names; check availability before creating a repository.

### Standalone public repositories

| Repository | Source | Must change first |
| --- | --- | --- |
| `gpui-kit-material` (existing) | Nothing by default; see decision 11 | PR only when its API is awkward |
| `gpui-material-symbols` (existing) | Icon assets | Nothing |
| `subtitle-parse` | Parser half of `subtitle.rs` | Local `Cue` type |
| `slide-detect` | `scene.rs` | `SlideParams`, local event type, progress callback, injected ffmpeg |
| `model-fetch` | `download.rs`, `hub.rs` | `Cancel`/`Progress` traits, no `tr!`, injected tool locator, opt-in IP lookup |
| ~~`apple-asr-sys`~~ | `native/apple-asr`, `build.rs`, `apple.rs` | Not created; see § Decisions |

Rule: published libraries return typed errors and events; only the app translates.
No `tr!` in them. Check name availability before creating a repository.

### Workspace crates in this repository

`crates/` holds `c2m-i18n`, `c2m-core`, `c2m-process`, `c2m-protocol`, `c2m-config`,
`c2m-dispatch`, `c2m-source`, `c2m-ai`, `c2m-asr`, `c2m-artifact`, `c2m-engine`, the
`course2md` facade and binary, and the test-only `c2m-test-http`; `desktop/` holds
`desktop-prefs`, `desktop-accounts`, `desktop-store`, `desktop-reader-core`, `desktop-tasks` (each under
`desktop/`), `desktop-ui` (`desktop/ui`) and `course2md-desktop` (`desktop`).
They form one Cargo workspace with one lock file. What each holds and the allowed dependency
directions are in `docs/CRATES.md`.

## Phases

`REDUCTION.md` is the execution guide for what each lever removes. Each wave ends green and
pushed; agents work on branches in sibling worktrees, and the coordinator merges.

Done: P0 setup, P1 deps, P2 bugs, docs rewrite, waves 1 to 3. Now: wave 4, fixing the findings of
acceptance round 1 (`ACCEPTANCE.md`); `STATUS.md` has the state.

**Wave 1, in parallel:**
- `split/engine`: lever 1, engine cleanup, plus the engine wheels.
- Spike: lever 2, sherpa-onnx, in a scratch directory; decides whether a second engine pass happens.
- `split/persist`: levers 3 and 4, persistence and derived task state.
- `split/foundation`: Material foundation in the app (kit theme installed, shell, icon asset
  source, small shared primitives). Page files are not rewritten here.
- `split/repos`: lever 10 engine half, `subtitle-parse`, `slide-detect`, `model-fetch`; repositories, `release-plz`, `cargo-deny`.
- `split/tooling`: T7 dependency licences shipped, remaining `AUDIT.md` wheels outside `src/`.
- Kit documentation PR (one concern, from kit `origin/main`, reviewed first).

**Wave 2, after wave 1 merges, in parallel by file set.** Each page agent rewrites its files on
the kit, removing its use of `theme::*` and the old widgets, taking the matching lever:
settings, setup and services (lever 6); library (lever 6); import and tasks (lever 7);
reader, including `note_text` → `TextView` (lever 8).

**Wave 3.** Delete `theme.rs`, `palettes.rs`, `motion.rs`, `choice_group.rs`, `select_field.rs`,
`focus_scroll.rs`, `window_controls.rs`, `scroll.rs`, `icons.rs` and what else lever 9 lists once
nothing refers to them. Lucide assets go. Engine workspace: break cycles per `AUDIT.md`, create
crates, merge the two Cargo workspaces, teach `tests/i18n_catalog.rs` about the workspace; CI,
`package.py`, `release.yml` and docs follow. Desktop crates: cut the listed edges, split `Desktop`,
`desktop-core`, `desktop-ui`. Lever 10 desktop half if still worthwhile.

**Wave 4.** Acceptance, below.

Metrics: `python3 docs/split/metrics.py`, which counts every workspace crate (it reads 77,921 at
`3db4a88`, against the 78,518 baseline counted before the crates existed). Record at each wave end
in `STATUS.md`, beside the secondary metric.

## Checks

One workspace:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --test i18n_catalog        # after any change to i18n/catalog.tsv or a tr! key
cargo deny check
```
Packaging scripts: `python3 -m unittest discover -s packaging -p 'test_*.py'` and the same for
`desktop/scripts` (Python 3.11 or later). Kit worktree: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. Before acceptance
also read `.github/workflows/ci.yml` and `desktop.yml` and run what they run.

## Dependency form

The desktop uses `git = "https://github.com/mizorewww/gpui-kit-material"` with a pinned `rev` of
that repository's `main`, in the dependency and every `[patch]` entry, so a clean clone and CI build.
For a kit change under test, point at the PR branch's commit; bump the rev to follow mainline.

## Acceptance

All of these, each by an independent subagent that did not implement that region:

1. **UI.** Every page and its loading, empty, failure and completion states in the
   running native app, captures in light and dark, against the rewritten design skill.
2. **App.** A fresh and a repeat task through the running app to a usable note;
   settings/login navigation, return, retry, cancel.
3. **CLI.** `cargo test`, `tests/cli_ux.rs`, real `course2md` runs on a fixture,
   `doctor`, `--json`, `run-task`, four locales.
4. **Plan conformance.** This file, item by item, including every bug and wheel row.

Findings get fixed and re-reviewed by a fresh reviewer. Failures are reported as
failures. Environment limits (no real Bilibili login, no NPU) are stated, not hidden.
