//! External programs: processes, tool lookup, yt-dlp and ffmpeg calls, locks and HTTP agents.

pub mod error;
pub mod media;
pub mod runtime;
pub mod tools;
pub mod ytdlp;

use c2m_core::{invalid, paths};
use c2m_i18n::tr;
