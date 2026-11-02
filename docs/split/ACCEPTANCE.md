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
