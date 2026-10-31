//! Builds with `--features capture`: render named pages to PNG files and quit,
//! so native layout can be reviewed without screen-recording permission.
//!
//! `COURSE2MD_CAPTURE` lists `page=path.png` pairs separated by `;`. Pages are
//! `onboarding[:<ai|account|model|cloud>]`, `home[:<state>]`, `library`, `tasks`, `settings:<category>` (appearance, generation,
//! services, storage, application) and `reader[:<title text>][~shots][~shot:<n>][~end]`
//! (the newest note, or the first whose title contains the text; it opens at
//! the top, ignoring saved reading positions; `~shots` opens the screenshot
//! tab, `~shot:<n>` opens the image viewer on the 0-based screenshot `n` of
//! that tab, `~end` scrolls the view to its end). `library:<state>` takes
//! `,`-separated states: `list`, `group`, `unfiled`, `folder~<name>` and
//! `search~<text>` (`:` also separates a state from its value, as in
//! `library:search:<text>`); each shot starts from the saved presentation,
//! all notes and an empty search. `COURSE2MD_CAPTURE_DELAY_MS` (default
//! 1800) is the wait after each navigation; `COURSE2MD_CAPTURE_PALETTE`
//! (a palette name such as `catppuccin_mocha`) and `COURSE2MD_CAPTURE_SCALE`
//! (1, 1.25, 1.5 or 2) override the saved appearance without changing it.
//! `COURSE2MD_CAPTURE_CHROME` (`windows` or `linux`) draws that platform's
//! caption buttons and leading edge instead of this one's, so the Windows and
//! Linux title bars can be reviewed from a macOS capture.
//!
//! `input:<steps>` dispatches simulated pointer and keyboard input to the
//! window as it is (see `simulate_input`), steps joined by `+`, then
//! captures; it neither navigates nor closes dialogs.
//!
//! Dialogs open over their page: `service-editor:<ai|speech|dialog|confirm>`
//! over Settings › 服务与账号, and `dialog:<name>` for `palette`, `rename`,
//! `delete`, `confirm` (a `ui::Confirm`) and `image` (the newest note; give it a longer
//! `COURSE2MD_CAPTURE_DELAY_MS`), `folder`, `storage` and `login`. Each
//! capture first closes the dialogs an earlier one opened, so one shot shows
//! one dialog. Nothing a dialog would change is saved unless its action is
//! taken.
//!
//! `dialog:storage` opens the operation dialog without starting a move, so it
//! shows the idle result state ("操作已完成"). Its progress, cancelling and
//! failure states only exist while a real move, relocation or cleanup runs
//! (from Settings › 存储 against a fixture library); the harness does not fake them.
use crate::{Desktop, Page, palettes::PaletteId, settings_ui::SettingsTab};
use gpui::{App, AsyncWindowContext, WeakEntity, Window};
use gpui_kit::component::WindowExt as _;
use std::{cell::Cell, path::PathBuf, time::Duration};

thread_local! {
    static PALETTE: Cell<Option<PaletteId>> = const { Cell::new(None) };
    static SCALE: Cell<Option<f32>> = const { Cell::new(None) };
    static CAPTION: Cell<Option<gpui_kit_material::components::Caption>> = const { Cell::new(None) };
    /// The saved library presentation (cards, grouped), restored before each
    /// library shot so one shot's state does not leak into the next.
    static LIBRARY_PRESENTATION: Cell<Option<(bool, bool)>> = const { Cell::new(None) };
    /// The saved processing-options visibility, restored before each home shot.
    static HOME_OPTIONS: Cell<Option<bool>> = const { Cell::new(None) };
}

/// The palette and text scale to paint instead of the saved ones.
pub fn palette_override() -> Option<PaletteId> {
    PALETTE.with(Cell::get)
}

/// Applies the capture palette to an appearance the way the window paints
/// it: that palette selected, in its own light or dark mode.
pub fn override_appearance(appearance: &mut crate::palettes::ThemePreferences) {
    if let Some(palette) = palette_override() {
        appearance.select(palette);
        appearance.mode = if palette.is_dark() {
            crate::palettes::Appearance::Dark
        } else {
            crate::palettes::Appearance::Light
        };
    }
}

/// The caption buttons to draw instead of this platform's.
pub fn caption_override() -> Option<gpui_kit_material::components::Caption> {
    CAPTION.with(Cell::get)
}

pub fn scale_override() -> Option<f32> {
    SCALE.with(Cell::get)
}

