//! Video sources: metadata, downloads, subtitles and Bilibili login.

pub mod auth;
pub mod fetch;
pub mod playback;
pub mod subtitle;

use c2m_config::config;
use c2m_core::{digest, fsio, meta, timeline};
use c2m_dispatch::{dispatch, pool};
use c2m_i18n::{println_tr, tr};
use c2m_process::{error, runtime, tools, ytdlp};
use c2m_protocol::progress;

/// Bilibili's CDN edge resets some rustls handshakes before they reach the
/// server (a ClientHello filter; browsers and curl pass because they present
/// the platform TLS stack). Every bilibili.com call in this crate — login,
/// account checks and page metadata — therefore uses the OS TLS provider, the
/// same ClientHello the platform's own apps make.
pub(crate) fn bilibili_agent_builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    runtime::agent_builder().tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    )
}
