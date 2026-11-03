# Audit findings the split plan is based on

Taken on 2026-10-30 at `3db4a88`. Line numbers refer to that commit. Star counts came
from `gh api` the same day. Superseded where `REDUCTION.md` disagrees: its kit audit showed the
"candidates to add to the kit", the CJK dependency of `note_text`, and porting patches were wrong.

## Engine facts

`src/` was 25,340 lines. 27 of 38 modules form one cycle (`apple artifact asr auth cli
config dispatch download error execution fetch hub llm login media models npu options
pipeline portable progress provider render runtime settings summarize ytdlp`), plus
`checkpoint`/`timeline`. Only `tools`, `math`, `language` have no `crate::` module deps.

### Cycle-breaking order

1. Leaves: `atomic_write`/`make_shared` out of `checkpoint`, `digest` out of `execution`,
   `i18n` with `language`.
2. Core types: `VideoMeta` (three modules use `fetch` only for it), `TranscriptEvent`,
   `FailureKind`, `Outcome`, `Stage`, `fmt_ts`, typed error markers from `error`.
3. `runtime` stops depending on `dispatch`: `retry`, `interruptible_sleep`, `map_pool`,
   `wait_ready` move up. `progress` stops depending on `artifact` and `models`
   (`Event` embeds `Outcomes`, `LocalModelStatus`).
4. `config` splits into types, paths, site URL, model table; `device_present` injected
   instead of `npu`; `clap::ValueEnum` moves off domain types (config.rs:87,343,371,384,
   llm.rs:36, settings.rs:39). `Target::from_request` stops coupling `artifact` to `execution`.
5. `dispatch`: `receive()` off ureq, `Failure` off `artifact::FailureKind`, `block()` off
   `progress::emit`.

### Module assignment

| Crate | Modules |
| --- | --- |
| `c2m-i18n` | `i18n`, `language`, `i18n/catalog.tsv` |
| `c2m-core` | `timeline`, `VideoMeta`, `FailureKind`, `Outcome`, `Stage`, digest, atomic write, `fmt_ts`, error markers |
| `c2m-process` | `runtime` process half, `tools`, `ytdlp`, `media`, `error` tool half |
| `c2m-protocol` | `progress` wire types, `execution` request types |
| `c2m-config` | `config`, `settings` |
| `c2m-source` | `fetch`, `auth`, subtitle evidence (`subtitle-parse` underneath) |
| `c2m-ai` | `llm`, `provider`, `login`, `summarize`, `dispatch` |
| `c2m-asr` | `asr`, `npu`, `models`, `model_status`; `apple` until `apple-asr-sys` |
| `c2m-artifact` | `artifact`, `portable`, `render`, `math`, `checkpoint` store |
| `c2m-engine` | `pipeline`, `execution` |
| `course2md` (bin) | `cli`, `main`, `doctor`, `wizard`, `options` |

### Do not publish

`auth` (unofficial Bilibili API), `login/codex` (reuses Codex CLI `client_id`; breakage and
terms exposure), `i18n`.

## Desktop facts

- `impl Desktop` in every page file: pages cannot leave the crate until `Desktop` is split.
- About 11.3k production lines and 5.7k test lines are nearly gpui-free: `desktop-core`
  takes `workspace`, `preferences`, `storage`, `organize`, `source`, `backend`, `activity`,
  `credentials`, `service_test`, `model_discovery`, `notes`, `reader_navigation`,
  `conversion`, `provider_serde`. `bounded_http` merges into the engine's `runtime`.
- Edges to cut: `storage.rs:383,387` and `organize.rs:121` (workspace `new_id`/`now`);
  `conversion.rs:15` (`text_source_index`); `backend.rs:742-744` (scroll re-export);
  `notes.rs:5` (`SharedString`); `model_discovery.rs:406` (`model_field`).
- `desktop-ui`: `note_text` (unless TextView spike wins), `math`, settings rows
  (`settings_ui.rs:212-579`), `task_log_box` (`task_ui.rs:3315`), `option_switch_tile`
  (`import_ui.rs:56`), `reader_ui.rs:2126,2142`.
