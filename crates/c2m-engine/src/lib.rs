//! The conversion pipeline and the task protocol.

pub mod execution;
pub mod options;
pub mod pipeline;
pub mod scene;

use c2m_ai::{llm, summarize};
use c2m_artifact::{artifact, portable};
use c2m_asr::{asr, checkpoint, models, npu};
use c2m_config::{config, settings};
use c2m_core::{digest, fsio, invalid, timeline};
use c2m_dispatch::{dispatch, job, pool};
use c2m_i18n::language;
use c2m_i18n::{eprintln_tr, println_tr, tr};
use c2m_process::{error, media, runtime, tools};
use c2m_protocol::progress;
use c2m_source::{fetch, subtitle};
