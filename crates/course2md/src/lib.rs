//! The `course2md` command line, and the one path the desktop reaches the engine through.

pub use c2m_ai::{llm, login, provider, summarize};
pub use c2m_artifact::{artifact, math, portable, render, summary_doc};
#[cfg(apple_native)]
pub use c2m_asr::apple;
pub use c2m_asr::{asr, checkpoint, download, hub, models, npu};
pub use c2m_config::{llm_settings, settings};
pub use c2m_core::{digest, fsio, invalid, meta, outcome, paths, timeline};
pub use c2m_dispatch::{dispatch, job, pool};
pub use c2m_engine::{execution, options, pipeline, scene};
pub use c2m_i18n::{eprint_tr, eprintln_tr, i18n, language, print_tr, println_tr, tr};
pub use c2m_process::{error, media, runtime, tools, ytdlp};
pub use c2m_protocol::{model_state, progress};
pub use c2m_source::{auth, fetch, subtitle};

/// The settings types, and the provider this machine defaults to.
pub mod config {
    pub use crate::asr::{apple_native_available, default_provider_hint};
    pub use c2m_config::config::*;
}

pub mod cli;
pub mod doctor;
pub mod wizard;
