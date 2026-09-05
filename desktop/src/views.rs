//! Native application pages and their presentation.
use super::*;
use gpui_component::{button::*, checkbox::Checkbox, progress::Progress, text::TextView};

impl Desktop {
    fn new_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_6()
            .child(heading(
                "把一节课，变成一份好笔记",
                "保留关键画面，让讲解与幻灯片一起留下。",
            ))
            .child(
                card()
                    .gap_5()
                    .child(self.input(Field::Source, "课程来源"))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("choose-video")
                                    .label("选择本地视频")
                                    .icon(IconName::FolderOpen)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.pick(false, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x6b7280))
                                    .child("支持本地视频与同名 SRT / VTT 字幕"),
                            ),
                    )
                    .child(self.input(Field::Output, "保存到"))
                    .child(
                        Button::new("choose-output").label("更改保存目录").on_click(
                            cx.listener(|this, _, window, cx| this.pick(true, window, cx)),
                        ),
                    ),
            )
            .child(
                card()
                    .gap_5()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("转换方式"))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .children(SOURCES.iter().enumerate().map(|(index, (_, label))| {
                                Button::new(("source-mode", index))
                                    .label(*label)
                                    .selected(self.source_mode == index)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.source_mode = index;
                                        cx.notify();
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x6b7280))
                            .child("字幕优先：有字幕就直接整理，没有字幕时再进行语音识别。"),
                    )
                    .child(div().text_sm().child("识别后端"))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .children(PROVIDERS.iter().enumerate().map(|(index, (_, label))| {
                                Button::new(("provider", index))
                                    .label(*label)
                                    .selected(self.provider == index)
                                    .disabled(self.source_mode == 1)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.provider = index;
                                        cx.notify();
                                    }))
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_5()
                            .child(
                                Checkbox::new("llm")
                                    .label("AI 校对与整理")
                                    .checked(self.llm)
                                    .on_click(cx.listener(|this, checked, _, cx| {
                                        this.llm = *checked;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("resume")
                                    .label("断点续跑")
                                    .checked(self.resume)
                                    .on_click(cx.listener(|this, checked, _, cx| {
                                        this.resume = *checked;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("keep-video")
                                    .label("保留下载的视频")
                                    .checked(self.keep_video)
                                    .on_click(cx.listener(|this, checked, _, cx| {
                                        this.keep_video = *checked;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(h_flex().gap_5().children(
                        ["Markdown", "HTML", "JSON"].into_iter().enumerate().map(
                            |(index, label)| {
                                Checkbox::new(("format", index))
                                    .label(label)
                                    .checked(self.formats[index])
                                    .on_click(cx.listener(move |this, checked, _, cx| {
                                        this.formats[index] = *checked;
                                        cx.notify();
                                    }))
                            },
                        ),
                    )),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x6b7280))
                            .child("原始本地视频会保留在原处。"),
                    )
                    .child(
                        Button::new("start")
                            .primary()
                            .icon(IconName::ArrowRight)
                            .label(if self.job.is_some() {
                                "查看运行中的任务"
                            } else {
                                "生成课程笔记"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.start(Kind::Convert, cx))),
                    ),
            )
            .into_any_element()
    }
    fn task_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut content = v_flex()
            .gap_5()
            .child(heading("任务进度", &self.task_status));
        if self.kind == Kind::Convert {
            content = content.child(card().gap_3().children(STAGES.iter().map(|(id, name)| {
                let status = self.stages.get(*id).map(String::as_str);
                let progress = self.progress.get(*id).copied();
                let label = match status {
                    Some("done") => "已完成",
                    Some("start") if self.job.is_some() => "进行中",
                    Some("start") => "已停止",
                    _ if self.completed.is_some() => "已跳过",
                    _ => "等待执行",
                };
                v_flex()
                    .gap_2()
                    .child(
                        h_flex().justify_between().child(*name).child(
                            div()
                                .text_sm()
                                .text_color(if status == Some("done") {
                                    rgb(0x15803d)
                                } else {
                                    rgb(0x6b7280)
                                })
                                .child(label),
                        ),
                    )
                    .when_some(progress, |view, (current, total)| {
                        view.child(
                            Progress::new(SharedString::from(format!("progress-{id}"))).value(
                                if status == Some("done") {
                                    100.
                                } else if total > 0 {
                                    (current as f32 / total as f32 * 100.).min(100.)
                                } else {
                                    0.
                                },
                            ),
                        )
                    })
            })));
        }
        if let Some(done) = self.completed.clone() {
            let course = Course {
                dir: done.out_dir.clone(),
                title: done.title.clone(),
                modified: std::time::SystemTime::now(),
            };
            content = content.child(
                card()
                    .gap_4()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(done.title),
                    )
                    .child(format!(
                        "{} 张画面 · {} 段讲解 · {:.1} 秒",
                        done.slides, done.segments, done.elapsed_secs
                    ))
                    .child(format!("已生成：{}", done.outputs.join("、")))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("read-result")
                                    .primary()
                                    .label("阅读笔记")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_course(course.clone(), cx)
                                    })),
                            )
                            .child(
                                Button::new("reveal-result")
                                    .label("打开文件夹")
                                    .on_click(move |_, _, cx| cx.reveal_path(&done.out_dir)),
                            ),
                    ),
            );
        }
        content = content.child(
            h_flex()
                .gap_2()
                .when(self.job.is_some(), |view| {
                    view.child(
                        Button::new("cancel")
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
                    )
                })
                .when(self.job.is_none(), |view| {
                    view.child(Button::new("retry").label("修改设置 / 新建任务").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.page = Page::New;
                            cx.notify();
                        }),
                    ))
                })
                .child(
                    Button::new("copy-logs")
                        .label("复制日志")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                this.logs.iter().cloned().collect::<Vec<_>>().join("\n"),
                            ));
                        })),
                ),
        );
        content
            .child(
                card()
                    .gap_3()
                    .child(div().font_weight(FontWeight::MEDIUM).child("运行日志"))
                    .child(
                        div()
                            .id("logs")
                            .h(px(260.))
                            .overflow_y_scroll()
                            .font_family("monospace")
                            .text_xs()
                            .children(self.logs.iter().enumerate().map(|(index, line)| {
                                div().id(("log", index)).py_1().child(line.clone())
                            })),
                    ),
            )
            .into_any_element()
    }
    fn library_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.value(Field::Search, cx).to_lowercase();
        let courses: Vec<_> = self
            .courses
            .iter()
            .filter(|course| course.title.to_lowercase().contains(&query))
            .cloned()
            .collect();
        v_flex()
            .gap_5()
            .child(heading("课程库", "已经整理好的知识，随时回来继续阅读。"))
            .child(
                h_flex()
                    .gap_3()
                    .child(Input::new(&self.inputs[&Field::Search]).aria_label("搜索课程"))
                    .child(
                        Button::new("refresh-library")
                            .label("刷新")
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_library(cx))),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x6b7280))
                    .child(self.output(cx).display().to_string()),
            )
            .when(courses.is_empty(), |view| {
                view.child(
                    card()
                        .py_12()
                        .items_center()
                        .gap_3()
                        .child(if self.loading {
                            "正在读取课程…"
                        } else if query.is_empty() {
                            "还没有课程笔记"
                        } else {
                            "没有匹配的课程"
                        })
                        .child(
                            Button::new("library-new")
                                .primary()
                                .label("添加一节课")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.page = Page::New;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .children(courses.into_iter().enumerate().map(|(index, course)| {
                let open = course.clone();
                let dir = course.dir.clone();
                card()
                    .gap_3()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(course.title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x6b7280))
                            .child(course.dir.display().to_string()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new(("course", index)).label("阅读笔记").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.open_course(open.clone(), cx)
                                }),
                            ))
                            .child(
                                Button::new(("folder", index))
                                    .ghost()
                                    .label("打开文件夹")
                                    .on_click(move |_, _, cx| cx.reveal_path(&dir)),
                            ),
                    )
            }))
            .into_any_element()
    }
    fn settings_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut content = v_flex()
            .gap_5()
            .child(heading(
                "设置",
                "配置识别服务与 AI 整理。更改在下一次任务时生效。",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("doctor")
                            .label("检查运行环境")
                            .disabled(self.job.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.start(Kind::Doctor, cx))),
                    )
                    .child(
                        Button::new("models")
                            .label("下载本地识别模型")
                            .disabled(self.job.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.start(Kind::Models, cx))),
                    ),
            );
        if !self.show_advanced {
            content = content
                .child(
                    card()
                        .gap_4()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("云端语音识别"),
                        )
                        .child(self.input(Field::AsrUrl, "服务地址"))
                        .child(self.input(Field::AsrKey, "识别 API Key"))
                        .child(self.input(Field::AsrModel, "识别模型")),
                )
                .child(
                    card()
                        .gap_4()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("AI 校对与整理"),
                        )
                        .child(self.input(Field::LlmUrl, "AI 服务地址"))
                        .child(self.input(Field::LlmKey, "AI API Key"))
                        .child(self.input(Field::LlmModel, "AI 模型")),
                );
        } else {
            content = content.child(
                card()
                    .gap_3()
                    .child("完整 TOML 配置（包含 API Key，请勿分享）")
                    .child(Textarea::new(&self.advanced).h(px(420.))),
            );
        }
        content
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-settings")
                            .primary()
                            .label("保存设置")
                            .disabled(self.job.is_some())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_settings(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("advanced")
                            .label(if self.show_advanced {
                                "返回常用设置"
                            } else {
                                "完整配置…"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                if !this.show_advanced {
                                    let text = toml::to_string_pretty(&this.edited_settings(cx))
                                        .unwrap_or_default();
                                    this.advanced
                                        .update(cx, |state, cx| state.set_value(text, window, cx));
                                }
                                this.show_advanced = !this.show_advanced;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("open-config")
                            .label("打开配置文件")
                            .on_click(|_, _, cx| {
                                cx.open_with_system(&course2md::settings::config_path())
                            }),
                    )
                    .child(
                        Button::new("reload-config")
                            .label("重新加载")
                            .on_click(cx.listener(|this, _, window, cx| {
                                match course2md::settings::load() {
                                    Ok(config) => {
                                        this.config = config;
                                        this.config_error = false;
                                        this.sync_settings(window, cx);
                                        this.message = Some("配置已重新加载".into());
                                    }
                                    Err(error) => this.message = Some(format!("{error:#}")),
                                }
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }
    fn result_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(backend::Preview {
            course,
            markdown,
            blocks,
        }) = self.preview.clone()
        else {
            return div().child("请选择一份课程笔记").into_any_element();
        };
        let dir = course.dir.clone();
        v_flex()
            .gap_5()
            .child(heading(&course.title, "课程笔记"))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("result-back")
                            .label("返回课程库")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page = Page::Library;
                                this.refresh_library(cx);
                                cx.notify();
                            })),
                    )
                    .child(Button::new("result-copy").label("复制 Markdown").on_click({
                        let markdown = markdown.clone();
                        move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(markdown.clone()))
                        }
                    }))
                    .child(
                        Button::new("result-folder")
                            .label("打开文件夹")
                            .on_click(move |_, _, cx| cx.reveal_path(&dir)),
                    )
                    .children(
                        ["course.md", "course.html", "structured.json"]
                            .into_iter()
                            .filter(|name| course.dir.join(name).is_file())
                            .map(|name| {
                                let path = course.dir.join(name);
                                Button::new(name)
                                    .label(name)
                                    .on_click(move |_, _, cx| cx.open_with_system(&path))
                            }),
                    ),
            )
            .child(
                card()
                    .gap_4()
                    .children(blocks.into_iter().enumerate().map(|(index, block)| {
                        match block {
                            backend::PreviewBlock::Markdown(text) => {
                                TextView::markdown(("preview", index), text)
                                    .selectable(true)
                                    .into_any_element()
                            }
                            backend::PreviewBlock::Image(path) => img(path)
                                .w_full()
                                .max_w_full()
                                .h(px(400.))
                                .object_fit(ObjectFit::Contain)
                                .rounded_lg()
                                .into_any_element(),
                        }
                    })),
            )
            .into_any_element()
    }
}
fn card() -> Div {
    v_flex()
        .min_w_0()
        .w_full()
        .p_6()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e7ec))
        .rounded_xl()
}
fn heading(title: &str, subtitle: &str) -> Div {
    v_flex()
        .gap_2()
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_string()),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x697586))
                .child(subtitle.to_string()),
        )
}
impl Render for Desktop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.page {
            Page::New => self.new_page(cx),
            Page::Task => self.task_page(cx),
            Page::Library => self.library_page(cx),
            Page::Settings => self.settings_page(cx),
            Page::Result => self.result_page(cx),
        };
        h_flex()
            .size_full()
            .bg(rgb(0xf6f7f9))
            .text_color(rgb(0x182230))
            .text_base()
            .child(
                v_flex()
                    .w(px(200.))
                    .h_full()
                    .flex_shrink_0()
                    .p_5()
                    .gap_6()
                    .bg(rgb(0xffffff))
                    .border_r_1()
                    .border_color(rgb(0xe4e7ec))
                    .child(
                        v_flex()
                            .gap_1()
                            .pt_3()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("course2md"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x697586))
                                    .child("让课程成为你的知识"),
                            ),
                    )
                    .child(
                        v_flex().gap_2().children(
                            [
                                (Page::New, "新建笔记", IconName::Plus),
                                (Page::Task, "任务进度", IconName::Loader),
                                (Page::Library, "课程库", IconName::BookOpen),
                                (Page::Settings, "设置", IconName::Settings),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(index, (page, label, icon))| {
                                Button::new(("nav", index))
                                    .ghost()
                                    .w_full()
                                    .icon(icon)
                                    .label(label)
                                    .selected(self.page == page)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.page = page;
                                        this.message = None;
                                        if page == Page::Library {
                                            this.refresh_library(cx);
                                        }
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(rgb(0x697586)).child(
                        if self.job.is_some() {
                            "● 任务运行中"
                        } else {
                            "本地处理 · 文件归你"
                        },
                    )),
            )
            .child(
                div()
                    .id("page-scroll")
                    .min_w_0()
                    .flex_1()
                    .h_full()
                    .overflow_y_scroll()
                    .p_8()
                    .child(
                        v_flex()
                            .min_w_0()
                            .w_full()
                            .max_w(px(980.))
                            .mx_auto()
                            .gap_5()
                            .when_some(self.message.clone(), |view, message| {
                                view.child(
                                    h_flex()
                                        .gap_3()
                                        .p_4()
                                        .rounded_lg()
                                        .bg(rgb(0xfff4db))
                                        .child(div().flex_1().text_sm().child(message))
                                        .child(
                                            Button::new("dismiss-message")
                                                .ghost()
                                                .label("关闭")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.message = None;
                                                    cx.notify();
                                                })),
                                        ),
                                )
                            })
                            .child(content),
                    ),
            )
    }
}
