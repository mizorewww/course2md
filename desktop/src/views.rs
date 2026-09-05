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
    fn provider_choices(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_2()
            .flex_wrap()
            .children(PROVIDERS.iter().enumerate().map(|(index, (_, name))| {
                let supported = match index {
                    1 => cfg!(all(target_os = "macos", target_arch = "aarch64")),
                    4 => cfg!(any(target_os = "linux", target_os = "windows")),
                    _ => true,
                };
                Button::new(("provider", index))
                    .h(px(32.))
                    .label(*name)
                    .disabled(!supported)
                    .selected(self.editing_options().provider == index)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.editing_options_mut().provider = index;
                        cx.notify();
                    }))
            }))
    }
    fn format_choices(&self, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_6()
            .children(
                ["Markdown", "HTML", "JSON"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| {
                        Checkbox::new(("format", index))
                            .label(label)
                            .checked(self.editing_options().formats[index])
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                this.editing_options_mut().formats[index] = *checked;
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
                                Button::new(("source-mode", index))
                                    .label(*label)
                                    .selected(self.task_options.source_mode == index)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.task_options.source_mode = index;
                                        cx.notify();
                                    }))
                            }),
                        ))
                        .when(self.task_options.source_mode != 1, |v| {
                            v.child(self.provider_choices(cx))
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
                        .icon(IconName::Loader)
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
    fn settings_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut view = v_flex().gap_0();
        match self.settings_tab {
            0 => {
                view = view
                    .child(
                        v_flex()
                            .gap_3()
                            .pb_6()
                            .child(section("窗口", ""))
                            .child(
                                Checkbox::new("system-titlebar")
                                    .label("使用系统标题栏")
                                    .checked(self.desktop_settings.system_titlebar)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.desktop_settings.system_titlebar = *value;
                                        cx.notify();
                                    })),
                            )
                            .when(
                                self.desktop_settings.system_titlebar != self.system_titlebar,
                                |view| view.child(muted("重启后生效")),
                            )
                            .child(
                                Checkbox::new("reduce-motion")
                                    .label("减少动态效果")
                                    .checked(self.desktop_settings.reduce_motion)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.desktop_settings.reduce_motion = *value;
                                        cx.set_reduce_motion(*value);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section("保存与导出", ""))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_end()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(self.input(Field::Output, "笔记保存目录")),
                                    )
                                    .child(
                                        Button::new("settings-output")
                                            .h(px(32.))
                                            .label("选择…")
                                            .accessibility_label("选择目录")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.pick(true, window, cx)
                                            })),
                                    ),
                            )
                            .child(self.format_choices(cx)),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section("任务偏好", ""))
                            .child(
                                Checkbox::new("settings-resume")
                                    .label("继续未完成的任务")
                                    .checked(self.editing_options().resume)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.editing_options_mut().resume = *value;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("settings-keep")
                                    .label("保留下载的视频")
                                    .checked(self.editing_options().keep_video)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.editing_options_mut().keep_video = *value;
                                        cx.notify();
                                    })),
                            ),
                    );
            }
            1 => {
                view = view
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section("默认识别方式", ""))
                            .child(h_flex().gap_2().flex_wrap().children(
                                SOURCES.iter().enumerate().map(|(index, (_, label))| {
                                    Button::new(("default-source", index))
                                        .label(*label)
                                        .selected(self.settings_options.source_mode == index)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.settings_options.source_mode = index;
                                            cx.notify();
                                        }))
                                }),
                            ))
                            .child(self.provider_choices(cx)),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section(
                                "云端语音服务",
                                "选择云端识别时，音频将发送至此服务。",
                            ))
                            .child(self.input(Field::AsrUrl, "服务地址"))
                            .child(self.input(Field::AsrModel, "模型名称"))
                            .child(self.input(Field::AsrKey, "API Key")),
                    );
            }
            2 => {
                view = view.child(
                    v_flex()
                        .w_full()
                        .gap_4()
                        .pb_6()
                        .child(section("AI 整理", ""))
                        .child(
                            Checkbox::new("settings-ai")
                                .label("默认启用 AI 整理")
                                .checked(self.editing_options().llm)
                                .on_click(cx.listener(|this, value, _, cx| {
                                    this.editing_options_mut().llm = *value;
                                    cx.notify();
                                })),
                        )
                        .child(self.input(Field::LlmUrl, "服务地址"))
                        .child(self.input(Field::LlmModel, "模型名称"))
                        .child(self.input(Field::LlmKey, "API Key"))
                        .child(muted("启用后，转录文字会发送至你配置的服务。")),
                );
            }
            _ => {
                view = view
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section("运行环境", ""))
                            .children(
                                self.environment
                                    .as_ref()
                                    .into_iter()
                                    .flat_map(|env| {
                                        [
                                            ("转换引擎", env.engine),
                                            ("ffmpeg", env.ffmpeg),
                                            ("ffprobe", env.ffprobe),
                                            ("yt-dlp · 在线视频", env.ytdlp),
                                            ("llama-server · GPU / CPU 识别", env.llama),
                                        ]
                                    })
                                    .map(|(name, ready)| {
                                        h_flex().justify_between().child(name).child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(if ready {
                                                    SUCCESS
                                                } else {
                                                    MUTED
                                                }))
                                                .child(if ready {
                                                    "已就绪"
                                                } else {
                                                    "未检测到"
                                                }),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("refresh-environment")
                                    .label("重新检测")
                                    .icon(IconName::Loader)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_environment(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(muted(if cfg!(target_os = "macos") {
                                "安装视频工具：brew install ffmpeg yt-dlp"
                            } else if cfg!(target_os = "windows") {
                                "安装视频工具：winget install Gyan.FFmpeg yt-dlp.yt-dlp"
                            } else {
                                "安装 ffmpeg，并使用 pipx install yt-dlp 安装下载工具。"
                            })),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section("模型与诊断", ""))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        Button::new("doctor")
                                            .label("运行完整诊断")
                                            .disabled(self.job.is_some())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.start(Kind::Doctor, cx)
                                            })),
                                    )
                                    .child(
                                        Button::new("models")
                                            .label("下载 GPU / CPU 模型")
                                            .disabled(self.job.is_some())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.start(Kind::Models, cx)
                                            })),
                                    ),
                            )
                            .child(muted("Apple 原生与 Intel NPU 模型在首次识别时下载。")),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .gap_4()
                            .pb_6()
                            .child(section(
                                "高级配置",
                                "需要调整截图采样、识别参数等选项时，编辑配置文件。",
                            ))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .child(
                                        Button::new("open-config").label("打开配置文件").on_click(
                                            |_, _, cx| {
                                                cx.open_with_system(
                                                    &course2md::settings::config_path(),
                                                )
                                            },
                                        ),
                                    )
                                    .child(
                                        Button::new("reload-config")
                                            .label("从文件重新加载")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                match course2md::settings::load() {
                                                    Ok(config) => {
                                                        this.config = config;
                                                        this.config_error = false;
                                                        this.sync_settings(window, cx);
                                                        this.message =
                                                            Some("已从文件重新加载设置".into());
                                                    }
                                                    Err(error) => {
                                                        this.message =
                                                            Some(format!("读取失败：{error:#}"))
                                                    }
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    );
            }
        }
        view.into_any_element()
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
            vec!["通用", "语音识别", "AI 整理", "运行环境"]
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
                Button::new(("tab", index))
                    .ghost()
                    .label(label)
                    .selected(selected == index)
                    .h(px(36.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.page == Page::Settings {
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
        if self.page == Page::New {
            row = row.child(
                Button::new("start")
                    .primary()
                    .h(px(32.))
                    .label(if self.job.is_some() {
                        "查看任务"
                    } else {
                        "生成笔记"
                    })
                    .disabled(self.job.is_none() && self.source_preview.is_none())
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Compare drafts once per render; only actual changes restart the debounce.
        let draft = self.edited_settings(cx);
        if draft != self.settings_snapshot {
            self.settings_snapshot = draft;
            self.settings_deadline = Some(Instant::now() + Duration::from_millis(700));
            self.settings_status = "未保存".into();
        }
        let content = match self.page {
            Page::New => self.new_page(cx),
            Page::Task => self.task_page(cx),
            Page::Library => self.library_page(cx),
            Page::Settings => self.settings_page(cx),
            Page::Result => self.result_page(cx),
        };
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
                        (Page::Library, "全部课程", IconName::BookOpen),
                        (Page::New, "添加课程", IconName::Plus),
                        (Page::Task, "任务", IconName::Loader),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (page, label, icon))| {
                        Button::new(("nav", index))
                            .ghost()
                            .w_full()
                            .h(px(32.))
                            .justify_start()
                            .accessibility_label(label)
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(icon))
                                    .child(label),
                            )
                            .selected(
                                self.page == page
                                    && (page != Page::Library || self.folder_filter.is_none()),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if page == Page::New {
                                    this.begin_add(window, cx);
                                } else {
                                    if page == Page::Library {
                                        this.folder_filter = None;
                                    }
                                    this.navigate(page, cx);
                                }
                            }))
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
                Button::new("nav-settings")
                    .ghost()
                    .w_full()
                    .h(px(32.))
                    .justify_start()
                    .accessibility_label("设置")
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(Icon::new(IconName::Settings))
                            .child("设置"),
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
        let status = if self.page == Page::Settings {
            self.settings_status.clone()
        } else if self.job.is_some() {
            self.task_summary()
        } else if self.preview_cancel.is_some() {
            "正在读取课程…".into()
        } else {
            String::new()
        };
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
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_3()
                    .gap_3()
                    .bg(rgb(SIDEBAR))
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .child(
                        Button::new("environment-status")
                            .ghost()
                            .h(px(24.))
                            .text_xs()
                            .label(match &self.environment {
                                None => "检测中…",
                                Some(e) if e.ready() => "就绪",
                                _ => "缺少视频工具",
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_tab = 3;
                                this.navigate(Page::Settings, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(status),
                    )
                    .when(self.job.is_some() && self.page != Page::Task, |v| {
                        v.child(
                            Button::new("status-task")
                                .ghost()
                                .h(px(24.))
                                .label("查看任务")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.navigate(Page::Task, cx)),
                                ),
                        )
                    }),
            )
    }
}
