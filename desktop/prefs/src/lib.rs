//! What the desktop app is set to and what it is set up with: preferences, the services
//! they name and their checks, saved credentials, this machine's components, and the one
//! way the desktop keeps a JSON record. Nothing here depends on GPUI.

pub mod bounded_http;
pub mod conversion;
pub mod credentials;
pub mod environment;
pub mod model_discovery;
pub mod model_status;
pub mod preferences;
pub mod provider_serde;
pub mod service_binding;
pub mod service_test;
pub mod setup;
pub mod store;

/// Shown when a blocking worker ends without a result.
pub fn interrupted() -> &'static str {
    c2m_i18n::tr!("操作意外中断，请重试")
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
