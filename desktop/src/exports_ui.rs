//! Exporting a note to a file: the save panel, the export itself and what it came to.
//! The reader and the library share one `NoteExports`, so an export started in one
//! shows in the other and only one runs at a time.
use crate::{Course, symbols, ui};
use desktop_reader_core::reader::{
    ExportFeedback, ExportState, export_file_name, export_version, exported_file_label,
};
use desktop_store::records::Records;
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit_material::{color::ColorRole::*, icons::Icon, prelude::*};
use std::{collections::BTreeMap, path::PathBuf, rc::Rc};

pub(crate) struct NoteExports {
    records: Entity<Records>,
    state: ExportState,
    /// The file each version was last exported to.
    exported: BTreeMap<PathBuf, PathBuf>,
    /// The note each export was of, for its retry.
    courses: BTreeMap<PathBuf, Rc<Course>>,
}

impl NoteExports {
    pub(crate) fn new(records: Entity<Records>) -> Self {
        Self {
            records,
            state: ExportState::default(),
            exported: BTreeMap::new(),
            courses: BTreeMap::new(),
        }
    }

    /// An export is running; another waits for it.
    pub(crate) fn exporting(&self) -> bool {
        self.state.pending.is_some()
    }

    /// The export running is of the version at `dir`.
    pub(crate) fn exporting_version(&self, dir: &std::path::Path) -> bool {
        self.state
            .pending
            .as_ref()
            .is_some_and(|(_, source)| source == dir)
    }

    /// The file the version at `dir` was last exported to.
    pub(crate) fn exported(&self, dir: &std::path::Path) -> Option<PathBuf> {
        self.exported.get(dir).cloned()
    }

    pub(crate) fn export(
        &mut self,
        course: Rc<Course>,
        format: course2md::config::OutputFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let version = course.dir.clone();
        let Some(generation) = self.state.begin(version.clone()) else {
            return;
        };
        let title = self.records.read(cx).library.course_display_title(&course);
        self.courses.insert(version.clone(), course);
        let suggested = export_file_name(&title, format);
        let receiver = cx.prompt_for_new_path(&self.records.read(cx).output(), Some(&suggested));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.state.cancel(generation, &version) {
                            cx.notify();
                        }
                    });
                    return;
                }
                _ => {
                    let _ = this.update(cx, |this, cx| {
                        if this.state.finish(
                            generation,
                            &version,
                            ExportFeedback {
                                title,
                                format,
                                result: Err(course2md::tr!("无法打开文件选择器，请重试。").into()),
                            },
                        ) {
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            if !this
                .update(cx, |this, _| this.state.is_pending(generation, &version))
                .unwrap_or(false)
            {
                return;
            }
            let target = path.clone();
            let source = version.clone();
            let exported =
                crate::spawn_blocking_io(move || export_version(&source, format, &target))
                    .await
                    .unwrap_or_else(|| {
                        Err(anyhow::anyhow!(course2md::tr!("导出时意外中断，请重试")))
                    });
            let _ = this.update(cx, |this, cx| {
                let result = exported
                    .map(|()| path.clone())
                    .map_err(|error| format!("{error:#}"));
                let success = result.is_ok();
                if !this.state.finish(
                    generation,
                    &version,
                    ExportFeedback {
                        title,
                        format,
                        result,
                    },
                ) {
                    return;
                }
                if success {
                    this.exported.insert(version, path);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What the last export of `course` came to, with its actions; none while there is none.
    pub(crate) fn feedback(&self, course: &Course, cx: &mut Context<Self>) -> Option<Div> {
        let feedback = self.state.feedback.get(&course.dir)?.clone();
        let id = format!("export-feedback:{}", course.dir.display());
        let child = |name: &str| SharedString::from(format!("{id}:{name}"));
        let source = course.dir.clone();
        let dismiss_label = course2md::tr!("关闭这份笔记的导出提示");
        let dismiss = ui::tip_button(
            child("dismiss"),
            symbols::close(),
            dismiss_label,
            |button| {
                button.on_click(cx.listener(move |this, _, _, cx| {
                    this.state.feedback.remove(&source);
                    cx.notify();
                }))
            },
        );
        Some(match feedback.result {
            Ok(path) => {
                let label = exported_file_label(&path);
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .items_center()
                    .child(Icon::from(symbols::check()).color(color(SUCCESS)))
                    .child(
                        accessible_text(child("text"), label.clone())
                            .aria_label(course2md::tr!("《{}》{}", feedback.title, label))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(TEXT_AUX)
                            .text_color(color(ON_SURFACE_VARIANT)),
                    )
                    .child(
                        material_button(child("reveal"), ButtonVariant::Text)
                            .icon(symbols::folder_open())
                            .child(course2md::tr!("打开导出文件夹"))
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                    .child(dismiss)
            }
            Err(error) => {
                let retry = course.dir.clone();
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(4.))
                    .child(
                        error_text(child("text"), course2md::tr!("导出未完成")).text_size(TEXT_AUX),
                    )
                    .child(
                        accessible_text(child("detail"), error)
                            .text_size(TEXT_AUX)
                            .text_color(color(ON_SURFACE_VARIANT))
                            .whitespace_normal(),
                    )
                    .child(
                        h_flex()
                            .gap(rem(8.))
                            .items_center()
                            .child(
                                material_button(child("retry"), ButtonVariant::Outlined)
                                    .tone(ButtonTone::Accent)
                                    .icon(symbols::refresh())
                                    .child(course2md::tr!("重试导出"))
                                    .disabled(self.exporting())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(course) = this.courses.get(&retry).cloned() {
                                            this.export(course, feedback.format, window, cx);
                                        }
                                    })),
                            )
                            .child(dismiss),
                    )
            }
        })
    }
}
