//! The Codex login's state; credentials stay in the CLI's token file.
use c2m_ai::login::codex::LoginStatus;

/// Recognize the engine's missing/expired login marker and replace it with in-app connection guidance.
pub fn login_required(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<c2m_ai::login::codex::CodexLoginRequired>()
            .is_some()
    })
}

/// The surface a Codex action belongs to: asynchronous results return only to the surface that started it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodexSurface {
    Editor,
    Onboarding,
    /// The account card on the services and accounts page: connection state only, no model input.
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CodexWork {
    Checking,
    Importing,
    Authorizing,
    Models,
    LoggingOut,
}

#[derive(Default)]
pub struct CodexUi {
    pub status: Option<LoginStatus>,
    pub working: Option<CodexWork>,
    /// The visible result of the last action; the bool marks a failure (shown in the danger color).
    pub notice: Option<(String, bool)>,
    pub surface: Option<CodexSurface>,
    pub authorize_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl CodexUi {
    pub fn begin(&mut self, surface: CodexSurface, work: CodexWork) {
        self.surface = Some(surface);
        self.working = Some(work);
        // A status check follows other actions and must not hide their result.
        if work != CodexWork::Checking {
            self.notice = None;
        }
    }
    pub fn busy(&self) -> bool {
        self.working.is_some()
    }

    /// The last action's result, shown only where that action started.
    pub fn notice_on(&self, surface: CodexSurface) -> Option<(String, bool)> {
        self.notice
            .clone()
            .filter(|_| self.surface == Some(surface))
    }

    pub fn cancel_authorization(&self) {
        if let Some(cancel) = &self.authorize_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    pub fn fail(&mut self, message: String) {
        self.notice = Some((message, true));
    }
    pub fn inform(&mut self, message: String) {
        self.notice = Some((message, false));
    }
}