- Stay in app: all pages, `task_runner`, `course_library`, `model_diagnostics`,
  `generation_settings`, `service_form`, `performance`, `capture`, `views`, `i18n`.

### Duplicated desktop/engine logic to merge

HTTP agent (`bounded_http.rs:11`, `runtime.rs:358`, `llm.rs:166`); bounded read
(`bounded_http.rs:48`, `source.rs:594`, `dispatch.rs:821`, `llm.rs:1033`); multipart
(`service_test.rs:495`, `asr.rs:728`); JSON extraction (`service_test.rs:603`, `llm.rs:684`);
process lifecycle (`backend.rs:44-139,440,703`, `runtime.rs:34,204,384`, `source.rs:296`);
no-window flag (`backend.rs:38`, `runtime.rs`); dir fsync (`storage.rs:289`, `artifact.rs:586`);
byte formatting (`activity.rs:619`, `main.rs:643`). Within desktop: `.json.bak` policy
(`organize.rs:44`, `preferences.rs:1008`, `workspace.rs:2447`), writer threads
(`preferences.rs:863-1006`, `workspace.rs:1250-1370`), ID schemes and clocks
(`workspace.rs:28,37`, `preferences.rs:23,64`).

## Desktop to kit map

| Desktop | Kit (`crates/material/src/`) |
| --- | --- |
| `palettes.rs` | `crates/color/src/palette.rs` (check saved preferences still deserialize) |
| `theme.rs:62` palette_colors, `:195` sync | `crates/color/src/scheme.rs`, `theme.rs` |
| `theme.rs:274-315,388-398` tokens, `:1186` shadows | `tokens/{shape,layout,typography,elevation}.rs` |
| `theme.rs:492,655,859,882,891` tiles | `components/tile.rs` |
| `theme.rs:905,932,1128` radio, ChoiceTile, Tint | `components/choice.rs` |
| `theme.rs:427,1200-1304,1465` text | `components/text.rs` |
| `theme.rs:2059,2144,1548` notice, badge, card | `components/feedback.rs` |
| `theme.rs:2165` chip | `components/chips.rs` |
| `theme.rs:1499` search | `components/search.rs` |
| `theme.rs:1595,1837,1855,1959` buttons | `components/button.rs`, `icon_button.rs` |
| `theme.rs:1970,2014` dialogs | `components/dialog.rs` |
| `theme.rs:2198` field label | `components/text_field.rs` |
| `theme.rs:2238` switch | `components/switch.rs` |
| `theme.rs:239`, `select_field.rs` | `components/menu.rs` (`ExposedDropdown`) |
| `motion.rs` springs, progress | `motion.rs`, `components/progress/*` |
| `choice_group.rs` | `components/button_group.rs`, `tabs.rs` |
| `focus_scroll.rs` | `components/focus.rs` |
| `window_controls.rs` | `components/caption.rs`, `title_bar.rs` |
| `icons.rs` | `icons.rs` (Symbols) |

Candidates to add to the kit: `transfer_status` (`motion.rs:330`), scrollbar policy
(`scroll.rs`), `ModalBackground` (`a11y.rs:45`), `bounded_scroll_panel` (`reader_ui.rs:6707`),
`gutter_scrollbar`, `scrolled_card_top` (`reader_ui.rs:2046,2061`), disclosure
(`task_ui.rs:669,764`), image viewer (`reader_ui.rs:191,5727-6421`), `reader_images.rs` after
removing `Locate = fn(&mut Desktop)`. Product layout stays in the app: `hero_top`,
`cover_frame`, `detail_row`, `nav_destination`, `drag_chip`.

## Patch triage (`desktop/patches`, status in `STATUS.md`)

