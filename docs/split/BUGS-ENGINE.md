# Engine and tooling bugs (P2)

Status of `AUDIT.md` § Bugs, E1–E22, T1–T6 and T8, on branch `split/bugs`.

| Bug | Status | Commit | How verified |
| --- | --- | --- | --- |
| E1 | fixed | 138ef2b | Test failed first: markers reach `downcast_ref` through the tip; a stop gets no login tip |
| E2 | fixed | b1c65de | Loopback tests: stalled body and silent stream abandoned; slow first byte and a long live stream survive |
| E3 | fixed | f5eb0fc | Test failed first: free 5xx is retryable and resends; billed 5xx still blocks |
| E4 | fixed | 138ef2b | Test failed first: non-UTF-8 output plus a full pipe timed out before the fix |
| E5 | fixed | 907abfa | Integration test failed first: a real `run-task` with a UTF-16 subtitle |
| E6 | fixed | 84c0e0c | Test failed first: torn character recovers; invalid bytes in an earlier line still error |
| E7 | fixed | 62c7546 | Tests failed first: literal brackets, numeric entities, YouTube-shaped rolling cues |
| E8 | fixed | bdca943 | Test of `runtime::lock_file_within` across two file descriptions; no test with two real processes |
| E9 | fixed | bdca943 | Unit tests on the callback parser and the shown text; the browser flow itself was not run |
| E10 | fixed | bdca943 | Unit test on the raw and decoded URL |
| E11 | fixed | 381680f | Unit tests: `LlmSettings` and `ConfigFile` Debug hide the key |
| E12 | fixed | f4b5637 | Unit test at and over the limit |
| E13 | fixed | 138ef2b | Test failed first: a cancelled probe returns at once |
| E14 | fixed | f26c860 | Unit test on `lang` and the caption; catalog tests. The portable line is covered by the catalog tests only |
| E15 | fixed | 429d86a | CLI test of `config init --force` and its backup |
| E16 | fixed | ac27dce | `uv pip compile` on Linux, Windows, macOS at Python 3.10 and 3.13; real import on macOS; no NPU run |
| E17 | fixed | 58ba1b0 | Unit test of the local signals; the opt-in lookup itself was not run |
| E18 | fixed | 7aab974 | Integration test failed first: same bodies in four languages; no resend after a language change |
| E19 | fixed | be97c7f | Unit test |
| E20 | fixed | be97c7f | Unit test |
| E21 | fixed | be97c7f | Unit test |
| E22 | fixed | be97c7f | Unit test; `tests/i18n_catalog.rs` now reads keys through the engine's parser |
| T1 | fixed | 216da8e | `packaging/test_install.py` runs the real script with a fake curl; 2 of 4 tests failed first |
| T2 | fixed | 87e1abb | Unit tests of `release-version.py`; actionlint 1.7.12 reported nothing. Workflows were not run |
| T3 | fixed | 0933ec7 | Forced resolve from a clean copy passes; a bad revision fails; all 39 pins unchanged |
| T4 | fixed | 721839c | Here, without Xcode: `cargo build --release` exits 101 with the message; `cargo build` warns and finishes |
| T5 | fixed | 23cb817 | Boundary logic run as a standalone Swift program at every cut; shim compiled and linked; not run on a long transcript |
| T6 | fixed | 23cb817 | Compiled and linked only; not run, because there is no metallib or model here |
| T8 | fixed in part | 07da58d | `_headers` comment and `build.rs` comment corrected, by reading |
| T8 "CUDA" | not a bug | none | The bytes spell C-U-D-A, NVIDIA's name; `asr.rs` also matches it as a device prefix |
| T8 `sources.py:26` | not done | none | Left alone as instructed; the file is being deleted |

## Decisions

- **E2.** A plain response has `timeout_recv_body` of 90 s; a stream has `timeout_per_read` of 90 s. `timeout_per_read` alone would also cut off a model that thinks past 90 s before its first byte, and `timeout_recv_body` alone would cut off a healthy long stream. The 300 s total stays.
- **ureq 3 multipart.** Not used. The API is outside ureq's version promise, and its form is consumed by sending, while a retry resends the same bytes.
- **Never-sent errors.** Only DNS failure, resolve and connect timeouts, and refused or unavailable connections count. Unreachable errors do not, because an established connection can also fail with them, so such a connect failure now asks before resending.
- **Environment proxies.** Still ignored, as under ureq 2. ureq 3 reads them by default, which would catch loopback services.
- **E17.** No IP lookup by default. A mainland China locale or time zone keeps domestic mirrors first, and other hosts remain fallbacks. `HF_ENDPOINT` overrides everything; `COURSE2MD_IP_LOOKUP=1` or `hub::allow_ip_lookup` opts in. A mainland user with a foreign locale and zone, or on Windows with a non-CN locale, tries Hugging Face first and falls back after it fails.
- **E18.** Everything sent to a service is one fixed text, the Simplified Chinese the desktop already sent, so desktop receipts stay valid. Excluding prompts from the identity would let an edited prompt replay old results. A CLI task left unfinished in another locale before this change sends its remaining requests once more.
- **E20.** `C` and `POSIX` select English. An unsupported language such as `fr_FR` still falls through to the next variable.

## Desktop changes needed

- `desktop/src/service_test.rs:255-261`: replace the `ureq::Error` downcasts on the `llm::test_connection` chain with `course2md::llm::RequestError` (`Status(u16)`, `Transport(String)`). The desktop's ureq 2 type no longer matches the engine's errors, so this compiles but silently loses the status.
- `desktop/Cargo.lock` needs regenerating for the engine's new dependencies.
- Not needed: the desktop's other ureq uses are its own requests, and `runtime::http_agent` and `dispatch::receive`, now `pub(crate)`, have no desktop callers.

## Checks

On commit 721839c: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass with `COURSE2MD_NO_APPLE=1`, as do the packaging tests in `packaging/`. Test counts: 235 unit (3 ignored), 15 `cli_ux`, 2 `i18n_catalog`, 3 `login_cli`, 1 `prompt_identity`, 2 `scene_synthetic`, 12 `task_execution`.

Not established:

- **Default Swift build.** It cannot finish on this machine: only Command Line Tools are installed, with no `metal`. The Swift proof used `--build-system native` through a temporary PATH wrapper outside the repository. The link also needed a one-off `-l swiftCompatibility56`, because `build.rs` looks for that library only in Xcode's layout. So no metallib was produced and the Apple code was not run. Clippy with the Apple cfg passes the same way; tests were not linked with it.
- **Desktop.** `cargo check --manifest-path desktop/Cargo.toml` was not run: another agent is porting it, and disk is tight.
- **`cargo update` advisories.** The update was applied, but the advisories in `AUDIT.md` were not checked individually; `cargo-deny` comes in P4.
