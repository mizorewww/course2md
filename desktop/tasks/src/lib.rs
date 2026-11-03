//! What the desktop does with a task: where its video comes from, the import page's
//! session, the plan a task is built from, running the engine for it, and how a task
//! reads on the task pages. Nothing here depends on GPUI.

pub mod backend;
pub mod import;
pub mod job;
pub mod plan;
pub mod run;
pub mod source;
pub mod task_view;
