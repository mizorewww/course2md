use super::*;
use crate::theme::*;
use gpui_component::{
    button::*,
    menu::{DropdownMenu, PopupMenuItem},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

impl Desktop {
    pub fn begin_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_none()
            && self.completed_source.as_deref() == Some(self.value(Field::Source, cx).as_str())
        {
            self.invalidate_source();
            self.online = true;
            self.show_options = false;
            self.last_source_input.clear();
            self.inputs[&Field::Source].update(cx, |state, cx| state.set_value("", window, cx));
            self.scrolls[Page::New as usize].set_offset(point(px(0.), px(0.)));
        }
        self.navigate(Page::New, cx);
        if self.online {
            self.inputs[&Field::Source].update(cx, |state, cx| state.focus(window, cx));
        }
    }

    pub fn invalidate_source(&mut self) {
        if let Some(cancel) = self.preview_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.preview_generation += 1;
        self.source_preview = None;
        self.preview_error = None;
    }
    pub fn inspect_source(&mut self, cx: &mut Context<Self>) {
        self.invalidate_source();
        let input = self.value(Field::Source, cx);
        self.last_source_input = input.clone();
        if input.is_empty() {
            self.preview_error = Some("请先粘贴视频链接或选择视频".into());
            cx.notify();
            return;
        }
        let generation = self.preview_generation;
        self.preview_workers += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.preview_cancel = Some(cancel.clone());
        let online = self.online;
        let task = cx
            .background_executor()
            .spawn(async move { source::inspect(input, online, cancel) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.preview_workers -= 1;
                if this.closing && this.preview_workers == 0 && this.job.is_none() {
                    cx.quit();
                    return;
                }
                if this.preview_generation != generation {
                    return;
                }
                this.preview_cancel = None;
                match result {
                    Ok(source) => this.source_preview = Some(source),
                    Err(e) => this.preview_error = Some(format!("无法预览课程：{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn folder_name(&self, id: Option<u64>) -> String {
        id.and_then(|id| self.library.folders.get(&id))
            .cloned()
            .unwrap_or_else(|| "未分类".into())
    }
    pub fn begin_folder(&mut self, id: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        let name = id
            .and_then(|id| self.library.folders.get(&id))
            .cloned()
            .unwrap_or_default();
        self.folder_editor = Some(id);
        self.delete_folder = None;
        self.inputs[&Field::FolderName].update(cx, |state, cx| {
            state.set_value(name, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }
    pub fn save_folder(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.folder_editor else {
            return;
        };
        let name = self.value(Field::FolderName, cx);
        let mut saved = None;
        match organize::Library::edit(&self.library_root, |library| {
            saved = Some(library.rename(id, &name)?);
            Ok(())
        }) {
            Ok(library) => {
                self.library = library;
                self.folder_editor = None;
                self.message = None;
                if self.page == Page::New {
                    self.target_folder = saved;
                } else {
                    self.folder_filter = saved;
                    self.page = Page::Library;
                }
            }
            Err(e) => self.message = Some(format!("无法保存文件夹：{e:#}")),
        }
        cx.notify();
    }
    pub fn folder_editor_view(&self, cx: &mut Context<Self>) -> Div {
        let mut view = v_flex().gap_3();
        if let Some(id) = self.folder_editor {
            view = view
                .p_4()
                .rounded_lg()
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(LINE))
                .child(if id.is_some() {
                    "重命名文件夹"
                } else {
                    "新建文件夹"
                })
                .child(Input::new(&self.inputs[&Field::FolderName]).aria_label("文件夹名称"))
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("cancel-folder").ghost().label("取消").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.folder_editor = None;
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("save-folder")
                                .primary()
                                .label("保存文件夹")
                                .on_click(cx.listener(|this, _, _, cx| this.save_folder(cx))),
                        ),
                );
        }
        if let Some(id) = self.delete_folder {
            view = view
                .p_4()
                .rounded_lg()
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(LINE))
                .child(format!("删除「{}」文件夹？", self.folder_name(Some(id))))
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("其中的课程会回到未分类，笔记和原视频都会保留。"),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("keep-folder").label("保留文件夹").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.delete_folder = None;
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("delete-folder")
                                .label("删除文件夹，保留课程")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    match organize::Library::edit(&this.library_root, |library| {
                                        library.remove(id);
                                        Ok(())
                                    }) {
                                        Ok(library) => {
                                            this.library = library;
                                            this.delete_folder = None;
                                            if this.folder_filter == Some(id) {
                                                this.folder_filter = Some(0);
                                            }
                                            if this.target_folder == Some(id) {
                                                this.target_folder = None;
                                            }
                                        }
                                        Err(e) => {
                                            this.message = Some(format!("无法删除文件夹：{e:#}"))
                                        }
                                    }
                                    cx.notify();
                                })),
                        ),
                );
        }
        view
    }
    pub fn folder_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let mut entries = vec![(Some(0), "未分类".to_owned())];
        entries.extend(
            self.library
                .folders
                .iter()
                .map(|(id, name)| (Some(*id), name.clone())),
        );
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .px_2()
                    .child(div().text_xs().text_color(rgb(MUTED)).child("我的文件夹"))
                    .child(
                        Button::new("new-folder")
                            .ghost()
                            .icon(IconName::Plus)
                            .accessibility_label("新建文件夹")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_folder(None, window, cx)
                            })),
                    ),
            )
            .children(entries.into_iter().map(|(id, name)| {
                let count = self
                    .courses
                    .iter()
                    .filter(|course| {
                        self.library
                            .folder(&self.library_root, &course.dir)
                            .unwrap_or(0)
                            == id.unwrap_or(0)
                    })
                    .count();
                Button::new(("folder-nav", id.unwrap_or(0) as usize))
                    .ghost()
                    .w_full()
                    .h(px(38.))
                    .accessibility_label(name.clone())
                    .selected(self.page == Page::Library && self.folder_filter == id)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(Icon::new(IconName::Folder))
                            .child(div().flex_1().min_w_0().text_ellipsis().child(name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(count.to_string()),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.folder_filter = id;
                        this.scrolls[Page::Library as usize].set_offset(point(px(0.), px(0.)));
                        this.navigate(Page::Library, cx);
                    }))
            }))
    }
    pub fn folder_picker(
        &self,
        course: Option<PathBuf>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let folder = course
            .as_ref()
            .and_then(|path| self.library.folder(&self.library_root, path))
            .or_else(|| {
                if course.is_none() {
                    self.target_folder
                } else {
                    None
                }
            });
        let label = self.folder_name(folder);
        let folders = self.library.folders.clone();
        let entity = cx.entity().downgrade();
        Button::new(("folder-picker", index))
            .ghost()
            .icon(IconName::Folder)
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                let mut entries = vec![(None, "未分类".to_owned())];
                entries.extend(folders.iter().map(|(id, name)| (Some(*id), name.clone())));
                entries.into_iter().fold(menu, |menu, (id, name)| {
                    let entity = entity.clone();
                    let course = course.clone();
                    menu.item(PopupMenuItem::new(name).checked(id == folder).on_click(
                        move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                if let Some(path) = &course {
                                    match organize::Library::edit(&this.library_root, |library| {
                                        library.assign(&this.library_root, path, id)
                                    }) {
                                        Ok(library) => {
                                            this.library = library;
                                            this.message = Some(format!(
                                                "课程已移至「{}」",
                                                this.folder_name(id)
                                            ));
                                        }
                                        Err(e) => {
                                            this.message = Some(format!("无法移动课程：{e:#}"))
                                        }
                                    }
                                } else {
                                    this.target_folder = id;
                                }
                                cx.notify();
                            });
                        },
                    ))
                })
            })
    }
    pub fn source_card(&self, cx: &mut Context<Self>) -> Div {
        let mut view = v_flex().gap_5().child(
            h_flex()
                .gap_2()
                .children([(true, "在线链接"), (false, "本地视频")].into_iter().map(
                    |(online, label)| {
                        Button::new(label)
                            .label(label)
                            .selected(self.online == online)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.online != online {
                                    this.online = online;
                                    this.invalidate_source();
                                    this.inputs[&Field::Source]
                                        .update(cx, |state, cx| state.set_value("", window, cx));
                                }
                                cx.notify();
                            }))
                    },
                )),
        );
        if self.online {
            view = view.child(
                v_flex()
                    .gap_2()
                    .child("视频链接")
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.inputs[&Field::Source])
                                        .aria_label("视频链接")
                                        .h(px(44.)),
                                ),
                            )
                            .child(
                                Button::new("preview-source")
                                    .primary()
                                    .h(px(44.))
                                    .label(if self.preview_cancel.is_some() {
                                        "正在读取…"
                                    } else {
                                        "预览课程"
                                    })
                                    .disabled(
                                        self.preview_cancel.is_some()
                                            || self.value(Field::Source, cx).is_empty(),
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.inspect_source(cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("支持 YouTube、Bilibili。先确认内容，再生成笔记。"),
                    ),
            );
        } else {
            view = view.child(
                v_flex()
                    .gap_3()
                    .p_6()
                    .bg(rgb(SURFACE))
                    .rounded_xl()
                    .border_1()
                    .border_color(rgb(LINE))
                    .items_center()
                    .child(Icon::new(IconName::FolderOpen).size_6())
                    .child("从录屏、讲座或本地课程开始")
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("同名 SRT / VTT 字幕会自动读取"),
                    )
                    .child(
                        Button::new("choose-video")
                            .primary()
                            .label("选择视频")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick(false, window, cx)),
                            ),
                    )
                    .when(!self.value(Field::Source, cx).is_empty(), |view| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .text_ellipsis()
                                .child(self.value(Field::Source, cx)),
                        )
                    }),
            );
        }
        if self.preview_cancel.is_some() {
            view = view.child(
                h_flex()
                    .gap_3()
                    .child(div().flex_1().child("正在读取标题、作者和封面…"))
                    .child(
                        Button::new("cancel-preview")
                            .ghost()
                            .label("取消预览")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.invalidate_source();
                                cx.notify();
                            })),
                    ),
            );
        }
        if let Some(error) = &self.preview_error {
            view = view.child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffefeb))
                    .child(error.clone())
                    .child(
                        Button::new("retry-preview")
                            .label("重新预览")
                            .on_click(cx.listener(|this, _, _, cx| this.inspect_source(cx))),
                    ),
            );
        }
        if let Some(source) = &self.source_preview {
            view = view
                .child(
                    v_flex()
                        .rounded_xl()
                        .overflow_hidden()
                        .bg(rgb(SURFACE))
                        .border_1()
                        .border_color(rgb(LINE))
                        .child(
                            div()
                                .h(px(250.))
                                .w_full()
                                .bg(rgb(SIDEBAR))
                                .flex()
                                .items_center()
                                .justify_center()
                                .when_some(source.cover.clone(), |view, path| {
                                    view.child(img(path).size_full().object_fit(ObjectFit::Contain))
                                })
                                .when(source.cover.is_none(), |view| view.child("暂无封面")),
                        )
                        .child(
                            v_flex()
                                .p_5()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xl()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(source.title.clone()),
                                )
                                .child(div().text_color(rgb(MUTED)).child(source.detail()))
                                .when_some(source.cover_error.clone(), |view, error| {
                                    view.child(div().text_xs().text_color(rgb(MUTED)).child(error))
                                }),
                        ),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child("归入文件夹")
                        .child(self.folder_picker(None, 0, cx))
                        .child(
                            Button::new("add-destination")
                                .ghost()
                                .icon(IconName::Plus)
                                .label("新建")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.begin_folder(None, window, cx)
                                })),
                        ),
                );
        }
        view
    }
}
