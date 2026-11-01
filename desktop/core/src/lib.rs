//! What the desktop app knows and decides, without a window: state, persistence,
//! preferences, the task record and the task logic. Nothing here depends on GPUI, so all
//! of it is tested without one. Pages render this crate's state and call its decisions.

pub mod activity;
pub mod backend;
pub mod bounded_http;
pub mod conversion;
pub mod credentials;
pub mod import;
pub mod locations;
pub mod model_discovery;
pub mod notes;
pub mod organize;
pub mod page;
pub mod plan;
pub mod preferences;
pub mod provider_serde;
pub mod reader_navigation;
pub mod service_test;
pub mod source;
pub mod storage;
pub mod store;
pub mod task_view;
pub mod workspace;

/// Shown when a blocking worker ends without a result.
pub fn interrupted() -> &'static str {
    course2md::tr!("操作意外中断，请重试")
}

/// Runs blocking IO on smol's blocking thread pool and delivers the result through a
/// channel. The work runs to completion even if the receiver is dropped.
///
/// This is the one way the desktop leaves the UI thread for blocking work: file and
/// process IO, keychain access, synchronous HTTP. GPUI's background executor dispatches
/// to GCD global queues, and macOS may run those blocks on the main thread via queue
/// override; `cx.spawn` always polls on the main thread. Blocking syscalls — keychain
/// reads that can wait on an authorization dialog, synchronous HTTP — therefore must
/// never run inside executor tasks. smol's pool is separate from GPUI's executor and
/// reuses its threads, so short operations do not each start a thread. Code that wants
/// a future instead of a channel may await `smol::unblock` directly; it is the same pool.
pub fn spawn_blocking_io<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> smol::channel::Receiver<T> {
    let (tx, rx) = smol::channel::bounded(1);
    smol::unblock(move || {
        let _ = tx.send_blocking(work());
    })
    .detach();
    rx
}
