//! The configuration file and the settings a task runs with.

pub mod config;
pub mod llm_settings;
pub mod settings;

use c2m_core::{fsio, invalid, paths};
use c2m_i18n::language;
use c2m_i18n::tr;