pub fn start(view: WeakEntity<Desktop>, window: &mut Window, cx: &mut App) {
    let Ok(spec) = std::env::var("COURSE2MD_CAPTURE") else {
        return;
    };
    if let Ok(name) = std::env::var("COURSE2MD_CAPTURE_PALETTE") {
        let palette = serde_json::from_value(serde_json::Value::String(name.clone()))
            .unwrap_or_else(|_| panic!("unknown capture palette {name}"));
        PALETTE.with(|cell| cell.set(Some(palette)));
    }
    if let Some(scale) = std::env::var("COURSE2MD_CAPTURE_SCALE")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        SCALE.with(|cell| cell.set(Some(scale)));
    }
    if let Ok(chrome) = std::env::var("COURSE2MD_CAPTURE_CHROME") {
        let caption = match chrome.as_str() {
            "windows" => gpui_kit_material::components::Caption::Windows,
            "linux" => gpui_kit_material::components::Caption::Linux,
            _ => panic!("unknown capture chrome {chrome}"),
        };
        CAPTION.with(|cell| cell.set(Some(caption)));
        window.refresh();
    }
    let delay = Duration::from_millis(
        std::env::var("COURSE2MD_CAPTURE_DELAY_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1800),
    );
    let shots: Vec<(String, PathBuf)> = spec
        .split(';')
        .filter_map(|pair| {
            let (page, path) = pair.split_once('=')?;
            Some((page.trim().to_string(), PathBuf::from(path.trim())))
        })
        .collect();
    window
        .spawn(cx, async move |cx: &mut AsyncWindowContext| {
            // Let the library load first, so a leading `reader` shot finds its note.
            cx.background_executor().timer(delay).await;
            for (page, path) in shots {
                if let Some(steps) = page.strip_prefix("input:") {
                    // Simulated input on whatever the previous shot left open.
                    for step in steps.split('+') {
                        simulate_input(step, cx).await;
                    }
                } else {
                    let _ = cx.update(|window, cx| {
                        view.update(cx, |this, cx| this.capture_navigate(&page, window, cx))
                    });
                }
                cx.background_executor().timer(delay).await;
                let _ = cx.update(|window, cx| {
                    // Draw now: a window hidden behind others gets no frames, and
                    // the image would show whatever it drew last.
                    window.refresh();
                    window.draw(cx).clear(cx);
                });
                let _ = cx.update(|window, _| match window.render_to_image() {
                    Ok(image) => {
                        if let Err(error) = image.save(&path) {
                            eprintln!("capture {page}: {error}");
                        }
                    }
                    Err(error) => eprintln!("capture {page}: {error:#}"),
                });
            }
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
}

/// One simulated input step, `kind~args` with `~`-separated arguments in
/// window pixels: `move~x~y`, `click~x~y[~count]`, `drag~x1~y1~x2~y2`, `wheel~x~y~dy[~dx]` (positive scrolls
/// the content up or left, as a downward or rightward wheel turn), `key~<keystroke>` (GPUI
/// syntax, e.g. `escape`, `right`, `cmd-f`; `equal` and `plus` name the
/// keys the spec reserves) and `wait~ms`. Each platform
/// event is dispatched in its own update, as native input arrives.
async fn simulate_input(step: &str, cx: &mut AsyncWindowContext) {
    use gpui::{
        Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
        PlatformInput, ScrollDelta, ScrollEvent, TouchPhase, point, px,
    };
    let mut parts = step.split('~');
    let kind = parts.next().unwrap_or_default();
    let numbers: Vec<f32> = parts.filter_map(|part| part.parse().ok()).collect();
    let position = (numbers.len() >= 2).then(|| point(px(numbers[0]), px(numbers[1])));
    let moved = |position| {
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        })
    };
    let events = match (kind, position) {
        ("move", Some(position)) => vec![moved(position)],
        ("click", Some(position)) => {
            // An optional third number repeats the click as a double or
            // triple click, counting up as the platform reports it.
            let clicks = numbers.get(2).map_or(1, |count| (*count as usize).max(1));
            let mut events = vec![moved(position)];
            for click_count in 1..=clicks {
                events.push(PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count,
                    first_mouse: false,
                }));
                events.push(PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count,
                }));
            }
            events
        }
        ("drag", Some(from)) if numbers.len() >= 4 => {
            let to = point(px(numbers[2]), px(numbers[3]));
            let mut events = vec![
                moved(from),
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
            ];
            events.extend((1..=8).map(|step| {
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from + (to - from) * (step as f32 / 8.),
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                })
            }));
            events.push(PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                modifiers: Modifiers::default(),
                click_count: 1,
            }));
            events
        }
        ("wheel", Some(position)) if numbers.len() >= 3 => vec![
            moved(position),
            PlatformInput::Scroll(ScrollEvent {
                position,
                delta: ScrollDelta::Pixels(point(
                    px(-numbers.get(3).copied().unwrap_or(0.)),
                    px(-numbers[2]),
                )),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
                source: Default::default(),
            }),
        ],
        ("key", _) => {
            // `=`, `;` and `+` separate the capture spec; name them instead.
            let key = match step.split_once('~').map(|(_, key)| key).unwrap_or_default() {
                "equal" => "=",
                "plus" => "+",
                key => key,
            };
            let _ = cx.update(|window, cx| {
                match Keystroke::parse_with_os(key, cx.platform_info().os) {
                    Ok(keystroke) => {
                        window.dispatch_keystroke(keystroke, cx);
                    }
                    Err(error) => eprintln!("capture input {step}: {error}"),
                }
            });
            Vec::new()
        }
        ("wait", _) if !numbers.is_empty() => {
            cx.background_executor()
                .timer(Duration::from_millis(numbers[0] as u64))
                .await;
            return;
        }
        _ => {
            eprintln!("capture input: unknown step {step}");
            Vec::new()
        }
    };
    for event in events {
        let _ = cx.update(|window, cx| {
            window.dispatch_event(event, cx);
        });
        cx.background_executor()
            .timer(Duration::from_millis(60))
            .await;
    }
    cx.background_executor()
        .timer(Duration::from_millis(400))
        .await;
}

