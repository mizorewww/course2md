//! Builds with `--features capture`: render named pages to PNG files and quit,
//! so native layout can be reviewed without screen-recording permission.
//!
//! `COURSE2MD_CAPTURE` lists `page=path.png` pairs separated by `;`. Pages are
//! `onboarding`, `home`, `library`, `tasks`, `settings:<category>` (appearance, generation,
//! services, storage, application) and `reader[:<title text>]` (the newest
//! note, or the first whose title contains the text). `library:<state>` takes
//! `,`-separated states: `list`, `group`, `unfiled`, `folder~<name>`, `search~<text>`. `COURSE2MD_CAPTURE_DELAY_MS` (default
//! 1800) is the wait after each navigation; `COURSE2MD_CAPTURE_PALETTE`
//! (a palette name such as `catppuccin_mocha`) and `COURSE2MD_CAPTURE_SCALE`
//! (1, 1.25, 1.5 or 2) override the saved appearance without changing it.
//!
//! Dialogs open over their page: `service-editor:<ai|speech|dialog|confirm>`
//! over Settings › 服务与账号, and `dialog:<name>` for `palette`, `rename`,
//! `delete` and `image` (the newest note; give it a longer
//! `COURSE2MD_CAPTURE_DELAY_MS`), `folder`, `storage` and `login`.
//! Nothing a dialog would change is saved unless its action is taken.
use crate::{Desktop, Page, palettes::PaletteId, settings_ui::SettingsTab};
use gpui::{App, AsyncWindowContext, WeakEntity, Window};
use std::{cell::Cell, path::PathBuf, time::Duration};

thread_local! {
    static PALETTE: Cell<Option<PaletteId>> = const { Cell::new(None) };
    static SCALE: Cell<Option<f32>> = const { Cell::new(None) };
}

/// The palette and text scale to paint instead of the saved ones.
pub fn palette_override() -> Option<PaletteId> {
    PALETTE.with(Cell::get)
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
            for (page, path) in shots {
                let _ = cx.update(|window, cx| {
                    view.update(cx, |this, cx| this.capture_navigate(&page, window, cx))
                });
                cx.background_executor().timer(delay).await;
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

impl Desktop {
    fn capture_navigate(&mut self, page: &str, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let (page, category) = page.split_once(':').unwrap_or((page, ""));
        // First use is reviewed on its own; captured pages show the app behind it.
        if page != "onboarding" {
            self.onboarding.active = false;
        }
        match page {
            "home" => self.navigate(Page::New, cx),
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
                let course = self
                    .courses
                    .iter()
                    .find(|course| category.is_empty() || course.title.contains(category))
                    .cloned();
                if let Some(course) = course {
                    self.open_course(course, cx);
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
                    "palette" => this.open_palette_picker(false, window, cx),
                    "storage" => this.open_storage_dialog(window, cx),
                    "login" => this.open_account_dialog(window, cx),
                    "image" => {
                        if let Some(course) = newest {
                            this.open_course(course, cx);
                        }
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

    /// Library states reached by navigation, applied without saving them.
    fn capture_library_state(
        &mut self,
        states: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        // Each shot starts from all notes and no search.
        let root = self.library_root.clone();
        self.select_library_scope(root, crate::FolderFilter::All, cx);
        self.inputs[&crate::Field::Search].update(cx, |input, cx| input.set_value("", window, cx));
        for state in states.split(',').filter(|state| !state.is_empty()) {
            let (state, value) = state.split_once('~').unwrap_or((state, ""));
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
