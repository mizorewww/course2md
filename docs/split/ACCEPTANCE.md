# Acceptance round 1

Findings of the four independent reviews of `d89579d` (UI, app flows, CLI, plan conformance), all FAIL.
Evidence paths are under the session scratchpad
`/private/tmp/claude-501/-Users-aac6fef-Developer-course2md/efbdb7fd-15b6-4010-9790-15ca1d457be9/scratchpad/accept-{ui,app,cli,plan}/`.
Each fix records its commit here. A fresh reviewer re-checks every item before acceptance.

Owner rulings: paid-request regressions are fixed, not waived. `TextView` block styling goes into the
kit's vendored engine (PR #16). Wave 2's row-plus-menu choices (H7, S13) are reversed: data entry uses
`ExposedDropdown`, as the owner asked for M3 dropdowns.

## Engine and CLI (agent: engine-fixes)

| ID | Sev | Finding |
|---|---|---|
| C1 | blocker | A plain CLI conversion under `COURSE2MD_LANG=en`, `ja` or `zh-Hant` gives the proofreading request a stable id different from baseline `3db4a88` (en `f3554590…` vs `08a691b6…`), so an upgraded task re-sends a finished, paid proofreading batch (one per 20 paragraphs). `zh-Hans`, `--json`, `run-task` match. Evidence `accept-cli/ev/paid-human-en/`, `ev/paid2/`, `ev/paid-proof/` |
| C2 | should | CI doesn't run `prompt_identity`, the test `docs/DESIGN.md:52` cites as the guarantee (and under an old path) |
| C3 | should | A single-file online video (yt-dlp renames to `media.mp4`) fails with "Downloaded video missing"; baseline too |
| C4 | minor | `llm status` prints `open-ai-compatible` (strum `Display`, `c2m-config/src/llm_settings.rs:16-20`); baseline `openai-compatible` |
| C5 | minor | JSON request bodies became pretty-printed (ureq 3), 85–134 bytes more per request |
| C6 | minor | `config show` floats `1.0`; template `["md","html"]`; log event renames (`api vad`→`vad`, `ssim scan done` lost `mode`); Japanese help punctuation and `--help を実行`. Restore baseline output unless a change is deliberate and recorded |
| C7 | minor | `RUST_LOG=course2md=...` no longer covers the engine (`c2m_*` targets): document it, or make the old filter work |
| C8 | minor | Human text bypasses the catalog: `llm status`, `config show`, `models list`, Ollama error, "正在加载本机模型…"; piping to `head` panics on broken pipe. Baseline too |
| C9 | should | Swift layer ignores `$HOME`: `models prepare` writes the real `~/Library/Caches/qwen3-speech` while `inspect` reads `$HOME`; a `QWEN3_CACHE_DIR` under `/private/tmp` is refused; ignored mirror test hard-codes `target/debug` |
| C10 | should | E11 incomplete: `RunOpts` derives `Debug` over `asr_api_key`, `llm_api_key` (`options.rs:132-133,198,278`) |
| C11 | should | `subtitle.rs:189,195` still defaults to `"zh"` |
| C12 | should | Desktop on `ureq = "2"` (`desktop/core`), with `bounded_http.rs:17-21` and `source.rs:627-630` setting `.timeout_read` and `.timeout` together (the E2 no-op pattern); merge HTTP agent, multipart, process lifecycle and byte formatting per AUDIT; `base64` 0.22 beside 0.23; unused `unicode-segmentation` in `desktop/Cargo.toml`; desktop crates not on workspace dependencies |
| C13 | should | AUDIT cycle steps 4 and 5 not done: `clap::ValueEnum` on domain types (8 sites, e.g. `config.rs:94`); dispatch tied to ureq and `progress::emit` (`dispatch.rs:367,658`). Odd edges: `c2m-protocol`→`c2m-config`/`c2m-process`, `c2m-artifact`→`c2m-dispatch`; `desktop-ui` reaches `c2m-artifact`/`c2m-i18n` past the facade; `desktop-core` pulls the whole facade incl. CLI and clap. About 80 `pub` items used only in their crate, three only by tests |
| C14 | should | `cargo deny` excludes the desktop crates; 8 unmaintained advisories without a desktop decision |

Resolutions (branch `split/engine-fixes`; evidence under the session scratchpad `engine-fixes/ev/`):

