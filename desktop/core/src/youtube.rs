//! The YouTube connection, as state: which browser's YouTube session yt-dlp reads.
//! No cookie passes through here.
use course2md::auth::youtube::{Browser, BrowserChoice, Profile};

/// The YouTube connection: which browser session YouTube runs use.
#[derive(Default)]
pub struct YoutubeUi {
    /// The saved choice; `None` is not connected.
    pub choice: Option<BrowserChoice>,
    pub load_error: Option<String>,
    /// The last check of the saved choice, when one ran.
    pub status: Option<YoutubeStep>,
    pub checking: bool,
    pub generation: u64,
    pub dialog: Option<YoutubeDialog>,
    /// The closed dialog's last state, so it can play its exit.
    pub closing: Option<YoutubeDialog>,
    pub retry_source: Option<(u64, String)>,
}

impl YoutubeUi {
    pub fn current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    pub fn close(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.closing = self.dialog.take().or(self.closing.take());
        self.retry_source = None;
    }
}

#[derive(Clone)]
pub struct YoutubeDialog {
    pub browsers: Vec<Browser>,
    pub browser: Option<usize>,
    /// The chosen browser's profiles, when it has more than one.
    pub profiles: Vec<Profile>,
    pub profile: Option<usize>,
    pub browser_open: bool,
    pub profile_open: bool,
    pub step: YoutubeStep,
}

impl YoutubeDialog {
    /// Starts on the saved choice, else on the first installed browser.
    pub fn new(browsers: Vec<Browser>, saved: Option<&BrowserChoice>) -> Self {
        let browser = saved
            .and_then(|choice| {
                browsers
                    .iter()
                    .position(|browser| *browser == choice.browser)
            })
            .or((!browsers.is_empty()).then_some(0));
        let mut dialog = Self {
            browsers,
            browser,
            profiles: Vec::new(),
            profile: None,
            browser_open: false,
            profile_open: false,
            step: YoutubeStep::Choose,
        };
        dialog.select_browser(browser, saved.and_then(|choice| choice.profile.as_deref()));
        dialog
    }

    /// Chooses a browser; `profile` keeps a saved profile when it still exists.
    pub fn select_browser(&mut self, index: Option<usize>, profile: Option<&str>) {
        self.browser = index;
        self.profiles = index
            .and_then(|index| self.browsers.get(index))
            .map(|browser| browser.profiles())
            .filter(|profiles| profiles.len() > 1)
            .unwrap_or_default();
        self.profile = (!self.profiles.is_empty()).then(|| {
            profile
                .and_then(|dir| {
                    self.profiles
                        .iter()
                        .position(|candidate| candidate.dir == dir)
                })
                .unwrap_or(0)
        });
        self.step = YoutubeStep::Choose;
    }

    pub fn choice(&self) -> Option<BrowserChoice> {
        Some(BrowserChoice {
            browser: *self.browsers.get(self.browser?)?,
            profile: self
                .profile
                .and_then(|index| self.profiles.get(index))
                .map(|profile| profile.dir.clone()),
        })
    }
}

/// Where connecting (or re-checking) a browser's YouTube session stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YoutubeStep {
    Choose,
    Checking,
    /// The browser holds no YouTube session.
    SignedOut,
    Failed {
        summary: String,
        detail: String,
    },
    Connected,
}
