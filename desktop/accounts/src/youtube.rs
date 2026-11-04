//! The YouTube connection, as state: which browser's YouTube session yt-dlp reads.
//! No cookie passes through here.
use c2m_source::auth::youtube::{Access, Browser, BrowserChoice, Profile};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The YouTube connection: which browser session YouTube runs use.
#[derive(Default)]
pub struct YoutubeUi {
    /// The saved choice; `None` is not connected.
    pub choice: Option<BrowserChoice>,
    pub load_error: Option<String>,
    /// The last check of the saved choice, when one ran.
    pub status: Option<YoutubeStep>,
    pub checking: bool,
    /// A look at the guide's folder is scheduled; one at a time.
    pub polling: bool,
    /// The generation of the re-check that set `checking`. Its result clears
    /// `checking` even when it arrives stale, unless a newer re-check took over.
    pub recheck: Option<u64>,
    pub generation: u64,
    pub dialog: Option<YoutubeDialog>,
    /// The closed dialog's last state, so it can play its exit.
    pub closing: Option<YoutubeDialog>,
    pub retry_source: Option<(u64, String)>,
    /// What the system answers about the saved browser's cookie folder, when
    /// looked: a blocked one is the settings row's warning.
    pub access: Option<Access>,
    /// Open the dialog again on this launch, on the browser it guided for: the
    /// person left it to give access (and macOS, or the guide, reopened the app).
    pub resume_guide: Option<Option<Browser>>,
}

impl YoutubeUi {
    pub fn current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// Ends the re-check started at `generation`, if it is still the running one.
    pub fn finish_recheck(&mut self, generation: u64) {
        if self.recheck == Some(generation) {
            self.recheck = None;
            self.checking = false;
        }
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
    /// The Full Disk Access guide, while the system keeps the chosen browser's
    /// cookie folder from the app (or just gave it).
    pub guide: Option<AccessGuide>,
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
            guide: None,
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
        // Full Disk Access belongs to the app, not to a browser: the guide keeps its
        // step for the newly chosen browser, whose own look then decides.
        let browser = self.selected_browser();
        if let (Some(guide), Some(browser)) = (&mut self.guide, browser) {
            guide.browser = browser;
        }
    }

    pub fn selected_browser(&self) -> Option<Browser> {
        self.browsers.get(self.browser?).copied()
    }

    /// The guide that stands in the way of connecting, if any: a check can only
    /// start once access is open (or the guide has seen it given).
    pub fn access_blocked(&self) -> bool {
        self.guide
            .as_ref()
            .is_some_and(|guide| guide.step != AccessStep::Granted)
    }

    /// What one look at `browser`'s cookie folder says. A look at a browser no
    /// longer chosen is stale and changes nothing; while a check runs or once it
    /// has an answer, the check's own result stands.
    pub fn probed(&mut self, browser: Browser, access: Access, active: bool, now: Instant) {
        if self.selected_browser() != Some(browser) || !matches!(self.step, YoutubeStep::Choose) {
            return;
        }
        self.guide = AccessGuide::next(self.guide.take(), browser, access, active, now);
    }

    /// A check found the folder closed. `access` is the app's own look now: closed
    /// too, the guide starts over; open, yt-dlp still could not read it (the app
    /// was given access while it ran), and only reopening the app applies it.
    pub fn check_blocked(&mut self, access: Access, now: Instant) {
        let Some(browser) = self.selected_browser() else {
            return;
        };
        self.step = YoutubeStep::Choose;
        let mut guide = AccessGuide::new(browser, AccessStep::Blocked, now);
        if access != Access::Blocked {
            guide.step = AccessStep::Restart;
            // The app's own look will keep finding the folder open; only a reopen
            // tells, so looking again would wrongly report access as given.
            guide.reopen_only = true;
        }
        self.guide = Some(guide);
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
        /// The system keeps the browser's data from the app; Full Disk Access
        /// in System Settings lets it through.
        blocked: bool,
    },
    Connected,
}

