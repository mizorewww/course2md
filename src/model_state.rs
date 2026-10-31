//! A local speech model's state on disk, as progress events report it.

use crate::config::AsrProvider;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    Missing,
    Partial,
    Cached,
    Loaded,
    Unsupported,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachePart {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub missing: Vec<String>,
    /// 一次遍历得到的 (相对路径, 字节数, 修改时间纳秒)，供指纹复用。
    #[serde(skip)]
    pub files: Vec<(String, u64, u128)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalModelStatus {
    pub provider: AsrProvider,
    pub model: String,
    pub state: CacheState,
    pub parts: Vec<CachePart>,
    pub bytes: u64,
    pub can_prepare: bool,
    pub last_error: Option<String>,
    pub loaded_at: Option<u64>,
    pub fingerprint: String,
}
