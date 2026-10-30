# course2md split plan

Working plan for splitting course2md into crates and moving the desktop app to
pure Material on `gpui-kit-material`. `STATUS.md` holds progress and the next
action; `AUDIT.md` holds the findings this plan is based on. Delete this
directory when the project is accepted.

**Resuming after a context loss:** read this file, then `STATUS.md`, then run
`git log --oneline main..split/workspace` here and
`git log --oneline origin/main..course2md-integration` in the kit worktree.

## Owner decisions

1. The app becomes **pure Material**. Lucide and the app's own tile geometry go;
   `.agents/skills/course2md-design/SKILL.md` must be rewritten to say so.
2. Reusable components are abstracted into `gpui-kit-material`, on a **worktree**,
   never on its `main`, which the owner is actively developing. The owner merges.
3. All GPUI Kit dependencies match `gpui-kit-material` exactly (same `gpui-kit`,
   `gpui-base`, `gpui-pre`/`gpui-fast`, same vendored patches).
4. Patches the kit already has are taken from the kit; do not port them twice.
5. Order: sync patches and deps, fix bugs, then the rest of this plan.
6. Develop with documents, atomic commits, frequent pushes.
7. `gh` may create **public** repositories for the split-out crates.
8. Acceptance: independent subagent reviews find no problems in UI, app and CLI,
   and find the result conforms to this plan.

## Locations

| What | Where | Branch |
| --- | --- | --- |
| App | `/Users/aac6fef/Developer/course2md` | `split/workspace` |
| Kit worktree | `/Users/aac6fef/Developer/gpui-kit-material-course2md` | `course2md-integration` (from `origin/main`) |
| Kit main (do not touch) | `/Users/aac6fef/Developer/gpui-kit-material` | `main` |
| Symbols (do not touch unless needed) | `/Users/aac6fef/Developer/gpui-material-symbols` | `main` |

The kit worktree starts from `origin/main`, so the owner's unpushed local commits
are not published by pushing this branch. Do not push the app's `main` or tags,
and do not change published releases.

## Target structure

Working names; check availability before creating a repository.

### Standalone public repositories

| Repository | Source | Must change first |
| --- | --- | --- |
| `gpui-kit-material` (existing) | Generic widgets from `theme.rs`, `motion.rs`, etc. | On the worktree |
| `gpui-material-symbols` (existing) | Icon assets | Nothing |
| `subtitle-parse` | Parser half of `subtitle.rs` | Local `Cue` type; tag-stripping bug fixed |
| `slide-detect` | `scene.rs` | `SlideParams`, local event type, progress callback, injected ffmpeg |
| `model-fetch` | `download.rs`, `hub.rs` | `Cancel`/`Progress` traits, no `tr!`, injected tool locator, opt-in IP lookup |
| `apple-asr-sys` (conditional) | `native/apple-asr`, `build.rs`, `apple.rs` | `--scratch-path`, `links` metadata, metallib path metadata, registered callbacks, no hard-coded prose. Repository only once it builds outside the app |

Rule: published libraries return typed errors and events; only the app translates.
No `tr!` in them. Second-batch candidates (`llm` JSON salvage, process utilities,
`dispatch` ledger, reader image cache, image viewer) stay internal unless a concrete
reason appears; the image viewer and reader image cache go to the kit if generic.

### Workspace crates in this repository

`crates/` holds `c2m-i18n`, `c2m-core`, `c2m-process`, `c2m-protocol`, `c2m-config`,
`c2m-source`, `c2m-ai`, `c2m-asr`, `c2m-artifact`, `c2m-engine`, and the `course2md`
binary; `desktop/` splits into `desktop-core`, `desktop-ui` and `course2md-desktop`.
Assignment is in `AUDIT.md`. The root and `desktop` become one Cargo workspace with
one lock file. CI, `package.py`, `release.yml` and the docs must follow.

## Phases

Each phase ends green (see Checks) and pushed.

- **P0 Setup.** Branch, kit worktree, these documents, scaffold residue removed.
- **P1 Patches and deps.** Decide every patch in `desktop/patches` against the kit
  vendor: already there, port to the kit worktree, or dropped by pure Material. Move
  the desktop from pinned Zed and `gpui-component` to the `gpui-kit` facade with the
  kit's `[patch]` tables. App must build and run at the end of this phase.
- **P2 Bugs.** Fix `AUDIT.md` § Bugs, each with a test where meaningful, one commit each.
- **P3 Engine workspace.** Break cycles in the order in `AUDIT.md`, create crates,
  merge the two Cargo workspaces, teach `tests/i18n_catalog.rs` about the workspace.
- **P4 Standalone repositories.** Extract, tests travel with the code, create public
  repos, depend by git revision. Add `release-plz` and `cargo-deny`.
- **P5 Pure Material.** Abstract reusable widgets into the kit worktree, replace
  `theme.rs`, `palettes.rs`, `motion.rs`, `choice_group.rs`, `select_field.rs`,
  `focus_scroll.rs`, `window_controls.rs`, `scroll.rs` and `icons.rs` by kit
  equivalents, rewrite the design skill and references, verify every page.
  Delete `sources.py`, `sources.lock.json`, `patches/`.
- **P6 Desktop crates.** Cut the listed edges, `desktop-core`, split `Desktop`, `desktop-ui`.
- **P7 Wheels and tooling.** `AUDIT.md` § Wheels; ship dependency licences.
- **P8 Acceptance.** See below.

P1 precedes P2 by owner decision. P2 engine fixes can run while P1 is underway, as
long as they are not in the same checkout at the same moment.

## Checks

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings      # engine; workspace-wide after P3
cargo test
cargo check --manifest-path desktop/Cargo.toml # until the workspaces merge
```
Kit worktree: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`. Before acceptance also read `.github/workflows/ci.yml` and
`desktop.yml` and run what they run.

## Dependency form

During development the desktop may use `path = "../../gpui-kit-material-course2md/..."`.
**Before acceptance** this flips to `git = "https://github.com/mizorewww/gpui-kit-material"`
with a pinned `rev` from `course2md-integration`, in both the dependency and every
`[patch]` entry, so a clean clone builds. Tracked in `STATUS.md`.

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
