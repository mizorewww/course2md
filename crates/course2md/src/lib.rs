pub use c2m_config::{llm_settings, settings};

/// The settings types, and the provider this machine defaults to.
pub mod config {
    pub use crate::asr::{apple_native_available, default_provider_hint};
    pub use c2m_config::config::*;
}

pub use c2m_core::{digest, fsio, invalid, meta, outcome, paths, timeline};
pub use c2m_i18n::{eprint_tr, eprintln_tr, print_tr, println_tr, tr};
pub use c2m_i18n::{i18n, language};
pub use c2m_process::{error, media, runtime, tools, ytdlp};
pub mod apple;
pub mod artifact;
pub mod asr;
pub mod auth;
pub mod checkpoint;
pub mod cli;
pub mod dispatch;
pub mod doctor;
pub mod download;
pub mod execution;
pub mod fetch;
pub mod hub;
pub mod job;
pub mod llm;
pub mod login;
pub mod math;
pub mod model_state;
pub mod models;
pub mod npu;
pub mod options;
pub mod pipeline;
pub mod pool;
pub mod portable;
pub mod progress;
pub mod provider;
pub mod render;
pub mod scene;
pub mod subtitle;
pub mod summarize;
pub mod summary_doc;
#[cfg(test)]
use c2m_test_http as test_http;
pub mod wizard;
