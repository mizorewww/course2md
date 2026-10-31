//! What a note records about its video and the summary it carries.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 我们关心的元数据字段子集。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VideoMeta {
    pub title: String,
    #[serde(default)]
    pub uploader: String,
    #[serde(default)]
    pub duration: f64,
    pub webpage_url: String,
    #[serde(default)]
    pub extractor: String,
    #[serde(default)]
    pub id: String,
}

impl VideoMeta {
    /// Local-file metadata uses the `local` extractor marker.
    pub fn is_local(&self) -> bool {
        self.extractor == "local"
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::fsio::atomic_write_shared(path, serde_json::to_string_pretty(self)?.as_bytes())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutlineItem {
    /// 章节起始秒数（绝对时间）
    pub t: f64,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub tldr: String,
    pub key_points: Vec<String>,
    pub outline: Vec<OutlineItem>,
}
