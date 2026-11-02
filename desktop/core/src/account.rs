//! The Bilibili account and its logins (QR code, password or SMS), as state. No
//! ticket or password is logged or stored in interface text.
use anyhow::Result;
use course2md::auth::{
    AccountProfile, AccountStatus,
    passport::{Passport, RiskCheck, SmsTicket},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
pub struct AccountUi {
    pub generation: u64,
    pub status_generation: u64,
    pub dialog: Option<QrDialogState>,
    /// The closed dialog's last state, so it can play its exit.
    pub closing: Option<QrDialogState>,
    pub modules: Option<Arc<Vec<Vec<bool>>>>,
    pub status: Option<AccountStatus>,
    pub checking: bool,
    pub has_saved_login: bool,
    pub status_error: Option<String>,
    /// The current status_error is the logout-incomplete message; a flag so a
    /// copy change cannot silently break the status derivation.
    pub logout_failed: bool,
    pub retry_source: Option<(u64, String)>,
    /// The dialog's sign-in method; the QR state above belongs to [`LoginMethod::Qr`].
    pub method: LoginMethod,
    pub form: LoginForm,
    /// The password or SMS attempt's connection, between its steps.
    pub passport: Option<Passport>,
    pub sms: Option<SmsTicket>,
    /// Bilibili asks the account's bound phone to confirm this login.
    pub risk: Option<RiskCheck>,
}

impl AccountUi {
    pub fn current(&self, generation: u64) -> bool {
        self.generation == generation && self.dialog.is_some()
    }
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.dialog = None;
        self.modules = None;
        self.reset_form();
    }

    pub fn close(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.closing = self.dialog.take().or(self.closing.take());
        self.retry_source = None;
        self.reset_form();
    }

    /// Ends a password or SMS attempt: its browser check stops waiting, and its
    /// connection, code and phone check are dropped.
    pub fn reset_form(&mut self) {
        self.form.stop();
        self.form = LoginForm::default();
        self.passport = None;
        self.sms = None;
        self.risk = None;
    }

    /// Switches the dialog's method. Late replies of the previous one are stale.
    pub fn choose_method(&mut self, method: LoginMethod) {
        if self.method == method {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.modules = None;
        self.reset_form();
        self.method = method;
    }

    pub fn apply_status(
        &mut self,
        generation: u64,
        has_saved_login: bool,
        result: Result<AccountStatus, String>,
    ) -> bool {
        if self.status_generation != generation {
            return false;
        }
        self.checking = false;
        self.has_saved_login = has_saved_login;
        match result {
            Ok(status) => {
                // A removed credential file must not keep the previous "saved" badge.
                self.has_saved_login = !matches!(status, AccountStatus::Disconnected);
                self.status = Some(status);
                self.status_error = None;
                self.logout_failed = false;
            }
            Err(error) => {
                self.status_error = Some(error);
                self.logout_failed = false;
            }
        }
        true
    }

    pub fn login_action(&self) -> Option<&'static str> {
        if matches!(self.status, Some(AccountStatus::Connected(_)))
            && self.has_saved_login
            && self.status_error.is_none()
        {
            None
        } else if self.has_saved_login || matches!(self.status, Some(AccountStatus::Expired)) {
            Some(course2md::tr!("重新登录 Bilibili"))
        } else {
            Some(course2md::tr!("登录 Bilibili"))
        }
    }

    pub fn can_resume_source(&self, generation: u64, input: &str, in_source: bool) -> bool {
        in_source
            && self
                .retry_source
                .as_ref()
                .is_some_and(|(saved_generation, saved_input)| {
                    *saved_generation == generation && saved_input == input
                })
    }
}

/// How the person signs in to Bilibili. One dialog offers all three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoginMethod {
    #[default]
    Qr,
    Password,
    Sms,
}

impl LoginMethod {
    pub const ALL: [Self; 3] = [Self::Qr, Self::Password, Self::Sms];

    pub fn id(self) -> &'static str {
        match self {
            Self::Qr => "qr",
            Self::Password => "password",
            Self::Sms => "sms",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Qr => course2md::tr!("扫码"),
            Self::Password => course2md::tr!("密码"),
            Self::Sms => course2md::tr!("短信"),
        }
    }
}

/// Where a password or SMS login stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FormStep {
    /// The fields are the person's to fill.
    #[default]
    Entry,
    /// The human check is open in the browser at `url` (unknown while it is fetched).
    Captcha { url: Option<String> },
    /// A request to Bilibili is in flight.
    Working,
}

#[derive(Default)]
pub struct LoginForm {
    pub step: FormStep,
    /// Bilibili's or the check's reason the last step failed.
    pub error: Option<String>,
    /// A code was sent: to the phone typed (SMS login) or the bound phone (check).
    pub code_sent: bool,
    cancel: Option<Arc<AtomicBool>>,
}

impl LoginForm {
    pub fn busy(&self) -> bool {
        matches!(self.step, FormStep::Captcha { .. } | FormStep::Working)
    }

    /// The flag a browser check waits on; set when the attempt ends.
    pub fn watch(&mut self) -> Arc<AtomicBool> {
        self.stop();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        cancel
    }