/// Where the person stands in giving the app Full Disk Access. The app cannot
/// see the switch itself, only whether the browser's folder lists, so each step
/// is what the folder and the person's last action show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessStep {
    /// The folder is closed; nothing done yet: open System Settings.
    Blocked,
    /// System Settings was opened from the guide: turn on course2md there. The app
    /// keeps looking, so a switch that applies at once moves on by itself.
    Waiting,
    /// Back in the app and still closed: macOS applies the switch to an app once it
    /// reopens.
    Restart,
    /// The folder lists now: connecting can go ahead.
    Granted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessGuide {
    pub browser: Browser,
    pub step: AccessStep,
    /// When the guide reached this step.
    pub since: Instant,
    /// While waiting, the app's window lost focus (the person went to System
    /// Settings); coming back after that is what moves the guide to a reopen.
    pub left: bool,
    /// The app was reopened for access and the folder is still closed: the switch
    /// is likely off, or on for another copy of the app.
    pub reopened: bool,
    /// Set by a check that failed although the app's own look found the folder
    /// open: nothing but a reopen applies the access, so looking again is moot.
    pub reopen_only: bool,
}

/// How often the guide looks at the folder again while it is shown.
pub const ACCESS_POLL: Duration = Duration::from_secs(2);

/// Time to find the switch before a return to the app counts as having tried it;
/// sooner, the person is likely still on their way to System Settings.
pub const ACCESS_RETURN_GRACE: Duration = Duration::from_secs(3);

impl AccessGuide {
    pub fn new(browser: Browser, step: AccessStep, now: Instant) -> Self {
        Self {
            browser,
            step,
            since: now,
            left: false,
            reopened: false,
            reopen_only: false,
        }
    }

    /// The guide after one look. `active`: the app's window is the active one, so
    /// after having left it the person is back from System Settings.
    pub fn next(
        guide: Option<Self>,
        browser: Browser,
        access: Access,
        active: bool,
        now: Instant,
    ) -> Option<Self> {
        let at = |step| Some(Self::new(browser, step, now));
        let guide = guide.map(|guide| Self { browser, ..guide });
        match (access, guide) {
            // Nothing to grant: the check reports what is missing.
            (Access::Absent, _) => None,
            (Access::Open, None) => None,
            (Access::Open, Some(guide))
                if guide.step == AccessStep::Granted || guide.reopen_only =>
            {
                Some(guide)
            }
            (Access::Open, Some(_)) => at(AccessStep::Granted),
            (Access::Blocked, None) => at(AccessStep::Blocked),
            // Taken away again after it was given.
            (Access::Blocked, Some(guide)) if guide.step == AccessStep::Granted => {
                at(AccessStep::Blocked)
            }
            (Access::Blocked, Some(mut guide)) if guide.step == AccessStep::Waiting => {
                if !active {
                    guide.left = true;
                } else if guide.left && now.duration_since(guide.since) >= ACCESS_RETURN_GRACE {
                    return at(AccessStep::Restart);
                }
                Some(guide)
            }
            (Access::Blocked, Some(guide)) => Some(guide),
        }
    }

    /// The guide on a launch the guide itself (or macOS) reopened the app for.
    pub fn resumed(browser: Browser, access: Access, now: Instant) -> Option<Self> {
        match access {
            Access::Open => Some(Self::new(browser, AccessStep::Granted, now)),
            Access::Blocked => Some(Self {
                reopened: true,
                ..Self::new(browser, AccessStep::Blocked, now)
            }),
            Access::Absent => None,
        }
    }

    /// The person opened System Settings from the guide.
    pub fn opened_settings(&mut self, now: Instant) {
        if self.step != AccessStep::Granted {
            self.step = AccessStep::Waiting;
            self.since = now;
            self.left = false;
            self.reopen_only = false;
        }
    }

    /// Whether the folder is worth looking at again.
    pub fn polls(&self) -> bool {
        self.step != AccessStep::Granted && !self.reopen_only
    }

    /// The guide's position for its stepper: the three steps, then all done.
    pub fn position(&self) -> usize {
        match self.step {
            AccessStep::Blocked => 0,
            AccessStep::Waiting => 1,
            AccessStep::Restart => 2,
            AccessStep::Granted => 3,
        }
    }
}

/// The `.app` bundle a program runs from, the thing System Settings lists and a
/// relaunch opens; `None` for a program outside a bundle (a development build).
pub fn app_bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .find(|dir| {
            dir.extension().is_some_and(|ext| ext == "app")
                && dir.join("Contents/Info.plist").is_file()
        })
        .map(Path::to_path_buf)
}