| Patch | Finding |
| --- | --- |
| `component-button-text-size` | Not upstream; drop with `MaterialButton` |
| `component-input-accessibility` | Partly in Kit 0.8.1 (`input.rs:49,111,292`); re-audit remainder |
| `component-menu-style` | Drop with `MaterialMenu` |
| `component-selectable-text-highlights` | Partly upstream (#3699); `set_range_highlights` is the alternative |
| `component-switch-geometry` | Drop with `MaterialSwitch` |
| `component-tooltip-lifecycle` | Possibly partial; check against `MaterialTooltip` |
| `zed-cjk-line-break` | **Not in kit vendor. Port to `gpui-pre` and `gpui-fast`.** `note_text.rs:30` depends on it |
| `zed-input-accessibility` | Not upstream; goes with the component input patch |
| `zed-macos-window` | Kit's `gpui-pre-macos/window.rs:2036` lacks the stopped-link fallback; audit and port if needed |
| `zed-metal-frame-lifetime` | In kit vendor |
| `zed-metal-present-stages` | Mostly superseded by `presentation_wait` |

## Bugs

Tick in `STATUS.md`.

### Engine
- E1 `fetch.rs:867-868,903`, `auth.rs:54`: `LoginTip` wrap hides `Bilibili412` and `Failure` from `downcast_ref`; backoff never fires, cancel looks like a login problem.
- E2 `llm.rs:165-171`, `fetch.rs:447-451`: ureq 2 `.timeout()` overrides `.timeout_read()`; the 90 s stall limit does nothing.
- E3 `dispatch.rs:762-766`: free services get a consent block on HTTP 5xx; the transport branch at :732 honours `Resend::Free`.
- E4 `fetch.rs:1051,1061`: non-UTF-8 yt-dlp output ends the reader, the pipe stays undrained, possible hang.
- E5 `pipeline.rs:236,696`: CLI sidecars use `read_to_string` not `subtitle::read_subtitle_text`.
- E6 `checkpoint.rs:238`: `read_to_string` fails when a torn tail splits a multi-byte char.
- E7 `subtitle.rs:475-481`: tag stripping eats `x < 3 and y > 2`, numeric entities undecoded, rolling cues.
- E8 `login/codex.rs:243`: refresh lock is in-process only; use `runtime::lock_file`.
- E9 `login/codex.rs:481-494`: OAuth denial cause discarded; terminal waits out the 180 s timeout.
- E10 `login/codex.rs:456`: unencoded `redirect_uri`.
- E11 `llm.rs:64`, `settings.rs:60`, `config.rs:416`: `Debug` derived over `api_key`.
- E12 `llm.rs:1034`: `test_connection` truncates silently.
- E13 `fetch.rs:980`: cancel not observed during yt-dlp probes.
- E14 `render.rs:214,229`, `portable.rs:181`: unlocalised output.
- E15 `main.rs:308`: `config init --force` skips backup and atomic write.
- E16 `npu.rs:96,121`: `uv run --with openvino-genai` is unpinned.
- E17 `hub.rs:143-147`: unannounced IP lookups.
- E18 `llm.rs:635,643,1011,925`: prompts vary with locale and are ledger identity, so resuming a CLI task in another locale can re-send paid requests. Prompts must not go through `tr!` by UI locale, or the identity must exclude it.
- E19 `i18n.rs:55-59`: `zh-Hans-HK/TW/MO` resolve to Traditional.
- E20 `i18n.rs:77-83`: `LC_ALL=C` falls through.
- E21 `i18n.rs:337-345`: `{name}` consumes an argument and prints `name}`.
- E22 `i18n.rs:162,410-415`: parser and unit test disagree on `#` lines; `tests/i18n_catalog.rs:230` unescape differs from `i18n.rs:188`.

### Desktop
- D1 `notes.rs:460-462`: nested images panic (`![![b](c)](e)`); `![x][PIC]` with `[pic]:` keeps the definition; trailing newline dropped.
- D2 `credentials.rs:381-385`: vault chosen on each launch on non-macOS, no migration; file vault has no cross-process lock.
- D3 `storage.rs:609-615,534-535`: O(n²) journal rewrite; each file hashed four times (`:371,634,656,667`).
- D4 `backend.rs:132,448`: bare `taskkill` instead of `system32_program`.
- D5 `source.rs:422-428`: `https://b23.tv/abc，看这个` keeps the glued text.
- D6 `service_test.rs:603-610`: stricter JSON parsing than production `llm.rs:684`.
- D7 `backend.rs:410-431`: non-UTF-8 line becomes `Event::error` and is lost; a persistent read error loops.
- D8 `source.rs:503,795,849`: hard-coded `"zh"` interface language.
- D9 `activity.rs:507`: compares a translated label with Chinese literals (`task_ui.rs:3436`).
- D10 `storage.rs:14`: `.course2md-move.json` never removed.
- D11 `source.rs:16` vs `:569` (mtime); `source.rs:548` unstable hash (use `execution::digest`); `math.rs:106` bypasses `CACHE_LIMIT`; `storage.rs:450-458` writes during read; `notes.rs:130` swallows the newer-schema error; `workspace.rs:1919,2466,2477,2494` `.expect` on a lock; `credentials.rs:313-317` unzeroized copies; misplaced doc comments `reader_navigation.rs:235`, `source.rs:153`.
- D12 `scroll.rs:10-30`: reads the preference once by spawning `defaults`; `Automatic` with a mouse unhandled (subsumed by P5 if the kit policy replaces it).
- D13 `backend.rs:492-497`: hand PATH walk misses PATHEXT; use `which::which_in`.

### Tooling
- T1 `install.sh:44-55`: checksum verification fails open.
- T2 `aur.yml:29`, `brew-tap.yml:24`, `release.yml:201`: a `dry_run` release triggers channel workflows.
- T3 `build.rs:204-207`, `Package.swift:11`: speech-swift tracks `main` with no `--force-resolved-versions`.
- T4 `build.rs:51-59`: failed Swift build warns and ships a release without the Apple backend.
- T5 `shim.swift:408-415`: 16 KB truncation can split UTF-8 (`apple.rs:286`).
- T6 `shim.swift:88-90,60-66`: `c2m_vad_detect` reloads each call, `c2m_vad_load` discards its model.
- T7 `package.py:58-65`: Rust and Swift dependency licences not shipped.
- T8 `site/_headers` `?v=` disagrees with pages; "CUDA" typo in `prebuilt.py:8`, `docs/PACKAGING.md:11`, `src/asr.rs:3,233` (check the spelling in the files); stale comment `build.rs:173`; `sources.py:26` old repo name.

## Wheels

### Replace

| Location | With | Stars | Caveat |
| --- | --- | --- | --- |
| `language.rs:69-155` | `whatlang` | 1,087 | Test kanji-heavy Japanese |
| `notes.rs:160-208`, `storage.rs:197-238` | `walkdir` | 1,555 | Keep symlink errors and cancel checks |
| `portable.rs:94-106` | `image::guess_format` | 5,898 | Confirm GIF/WebP with current features |
| ureq 2, `asr.rs:716`, `service_test.rs:495` | `ureq` 3 | 2,219 | About 57 sites; multipart is `unversioned` |
| `tests/i18n_catalog.rs:39-191` | `syn` (dev) | 3,437 | Rows 119, 420 pass only through `tracing` strings |
| `credentials.rs:116-184` | `keyring` (already a dep, 776; consolidation) | | Test reading existing macOS items first |
| `workspace.rs:28` | `uuid` (already a dep, 1,248) | | Check `execution::valid_id` accepts the shape |
| `settings.rs:170` (optional) | `toml_edit` | 1,087 | |

Add: `release-plz` 1,514, `cargo-deny` 2,470. Dependency bumps: `cargo update` (rustls,
webpki, tokio advisories), zip 9, toml 1, indicatif 0.18, getrandom 0.4, sha2 0.11, base64 0.23.

### Spike, then decide

Non-Apple VAD (`asr.rs:18`) vs sherpa-onnx 15,620 + silero-vad 10,563; `folder_tree.rs` vs Kit
`Tree` (drag needed); `note_text.rs` vs Kit `TextView` (formula and copy behaviour needed).

### Do not replace

`tr!` (fluent), LLM client (async-openai, rig), JSON state store (rusqlite, redb, sled), image and
formula caches (moka), OAuth/JWT (oauth2-rs, jsonwebtoken), retry and pool (backon, rayon), config
layering (config-rs, confy), ffmpeg calls (rust-ffmpeg), packaging (cargo-dist, goreleaser,
cargo-bundle, tauri), Zed `markdown` (GPL-3.0), whisper.cpp, search (regex, nucleo).