    pub fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[derive(Clone)]
pub enum QrDialogState {
    Generating,
    Waiting(u64),
    Confirming(u64),
    Expired,
    Error(String),
    Success(AccountProfile),
}

pub enum QrPollResult {
    Waiting,
    Confirming,
    Expired,
    Authenticated(Result<AccountProfile>),
    Failed,
}

/// The QR code's size and whether the text sits beside it. A short window keeps the
/// code whole: the text moves beside it before the code shrinks. The code never
/// scrolls, and `body` is the height the dialog leaves for its content.
pub fn qr_layout(content_width: f32, body: f32, rem_size: f32) -> (f32, bool) {
    let gap = rem_size * (16. / 14.);
    let preferred = 260_f32.min(content_width);
    let text = rem_size * 4.5;
    let beside = body < preferred + gap + text
        && content_width >= preferred.min(body) + gap + rem_size * 12.;
    let code = if beside {
        preferred.min(body)
    } else {
        preferred.min((body - gap - text).max(0.))
    };
    (code.floor(), beside)
}

/// Whole physical pixels avoid blurry modules. Centering includes at least
/// four white modules on every side, even when the available size is fractional.
pub fn qr_raster(code_size: f32, module_count: usize, scale: f32) -> (f32, f32) {
    let total = (module_count + 8) as f32;
    // Keep a pixel of slack at each edge so snapping a fractional window origin
    // cannot take part of the four-module quiet zone away.
    let unit = ((code_size * scale - 2.).max(0.) / total).floor() / scale;
    let offset = (code_size - unit * total) / 2. + unit * 4.;
    (unit, offset)
}

#[cfg(test)]
mod tests {
    use super::{AccountUi, QrDialogState, qr_layout, qr_raster};
    use course2md::auth::{AccountProfile, AccountStatus};

    #[test]
    fn qr_layout_keeps_the_complete_code_inside_short_and_regular_dialogs() {
        // A short window at 200% text: the text moves beside a whole code.
        let (code, beside) = qr_layout(812., 268., 28.);
        assert!(beside);
        assert!(code <= 268.);
        assert!(code >= 196.);

        let (code, beside) = qr_layout(400., 500., 14.);
        assert!(!beside);
        assert_eq!(code, 260.);

        for (width, height) in [(400., 300.), (812., 268.), (560., 800.)] {
            for scale in crate::preferences::FONT_SCALES {
                let (code, _) = qr_layout(width, height, 14. * scale);
                assert!(code > 0. && code <= width && code <= height);
            }
        }
    }

    #[test]
    fn qr_physical_pixels_retain_four_white_modules_after_origin_snapping() {
        for code_size in [196., 195., 260.] {
            for module_count in [21, 41, 57, 77] {
                for scale in [1., 2.] {
                    let (unit, offset) = qr_raster(code_size, module_count, scale);
                    assert!(unit > 0.);
                    assert_eq!((unit * scale).fract(), 0.);
                    for origin in [0., 0.125, 0.25, 0.5, 0.75] {
                        let first = ((origin + offset) * scale).round() / scale;
                        let last = ((origin + offset + (module_count - 1) as f32 * unit) * scale)
                            .round()
                            / scale
                            + unit;
                        assert!(first - origin >= unit * 4. - 0.001);
                        assert!(origin + code_size - last >= unit * 4. - 0.001);
                    }
                }
            }
        }
    }

    #[test]
    fn closing_and_refreshing_reject_late_qr_results() {
        let mut account = AccountUi {
            dialog: Some(QrDialogState::Generating),
            ..Default::default()
        };
        let first = account.generation;
        assert!(account.current(first));
        account.invalidate();
        assert!(!account.current(first));
        account.dialog = Some(QrDialogState::Generating);
        assert!(!account.current(first));
        assert!(account.current(account.generation));
    }

    #[test]
    fn temporary_validation_failure_keeps_login_and_allows_repair() {
        let connected = AccountStatus::Connected(AccountProfile {
            name: "Fixture account".into(),
        });
        let mut account = AccountUi {
            status_generation: 7,
            status: Some(connected.clone()),
            has_saved_login: true,
            checking: true,
            ..Default::default()
        };
        assert!(account.apply_status(7, true, Err("Network unavailable".into())));
        assert_eq!(account.status, Some(connected.clone()));
        assert!(account.has_saved_login);
        assert!(!account.checking);
        assert_eq!(account.login_action(), Some("重新登录 Bilibili"));

        assert!(account.apply_status(7, true, Ok(connected)));
        assert!(account.status_error.is_none());
        assert_eq!(account.login_action(), None);

        assert!(account.apply_status(7, true, Ok(AccountStatus::Expired)));
        assert_eq!(account.login_action(), Some("重新登录 Bilibili"));
        assert!(account.apply_status(7, true, Ok(AccountStatus::Disconnected)));
        assert!(!account.has_saved_login);
        assert_eq!(account.login_action(), Some("登录 Bilibili"));
    }

    #[test]
    fn late_account_check_cannot_replace_a_newly_authenticated_account() {
        let connected = AccountStatus::Connected(AccountProfile {
            name: "New account".into(),
        });
        let mut account = AccountUi {
            status_generation: 4,
            status: Some(connected.clone()),
            has_saved_login: true,
            ..Default::default()
        };
        assert!(!account.apply_status(3, false, Ok(AccountStatus::Disconnected)));
        assert_eq!(account.status, Some(connected));
        assert!(account.has_saved_login);
        assert!(account.status_error.is_none());
    }

    #[test]
    fn source_continuation_requires_the_same_input_and_origin_after_login() {
        let input = "https://www.bilibili.com/video/BVfixture";
        let mut account = AccountUi {
            retry_source: Some((5, input.into())),
            dialog: Some(QrDialogState::Waiting(120)),
            ..Default::default()
        };
        assert!(account.can_resume_source(5, input, true));
        assert!(!account.can_resume_source(6, input, true));
        assert!(!account.can_resume_source(5, "https://b23.tv/changed", true));
        // Settings and an active guide are not the originating source view.
        assert!(!account.can_resume_source(5, input, false));
        account.invalidate();
        account.dialog = Some(QrDialogState::Waiting(180));
        assert!(account.can_resume_source(5, input, true));
        account.close();
        assert!(!account.can_resume_source(5, input, true));
    }
}
