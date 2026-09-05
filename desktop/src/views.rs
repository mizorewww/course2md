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
        .p_5()
        .gap_4()
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(LINE))
        .rounded_xl()
}

impl Desktop {
    fn provider_choices(&self, cx: &mut Context<Self>) -> Div {
        let descriptions = [
            "使用已保存的设置，未设置时按平台选择",
            "Apple Silicon · 本地运行，首次下载模型",
            "通过 llama-server 使用显卡加速",
            "通过 llama-server 使用 CPU",
            "Intel NPU · 需要 OpenVINO 运行时",
            "使用已配置的语音服务 · 音频会上传",
        ];
        v_flex().gap_2().children((0..3).map(|row| {
            h_flex()
                .gap_2()
                .children((row * 2..row * 2 + 2).map(|index| {
                    let platform_supported = match index {
                        1 => cfg!(all(target_os = "macos", target_arch = "aarch64")),
                        4 => cfg!(any(target_os = "linux", target_os = "windows")),
                        _ => true,
                    };
                    Button::new(("provider", index))
                        .w_full()
                        .flex_1()
                        .h(px(76.))
                        .justify_start()
                        .selected(self.provider == index)
                        .disabled(!platform_supported)
                        .accessibility_label(PROVIDERS[index].1)
                        .child(
                            v_flex()
                                .gap_1()
                                .items_start()
                                .w_full()
                                .whitespace_normal()
                                .min_w_0()
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(PROVIDERS[index].1),
                                )
                                .child(div().text_xs().text_color(rgb(MUTED)).child(
                                    if platform_supported {
                                        descriptions[index]
                                    } else {
                                        "当前平台不支持"
                                    },
                                )),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.provider = index;
                            cx.notify();
                        }))
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
                            .checked(self.formats[index])
                            .on_click(cx.listener(move |this, checked, _, cx| {
                                this.formats[index] = *checked;
                                cx.notify();
                            }))
                    }),
            )
    }
    fn new_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut content = v_flex().gap_5().child(self.source_card(cx)).when(
            self.source_preview.is_some(),
            |view| {
                view.child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_1().text_color(rgb(MUTED)).child(format!(
                            "{} · {}",
                            SOURCES[self.source_mode].1,
                            if self.llm {
                                "AI 整理已启用"
                            } else {
                                "保留原始讲解"
                            }
                        )))
                        .child(
                            Button::new("more-options")
                                .ghost()
                                .label(if self.show_options {
                                    "收起转换选项"
                                } else {
                                    "转换选项"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_options = !this.show_options;
                                    cx.notify();
                                })),
                        ),
                )
            },
        );
        if self.show_options && self.source_preview.is_some() {
            content = content.child(
                card()
                    .child(section(
                        "识别与输出选项",
                        "只影响这次转换。默认设置可在侧栏中修改。",
                    ))
                    .when(self.source_mode != 1, |view| {
                        view.child(self.provider_choices(cx))
                    })
                    .when(self.provider == 5 && self.source_mode != 1, |view| {
                        view.child(
                            h_flex()
                                .gap_3()
                                .justify_between()
                                .child(muted(if self.config.asr_api.model.is_empty() {
                                    "云端服务尚未配置"
                                } else {
                                    "使用已保存的云端服务"
                                }))
                                .child(
                                    Button::new("configure-asr").label("配置语音服务").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.settings_tab = 1;
                                            this.navigate(Page::Settings, cx);
                                        }),
                                    ),
                                ),
                        )
                    })
                    .child(section("导出格式", ""))
                    .child(self.format_choices(cx))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_5()
                            .child(
                                Checkbox::new("resume")
                                    .label("继续上次未完成的转换")
                                    .checked(self.resume)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.resume = *value;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("keep-video")
                                    .label("保留下载的视频")
                                    .checked(self.keep_video)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.keep_video = *value;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        content
            .when(self.show_options && self.source_preview.is_some(), |view| {
                view.child(
                    v_flex()
                        .gap_3()
                        .child(h_flex().gap_2().children(SOURCES.iter().enumerate().map(
                            |(index, (_, name))| {
                                Button::new(("source-mode", index))
                                    .label(*name)
                                    .selected(self.source_mode == index)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.source_mode = index;
                                        cx.notify();
                                    }))
                            },
                        )))
                        .child(
                            Checkbox::new("llm")
                                .label("使用 AI 整理文字")
                                .checked(self.llm)
                                .on_click(cx.listener(|this, checked, _, cx| {
                                    this.llm = *checked;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }
    fn task_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.logs.is_empty()
            && self.stages.is_empty()
            && self.job.is_none()
            && self.completed.is_none()
        {
            return self.empty_state("还没有转换任务", "添加课程后，可以在这里查看处理进度。", cx);
        }
        let mut content = v_flex().gap_4();
        if self.kind == Kind::Convert {
            content = content.child(card().gap_4().children(STAGES.iter().enumerate().map(
                |(index, (id, name))| {
                    let status = self.stages.get(*id).map(String::as_str);
                    let running = status == Some("start") && self.job.is_some();
                    let done = status == Some("done");
                    let label = if done {
                        "已完成"
                    } else if running {
                        "正在处理"
                    } else if status == Some("start") {
                        "已停止"
                    } else if self.completed.is_some() {
                        "无需处理"
                    } else {
                        "等待中"
                    };
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .size(px(26.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(rgb(if done {
                                            0xe7f2eb
                                        } else if running {
                                            TINT
                                        } else {
                                            CANVAS
                                        }))
                                        .text_color(rgb(if done {
                                            SUCCESS
                                        } else if running {
                                            BLUE
                                        } else {
                                            MUTED
                                        }))
                                        .text_xs()
                                        .child(if done {
                                            "✓".to_owned()
                                        } else {
                                            (index + 1).to_string()
                                        }),
                                )
                                .child(div().flex_1().child(*name))
                                .child(muted(label)),
                        )
                        .when(running, |view| {
                            if let Some((current, total)) = self.progress.get(*id) {
                                view.child(Progress::new(("stage-progress", index)).value(
                                    if *total > 0 {
                                        *current as f32 / *total as f32 * 100.
                                    } else {
                                        0.
                                    },
                                ))
                            } else {
                                view
                            }
                        })
                },
            )));
        }
        if let Some(done) = self.completed.clone() {
            let course = Course::from_completed(&done);
            content = content.child(
                card()
                    .child(section(
                        &done.title,
                        &format!(
                            "{} 张截图 · {} 段讲解 · {:.1} 秒 · {} 个文件",
                            done.slides,
                            done.segments,
                            done.elapsed_secs,
                            done.outputs.len()
                        ),
                    ))
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
        content
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Button::new("toggle-logs")
                            .ghost()
                            .label(if self.show_logs {
                                "收起运行详情"
                            } else {
                                "查看运行详情"
                            })
                            .icon(if self.show_logs {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_logs = !this.show_logs;
                                cx.notify();
                            })),
                    )
                    .child(Button::new("copy-logs").ghost().label("复制日志").on_click(
                        cx.listener(|this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                this.logs.iter().cloned().collect::<Vec<_>>().join("\n"),
                            ));
                            this.message = Some("日志已复制".into());
                            cx.notify();
                        }),
                    )),
            )
            .when(self.show_logs, |view| {
                view.child(
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
                )
            })
            .into_any_element()
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
                    .rounded_xl()
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
                        "把值得回看的课程，留在这里"
                    },
                    if self.folder_filter.is_some() {
                        "添加课程时可直接归档，也可以从全部课程中移入。"
                    } else {
                        "从一个视频链接开始，积累自己的学习资料库。"
                    },
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
                .gap_5()
                .items_start()
                .children(courses.iter().enumerate().map(|(col, course)| {
                    let index = row * 2 + col;
                    let open = course.clone();
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .rounded_xl()
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
                                        .h(px(44.))
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
        let mut view = v_flex().gap_4();
        match self.settings_tab {
            0 => {
                view = view
                    .child(
                        card()
                            .child(section("保存与导出", "为新任务保存默认选项。"))
                            .child(self.input(Field::Output, "笔记保存目录"))
                            .child(Button::new("settings-output").label("选择目录").on_click(
                                cx.listener(|this, _, window, cx| this.pick(true, window, cx)),
                            ))
                            .child(self.format_choices(cx)),
                    )
                    .child(
                        card()
                            .child(section("任务偏好", ""))
                            .child(
                                Checkbox::new("settings-resume")
                                    .label("继续未完成的任务，复用已处理的内容")
                                    .checked(self.resume)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.resume = *value;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Checkbox::new("settings-keep")
                                    .label("保留从网上下载的视频")
                                    .checked(self.keep_video)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.keep_video = *value;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(muted("本地视频始终保留在原位置。"));
            }
            1 => {
                view = view
                    .child(
                        card()
                            .child(section(
                                "默认识别方式",
                                "字幕可用时，字幕优先模式不会调用语音识别。",
                            ))
                            .child(self.provider_choices(cx)),
                    )
                    .child(
                        card()
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
                    card()
                        .child(section("AI 整理", "使用兼容服务校对转录文字、整理段落。"))
                        .child(
                            Checkbox::new("settings-ai")
                                .label("默认启用 AI 整理")
                                .checked(self.llm)
                                .on_click(cx.listener(|this, value, _, cx| {
                                    this.llm = *value;
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
                        card()
                            .child(section("运行环境", "开始转换前检测本机工具。"))
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
                        card()
                            .child(section("模型与诊断", "需要本地语音识别时再下载模型。"))
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
                        card()
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
        let Some(preview) = self.preview.clone() else {
            return muted("正在打开笔记…").into_any_element();
        };
        match self.result_tab {
            0 => card()
                .p_7()
                .gap_5()
                .children(
                    preview
                        .blocks
                        .into_iter()
                        .enumerate()
                        .map(|(index, block)| match block {
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
                        }),
                )
                .into_any_element(),
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
                    .into_iter()
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
    fn page_header(&self, cx: &mut Context<Self>) -> Div {
        let (title, subtitle) = match self.page {
            Page::New => (
                "添加课程".to_owned(),
                "把课程中的画面与讲解，整理成可以回看的笔记。".to_owned(),
            ),
            Page::Library => (
                self.folder_filter
                    .map(|id| {
                        if id == 0 {
                            "未分类".into()
                        } else {
                            self.library
                                .folders
                                .get(&id)
                                .cloned()
                                .unwrap_or_else(|| "课程库".into())
                        }
                    })
                    .unwrap_or_else(|| "全部课程".into()),
                format!(
                    "{} 份笔记 · 保存在你的电脑上",
                    self.courses
                        .iter()
                        .filter(|course| self.folder_filter.is_none_or(|id| self
                            .library
                            .folder(&self.library_root, &course.dir)
                            .unwrap_or(0)
                            == id))
                        .count()
                ),
            ),
            Page::Settings => (
                "设置".into(),
                "配置默认选项与服务，更改在保存后生效。".into(),
            ),
            Page::Task => (
                if self.job.is_some() {
                    "正在处理"
                } else {
                    "任务记录"
                }
                .into(),
                self.task_status.clone(),
            ),
            Page::Result => self
                .preview
                .as_ref()
                .map(|preview| {
                    (
                        preview.course.title.clone(),
                        format!(
                            "{} 张截图 · {} 段讲解",
                            preview.course.slides, preview.course.segments
                        ),
                    )
                })
                .unwrap_or(("课程笔记".into(), "正在读取…".into())),
        };
        let mut header = v_flex().gap_5().child(
            h_flex()
                .gap_3()
                .when(matches!(self.page, Page::Result | Page::New), |view| {
                    view.child(
                        Button::new("result-back")
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .accessibility_label("返回上一页")
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
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_ellipsis()
                                .child(title),
                        )
                        .child(muted(subtitle)),
                ),
        );
        if matches!(self.page, Page::Settings | Page::Result) {
            header = header.child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .justify_between()
                    .child(self.tabs(cx))
                    .when(self.page == Page::Result, |view| {
                        view.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("result-copy")
                                        .ghost()
                                        .label("复制文稿")
                                        .disabled(
                                            self.preview.as_ref().is_none_or(|p| !p.has_markdown),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(preview) = &this.preview {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    preview.markdown.clone(),
                                                ));
                                                this.message = Some("文稿已复制".into());
                                                cx.notify();
                                            }
                                        })),
                                )
                                .child(
                                    Button::new("result-folder")
                                        .ghost()
                                        .icon(IconName::FolderOpen)
                                        .accessibility_label("打开结果文件夹")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Some(preview) = &this.preview {
                                                cx.reveal_path(&preview.course.dir);
                                            }
                                        })),
                                ),
                        )
                    }),
            );
        }
        header
    }
    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let footer = h_flex()
            .gap_4()
            .justify_between()
            .w_full()
            .max_w(px(780.))
            .mx_auto();
        match self.page {
            Page::New => footer
                .child(muted(if self.job.is_some() {
                    "任务在后台运行，输入内容会保留。"
                } else if self.source_preview.is_none() {
                    "先预览并确认课程内容。"
                } else {
                    "笔记生成后会自动加入课程库。"
                }))
                .child(
                    Button::new("start")
                        .primary()
                        .h(px(42.))
                        .px_6()
                        .label(if self.job.is_some() {
                            "查看进行中的任务"
                        } else {
                            "生成笔记"
                        })
                        .icon(IconName::ArrowRight)
                        .disabled(self.job.is_none() && self.source_preview.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.start(Kind::Convert, cx))),
                ),
            Page::Settings => footer
                .child(muted(if self.job.is_some() {
                    "任务结束后可以保存设置。"
                } else {
                    "切换页面会保留未保存的输入。"
                }))
                .child(
                    Button::new("save-settings")
                        .primary()
                        .h(px(40.))
                        .label("保存设置")
                        .disabled(self.job.is_some())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.save_settings(window, cx)),
                        ),
                ),
            _ => footer
                .child(muted(if self.job.is_some() {
                    "可以切换页面，任务会继续运行。"
                } else {
                    "重试会保留你上次输入的内容。"
                }))
                .child(if self.job.is_some() {
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
                        }))
                } else {
                    Button::new("retry")
                        .primary()
                        .label("返回并调整")
                        .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::New, cx)))
                }),
        }
    }
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
            .bg(rgb(CANVAS))
            .text_color(rgb(INK))
            .text_sm()
            .child(
                v_flex()
                    .w(px(220.))
                    .h_full()
                    .flex_shrink_0()
                    .px_3()
                    .py_6()
                    .gap_6()
                    .bg(rgb(SIDEBAR))
                    .border_r_1()
                    .border_color(rgb(LINE))
                    .child(
                        h_flex()
                            .gap_3()
                            .px_3()
                            .child(
                                div()
                                    .size(px(32.))
                                    .rounded_lg()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(SURFACE))
                                    .text_color(rgb(BLUE))
                                    .child(Icon::new(IconName::BookOpen)),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div().font_weight(FontWeight::SEMIBOLD).child("course2md"),
                                    )
                                    .child(
                                        div().text_xs().text_color(rgb(MUTED)).child("课程笔记"),
                                    ),
                            ),
                    )
                    .child(
                        v_flex().gap_1().children(
                            [
                                (Page::New, "添加课程", IconName::Plus),
                                (Page::Library, "全部课程", IconName::BookOpen),
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(index, (page, label, icon))| {
                                Button::new(("nav", index))
                                    .ghost()
                                    .when(page == Page::New, |button| button.primary())
                                    .w_full()
                                    .h(px(40.))
                                    .justify_start()
                                    .accessibility_label(label)
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_3()
                                            .child(Icon::new(icon))
                                            .child(label),
                                    )
                                    .selected(
                                        (self.page == page
                                            && (page != Page::Library
                                                || self.folder_filter.is_none()))
                                            || self.page == Page::Result && page == Page::Library,
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if page == Page::New {
                                            this.begin_add(window, cx);
                                            return;
                                        }
                                        if page == Page::Library {
                                            this.folder_filter = None;
                                        }
                                        this.navigate(page, cx)
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
                        Button::new("nav-task")
                            .ghost()
                            .w_full()
                            .h(px(38.))
                            .selected(self.page == Page::Task)
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_3()
                                    .child(Icon::new(IconName::Loader))
                                    .child(if self.job.is_some() {
                                        "正在处理"
                                    } else {
                                        "任务记录"
                                    }),
                            )
                            .accessibility_label("任务记录")
                            .on_click(cx.listener(|this, _, _, cx| this.navigate(Page::Task, cx))),
                    )
                    .child(
                        Button::new("nav-settings")
                            .ghost()
                            .w_full()
                            .h(px(40.))
                            .justify_start()
                            .accessibility_label("设置")
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_3()
                                    .child(Icon::new(IconName::Settings))
                                    .child("设置"),
                            )
                            .selected(self.page == Page::Settings)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.navigate(Page::Settings, cx)),
                            ),
                    )
                    .child(
                        Button::new("environment-status")
                            .ghost()
                            .w_full()
                            .h_auto()
                            .py_3()
                            .justify_start()
                            .accessibility_label("查看运行环境")
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().size(px(6.)).rounded_full().bg(rgb(
                                        if self.environment.as_ref().is_some_and(|env| env.ready())
                                        {
                                            SUCCESS
                                        } else {
                                            MUTED
                                        },
                                    )))
                                    .child(div().text_xs().text_color(rgb(MUTED)).child(
                                        match &self.environment {
                                            None => "正在检测环境…",
                                            Some(env) if env.ready() => "转换环境已就绪",
                                            _ => "需要安装视频工具",
                                        },
                                    )),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_tab = 3;
                                this.navigate(Page::Settings, cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .px_8()
                            .pt_7()
                            .pb_5()
                            .flex_shrink_0()
                            .child(self.page_header(cx).w_full().max_w(px(780.)).mx_auto()),
                    )
                    .when(
                        self.folder_editor.is_some() || self.delete_folder.is_some(),
                        |view| view.child(div().px_8().pb_4().child(self.folder_editor_view(cx))),
                    )
                    .when_some(self.message.clone(), |view, message| {
                        view.child(
                            div().px_8().pb_3().child(
                                h_flex()
                                    .w_full()
                                    .max_w(px(780.))
                                    .mx_auto()
                                    .gap_3()
                                    .p_3()
                                    .rounded_lg()
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
                    .child(
                        div()
                            .id(("page-scroll", self.page as usize))
                            .min_w_0()
                            .flex_1()
                            .overflow_y_scroll()
                            .track_scroll(&self.scrolls[self.page as usize])
                            .px_8()
                            .pb_6()
                            .child(div().w_full().max_w(px(780.)).mx_auto().child(content)),
                    )
                    .when(
                        matches!(self.page, Page::New | Page::Settings | Page::Task),
                        |view| {
                            view.child(
                                div()
                                    .flex_shrink_0()
                                    .border_t_1()
                                    .border_color(rgb(LINE))
                                    .px_8()
                                    .py_4()
                                    .bg(rgb(CANVAS))
                                    .child(self.footer(cx)),
                            )
                        },
                    ),
            )
    }
}