/// The note left when the person goes to give access, so the dialog opens again
/// after the app is reopened for it. Older than [`RESUME_WINDOW`], it is stale.
pub fn resume_marker(cache: &Path) -> PathBuf {
    cache.join("youtube-access-guide")
}

pub const RESUME_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Whether `marker` asks this launch to open the dialog again, and for which
/// browser (the note holds its id, as yt-dlp names it).
pub fn resume_due(marker: &Path, now: std::time::SystemTime) -> Option<Option<Browser>> {
    let fresh = std::fs::metadata(marker)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age <= RESUME_WINDOW);
    fresh.then(|| {
        std::fs::read_to_string(marker)
            .ok()
            .and_then(|id| Browser::from_id(&id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guide_dialog() -> YoutubeDialog {
        YoutubeDialog {
            browsers: vec![Browser::Chrome, Browser::Firefox],
            browser: Some(0),
            profiles: Vec::new(),
            profile: None,
            browser_open: false,
            profile_open: false,
            step: YoutubeStep::Choose,
            guide: None,
        }
    }

    fn step(dialog: &YoutubeDialog) -> Option<AccessStep> {
        dialog.guide.as_ref().map(|guide| guide.step)
    }

    #[test]
    fn the_guide_moves_from_blocked_to_waiting_to_restart_to_granted() {
        let start = Instant::now();
        let mut dialog = guide_dialog();
        // Open from the start: no guide at all.
        dialog.probed(Browser::Chrome, Access::Open, true, start);
        assert_eq!(step(&dialog), None);
        assert!(!dialog.access_blocked());
        dialog.probed(Browser::Chrome, Access::Blocked, true, start);
        assert_eq!(step(&dialog), Some(AccessStep::Blocked));
        assert!(dialog.access_blocked());
        // Still blocked while the person is here: nothing happened yet.
        dialog.probed(Browser::Chrome, Access::Blocked, true, start + ACCESS_POLL);
        assert_eq!(step(&dialog), Some(AccessStep::Blocked));
        let opened = start + ACCESS_POLL;
        dialog.guide.as_mut().unwrap().opened_settings(opened);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
        // System Settings slow to come forward: the app never lost focus, so a
        // look after the grace is not a return.
        let late = opened + ACCESS_RETURN_GRACE;
        dialog.probed(Browser::Chrome, Access::Blocked, true, late);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
        // Away in System Settings, then back too soon to have tried: still waiting.
        let opened = late;
        dialog.guide.as_mut().unwrap().opened_settings(opened);
        dialog.probed(Browser::Chrome, Access::Blocked, false, opened);
        dialog.probed(Browser::Chrome, Access::Blocked, true, opened + ACCESS_POLL);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
        // Back, after a while, and still closed: the switch waits for a reopen.
        let back = opened + ACCESS_RETURN_GRACE;
        dialog.probed(Browser::Chrome, Access::Blocked, true, back);
        assert_eq!(step(&dialog), Some(AccessStep::Restart));
        assert_eq!(dialog.guide.as_ref().unwrap().position(), 2);
        // A switch that applies at once opens the folder from any step.
        dialog.probed(Browser::Chrome, Access::Open, true, back + ACCESS_POLL);
        assert_eq!(step(&dialog), Some(AccessStep::Granted));
        assert!(!dialog.access_blocked());
        assert!(!dialog.guide.as_ref().unwrap().polls());
        assert_eq!(dialog.guide.as_ref().unwrap().position(), 3);
        // Taken away again: back to the start.
        dialog.probed(
            Browser::Chrome,
            Access::Blocked,
            true,
            back + ACCESS_POLL * 2,
        );
        assert_eq!(step(&dialog), Some(AccessStep::Blocked));
    }

    #[test]
    fn granted_straight_from_waiting_and_absent_folders_need_no_guide() {
        let now = Instant::now();
        let mut dialog = guide_dialog();
        dialog.probed(Browser::Chrome, Access::Blocked, true, now);
        dialog.guide.as_mut().unwrap().opened_settings(now);
        dialog.probed(Browser::Chrome, Access::Open, false, now + ACCESS_POLL);
        assert_eq!(step(&dialog), Some(AccessStep::Granted));
        // A folder that is not there is the check's to report, not the guide's.
        dialog.probed(Browser::Chrome, Access::Absent, true, now);
        assert_eq!(step(&dialog), None);
    }

    #[test]
    fn a_stale_look_or_another_browser_keeps_the_guides_progress() {
        let now = Instant::now();
        let mut dialog = guide_dialog();
        dialog.probed(Browser::Chrome, Access::Blocked, true, now);
        dialog.guide.as_mut().unwrap().opened_settings(now);
        // A look at Firefox that arrives while Chrome is chosen.
        dialog.probed(Browser::Firefox, Access::Open, true, now);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
        // The access is the app's: another browser keeps the step reached.
        dialog.select_browser(Some(1), None);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
        assert_eq!(dialog.guide.as_ref().unwrap().browser, Browser::Firefox);
        // A late look at the browser no longer chosen changes nothing.
        dialog.probed(Browser::Chrome, Access::Open, true, now);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
        dialog.probed(Browser::Firefox, Access::Open, true, now);
        assert_eq!(step(&dialog), Some(AccessStep::Granted));
        // While a check runs, its result decides, not the poll.
        dialog.step = YoutubeStep::Checking;
        dialog.probed(Browser::Firefox, Access::Blocked, true, now);
        assert_eq!(step(&dialog), Some(AccessStep::Granted));
    }

    #[test]
    fn a_reopened_app_finishes_or_says_it_is_still_closed() {
        let now = Instant::now();
        let granted = AccessGuide::resumed(Browser::Chrome, Access::Open, now).unwrap();
        assert_eq!(granted.step, AccessStep::Granted);
        let still = AccessGuide::resumed(Browser::Chrome, Access::Blocked, now).unwrap();
        assert_eq!((still.step, still.reopened), (AccessStep::Blocked, true));
        assert!(still.polls());
        assert_eq!(
            AccessGuide::resumed(Browser::Chrome, Access::Absent, now),
            None
        );
    }

    #[test]
    fn a_check_that_finds_the_folder_closed_guides_or_asks_for_a_reopen() {
        let now = Instant::now();
        let mut dialog = guide_dialog();
        dialog.step = YoutubeStep::Checking;
        dialog.check_blocked(Access::Blocked, now);
        assert_eq!(dialog.step, YoutubeStep::Choose);
        assert_eq!(step(&dialog), Some(AccessStep::Blocked));
        dialog.step = YoutubeStep::Checking;
        dialog.check_blocked(Access::Open, now);
        assert_eq!(dialog.step, YoutubeStep::Choose);
        assert_eq!(step(&dialog), Some(AccessStep::Restart));
        // The app's own look still finds the folder open: that is no grant, and
        // there is nothing to look for until the app reopens.
        assert!(!dialog.guide.as_ref().unwrap().polls());
        dialog.probed(Browser::Chrome, Access::Open, true, now + ACCESS_POLL);
        assert_eq!(step(&dialog), Some(AccessStep::Restart));
        // Settings opened again from there: waiting once more.
        dialog.guide.as_mut().unwrap().opened_settings(now);
        assert_eq!(step(&dialog), Some(AccessStep::Waiting));
    }

    #[test]
    fn the_bundle_is_the_app_that_holds_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Apps/course2md.app");
        let exe = app.join("Contents/MacOS/course2md");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(app_bundle_of(&exe), None);
        std::fs::write(app.join("Contents/Info.plist"), "").unwrap();
        assert_eq!(app_bundle_of(&exe), Some(app.clone()));
        // A development build is no bundle.
        assert_eq!(
            app_bundle_of(&dir.path().join("target/debug/course2md-desktop")),
            None
        );
    }

    #[test]
    fn the_resume_note_is_honoured_only_while_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let marker = resume_marker(dir.path());
        let now = std::time::SystemTime::now();
        let soon = now + Duration::from_secs(60);
        assert_eq!(resume_due(&marker, now), None);
        std::fs::write(&marker, "brave").unwrap();
        assert_eq!(resume_due(&marker, soon), Some(Some(Browser::Brave)));
        std::fs::write(&marker, "").unwrap();
        assert_eq!(resume_due(&marker, soon), Some(None));
        let late = now + RESUME_WINDOW + Duration::from_secs(60);
        assert_eq!(resume_due(&marker, late), None);
    }
}
