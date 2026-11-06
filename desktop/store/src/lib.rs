//! What the desktop keeps: the workspace record of drafts and tasks, the libraries and
//! their folders, moving and backing them up, published notes and each task's activity.
//! Nothing here depends on GPUI.

pub mod activity;
pub mod library;
pub mod locations;
pub mod migrate;
pub mod notes;
pub mod order;
pub mod organize;
pub mod page;
pub mod records;
pub mod storage;
pub mod workspace;
