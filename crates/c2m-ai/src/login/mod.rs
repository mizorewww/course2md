//! `--login/--logout <平台>` 的 CLI 实现。凭据统一存 `{config_dir}/auth/`（0600 原子写）。

pub mod codex;
pub mod ollama;
