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

/// Runs blocking IO on smol's blocking thread pool and resolves to its result, or to
/// `None` when the worker ended without one. The work starts at once and runs to
/// completion even if the returned future is dropped.
///
/// This is the one way the desktop leaves the UI thread for blocking work: file and
/// process IO, keychain access, synchronous HTTP. GPUI's background executor dispatches
/// to GCD global queues, and macOS may run those blocks on the main thread via queue
/// override; `cx.spawn` always polls on the main thread. Blocking syscalls — keychain
/// reads that can wait on an authorization dialog, synchronous HTTP — therefore must
/// never run inside executor tasks. smol's pool is separate from GPUI's executor and
/// reuses its threads, so short operations do not each start a thread.
pub fn spawn_blocking_io<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = Option<T>> {
    let (tx, rx) = smol::channel::bounded(1);
    smol::unblock(move || {
        let _ = tx.send_blocking(work());
    })
    .detach();
    async move { rx.recv().await.ok() }
}

/// [`spawn_blocking_io`] for work that can fail; a worker that ended without a result
/// was interrupted.
pub fn spawn_blocking_result<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> impl Future<Output = anyhow::Result<T>> {
    let worker = spawn_blocking_io(work);
    async move {
        worker
            .await
            .unwrap_or_else(|| Err(anyhow::anyhow!(interrupted())))
    }
}
