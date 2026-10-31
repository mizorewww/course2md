//! AI services: proofreading, translation, summaries and their logins.

pub mod llm;
pub mod login;
pub mod provider;
pub mod summarize;

use c2m_config::{config, llm_settings, settings};
use c2m_core::{fsio, meta, timeline};
use c2m_dispatch::{dispatch, pool};
use c2m_i18n::language;
use c2m_i18n::{println_tr, tr};
use c2m_process::runtime;
use c2m_protocol::progress;
#[cfg(test)]
use c2m_test_http as test_http;