impl Desktop {
    fn capture_navigate(&mut self, page: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let (page, category) = page.split_once(':').unwrap_or((page, ""));
        // One dialog per shot: close whatever the previous capture opened.
        window.close_all_dialogs(cx);
        self.close_settings_dialogs(cx);
        // First use is reviewed on its own; captured pages show the app behind it.
        if page != "onboarding" {
            self.onboarding.active = false;
        }
        match page {
            // First use opens over the app even when it was completed (or an
            // earlier shot left it); `onboarding:<step>` picks the step.
            "onboarding" => {
                if !self.onboarding.active {
                    self.navigate(Page::New, cx);
                    self.start_onboarding(window, cx);
                }
                self.capture_setup_step(category, cx);
            }
            "home" => self.capture_home(category, window, cx),
            "library" => {
                self.navigate(Page::Library, cx);
                self.capture_library_state(category, window, cx);
            }
            "tasks" => self.navigate(Page::Task, cx),
            "settings" => {
                self.open_settings(window, cx);
                let tab = match category {
                    "generation" => SettingsTab::Generation,
                    "services" => SettingsTab::Services,
                    "storage" => SettingsTab::Storage,
                    "application" => SettingsTab::Application,
                    _ => SettingsTab::Appearance,
                };
                self.select_settings_tab(tab, window, cx);
            }
            "service-editor" => {
                self.open_settings(window, cx);
                self.select_settings_tab(SettingsTab::Services, window, cx);
                self.capture_service_editor(category, window, cx);
            }
            "dialog" => self.capture_dialog(category, window, cx),
            "reader" => {
                // `<title>[~shots][~shot:<n>][~end]`: the screenshot tab, the
                // image viewer on frame `n` (0-based), the end of the view.
                let mut parts = category.split('~');
                let title = parts.next().unwrap_or_default();
                let (mut shots, mut shot, mut end) = (false, None, false);
                for part in parts {
                    match part.split_once(':') {
                        Some(("shot", index)) => shot = index.parse::<usize>().ok(),
                        _ if part == "shots" => shots = true,
                        _ if part == "end" => end = true,
                        _ => {}
                    }
                }
                let course = self
                    .courses
                    .iter()
                    .find(|course| title.is_empty() || course.title.contains(title))
                    .cloned();
                // A shot opens the note at its top unless it asks for the end:
                // record the open note's position now (so opening does not
                // record it again afterwards), then forget the loaded reading
                // positions so an earlier `~end` shot or a review session does
                // not leak in.
                self.save_reading_position(cx);
                if let Some(workspace) = &mut self.workspace {
                    workspace.state.positions.clear();
                }
                if let Some(course) = course {
                    self.open_course(course, cx);
                }
                if end || shots || shot.is_some() {
                    // The note's blocks and screenshots load after it opens.
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(1200))
                            .await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.capture_reader_view(shots, shot, end, window, cx)
                        });
                    })
                    .detach();
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Opens one dialog over the page it belongs to, once the page has
    /// loaded what the dialog acts on (the notes and their folders).
    fn capture_dialog(&mut self, name: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        match name {
            "palette" | "storage" => {
                self.open_settings(window, cx);
                let tab = if name == "palette" {
                    SettingsTab::Appearance
                } else {
                    SettingsTab::Storage
                };
                self.select_settings_tab(tab, window, cx);
            }
            "login" => self.navigate(Page::New, cx),
            "image" => {}
            _ => self.navigate(Page::Library, cx),
        }
        let name = name.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let newest = this.courses.first().map(|course| (**course).clone());
                match name.as_str() {
                    "palette" => this.open_palette_picker(false, cx),
                    "storage" => this.open_storage_dialog(window, cx),
                    "login" => this.open_account_dialog(window, cx),
                    "image" => {
                        if let Some(course) = newest {
                            this.open_course(course, cx);
                        }
                    }
                    "confirm" => {
                        crate::ui::Confirm::new("Delete note", "Keep note", "Move to Trash")
                            .danger()
                            .text("The note moves to the Trash. You can restore it from there.")
                            .ask(|_, _, _| {}, window, cx)
                    }
                    "folder" => this.begin_folder(None, window, cx),
                    "rename" => {
                        if let Some(course) = newest {
                            this.begin_course_rename(course, window, cx);
                        }
                    }
                    "delete" => {
                        if let Some(course) = newest {
                            this.confirm_course_delete(course, window, cx);
                        }
                    }
                    _ => {}
                }
            });
            if name == "image" {
                // The note's screenshots load after it opens.
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                let _ =
                    this.update_in(cx, |this, window, cx| this.open_reader_image(0, window, cx));
            }
        })
        .detach();
    }

    /// Home enters the workbench the way the app bar does. `home:<state>`
    /// takes `,`-separated states, applied without saving: `options-hidden`
    /// and `options-shown` set the processing options' visibility, `submit`
    /// presses 开始转换 for the previewed video (`COURSE2MD_CAPTURE_SOURCE`),
    /// and `next` presses 设置下一个视频.
    fn capture_home(&mut self, states: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let current = self.desktop_settings.home_options;
        self.desktop_settings.home_options = HOME_OPTIONS.with(|cell| {
            let saved = cell.get().unwrap_or(current);
            cell.set(Some(saved));
            saved
        });
        if self.prepare_workbench_input(window, cx) {
            self.navigate(Page::New, cx);
        }
        for state in states.split(',').filter(|state| !state.is_empty()) {
            match state {
                "options-hidden" => self.desktop_settings.home_options = false,
                "options-shown" => self.desktop_settings.home_options = true,
                "submit" => self.start_conversion(window, cx),
                "next" => self.set_up_next_video(window, cx),
                _ => {}
            }
        }
    }

    /// Library states reached by navigation, applied without saving them.
    fn capture_library_state(
        &mut self,
        states: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // Each shot starts from the saved presentation, all notes and no search.
        let current = (
            self.desktop_settings.library_cards,
            self.desktop_settings.library_group_folders,
        );
        let saved = LIBRARY_PRESENTATION.with(|cell| {
            let saved = cell.get().unwrap_or(current);
            cell.set(Some(saved));
            saved
        });
        (
            self.desktop_settings.library_cards,
            self.desktop_settings.library_group_folders,
        ) = saved;
        let root = self.library_root.clone();
        self.select_library_scope(root, crate::FolderFilter::All, cx);
        self.inputs[&crate::Field::Search].update(cx, |input, cx| input.set_value("", window, cx));
        for state in states.split(',').filter(|state| !state.is_empty()) {
            let (state, value) = state.split_once(['~', ':']).unwrap_or((state, ""));
            match state {
                "list" => self.desktop_settings.library_cards = false,
                "group" => self.desktop_settings.library_group_folders = true,
                "unfiled" => {
                    let root = self.library_root.clone();
                    self.select_library_scope(root, crate::FolderFilter::Unfiled, cx);
                }
                "folder" => {
                    let root = self.library_root.clone();
                    let found = self.library_indexes.get(&root).and_then(|library| {
                        library
                            .folders
                            .iter()
                            .find(|(_, name)| name.as_str() == value)
                            .map(|(id, _)| *id)
                    });
                    if let Some(id) = found {
                        self.select_library_scope(root, crate::FolderFilter::Folder(id), cx);
                    }
                }
                "search" => {
                    let value = value.to_owned();
                    self.inputs[&crate::Field::Search]
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
                _ => {}
            }
        }
    }
}