| ID | Commit | Resolution and evidence |
|---|---|---|
| C1 | `ai: proofreading receipts saved by 2.0.1 in any language are found again` | Cause: E18 (`7aab974`) fixed the proofreading rule and vision note to Simplified Chinese, so the body, the ledger identity, changed for CLI tasks begun in English, Traditional Chinese or Japanese (`--json` and `run-task` always sent Chinese). Reverting E18 would make a language change resend again, so the earlier wording of each language is kept as a former identity: `RequestSpec::former`, and a receipt under one continues under its id. `prompt_identity` checks 16 ids taken from `3db4a88` (vision, custom prompt, four languages, OpenAI-compatible and Ollama): completed ones replay in every language, an in-flight one stays unresolved, new tasks keep the Simplified Chinese id. Matrix of 9 option sets × 4 languages × human/`--json`/`run-task` (`ids-base.json`, `ids-new0.json`): proofreading in the three languages was the only id that differed. 2.0.1 killed mid-proofreading in each language, resumed by this build: 0 requests, the 2.0.1 receipt marked uncertain (`proof/`) |
| C2 | `ci: run prompt_identity; DESIGN.md cites its path` | `ci.yml` runs `--test prompt_identity` with the other integration tests on all three platforms; `docs/DESIGN.md:52` names `crates/course2md/tests/prompt_identity.rs` |
| C9 | `asr: the Swift downloader uses the cache models inspect reads` | `main` sets `QWEN3_CACHE_DIR` before any thread to the directory `inspect` uses (`$HOME/Library/Caches` by default), with `/private/tmp` given as `/tmp` because speech-swift's containment check compares a standardized directory (which drops `/private`) with an unstandardized file. Native build: `models prepare --provider coreml` with `HOME` under `/private/tmp` wrote 675 MB to `$HOME/Library/Caches/qwen3-speech`, nothing to the real `~/Library/Caches`; with `QWEN3_CACHE_DIR=/private/tmp/...` it downloaded and `inspect` read the same directory (`c9-*.ndjson`). The mirror test finds the binary beside its own target directory and passed (15 s) |
| C10 | `engine: RunOpts no longer derives Debug` | Nothing formats `RunOpts`, so the derive is removed rather than written by hand; the workspace builds without it |
| C11 | `source: subtitle files follow the interface language` | Sidecars and a downloaded track's directory prefer the interface language, then English, as the desktop does since D8. `--json` and `run-task` run in Simplified Chinese, so their choice is unchanged; a resumed task keeps its cached transcript, so no request changes under it. Unit test over five languages |
| C4 | `config: the OpenAI-compatible provider shows its 2.0.1 name` | `Display` and `as_str` give `openai-compatible` again; serde keeps `open-ai-compatible`, which 2.0.1 also wrote to files. Unit test on both forms |
| C5 | `http: JSON request bodies are compact again` | `runtime::send_json` serializes with `serde_json::to_vec`; the AI requests, the connection test, the Codex refresh and the NPU worker use it. Ledger ids hash the value, not the bytes, so they do not change. `prompt_identity` asserts every body equals its compact form |
| C6 | `cli: config output, log events and Japanese help read as in 2.0.1` | `config show` prints numbers as Rust does (`1`, `10`, `20`; `max_speech` from its f32); the template spaces lists (`["md", "html"]`, unit test); `api vad` (with `endpoint`, `model`), `npu vad`, `vad` with `engine=silero-coreml` per backend again; `ssim scan done` has `mode` again; the new `asr ready` timing moves to debug; Japanese `コマンド：`, `オプション：` and `--help を参照してください` restored in the catalog |
| C7 | `cli: RUST_LOG=course2md=... covers the engine crates again` | Each `course2md` directive is repeated for the eleven `c2m_*` crates (`course2md::asr=debug` also becomes `c2m_asr::asr=debug`). `RUST_LOG=warn,course2md=info` on a subtitle run prints the same four INFO lines as 2.0.1 (`d89579d`: one); unit test |
| C3 | `d31deda`, `49fbb45` | yt-dlp writes into a private directory as `media.%(ext)s`, and the one finished media file is moved into place, so a single-file format no longer ends as "Downloaded video missing". A failed run leads with yt-dlp's last `ERROR` line; a dropped connection is retried only when it is yt-dlp's final reason. Unit tests `a_download_is_the_one_finished_file_whatever_yt_dlp_named_it`, `a_failed_run_puts_yt_dlps_reason_first_and_the_login_hint_after_it`, `only_a_run_that_ended_on_a_dropped_connection_is_retried_for_it`, `a_preview_is_retried_only_when_yt_dlp_ended_on_a_dropped_connection` |
| C8 | `e48430c`, `d0228f4` | `llm status`, `config show`, `models list`, the setup messages, the Ollama connection errors and the Swift shim's "正在加载本机模型…" go through the catalog in all four languages (`i18n_catalog` covers the new rows). A closed stdout (`config show \| head`) exits with status 141, as a SIGPIPE-killed program would, instead of panicking; no automated test for it |
| C12 | `b6b232c`, `125eef5`, `074098a`, `c3cb763` | The desktop's last ureq 2 user moved to ureq 3 through `runtime::agent_builder`, so its read and write timeouts take effect (loopback tests `an_error_status_arrives_with_its_body_and_the_key_is_sent`, `a_redirect_is_not_followed_and_a_large_body_is_refused`). Every external dependency of the desktop crates is declared once in the workspace; the desktop's own `base64` is 0.23 (0.22 remains only through `ratex-svg`) and `unicode-segmentation` left the app. Tool checks and process kills use the engine's `run_ok` and `kill_process`. Not merged, by reading: the multipart bodies are two different forms (the service test's sample upload, the engine's transcription upload), and the byte formatters print differently (CLI `format_size` one decimal GB or whole MB; desktop `activity::bytes` two decimals, KB and a localized byte count), so merging either would change output |
| C13 | `d93dc6a`, `7f89a08`, `78aaa95`, `5147873`, `c721b84`, `ccb7076`, `4d9244d` | `c2m-config` drops clap: the CLI parses its enums through `options::choice`, and every `-h`/`--help` page and invalid-value error in the four languages is byte-identical. The request ledger takes any transport's answer (`receive` moved to `runtime`; test `only_failures_before_sending_count_as_unsent` moved with it). `c2m-protocol` depends only on `c2m-core` and `c2m-i18n`. The facade's CLI is a default `cli` feature, and the desktop takes the facade without it, so a desktop build compiles no clap. 49 engine and 66 desktop items used only in their crate are `pub(crate)`. The desktop's library crates name the `c2m-*` crates they use, as `docs/CRATES.md` § Dependency directions records; `c2m-artifact` → `c2m-dispatch` stays, since the artifact uses the ledger and pool and dispatch sits above it in that order |
| C14 | `7ca0b20` | `cargo deny` covers the whole workspace: licenses match what `packaging/licenses/about.toml` ships, the GPUI git sources are allowed, and each unmaintained advisory the desktop brings (ttf-parser's existing entry widened, seven new) is ignored with the path that reaches it and what would remove it |

## Desktop behaviour (agent: desktop-logic)

| ID | Sev | Finding |
|---|---|---|
| A2 | should | Kill during an in-flight AI request, relaunch: after the surviving worker saves, the task stays 结果尚未确认 with only cancel; 调整后重试 refused as duplicate. Baseline too |
| A3 | should | A queued task intermittently fails on its own `.task.lock` (目录正被另一任务使用); baseline too |
| A4 | should | Real moving footage ends partial: screenshots 未采样到任何帧; baseline same on synthetic video |
| A6 | should | 正在处理 stays on a note after the resend finished in a follow-up task |
| A7 | should | A resend that fails to start leaves 这些请求已获得重新发送授权 stuck |
| A8 | minor | API key stays in the Keychain, and drafts in `services.json`, after cancelling Add Service |
| A9 | minor | Connection refused is reported as possibly charged (`outcome_unknown`) |
| A10 | minor | After a language change, the settings title and search placeholder keep the old language until restart; load-time settings messages are Chinese under an English UI (`Store::open` before `i18n::apply`) |
| A12 | should | By reading, to confirm and fix: formula cache reset blanks formulas (`ui/src/math.rs:36-43`); newer-schema note fallback/overwrite (`core/src/notes.rs:127-131`, `c2m-artifact/src/artifact.rs:348-409`); resumed library move accepts a stale same-size copy (`core/src/storage.rs:519-525`), risk of silent data loss |
| A13 | should | `workspace.rs:2069,2219`, `preferences.rs`, `organize.rs` open-code `.json.bak` beside `store.rs` |
| A14 | minor | Palette chooser with an unwritable settings dir: confirm a failed save keeps the dialog open with its failure (skill rule) |

## Desktop UI (agent: desktop-ui)

| ID | Sev | Finding |
|---|---|---|
| A1 | blocker | Four `ConnectedButtonGroup`s do nothing on click or keys: library 列表, reader 截图 tab, workbench 仅字幕/语音识别, local/link switch. Pages use `.on_activate` (`course_library.rs:1334`, `reader_ui.rs:1937`, `import_ui.rs:652,1661`) but the kit's required single selection only calls `on_change` |
| A5 | should | Features lost against baseline: clear (×) in settings search and in screenshot search; recovery of an unsaved settings change after relaunch (removed in `cdd31e3`); time chip and maximize button on screenshots in the note |
| U1 | blocker | Dropdowns became full-width rows with a chevron opening a menu: home options; settings subtitle language, interface language, local engine, AI service. Use `ExposedDropdown` for choosing a value everywhere |
| U2 | should | Reader resize: 79k chars, 15/64 draws over 100 ms (worst 489 ms); 317k chars worst 1,039 ms, measured at load ~240. Re-measure on a quiet machine and report; a fix needs `list.rs` in `gpui-pre` and `gpui-fast` and an owner ruling |
| U3 | should | O3 false: first-use model step lacks its purple tinted circle |
| U4 | should | G17 false: page titles measure 24, not 28; the role override doesn't apply |
| U5 | should | Needs-attention task card: long title clipped mid-glyph without ellipsis, kebab not visible |
| U6 | should | Partial and uncertain cards show red circle plus amber chip and notice; a partial note is a warning |
| U7 | should | Palette picker ~720 wide vs 918; names wrap; outlines broken at bottom; no selection ring; fourth card cut at 200% |
| U8 | should | Off switches pale like disabled; a third switch disabled with no visible reason |
| U9 | should | Library destination icon `newsstand` reads as a chart |
| U10 | should | Library sidebar selected item is a rounded rectangle, not a pill; height ~60 not 44 (kit offers 40 or 56: kit PR) |
| U11 | should | R4: section timestamp split into bare `00:00` plus pill; restore one pill on the heading row via the inline `SourceLink` plugin |
| U12 | should | Collection link shows a red error ring with no error while candidates load |
| U13 | should | First-use model step shows 重新检查 twice |
| U14 | should | Wording: "Saved name" for 保存名称 (catalog line 315); "Screenshots was not generated"; stray Japanese space before は |
| U15–U25 | minor | App-bar current destination and group-by-folder neutral instead of tonal; preview's green "has subtitles" chip gone; Nord red on slate; settings rail band; wordmark weight; reader bar centring; queued-row hover band gaps; 复制与导出 lost chevron; diagnostics warning wrap; focus after closing find lands on "more"; hand-painted borders `import_ui.rs:758,2527`, `reader_ui.rs:4002`, hand chip `import_ui.rs:1201`, `views.rs:76,82`, `reader_ui.rs:2024`, brand colour twice (`account_ui.rs:85`, `import_ui.rs:126`), dead symbols `open_in_full`, `progress_activity`, `storage`; `MaterialDivider` unused; duration chip hand-painted |
| P3 | should | Partial `restore` rows by source: G3 (9 untoned Outlined), G4 (10 of 12 quiet actions primary), G5 (find and view tools without circle), G8 (`library_ui.rs:969` radius 4; two fields without floating label), H4 (16/16 vs 20/24), L1, L6, T6 (padding 16 vs 20), R9 (radius 16, not pill), R12 (action off the image), S1 (selected circle; compact rail plain), S11, O3. Fix `DESIGN-RESTORE.md` so it is true |
| R8 | should | After kit PR #16 merges: summary as a surface; close the `inline_formula` escape hole (`desktop/ui/src/note_text.rs:107`) |

### Desktop UI fixes

Each line names the commit by subject; evidence is under the session scratchpad `desktop-ui/shots/`.

- A1, `desktop: groups with a required choice report it through on_change`: all nine `ConnectedButtonGroup`s
  (library view, reader views, source kind, home text source, settings text source, recognition mode, appearance mode,
  text size, service check) changed by click and by arrow key in the running app (`live/a1a` to `live/a1h`). The reader's
  view switch now keeps focus, so its arrow keys go on choosing.
- A5 (unsaved settings), `desktop: offer a settings change whose save failed again after relaunch`: a failed
  save leaves its value in `desktop-preferences.recovery/`, as before `cdd31e3`; relaunch shows it with 重试保存,
  and a successful save or a reset removes it. Read-only settings directory, relaunch, retry, relaunch (`live/a5a` to `live/a5d`).
- A5 (search clear), `desktop: clear the settings and screenshot searches in place`: a × in the field while it holds
  text, as the library's search has; typed and cleared by real input (`live/a5e`, `live/a5f`).

- Kit pin, `desktop: pin gpui-kit-material main at 79bce30`: both dependency and all six `[patch]` entries;
  the hero field and outlined fact chips follow the kit's API.
- U25 (duration chip), `desktop: a video's duration over its cover is the kit's media label`: preview and recent
  notes (`t1/home-recent`, `m1/home-preview`).
- A5 (screenshots), `desktop: a screenshot in the note shows its time and an enlarge action` (`m1/reader-end`).
- R8, `desktop: a formula in note text stays on one line` and `desktop: the note's summary is a panel again`
  (`r8/reader-light`, `r8/reader-dark`).
- U14, `i18n: the rename button and missing-part notices read naturally`.
- U9 and dead symbols, `desktop: the library destination shows books, not a chart`; `progress_activity` is the kit's
  spinner and stays, listed with the kit's symbols.
- U13, `desktop: a model surface under an engine obstacle offers one 重新检查` (source only: no installable
  engine obstacle could be produced on this machine).
- U3, `desktop: a model's surface leads with its purple circle` (`u3/model`, `u3/model-dark`).
- U5, U6, `desktop: only a failed task's card leads with a red circle` (`t1/tasks`).
- U15 and L6, `desktop: the current destination and folder grouping are tonal when on` (`u15/lib`, `u15/lib-group-dark`).
- U8, `desktop: the screenshot switch says why it is off and disabled` (`u8/g3`). The enabled off switches are the
  kit's M3 off state and read apart from the disabled one in the same capture; unchanged.
- U12, `desktop: a listed collection leaves the link field unringed` (source only: no network fixture).
- U11, `desktop: a section's time and its watch action are one pill again` (`u11/r`).
- G3, G5, T6: `desktop: every outlined action has a primary label`, `desktop: the reader's find and view tools sit in
  the neutral circle` (`g5/r`), `desktop: a task's own card on Tasks has roomy padding` (`t6/tasks-dark`).
- Open, waiting on kit PRs: U1 (#18, #19), U4/G17 (#20), U10/L1 and R9 (#21). U2 not re-measured: load average
  30 to 36 from other builds, so a measurement would not mean anything. U7/S11 needs a kit dialog wider than 720
  and a selected-card ring. `DESIGN-RESTORE.md` lists the rest under Not finished.

- Round 3, after merging `split/workspace` (`fbc7bb0`): U19 wordmark weight; U22 export chevron; U24 find returns
  focus to the note (`u22/after-find`); U16 confirmed-subtitle success line (`u16/h`); U25 one Bilibili mark
  (`symbols::bilibili()`); G8 and H4 (`h4/h`, `h4/opts2`); G4 (`g4/t`). R12 kept by the design language.
- Not app-side: U17 (Nord's error roles read dim on its slate surface) is the kit palette; U18 and S1 need a
  `NavigationRail` ground and tinted icons; U25's dashed drop zones need a kit drop target; U7/S11 a wider dialog
  and a selected card. U20, U21, U23 were not reproduced at the capture size (1280 wide). U2: load average 29 to 37
  throughout, so not measured.

- U10/L1 (part), `desktop: the sidebar's drop outline adds no height`: rows measured 60 as 56 plus a transparent
  2px drop border on each side; the outline is now drawn inside the row (`l1/l-click`, rows at a 56 pitch, click and
  hover still reach the item). The 44 height and pill wait on kit #21 and its pill split-off.

- Round 4: the workbench's screenshot switch says why it is disabled (`iv/ho`); the image viewer is framed by the
  kit's outlined `CardSurface` (`iv/vd`); capture builds take `size:<w>x<h>`. At 900 and 1600 wide and 200% text:
  U23 reproduced and fixed (`sz/a200s2`, `sz/awide`); the reader's folded play action was the one tool without a
  container, now outlined (`sz/r900`). U20: at 1600 wide the breadcrumb and title sit about 4pt above the back
  button's centre; not fixed. U21: queued rows could not be held queued on this machine (the runner starts and
  fails them without tools), so not reproduced. U17: Nord's palette is kit data (`crates/color/src/palette.rs`);
  a kit PR for its error contrast is being prepared. U2: load average 8 to 13 during the attempt, so not measured;
  capture builds with `performance` log draw histograms only, frame events need `COURSE2MD_FRAME_TRACE`.
- U7 (200% text), `desktop: a palette card's preview gives up height before its name is cut`: the grid already
  reflows to fewer columns; on a short window the scrolling body was shorter than one row, so names were cut under
  the previews (`pal/p200w2`, `pal/p200d`, `pal/p100`). Kit #23's width cap is rem-scaled and does not address this.
- Kit PRs open for the rest: #18 and #20 ready for the owner; #19, #21, #22, #23, #24, #25, #26 in review.

- Kit main `ae6b517` (#18, #20, #21 merged), `desktop: pin gpui-kit-material main at ae6b517`. U4/G17: page titles
  now measure 28 with no app change (`u10/l`). U10/L1, `desktop: sidebar rows are 44 tall`: 44 rows at density −3
  (`u10/l`, `u10/ld`); the pill waits on #26. S1, `desktop: narrow settings keep their categories as the tinted list`
  (`s1/c`, `s1/cd`, `s1/cen`). U1 waits on #19.

- Kit main `53fa0fd` (all of #19 to #27 merged), `desktop: pin gpui-kit-material main at 53fa0fd`. L1 pill
  (`pill/l`, `pill/ld`); R9 pills (source only: the fixture notes have no outline); S11 (`s11/p`, `s11/pd`, `s11/p200`);
  drop zones on `drop_target` (source only: the capture build cannot drag files from outside); U1 dropdowns
  (`u1/gopen`, `u1/ho`, `u1/ad`). U17 is the kit's palette change (#27): every palette's error colours follow it.

- U20, `desktop: the reader bar's breadcrumb and title centre on the back button`: within 1pt at 1600 wide
  (`u20/r`). Dropdown menus covering their own field in a short window: a kit bug (the popover clamps instead of
  switching sides); draft kit #28 opens them above, or scrolls on the roomier side. U2: load 21 to 51 while
  attempted, not measured.

## Plan conformance (after the three fix agents)

| ID | Sev | Finding |
|---|---|---|
| P1 | blocker | Reduction -19.8% by metric, about -13% real, against -40%; rose +1,009 lately; never re-estimated |
| P2 | blocker | Desktop is not a thin wrapper: binary 28,371 lines, 45% of the repo; `impl Desktop` in 21 files; `desktop-core` one 15,307-line crate with 406 public items |
| P4 | should | Stale docs: `STATUS.md`, `PLAN.md` (Locations, crate list, phase state, checks, 78,464), `MIGRATION.md:8,15`, `desktop/assets/themes/SOURCES.md:3`, `BUGS-ENGINE.md:51-52`, D12, `DESIGN-RESTORE.md` R4 and R6, `docs/DESIGN.md:52`, `src/a11y.rs` naming, no contributor doc on the crates |
| P7 | should | Undecided without record: kit `Tree` vs `folder_tree`, syn for the catalog test, keyring, toml_edit, `apple-asr-sys`, lever 10 desktop half |
| P9 | should | `packaging.yml` never ran on this branch; no release dry run on the new layout |
| P13 | minor | Leftovers: kit branches `course2md-design`, `course2md-integration` and stale others; `gpui-kit-material-design`, `gpui-kit-material-qa` worktrees; stray `fixture.mp4`, empty `.claude/`; 12 GB `target` |
| P14 | minor | Four unbuildable side-branch commits (`e771098`, `3080007`, `2cd910d`, `5e9f046`): document `git bisect --first-parent`; reverts `d89579d`, `29cdd24` lack attribution |

## Desktop behaviour: resolutions

Each line is recorded in the commit it names. "Before" means the new test was run against the old code and failed.

- **A12 move.** A resumed move skipped any copy of the same size whose time was no older than its original's, so an edited same-size file that a sync tool or a restore stamped earlier than the copy was published stale. Copies now carry their original's modification time, and a restart skips only an exact match; a coarser destination clock only costs a recopy. Test `a_restart_recopies_a_same_size_source_stamped_earlier_than_its_copy`, failed before. Commit "storage: a resumed move recopies a file unless size and time match its original".
- **A12 formulas.** Confirmed: a parsed note keeps a formula's asset path, and a cache reset dropped the SVG behind it, so `asset` and `tile` returned nothing and the formula drew blank until the note was parsed again. The path now spells the formula (`math/{d|i}{hex}.svg`, tiles `-{i}of{n}`), so a miss typesets it again. Test `a_formula_parsed_before_a_reset_still_draws_after_it`; the old test asserted the blank (`asset(&first.path).is_none()`). Commit "math: a formula drawn after a cache reset is typeset again from its path".
- **A12 newer schema.** Confirmed both halves. A current pointer naming a newer manifest fell back to an older version, because `read_manifest` had no typed error; and `publish` parsed a newer pointer as its own and replaced it. `c2m-artifact` now returns `NewerSchema` for a pointer (`read_current`) or manifest above schema 1; publishing stops, and the library lists an issue instead of an older version, also when the newer manifest is found during the fallback. Tests `publishing_leaves_a_newer_apps_pointer_alone` and `a_newer_manifest_under_a_current_pointer_is_not_replaced_by_an_older_version`, both failed before. Commit "artifact: a newer app's pointer or manifest is refused, never replaced or bypassed".
- **A3.** Two workers for one task. Starting a queued task reads the library on a blocking thread while the task is still `Queued`, `work.job` is empty and the old `start_pending` flag is off (it only covered credential lookup). Every poll calls `start_next_task`, and `finish_task` wakes the poll right after a task ends, so the next queued task could be picked twice; `start_task_request` then checked its stale copy, and the second worker failed on the first one's `.task.lock` (the reviewer's t8 `events.jsonl`: `fetch start`, then that error). The start is now claimed in `desktop-core` (`WorkState::claim_start`) from the pick until the job is installed or the start fails, and the request re-checks the live record. Test `a_task_being_started_holds_dispatch_until_its_start_ends` (new API, so it could not run before). Natively the race did not recur in my before-binary runs under load, so the cause rests on the evidence and reading. A second route to the same error, a worker that survived a killed app, is A2's. Commit "tasks: one start at a time, held from the pick until the job runs".
- **A2.** `d033b74`, `8839085`. A task whose folder lock is still held by a worker that outlived a killed app is held: shown as saving, never started beside it, checked every second. Once the lock is free its receipts are read, and a task whose requests all settled returns to its intent and continues from the saved answers; a retry no longer counts its own parent as a duplicate. The worker now waits up to two seconds for the lock before refusing, so the desktop's probe of the lock cannot make a new worker fail on its own folder. Tests `a_worker_outliving_a_killed_app_is_waited_for_and_its_answer_used`, `an_uncertain_task_whose_requests_settled_goes_back_to_its_intent`, `retrying_an_uncertain_task_unchanged_is_not_refused_as_a_duplicate`.
- **A4.** `d8e042e`. When stable mode finds no slide (footage that keeps moving never holds for `stable_secs`), the scan runs again in first mode and takes the first picture after each cooldown. Test `moving_footage_still_yields_screenshots`: a synthetic scrolling card now yields pictures at 0 s, 10 s and 19 s. The reviewer's real footage was not re-run.
- **A6.** `0b3171d`. The note's busy check skips an attempt a follow-up replaced (`handled_by` set), so a finished resend no longer leaves the note 正在处理. The library's busy-check unit test gained the replaced-attempt case.
- **A7.** `b3b2df7`. Authorizing a resend is refused only while the task waits or runs; otherwise the earlier authorization was never used and confirming again replaces it. The workspace's authorization test gained the failed-start case.
- **A8.** `a309512`. Cancelling Add Service removes the key that no saved version or other edit uses, off the UI thread, from the service editor and from first use. Tests `a_cancelled_draft_releases_only_its_own_key` and a Keychain variant (ignored by default, under a test-only service name).
- **A9.** `7471a10`. With redirects off, an unknown host, a refused or timed-out connection or an invalid address in a service test ends not sent, with its own message, instead of possibly charged. Test `a_refused_connection_is_not_sent_rather_than_possibly_charged`.
- **A10.** `30c24de`, `dfe80dc`. A language change sets the placeholders of the settings, home link, library and reader searches and the menu bar again; the saved language is read before the settings store opens, so load-time messages use it. Test `the_saved_language_is_read_before_the_store_opens`; in the capture build (with the new `language:<code>` step) the settings title and search placeholder switch at once, and with Japanese saved under an English system a damaged group is reported in Japanese.
- **A13.** `b87f9a3`, `05d2246`. `store.rs` owns `backup_path`, `read_backup` (with the size check) and `set_aside`; the workspace, settings and folder records go through them instead of their own `.json.bak` code. Existing tests.
- **A14.** No change needed. Verified natively by the desktop-logic agent: with the settings folder read-only, choosing a palette keeps the dialog open with the failure notice, 重试保存 and Close, and the selection does not change.

## Plan conformance: resolutions

P5, P6, P8, P10, P11 and P12 were in the plan reviewer's report but not copied into this file, and that report's
evidence directory is gone; no commit message or document names them. They cannot be reconstructed here, so round 2's
plan reviewer re-derives anything they held.

- **P1.** Re-estimated by reading every desktop file over 400 lines and every engine crate: about 3,500 lines looked removable with every feature kept. The reduction chunks delivered -835: engine and process helpers (`ee628b9`), tasks (`b440adc`), reader (`92566f3`), library and storage (`720b2b3`), settings and setup (`2dcb909`), async idiom (`edf140d`). The metric at `edf140d` reads 66,768 (-15.0%); `REDUCTION.md` § Working target restates the target as about 66,800 under the no-drop rule, and `STATUS.md` lists the owner questions that would allow more (`126cb4d`). The -40% is not reached.
- **P2.** In part. `desktop-core` is five crates, merged in `2f9c0ca`: `desktop-prefs`, `desktop-store`, `desktop-tasks`, `desktop-reader-core` and `desktop-accounts` (`9b77e05` to `17c386c`), described in `docs/CRATES.md` (`17f0d36`); they name the engine crates they use (`ccb7076`), and 66 items became `pub(crate)` (`4d9244d`). `Desktop` itself is not split: the binary is 29,451 lines (44% of the production total) and `impl Desktop` appears in 23 files of `desktop/src`.
- **P3.** Rows restored and `DESIGN-RESTORE.md` § Finished after the audit made true: G3 `4ae516e`, G4 `1e8a11f`, G5 `aa4b32e`, G8 `bd19858`, H4 `0e0fbbb`, L1 `35d0729`, `f74014d`, `17691d8`, L6 `4b49115`, T6 `e0006da`, R9 `63282d2`, S1 `5766d83`, S11 `f69c8d4`, `5d74921`, O3 `a5e3e65`; R12 kept by the design language. Documents `b1d30a3`, `f57dc3a`, `186a95c`, `087a4d5`, `dccef54`.
- **P4.** `506099d` (`MIGRATION.md`, `themes/SOURCES.md`, `BUGS-ENGINE.md`, D12, R6), `698103b` (`STATUS.md`, `PLAN.md` locations, crate list, phase, checks and metric), `c37641f` (`docs/CRATES.md`, the contributor page on the crates). `docs/DESIGN.md:52` was already right after C2, and R4 already under Finished. Stale lines the round-2 plan review found: `desktop/core` paths `0c769cc`, D11 `ecf916a`, R6, H7 and S13 `22c15a7`, the secondary metric `7d5f8ec`. Open: `desktop/src/a11y.rs` still holds validation diagnostics under that name.
- **P7.** `c915164`: `PLAN.md` § Decisions records each of `Tree` against `folder_tree`, `syn` for the catalog test, `keyring`, `toml_edit`, `apple-asr-sys` and lever 10's desktop half, with its reason and what would reopen it (merged in `baeab9a`).
- **P9.** In part. `dae7dee` makes `package.py` honour `CARGO_TARGET_DIR` and records a local macOS dry run on the merged workspace (ad hoc signing, no notarization, nothing uploaded): CLI, desktop, Apple speech module, ffmpeg and dav1d from source, yt-dlp, licenses, `.app`, DMG and zip built and verified; the packaged CLI's `doctor` found the bundled tools. `e7c2343` retries tool downloads and falls back to ffmpeg tarball mirrors. `packaging.yml` itself has still not run on `split/workspace` (its last runs are on `main` and `feat/bundle-tools`, 2026-10-28), and Windows and Linux were not dry-run.
- **P13.** As checked on 2026-11-03: the `gpui-kit-material-design` worktree is gone, and `gpui-kit-material-qa` is a repository of its own, not a kit worktree; `fixture.mp4` and the empty `.claude/` are not in the checkout. The kit branches `course2md-design` and `course2md-integration` stay until the owner confirms them (`STATUS.md` § Open problems); the local kit clone still has its `design/*` and `text/block-surface` branches. `target` is a local build cache and was not cleaned.
- **P14.** `c37641f`: `docs/CRATES.md` § Bisecting names the four commits that do not build alone and gives `git bisect --first-parent`; the reverts `d89579d` and `29cdd24` stay without attribution, since changing it would rewrite published history (merged in `baeab9a`).

# Acceptance round 2

In progress, with fresh reviewers (UI, app, CLI, plan) per `PLAN.md` § Acceptance. The coordinator adds the findings
and their resolutions here.

## UI findings: resolutions

Branch `split/ui2`. Native captures from the capture build, Paper (light) and Ink (dark), before (the reviewer's
binary, `126cb4d`) and after, on a copy of the reviewer's fixture, at 992×620 and 1280×620 pt, 720 wide for narrow
settings; Japanese and English for V1.

- **U21, V6.** `42e74702`. A task row is the kit's tile button holding the grip, the note row's inset cover (8
  corners), the text and the commands, so hover and press cover the whole row. Queued rows were held queued with a
  stand-in worker (`COURSE2MD_BIN` → a script that sleeps), then hovered.
- **R9.** `fcfaff27`. The contents beside the note were filled surface-container-low on the surface-container page;
  each entry now rests on its ground, only the current one filled.
- **U25, Q9.** `bb5c8c49`. The two `border_t_1` hairlines are `MaterialDivider` (pixel-identical in first use). The
  mark no longer clips the icon, which carries its own rounded square (as on 关于); differs from before by sub-pixel
  corner antialiasing only. The wordmark stays `rem(20.)`: no type role is 20 (title medium 16, title large 22).
- **V1.** `69261b4e`, `e6dc55d4`. The Bilibili login hint was appended to any failure that was not yt-dlp's own
  answer, so a missing yt-dlp got it; only a refusal yt-dlp reports, and for Bilibili only one about an account, gets
  a hint now. `run-task` output stays Simplified Chinese; the desktop translates each known part of a failure through
  the catalog (`c2m_i18n::i18n::engine_message`) and drops the misplaced hint from records older engines wrote. Tests
  `bilibili_suggests_an_account_only_when_one_could_help`, `a_login_tip_keeps_the_markers_retries_look_for` (a spawn
  failure gets no hint), `an_engine_message_reads_in_the_interface_language`,
  `a_worker_failure_reads_in_the_interface_language`.
- **V2, V7.** `f424d2f6`. The narrow pane keeps the 设置 title and the search; a search matching nothing shows
  没有匹配的设置 with 清除搜索 in the content pane instead of the ruled-out category.
- **V3.** `b12e6288`. 开源许可 is one outlined link per license, not a value dropdown. The kit's `ExposedDropdown`
  still cuts a long value without an ellipsis (its static value has no `text_ellipsis`); not changed.
- **V4.** `82e88c16`. Reproduced on an empty profile: the field's own notification after Enter read as a new edit
  (the draft did not hold the typed link yet) and cleared the message. The draft now records the input first.
- **V5.** `05c2ebdc`. Home's attention chip takes the Tasks page's status kind.
- **V8.** `9b06ce29`. The gallery's card stays; the find bar's 无匹配 goes for screenshots.
- **V9.** `7a1470ee`. Without the contents beside it, both views keep the note's measure, so the switch stays put;
  fewer screenshots than columns share the row, each at most twice the narrowest column.
- **V10.** `466874db`. The text toggle shows `subtitles`.
- **V11.** `bfd0d1ce`. The kit's list item cannot tell whether its name was cut, so the sidebar rows have no tooltip.
- **V12.** `c0230a05`. Device facts and tool states are the list item's supporting (body) text.
- **V13.** `5a71c323`. 取消任务 is a neutral text button on Tasks, Home's card and Home's processing list.
- **V14.** Kit side: the connected group's ring is inward and secondary, invisible on a primary-filled selected
  option (captured after click and arrow keys). Draft kit PR #29 draws it at the M3 outer offset in a ring layer
  painted after the options. Not yet verified in the app; the app pins the kit and needs a repin once merged.
- **V15.** `08e2636d`. Rows 146, 246, 1014, 1465, also 1062 and 1548; row 699's translations held a second garbled
  copy.
- **Q10.** `5f49d50a`. The capture's `~end` asks the restore, which runs on the frame after the view's content
  reaches its list, instead of waiting 800 ms.
- **R6.** `97744d7f`. The reader bar is 56; breadcrumb and title still centre on the back button.

## CLI: resolutions (agent: cli2)

| ID | Commit | Resolution and evidence |
|---|---|---|
| D1 | `b3729cc` | Every log subscriber also passes a fixed filter holding ureq (and ureq-proto), rustls, tokio-rustls, native-tls, openssl, hyper and h2 at `info`, whatever `RUST_LOG` says; the desktop installs no subscriber and logs through `run-task`. `llm setup` at `RUST_LOG=trace` and at `ureq_proto=trace,…` (plain) and a `run-task` summary at trace (NDJSON) against loopback servers: no six characters of the key in the output; both tests fail without the filter. Real run `x.mp4 --llm --llm-api-key … -vv` (plain and `--json`, no network): no key fragment, no `ureq` line, engine logs kept |
| D2 | `29a53c7` | Online track choice passes the person's interface language, recorded before `--json`/`run-task` pin messages to Simplified Chinese, as the desktop does. The language a task's first choice used is kept in its work folder (`.subs/language`), so a continued task reads the same track and its paid ids stay; a work folder from an earlier version keeps preferring Chinese. Fake yt-dlp with zh-Hans and en tracks of a French video: en → en, zh-Hans → zh-Hans, plain and `--json`; continued in zh-Hans → en; legacy folder → zh-Hans. Local sidecars still follow the message language as in C11 |
| D3 | `1e8bb60` | Password and SMS login check for a terminal first and say, in four languages, to run the command in a terminal or use QR login |
| D4 | `581614f` | Not fixed text by design: it carried 2.0.1's doc comments over. `--method`, `--transcript-source` and `--asr-api-mode` value descriptions, the "Possible values:" heading and the `[possible values: …]` hint now go through the catalog |
| D6 | `0734b09` | Preparation checks the installation's Metal library before any download, so a missing component fails at once without the mirror or Hugging Face; a helper prints its bare error, so the parent's message has one prefix. Without the library, in four languages and `--json`: one localized error, no retry |
