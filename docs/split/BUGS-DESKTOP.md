# Desktop bugs (P2)

Status of `AUDIT.md` § Bugs → Desktop, the find field keys, and the gaps left by `BUGS-ENGINE.md`. Verified on `ef6d8ca`, except the final document commit.

"Unit test" means a test added with the fix. These tests were not run against the old code first, so they were not seen to fail.

| Item | Status | Commit | How verified |
| --- | --- | --- | --- |
| D1 | fixed | 06c4293 | Unit tests: nested images, case-differing labels, a lookalike in code, a definition shared with a link, line endings |
| D2 | fixed | 7b0590a | Unit tests on macOS with a fake system store: both flip directions, failed and garbled moves, two instances, 12 concurrent writers, a corrupt file. `credentials.rs` compiled for Linux and Windows against a stub engine. Never run on those systems, and never against a real keyring |
| D3 | fixed | f9006a1, ef6d8ca | Existing crash-recovery tests pass. New unit tests: a 40-file move makes 80 hashes and 2 journal writes; a crash with an unrecorded temporary file resumes; another move's temporary file is refused; a source edited or added, or a copy resized, after its copy stops the move |
| D4 | fixed | 57a2ce5 | By reading only. The line is `cfg(windows)`, and the Windows build cannot run here (the `gpui-pre` build script fails for that target) |
| D5 | fixed | 68d0fd0 | Unit test |
| D6 | fixed | 7c58e55 | Unit test: trailing commas, fences and prose pass; bad input does not. Needed one engine change: `llm::extract_json_object` is now `pub` |
| D7 | fixed | 8bcec64 | Unit tests: invalid UTF-8 keeps the line and those after it; a failing stream reports once and ends |
| D8 | fixed | be990a5 | Unit test on local tracks. It also fixes `import_ui.rs` and `library_ui.rs`, five call sites in all (the commit message says six) |
| D9 | fixed | bdb7daf | Unit test on the term for each part, in Simplified Chinese. The app was not run in another language; the choice no longer depends on the text |
| D10 | fixed | 8349fcc | Unit tests: read, settle and abandon. Also removes markers left by earlier versions |
| D11 mtime | fixed | 61e3059 | Unit test: a cache hit refreshes the time |
| D11 hash | fixed | 61e3059 | Unit test against a fixed SHA-256 value. Old cache files are not reused and age out |
| D11 `math.rs` | fixed | 9f94b60 | Unit test: slices stay within the limit |
| D11 writes during read | fixed | 8349fcc | Unit test: `read_records` leaves the journal unchanged |
| D11 newer schema | fixed in part | 0e0e8b6 | Unit test. Pointer schema only: `read_manifest` has no typed error, so a newer manifest under a current pointer still falls back |
| D11 lock `.expect` | fixed | 25bc804 | Existing tests; poisoning itself is not tested |
| D11 unzeroized copies | fixed | 7b0590a | By reading. `serde_json` parsing can still leave transient copies, and escapes beyond the reserved size can reallocate |
| D11 doc comments | fixed | 70a8cb3 | By reading |
| D12 | not done | none | Subsumed by P5, as instructed |
| D13 | fixed | a99b2f6 | Existing test passes on macOS. PATHEXT was not exercised, because that needs Windows. `which` also requires the executable bit on Unix, which the old walk did not |
| Find field keys | fixed | df84fc7 | Capture feature with simulated keystrokes through `window.dispatch_keystroke`, not hardware keys, on a fixture note. Focus in the field: `cmd-f`, `1` gives 1/6, `cmd-g` 2/6, Escape closes the field and clears the highlights (PNGs viewed). `cmd-shift-g` gives a file the same size as the 1/6 shot; that PNG was not opened. No automated test |
| Metallib in release | fixed | d2ae9ce | Xcode 27.2 beta, stub `swift` that succeeds, clean worktree: `cargo check --release` exits 101 with the message; `COURSE2MD_NO_APPLE=1` passes; a dev check warns and finishes. Real Swift build: `target/release/mlx.metallib` present |
| Apple build and tests | done | none | `cargo test` without `COURSE2MD_NO_APPLE`: linked and passed, see Checks |
| T6 VAD | run | 14f4211, 9fb147b | New ignored test through the shim, with the 1.4 MB Silero model and `say` speech: load 546 ms, then 128, 110 and 80 ms per detection, consistent with no reload. Speech gave one segment, a repeat gave the same result, silence gave none |
| T5 truncation | not run in the app | none | `c2m_asr_transcribe` needs an ASR model, and the smallest is about 2.3 GB (Qwen3 0.6B CoreML 1.6 GB plus weights 0.7 GB), so none was downloaded. `utf8PrefixLength`, extracted from the current shim, passed 23 cuts of a mixed-width string |

## Decisions

- **D2.** The choice is not stored, so it cannot go stale. New keys go to the keyring when it is reachable, and a reference is looked up in the file, then the keyring. File keys move up on a background thread and leave the file only after reading back equal. Nothing moves down, because an unreachable keyring cannot be read; those keys wait there and the error says so. A keyring that reports available but refuses writes still fails the save, with no silent plaintext fallback. The file vault no longer caches: it reads on each use and changes under `desktop-credentials.lock`.
- **D3.** Temporary copies are named `.course2md-copy-<move id>-*`, so a resume recognizes them with no journal write, and a crash before recording no longer blocks it. Older journals' `temporary_files` are still honoured. Closing checks compare size and modification time, within what `REDUCTION.md` accepts for moves. A write that keeps both size and modification time would pass them; `cleanup_backup` still hashes the source before deleting it.
- **D11 newer schema.** The note leaves the list and appears as a library issue rather than showing an older version, so this app cannot republish over a note it cannot read.

## Checks

On `ef6d8ca`, with `DEVELOPER_DIR` set to Xcode 27.2 beta and the Apple module built:

- Root: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. All pass: 235 unit (5 ignored), 15 `cli_ux`, 2 `i18n_catalog`, 3 `login_cli`, 1 `prompt_identity`, 2 `scene_synthetic`, 12 `task_execution`.
- Desktop: `cargo fmt --check`, `cargo clippy --all-targets --no-deps -- -D warnings`, `cargo test`. All pass: 335 passed, 3 ignored.

Not established: the whole desktop on Linux or Windows (`wayland-backend` and `gpui-pre` build scripts fail when cross-checking from here), and a real conversion through the app.

## For the kit

Nothing from these fixes belongs in the kit.
