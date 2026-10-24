//! Builds with `--features capture`: render named pages to PNG files and quit,
//! so native layout can be reviewed without screen-recording permission.
//!
//! `COURSE2MD_CAPTURE` lists `page=path.png` pairs separated by `;`. Pages are
//! `onboarding`, `home`, `library`, `tasks`, `settings:<category>` (appearance, generation,
//! services, storage, application) and `reader[:<title text>]` (the newest
//! note, or the first whose title contains the text). `COURSE2MD_CAPTURE_DELAY_MS` (default
//! 1800) is the wait after each navigation; `COURSE2MD_CAPTURE_PALETTE`
//! (a palette name such as `catppuccin_mocha`) and `COURSE2MD_CAPTURE_SCALE`
//! (1, 1.25, 1.5 or 2) override the saved appearance without changing it.
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
            "library" => self.navigate(Page::Library, cx),
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
}
