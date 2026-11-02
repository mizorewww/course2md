//! What the desktop app knows and decides, without a window: state, persistence,
//! preferences, the task record and the task logic. Nothing here depends on GPUI, so all
//! of it is tested without one. Pages render this crate's state and call its decisions.

pub mod account;
pub mod activity;
pub mod backend;
pub mod codex;
pub mod import;
pub mod job;
pub mod library;
pub mod locations;
pub mod notes;
pub mod ollama;
pub mod organize;
pub mod page;
pub mod plan;
pub mod reader;
pub mod reader_navigation;
pub mod source;
pub mod storage;
pub mod task_view;
pub mod workspace;
pub mod youtube;

pub use desktop_prefs::{
    bounded_http, conversion, credentials, environment, interrupted, model_discovery, model_status,
    preferences, provider_serde, service_binding, service_test, setup, spawn_blocking_io, store,
};
