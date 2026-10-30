# Reduction plan with every feature kept

From the audit of 2026-10-31 at `699adba` (78,281 production Rust lines then; baseline
78,518). Estimates, not prototyped; about ±15% on lines removed. **Top with all features
kept: about 44,500 lines, true deletion 43%; 42,100 in this repo (46%) after moving 2,400 to
owned repos.** 60% is not reachable by deletion under the no-drop rule. Lever 9 is a
placeholder until the kit audit reports.

Do levers 1 and 3 before P3: they change crate boundaries.

| # | Lever | Net lines | Library |
| --- | --- | ---: | --- |
| 1 | Engine cleanup: one `Transcribe` trait, one VAD and chunk loop (`asr.rs:171-260,405-490`, `npu.rs:365-430`, `apple.rs:444-490`), pipeline entries as tasks (`pipeline.rs:32-145,275-534`), LLM paths and salvage, merged tool lookup, process runners, HTTP agents | -3,314 | pulldown-cmark, whatlang, oauth2-rs (-60 only) |
| 2 | Spike, then one ASR engine with Apple/GPU/CPU/NPU as provider values | -2,160 | sherpa-onnx |
| 3 | Persistence: one store, sync commits, no writer threads, restore from snapshot, relocate and reassociate merged, `dispatch` keeps sending/uncertain/resend only | -4,724 (-4,354 on JSON) | walkdir, uuid, chrono, tempfile |
| 4 | Thin task driver, derived state: engine tees to `events.jsonl`, 7 of 17 `TaskRecord` fields derived | in 7 | |
| 5 | Primitives: field table, async-status surface, menu and confirm helpers, `task_actions` table, one `TaskCard`, one note row | in 6-8 | |
| 6 | Settings, setup, library (22,728 → 12,306): form -2,312, onboarding on shared parts -1,944, async surface -1,886, shell -1,662, note row -1,102, service test -645, menus -505, service list -431, Tree -335 | -10,422 | gpui-kit Tree |
| 7 | Import and task pages (12,024 → 7,155): recovery surface, options form, subtitles, card, follow state | -4,799 | |
| 8 | Reader (8,045 → 3,950): viewer, render, position, menus, gallery, export, shell | -4,095 | walkdir |
| 9 | Theme, widgets, `note_text` (6,235 → ?) | -4,235 placeholder | gpui-kit |
| 10 | Move to owned repos: `subtitle` 560, `scene` 300, `download`+`hub` 635, viewer and image cache 510, form primitives 400 | -2,405 moved | |

## Decisions applied to the audit

The audit assumed some costs that amount to dropped features or lost guarantees. Under the
owner's no-drop rule:

- **No SQLite.** rusqlite's own contribution is about 370 lines; the rest comes from the
  simplifications. Not worth a C dependency and losing tasks and folders travelling with a
  library. Keep JSON/JSONL, take the simplifications. Lever 3 = -4,354.
- **Small windows stay supported** (lever 8 assumed otherwise). Budget about +300.
- **Keep:** paid-request safety in `dispatch` (a sent but unconfirmed paid request must not
  resend silently); no silent loss on library move (size-and-count check is acceptable, an
  interrupted move restarting is acceptable).
- **sherpa-onnx is a spike and must not regress.** Pass bar: Qwen3 speed on Apple and
  Windows no worse than today, Whisper on NPU, and existing users' model caches handled.
  Without it the target becomes 46,700 total, 44,300 in repo.

Working target after these: **about 45,000 lines (-43%)**, adjusted when lever 9 is known.

## Libraries rejected after checking coverage

async-openai (ledger hashes the raw body, no Codex), rig, reqwest (async rewrite for 0
lines), moka (no Pending/Failed states), rust-ffmpeg, config-rs (small macro gets it), redb,
sqlx, backon.

## Repetition behind the savings

Switch rows `generation_settings.rs:377-403`; action pairs `task_ui.rs:1696,2717`; error
boilerplate `task_runner.rs:1397` (`workspace_error =` 26 times); stale-result guards
`import_ui.rs:708,866`; note and gallery branching `reader_ui.rs:3504-3519`; writer threads
`workspace.rs:1285`, `preferences.rs:946`; parallel flows `settings_ui.rs:2261`,
`onboarding.rs:666,701`, `service_editor.rs:1408`; double event folding
`task_runner.rs:1704-1782`, `main.rs:1041-1139`.
