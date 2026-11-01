//! The Bilibili account and its QR login, as state. No ticket is logged or stored in
//! interface text.
use anyhow::Result;
use course2md::auth::{AccountProfile, AccountStatus};
use std::sync::Arc;

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
}

impl AccountUi {
    pub fn current(&self, generation: u64) -> bool {
        self.generation == generation && self.dialog.is_some()
    }
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.dialog = None;
        self.modules = None;
    }

    pub fn close(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.closing = self.dialog.take().or(self.closing.take());
        self.retry_source = None;
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
