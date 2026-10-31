//! Published notes: rendering, manifests and portable exports.

pub mod artifact;
pub mod math;
pub mod portable;
pub mod render;
pub mod summary_doc;

use c2m_config::config;
use c2m_core::{digest, fsio, meta, outcome, timeline};
use c2m_dispatch::{dispatch, pool};
use c2m_i18n::tr;
use c2m_i18n::{i18n, language};
use c2m_process::runtime;
