//! Native pages: stable navigation and actions surround independently scrolling content.
use super::*;
use crate::theme::*;
use gpui_component::{button::*, checkbox::Checkbox, progress::Progress, text::TextView};

fn muted(text: impl Into<SharedString>) -> Div {
    div().text_sm().text_color(rgb(MUTED)).child(text.into())
}
fn section(title: &str, description: &str) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .when(!description.is_empty(), |view| {
            view.child(muted(description.to_owned()))
        })
}
fn card() -> Div {
    v_flex()
        .w_full()
        .min_w_0()
        .p_4()
        .gap_4()
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(LINE))
        .rounded_md()
}

impl Desktop {
    pub fn format_choices(&self, cx: &mut Context<Self>) -> Div {
        h_flex().gap_3().flex_wrap().children(
            ["Markdown", "HTML", "JSON"]
                .into_iter()
                .enumerate()
                .map(|(index, label)| {
                    let checked = self.editing_options().formats[index];
                    choice(Button::new(("format", index)), checked)
                        .min_w(px(108.))
                        .accessibility_label(label)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Icon::new(IconName::Check).size_4().opacity(if checked {
                                    1.
                                } else {
                                    0.
                                }))
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.editing_options_mut().formats[index] = !checked;
                            cx.notify();
                        }))
                }),
        )
    }
    fn new_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut view = v_flex().gap_4().child(self.source_card(cx));
        if self.source_preview.is_some() {
            view = view.child(
                h_flex().gap_3().child(
                    Button::new("more-options")
                        .ghost()
                        .label("转换选项")
                        .icon(if self.show_options {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_options = !this.show_options;
                            cx.notify();
                        })),
                ),
            );
            if self.show_options {
                let options =
                    card()
                        .gap_4()
                        .child(h_flex().gap_2().flex_wrap().children(
                            SOURCES.iter().enumerate().map(|(index, (_, label))| {
                                choice(
                                    Button::new(("source-mode", index)).label(*label),
                                    self.task_options.source_mode == index,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.task_options.source_mode = index;
                                        cx.notify();
                                    },
                                ))
                            }),
                        ))
                        .when(self.task_options.source_mode != 1, |v| {
                            v.child(self.engine_panel(cx))
                        })
                        .child(self.format_choices(cx))
                        .child(
                            Checkbox::new("llm")
                                .label("AI 整理")
                                .checked(self.task_options.llm)
                                .on_click(cx.listener(|this, value, _, cx| {
                                    this.task_options.llm = *value;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new("resume")
                                .label("继续上次未完成的转换")
                                .checked(self.task_options.resume)
                                .on_click(cx.listener(|this, value, _, cx| {
                                    this.task_options.resume = *value;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Checkbox::new("keep-video")
                                .label("保留下载的视频")
                                .checked(self.task_options.keep_video)
                                .on_click(cx.listener(|this, value, _, cx| {
                                    this.task_options.keep_video = *value;
                                    cx.notify();
                                })),
                        );
                view = view.child(reveal(options, "options-reveal", cx));
            }
        }
        view.into_any_element()
    }
    fn active_work(&self) -> Vec<(&str, &activity::Activity)> {
        let preparing = self
            .progress
            .iter()
            .any(|(id, item)| id.starts_with("model") && !item.done);
        self.progress
            .iter()
            .filter(|(id, item)| {
                !item.done
                    && self.job.is_some()
                    && !(id.as_str() == "transcribe" && preparing && item.current == 0)
            })
            .map(|(id, item)| (id.as_str(), item))
            .collect()
    }
    fn task_summary(&self) -> String {
        if self.cancelling {
            return "正在取消…".into();
        }
        let active = self.active_work();
        match active.len() {
            0 => self.task_status.clone(),
            1 => format!(
                "{} · {}",
                activity::title(active[0].0),
                active[0].1.detail(active[0].0, true)
            ),
            count => format!(
                "{count} 项并行 · {}",
                active
                    .iter()
                    .map(|(id, _)| activity::title(id))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        }
    }
    fn task_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.progress.is_empty() && self.job.is_none() && self.logs.is_empty() {
            return self.empty_state("暂无任务", "", cx);
        }
        let active = self.active_work();
        let active_ids: Vec<_> = active.iter().map(|(id, _)| *id).collect();
        let mut content = v_flex().gap_4();
        if self.job.is_none() {
            content = content.child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.task_status.clone()),
            );
        }
        if self.job.is_some() {
            content = content.child(div().font_weight(FontWeight::MEDIUM).child(
                if active.len() > 1 {
                    format!("{} 项并行处理中", active.len())
                } else {
                    self.task_summary()
                },
            ));
        }
        for (index, (id, item)) in self.progress.iter().enumerate() {
            let running = active_ids.contains(&id.as_str());
            let waiting = self.job.is_some() && !item.done && !running;
            content = content.child(
                v_flex()
                    .gap_2()
                    .py_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().size(px(8.)).rounded_full().bg(rgb(if item.done {
                                SUCCESS
                            } else if running {
                                BLUE
                            } else {
                                MUTED
                            })))
                            .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(
                                if item.workers > 1 {
                                    format!(
                                        "{} · 最多 {} 路并行",
                                        activity::title(id),
                                        item.workers
                                    )
                                } else {
                                    activity::title(id)
                                },
                            ))
                            .child(muted(if waiting {
                                "等待模型就绪".into()
                            } else {
                                item.detail(id, self.job.is_some())
                            })),
                    )
                    .when(running, |row| {
                        row.child(
                            Progress::new(("activity", index))
                                .accessibility_label(activity::title(id))
                                .loading(item.fraction().is_none())
                                .value(item.fraction().unwrap_or(0.) * 100.)
                                .h(px(4.)),
                        )
                    }),
            );
        }
        if let Some(done) = self.completed.clone() {
            let course = Course::from_completed(&done);
            content = content.child(
                h_flex()
                    .gap_4()
                    .child(div().flex_1().child(done.title))
                    .child(
                        Button::new("read-result")
                            .primary()
                            .label("阅读笔记")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_course(course.clone(), cx)
                            })),
                    ),
            );
        }
        content =
            content.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("toggle-logs")
                            .ghost()
                            .label(if self.show_logs {
                                "收起日志"
                            } else {
                                "日志"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_logs = !this.show_logs;
                                cx.notify();
                            })),
                    )
                    .when(self.show_logs, |row| {
                        row.child(Button::new("copy-logs").ghost().label("复制").on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    this.logs.iter().cloned().collect::<Vec<_>>().join("\n"),
                                ))
                            }),
                        ))
                    }),
            );
        if self.show_logs {
            content = content.child(
                card().child(
                    TextView::markdown(
                        "logs",
                        self.logs
                            .iter()
                            .map(|line| format!("    {line}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .selectable(true)
                    .text_xs(),
                ),
            );
        }
        content.into_any_element()
    }
    fn empty_state(&self, title: &str, description: &str, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .items_center()
            .justify_center()
            .py_16()
            .gap_4()
            .child(
                div()
                    .size(px(56.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(rgb(TINT))
                    .text_color(rgb(BLUE))
                    .child(Icon::new(IconName::BookOpen).size_6()),
            )
            .child(section(title, description).items_center())
            .child(
                Button::new("empty-new")
                    .primary()
                    .label("添加课程")
                    .on_click(cx.listener(|this, _, window, cx| this.begin_add(window, cx))),
            )
            .into_any_element()
    }
    fn library_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.value(Field::Search, cx).to_lowercase();
        let courses = self
            .courses
            .iter()
            .filter(|course| {
                course.title.to_lowercase().contains(&query)
                    && self.folder_filter.is_none_or(|id| {
                        self.library
                            .folder(&self.library_root, &course.dir)
                            .unwrap_or(0)
                            == id
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut view = v_flex().gap_4().child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&self.inputs[&Field::Search]).aria_label("搜索课程")),
                )
                .child(
                    Button::new("refresh-library")
                        .ghost()
                        .icon(icons::refresh())
                        .label("刷新")
                        .disabled(self.loading)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                ),
        );
        if let Some(id) = self.folder_filter.filter(|id| *id != 0) {
            view = view.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("rename-folder")
                            .ghost()
                            .label("重命名")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.begin_folder(Some(id), window, cx)
                            })),
                    )
                    .child(
                        Button::new("remove-folder")
                            .ghost()
                            .label("删除文件夹")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_folder = Some(id);
                                this.folder_editor = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        if self.loading {
            view = view.child(muted("正在读取课程…"));
        } else if courses.is_empty() {
            view = view.child(if query.is_empty() {
                self.empty_state(
                    if self.folder_filter.is_some() {
                        "这个文件夹还没有课程"
                    } else {
                        "暂无课程"
                    },
                    "",
                    cx,
                )
            } else {
                v_flex()
                    .py_12()
                    .gap_3()
                    .items_center()
                    .child("没有匹配的课程")
                    .child(
                        Button::new("clear-search")
                            .label("清空搜索")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.inputs[&Field::Search]
                                    .update(cx, |state, cx| state.set_value("", window, cx));
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            });
        }
        view.children(courses.chunks(2).enumerate().map(|(row, courses)| {
            h_flex()
                .gap_4()
                .items_start()
                .children(courses.iter().enumerate().map(|(col, course)| {
                    let index = row * 2 + col;
                    let open = course.clone();
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .rounded_md()
                        .overflow_hidden()
                        .bg(rgb(SURFACE))
                        .border_1()
                        .border_color(rgb(LINE))
                        .child(
                            Button::new(("read-course", index))
                                .ghost()
                                .w_full()
                                .h_auto()
                                .aspect_ratio(16. / 9.)
                                .p_0()
                                .accessibility_label(format!("阅读 {}", course.title))
                                .child(
                                    div()
                                        .size_full()
                                        .bg(rgb(SIDEBAR))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .when_some(course.thumbnail.clone(), |view, path| {
                                            view.child(
                                                img(path).size_full().object_fit(ObjectFit::Cover),
                                            )
                                        })
                                        .when(course.thumbnail.is_none(), |view| {
                                            view.child(Icon::new(IconName::BookOpen).size_6())
                                        }),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_course(open.clone(), cx)
                                })),
                        )
                        .child(
                            v_flex()
                                .p_4()
                                .gap_3()
                                .child(
                                    Button::new(("read-title", index))
                                        .ghost()
                                        .w_full()
                                        .h_auto()
                                        .min_h(px(28.))
                                        .p_0()
                                        .accessibility_label(format!("阅读 {}", course.title))
                                        .child(
                                            div()
                                                .w_full()
                                                .line_clamp(2)
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(course.title.clone()),
                                        )
                                        .on_click({
                                            let course = course.clone();
                                            cx.listener(move |this, _, _, cx| {
                                                this.open_course(course.clone(), cx)
                                            })
                                        }),
                                )
                                .child(muted(format!(
                                    "{} 张截图 · {} 段讲解",
                                    course.slides, course.segments
                                )))
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .child(self.folder_picker(
                                            Some(course.dir.clone()),
                                            index + 1,
                                            cx,
                                        ))
                                        .child(
                                            Button::new(("course-files", index))
                                                .ghost()
                                                .icon(IconName::FolderOpen)
                                                .accessibility_label("打开导出文件")
                                                .on_click({
                                                    let dir = course.dir.clone();
                                                    move |_, _, cx| cx.reveal_path(&dir)
                                                }),
                                        ),
                                ),
                        )
                }))
                .when(courses.len() == 1, |view| view.child(div().flex_1()))
        }))
        .into_any_element()
    }
    fn result_page(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = self.preview.as_ref() else {
            return muted("正在打开笔记…").into_any_element();
        };
        match self.result_tab {
            0 => {
                card()
                    .p_7()
                    .gap_4()
                    .children(
                        preview.blocks.iter().cloned().enumerate().map(
                            |(index, block)| match block {
                                backend::PreviewBlock::Markdown(text) => {
                                    TextView::markdown(("preview", index), text)
                                        .selectable(true)
                                        .into_any_element()
                                }
                                backend::PreviewBlock::Image(path) => img(path)
                                    .w_full()
                                    .max_w_full()
                                    .h(px(330.))
                                    .object_fit(ObjectFit::Contain)
                                    .border_1()
                                    .border_color(rgba(0x00000019))
                                    .rounded_lg()
                                    .into_any_element(),
                            },
                        ),
                    )
                    .into_any_element()
            }
            1 => v_flex()
                .gap_4()
                .when(preview.frames.is_empty(), |view| {
                    view.child(muted("这份课程没有截图。"))
                })
                .children(preview.frames.chunks(2).enumerate().map(|(row, frames)| {
                    h_flex()
                        .gap_4()
                        .items_start()
                        .children(frames.iter().enumerate().map(|(column, path)| {
                            let path = path.clone();
                            let open = path.clone();
                            let number = row * 2 + column + 1;
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    Button::new(("frame", number))
                                        .p_0()
                                        .h(px(210.))
                                        .w_full()
                                        .overflow_hidden()
                                        .accessibility_label(format!("打开第 {number} 张截图"))
                                        .child(
                                            img(path)
                                                .w_full()
                                                .h(px(200.))
                                                .object_fit(ObjectFit::Contain),
                                        )
                                        .on_click(move |_, _, cx| cx.open_with_system(&open)),
                                )
                                .child(muted(format!("画面 {number:02}")))
                        }))
                        .when(frames.len() == 1, |view| view.child(div().flex_1()))
                }))
                .into_any_element(),
            _ => v_flex()
                .gap_3()
                .child(muted("选择文件，在系统默认应用中打开。"))
                .children(
                    [
                        ("course.md", "Markdown 笔记", "适合编辑、归档与分享"),
                        ("course.html", "网页笔记", "在浏览器中阅读图文内容"),
                        ("structured.json", "结构化数据", "保留时间线，方便后续处理"),
                    ]
                    .iter()
                    .cloned()
                    .filter(|(name, _, _)| preview.outputs.iter().any(|output| output == name))
                    .map(|(name, title, description)| {
                        let path = preview.course.dir.join(name);
                        card().p_4().child(
                            h_flex()
                                .gap_4()
                                .child(Icon::new(IconName::File).text_color(rgb(BLUE)))
                                .child(section(title, description).flex_1())
                                .child(
                                    Button::new(name)
                                        .label("打开")
                                        .on_click(move |_, _, cx| cx.open_with_system(&path)),
                                ),
                        )
                    }),
                )
                .into_any_element(),
        }
    }
    fn tabs(&self, cx: &mut Context<Self>) -> Div {
        let tabs: Vec<&str> = if self.page == Page::Settings {
            vec!["通用", "语音识别", "AI 整理", "运行环境", "关于"]
        } else {
            vec!["文稿", "截图", "文件"]
        };
        let selected = if self.page == Page::Settings {
            self.settings_tab
        } else {
            self.result_tab
        };
        h_flex()
            .gap_2()
            .children(tabs.into_iter().enumerate().map(|(index, label)| {
                choice(Button::new(("tab", index)).label(label), selected == index)
                    .h(px(36.))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.blur_fields(
                            &[
                                Field::Output,
                                Field::AsrUrl,
                                Field::AsrModel,
                                Field::AsrKey,
                                Field::LlmUrl,
                                Field::LlmModel,
                                Field::LlmKey,
                            ],
                            window,
                            cx,
                        );
                        if this.page == Page::Settings {
                            if this.settings_tab != index {
                                this.settings_transition = this.settings_transition.wrapping_add(1);
                            }
                            this.settings_tab = index;
                        } else {
                            this.result_tab = index;
                        }
                        this.scrolls[this.page as usize].set_offset(point(px(0.), px(0.)));
                        cx.notify();
                    }))
            }))
    }
    fn page_title(&self) -> String {
        match self.page {
            Page::New => "添加课程".into(),
            Page::Library => self
                .folder_filter
                .map(|id| {
                    if id == 0 {
                        "未分类".into()
                    } else {
                        self.library.folders.get(&id).cloned().unwrap_or_default()
                    }
                })
                .unwrap_or_else(|| "全部课程".into()),
            Page::Task => "任务".into(),
            Page::Settings => "设置".into(),
            Page::Result => self
                .preview
                .as_ref()
                .map(|p| p.course.title.clone())
                .unwrap_or_else(|| "笔记".into()),
        }
    }
    fn page_header(&self, cx: &mut Context<Self>) -> Div {
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .h(px(48.))
            .when(matches!(self.page, Page::Result | Page::New), |row| {
                row.child(
                    Button::new("back")
                        .ghost()
                        .icon(IconName::ArrowLeft)
                        .accessibility_label("返回")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigate(
                                if this.page == Page::New {
                                    Page::Library
                                } else {
                                    this.result_origin
                                },
                                cx,
                            )
                        })),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.page_title()),
            );
        if self.page == Page::New && (self.source_preview.is_some() || self.job.is_some()) {
            row = row.child(
                Button::new("start")
                    .primary()
                    .h(px(32.))
                    .label(if self.job.is_some() {
                        "查看任务"
                    } else {
                        "生成笔记"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.start(Kind::Convert, cx))),
            );
        } else if self.page == Page::Task && self.job.is_some() {
            row = row.child(
                Button::new("cancel")
                    .h(px(32.))
                    .label(if self.cancelling {
                        "正在取消…"
                    } else {
                        "取消任务"
                    })
                    .disabled(self.cancelling)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(job) = &this.job {
                            job.cancel();
                            this.cancelling = true;
                            cx.notify();
                        }
                    })),
            );
        } else if self.page == Page::Task
            && self.kind == Kind::Convert
            && self.completed.is_none()
            && !self.logs.is_empty()
        {
            row = row.child(
                Button::new("retry")
                    .label("调整并重试")
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::New, cx))),
            );
        } else if self.page == Page::Result {
            row = row
                .child(
                    Button::new("result-copy")
                        .ghost()
                        .label("复制文稿")
                        .disabled(self.preview.as_ref().is_none_or(|p| !p.has_markdown))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(p) = &this.preview {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    p.markdown.clone(),
                                ));
                            }
                        })),
                )
                .child(
                    Button::new("result-folder")
                        .ghost()
                        .icon(IconName::FolderOpen)
                        .accessibility_label("打开结果文件夹")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(p) = &this.preview {
                                cx.reveal_path(&p.course.dir);
                            }
                        })),
                );
        }
        v_flex()
            .gap_2()
            .child(row)
            .when(matches!(self.page, Page::Settings | Page::Result), |v| {
                v.child(self.tabs(cx)).pb_4()
            })
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.setup_open && self.page == Page::Settings && self.settings_tab == 4 {
            self.blur_fields(
                &[
                    Field::Output,
                    Field::AsrUrl,
                    Field::AsrModel,
                    Field::AsrKey,
                    Field::LlmUrl,
                    Field::LlmModel,
                    Field::LlmKey,
                ],
                window,
                cx,
            );
        }
        // Compare drafts once per render; only actual changes restart the debounce.
        let draft = self.edited_settings(cx);
        if !self.setup_open && draft != self.settings_snapshot {
            self.settings_snapshot = draft;
            self.settings_deadline = Some(Instant::now() + Duration::from_millis(700));
            self.settings_status = "未保存".into();
        }
        let content = match self.page {
            Page::New => self.new_page(cx),
            Page::Task => self.task_page(cx),
            Page::Library => self.library_page(cx),
            Page::Settings => self.settings_page(window, cx),
            Page::Result => self.result_page(cx),
        };
        let settings_problem = self.config_error
            || self.settings_status.starts_with("未保存：")
            || self.settings_status.starts_with("保存失败");
        let sidebar = v_flex()
            .w(px(200.))
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_4()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .child(
                v_flex().gap_1().children(
                    [
                        (Page::Library, "全部课程", Icon::new(IconName::BookOpen)),
                        (Page::New, "添加课程", Icon::new(IconName::Plus)),
                        (Page::Task, "任务", icons::task()),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (page, label, icon))| {
                        navigation(
                            Button::new(("nav", index)),
                            self.page == page
                                && (page != Page::Library || self.folder_filter.is_none()),
                        )
                        .w_full()
                        .h(px(32.))
                        .justify_start()
                        .accessibility_label(label)
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .child(icon.size(px(20.)))
                                .child(label),
                        )
                        .selected(
                            self.page == page
                                && (page != Page::Library || self.folder_filter.is_none()),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if page == Page::New {
                                    this.begin_add(window, cx);
                                } else {
                                    if page == Page::Library {
                                        this.folder_filter = None;
                                    }
                                    this.navigate(page, cx);
                                }
                            },
                        ))
                    }),
                ),
            )
            .child(
                div()
                    .id("folder-scroll")
                    .min_h_0()
                    .flex_1()
                    .overflow_y_scroll()
                    .child(self.folder_sidebar(cx)),
            )
            .child(
                navigation(Button::new("nav-settings"), self.page == Page::Settings)
                    .w_full()
                    .h(px(32.))
                    .justify_start()
                    .accessibility_label(if settings_problem {
                        "设置，未保存"
                    } else {
                        "设置"
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(Icon::new(IconName::Settings).size(px(20.)))
                            .child(if settings_problem {
                                "设置 · 未保存"
                            } else {
                                "设置"
                            }),
                    )
                    .selected(self.page == Page::Settings)
                    .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Settings, cx))),
            );
        let body = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .px(px(24.))
                    .flex_shrink_0()
                    .child(self.page_header(cx)),
            )
            .when(
                self.folder_editor.is_some() || self.delete_folder.is_some(),
                |v| v.child(div().px(px(24.)).pb_4().child(self.folder_editor_view(cx))),
            )
            .when_some(self.message.clone(), |v, message| {
                v.child(
                    div().px(px(24.)).pb_3().child(
                        h_flex()
                            .gap_3()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(TINT))
                            .child(div().flex_1().child(message))
                            .child(
                                Button::new("dismiss-message")
                                    .ghost()
                                    .icon(IconName::Close)
                                    .accessibility_label("关闭提示")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.message = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
                )
            })
            .when(
                self.page == Page::Settings
                    && self.settings_tab != 4
                    && self.invalid_setting(cx).is_none()
                    && (self.settings_status.starts_with("未保存：")
                        || self.settings_status.starts_with("保存失败")
                        || self.config_error),
                |v| {
                    v.child(
                        div()
                            .px(px(24.))
                            .pb_3()
                            .text_color(rgb(0xb64032))
                            .child(self.settings_status.clone()),
                    )
                },
            )
            .child(
                div()
                    .id(("page-scroll", self.page as usize))
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scrolls[self.page as usize])
                    .px(px(24.))
                    .pb_6()
                    .child(content),
            );
        v_flex()
            .size_full()
            .bg(rgb(CANVAS))
            .text_color(rgb(INK))
            .text_size(px(14.))
            .when(!self.system_titlebar, |v| {
                v.child(
                    TitleBar::new().bg(rgb(SIDEBAR)).child(
                        h_flex().w_full().gap_3().child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(MUTED))
                                .child("course2md"),
                        ),
                    ),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(sidebar)
                    .child(body),
            )
            .when(self.job.is_some(), |v| {
                v.child(
                    h_flex()
                        .h(px(36.))
                        .flex_shrink_0()
                        .px_4()
                        .gap_3()
                        .bg(rgb(SURFACE))
                        .border_t_1()
                        .border_color(rgb(LINE))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .child(self.task_summary()),
                        )
                        .when(self.page != Page::Task, |v| {
                            v.child(Button::new("status-task").label("查看任务").on_click(
                                cx.listener(|this, _, _, cx| this.navigate(Page::Task, cx)),
                            ))
                        }),
                )
            })
            .children(Root::render_dialog_layer(window, cx))
    }
}
