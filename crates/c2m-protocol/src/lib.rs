//! Progress events on the wire and on the terminal.

pub mod model_state;
pub mod progress;

use c2m_config::config;
use c2m_core::{invalid, outcome};
use c2m_i18n::tr;
use c2m_process::{error, runtime};
