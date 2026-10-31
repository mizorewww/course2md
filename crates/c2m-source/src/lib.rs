//! Video sources: metadata, downloads, subtitles and Bilibili login.

pub mod auth;
pub mod fetch;
pub mod subtitle;

use c2m_config::config;
use c2m_core::{digest, fsio, meta, timeline};
use c2m_dispatch::{dispatch, pool};
use c2m_i18n::{println_tr, tr};
use c2m_process::{error, runtime, tools, ytdlp};
use c2m_protocol::progress;
