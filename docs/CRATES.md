# Crate layout

course2md is one Cargo workspace with one `Cargo.lock`. The engine and the command line live in
`crates/`, the desktop app in `desktop/`. A plain `cargo build` or `cargo test` covers
`crates/*` (the root's `default-members`), which builds on machines without the desktop's
native libraries; `--workspace` adds the desktop. Every crate inherits its version from
`[workspace.package]` and none is published to crates.io.

## Crates

| Crate | Path | What it holds |
| --- | --- | --- |
| `c2m-i18n` | `crates/c2m-i18n` | The message catalog (`i18n/catalog.tsv`), `tr!` and language names |
| `c2m-core` | `crates/c2m-core` | Types and small helpers every other crate shares |
| `c2m-process` | `crates/c2m-process` | External programs: process lifecycle, tool lookup, yt-dlp and ffmpeg calls, locks, HTTP agents |
| `c2m-protocol` | `crates/c2m-protocol` | Progress events, as JSON on the wire and as bars on the terminal |
| `c2m-config` | `crates/c2m-config` | `config.toml` and the settings a task runs with |
| `c2m-dispatch` | `crates/c2m-dispatch` | The paid-request ledger and the task control every long step checks |
| `c2m-source` | `crates/c2m-source` | Video sources: metadata, downloads, subtitles, Bilibili and YouTube logins |
| `c2m-asr` | `crates/c2m-asr` | Speech recognition backends and their models; its `build.rs` builds the Apple speech module from `native/apple-asr` |
| `c2m-ai` | `crates/c2m-ai` | AI services: proofreading, translation, summaries and their logins |
| `c2m-artifact` | `crates/c2m-artifact` | Published notes: rendering, manifests and portable exports |
| `c2m-engine` | `crates/c2m-engine` | The conversion pipeline and the task protocol (`run-task`) |
| `course2md` | `crates/course2md` | The engine's facade for the desktop, and the `course2md` binary; the default `cli` feature adds clap, the setup wizard and `doctor` |
| `c2m-test-http` | `crates/c2m-test-http` | A loopback HTTP server, a dev-dependency only |
| `desktop-prefs` | `desktop/prefs` | Preferences, services and their checks, credentials, this machine's components, and how the desktop keeps a JSON record |
| `desktop-accounts` | `desktop/accounts` | The sign-ins' state: Bilibili, YouTube, Codex and Ollama |
| `desktop-store` | `desktop/store` | The workspace record of drafts and tasks, libraries and their folders, published notes, task activity |
| `desktop-reader-core` | `desktop/reader-core` | What the reader shows of a note, its links and reading positions |
| `desktop-tasks` | `desktop/tasks` | Sources, the import session, task plans, engine runs and the task view |
| `desktop-ui` | `desktop/ui` | Shared compositions on `gpui-kit-material` (settings rows, confirm dialogs, note text, formulas, symbols), without page state |
| `course2md-desktop` | `desktop` | The app: pages, windows and the `course2md-desktop` binary |

Three libraries split out of the engine live in repositories of their own and are pinned by
git revision in the root `Cargo.toml`: `subtitle-parse` (used by `c2m-source` and `c2m-ai`),
`slide-detect` (`c2m-engine`) and `model-fetch` (`c2m-asr`). They return typed errors and
events and carry no `tr!`; the crate that calls them translates. The desktop's interface comes
from `gpui-kit-material`, pinned at one revision of its `main` in `[workspace.dependencies]`
and in every `[patch]` entry.

## Dependency directions

Each crate depends only on crates in a row above its own:

```
c2m-i18n
c2m-core
c2m-process   c2m-protocol   c2m-config
c2m-dispatch
c2m-source    c2m-asr        c2m-ai        c2m-artifact
c2m-engine
course2md
desktop-prefs        desktop-accounts   desktop-ui
desktop-store
desktop-reader-core  desktop-tasks
course2md-desktop
```

- `c2m-protocol` depends only on `c2m-core` and `c2m-i18n`; `c2m-dispatch` on those two,
  `c2m-process` and `c2m-protocol`.
- `c2m-source`, `c2m-asr`, `c2m-ai` and `c2m-artifact` do not depend on each other. What two of
  them share belongs further up the list.
- Engine crates never depend on GPUI, the kit or a desktop crate.
- The app, `course2md-desktop`, reaches the engine through the `course2md` facade, built with
  `default-features = false` so it carries no command line. The desktop's library crates name
  the `c2m-*` crates they use instead: `desktop-accounts` needs only `c2m-source`, `c2m-ai` and
  `c2m-i18n`, `desktop-reader-core` only `c2m-artifact`, `c2m-config` and `c2m-i18n` beside
  `desktop-store`, and `desktop-ui` `c2m-artifact` and `c2m-i18n`.
- `desktop-prefs`, `desktop-accounts`, `desktop-store`, `desktop-reader-core` and `desktop-tasks`
  have no GPUI; `desktop-prefs` may use `gpui-kit-material-color` for palettes.
- Among them, `desktop-store` depends on `desktop-prefs`; `desktop-reader-core` and
  `desktop-tasks` depend on `desktop-store` (and `desktop-tasks` on `desktop-prefs`), not on
  each other. `desktop-accounts` depends on no desktop crate; its tests read `desktop-prefs`.
- `desktop-ui` depends on none of them; pages in `course2md-desktop` join them.
- The `test-support` features of `desktop-prefs` and `desktop-store` (an in-memory credential
  vault, workspace and note fixtures) are enabled only under `[dev-dependencies]`.
- `c2m-test-http` appears only under `[dev-dependencies]`.

A change that needs an edge against these directions usually means a type sits in the wrong
crate: move it rather than adding the edge. Cargo rejects cycles; the rest is kept by review.

## Logs

Each crate logs under its own target (`c2m_asr`, `c2m_source`, ...). `RUST_LOG=course2md=debug`
also covers every `c2m_*` crate, so filters written for the single-crate engine still work.

## Bisecting

Some commits on merged side branches do not build on their own: `e771098`, `3080007`,
`2cd910d` and `5e9f046` were made while a branch was mid-change, and only its merge builds.
Bisect along the commits of `main` and `split/workspace` themselves, where every merge is checked as a whole:

```sh
git bisect start --first-parent <bad> <good>
```

`--first-parent` keeps bisect on the commits of the branch itself, so it tests each merge as
a whole rather than the steps inside it. To look inside a merge once it is found, bisect again
between its two parents without the flag and `git bisect skip` a commit that does not build.

The reverts `d89579d` and `29cdd24`, which undo temporary CI triggers, carry no
`Co-Authored-By` line; correcting that would mean rewriting published history, so they stay.
