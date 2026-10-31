//! Speech recognition backends, their models and the transcript checkpoint.

pub mod apple;
pub mod asr;
pub mod checkpoint;
pub mod download;
pub mod hub;
pub mod models;
pub mod npu;

use c2m_config::{config, settings};
use c2m_core::{digest, fsio, timeline};
use c2m_dispatch::{dispatch, job, pool};
use c2m_i18n::{eprintln_tr, println_tr, tr};
use c2m_process::{error, runtime, tools};
use c2m_protocol::{model_state, progress};
#[cfg(test)]
use c2m_test_http as test_http;
