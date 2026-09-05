//! Settings use one field height, shared section edges, and progressive disclosure.
use super::*;
use crate::theme::*;
use gpui_component::{button::*, switch::Switch};

fn group(title: &'static str) -> Div {
    v_flex()
        .w_full()
        .gap_4()
        .p_4()
        .bg(rgb(SURFACE))
        .rounded_lg()
        .border_1()
        .border_color(rgb(LINE))
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
}
fn preference(label: &'static str, hint: &'static str, control: Switch) -> Div {
    h_flex()
        .w_full()
        .min_h(px(44.))
        .gap_4()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(label)
                .when(!hint.is_empty(), |v| {
                    v.child(div().text_sm().text_color(rgb(MUTED)).child(hint))
                }),
        )
        .child(control.accessibility_label(label).p_2())
}

impl Desktop {
    pub fn directory_field(&self, id: &'static str, cx: &mut Context<Self>) -> Div {
        v_flex()
            .gap_2()
            .child(div().font_weight(FontWeight::MEDIUM).child("笔记保存位置"))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.inputs[&Field::Output])
                                .min_h(px(36.))
                                .max_h(px(36.))
                                .aria_label("笔记保存位置")
                                .w_full(),
                        ),
                    )
                    .child(
                        Button::new(id)
                            .h(px(36.))
                            .min_h(px(36.))
                            .flex_shrink_0()
                            .label("选择文件夹")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick(true, window, cx)),
                            ),
                    ),
            )
            .when_some(self.field_error(Field::Output, cx), |v, message| {
                v.child(div().text_sm().text_color(rgb(0xa32626)).child(message))
            })
    }
    pub fn settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.settings_tab == 4 {
            return self.about_page(cx);
        }
        let mut view = v_flex().w_full().max_w(px(760.)).gap_6();
        match self.settings_tab {
            0 => {
                view = view
                    .child(
                        group("窗口")
                            .child(preference(
                                "使用系统标题栏",
                                "重启后生效",
                                Switch::new("system-titlebar")
                                    .checked(self.desktop_settings.system_titlebar)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.desktop_settings.system_titlebar = *value;
                                        cx.notify();
                                    })),
                            ))
                            .child(preference(
                                "减少动态效果",
                                "",
                                Switch::new("reduce-motion")
                                    .checked(self.desktop_settings.reduce_motion)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.desktop_settings.reduce_motion = *value;
                                        cx.set_reduce_motion(*value);
                                        cx.notify();
                                    })),
                            )),
                    )
                    .child(
                        group("保存与导出")
                            .child(self.directory_field("settings-output", cx))
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child("导出格式")
                                    .child(self.format_choices(cx)),
                            ),
                    )
                    .child(
                        group("任务偏好")
                            .child(preference(
                                "继续未完成的任务",
                                "",
                                Switch::new("settings-resume")
                                    .checked(self.settings_options.resume)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.settings_options.resume = *value;
                                        cx.notify();
                                    })),
                            ))
                            .child(preference(
                                "保留下载的视频",
                                "",
                                Switch::new("settings-keep")
                                    .checked(self.settings_options.keep_video)
                                    .on_click(cx.listener(|this, value, _, cx| {
                                        this.settings_options.keep_video = *value;
                                        cx.notify();
                                    })),
                            )),
                    )
                    .child(
                        Button::new("reopen-setup")
                            .h(px(36.))
                            .min_h(px(36.))
                            .self_start()
                            .label("重新打开设置引导")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_setup(window, cx)),
                            ),
                    );
            }
            1 => {
                let cloud = v_flex()
                    .pt_4()
                    .gap_4()
                    .child(self.input(Field::AsrUrl, "服务地址", cx))
                    .child(self.input(Field::AsrModel, "模型名称", cx))
                    .child(self.input(Field::AsrKey, "API Key", cx));
                let cloud = disclosure(
                    "settings-cloud",
                    self.settings_options.provider == 5 && self.settings_options.source_mode != 1,
                    cloud,
                    window,
                    cx,
                );
                view = view
                    .child(
                        group("转录来源")
                            .child(h_flex().gap_3().flex_wrap().children(
                                SOURCES.iter().enumerate().map(|(index, (_, label))| {
                                    choice(
                                        Button::new(("default-source", index)).label(*label),
                                        self.settings_options.source_mode == index,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            if index == 1 {
                                                this.blur_fields(
                                                    &[
                                                        Field::AsrUrl,
                                                        Field::AsrModel,
                                                        Field::AsrKey,
                                                    ],
                                                    window,
                                                    cx,
                                                );
                                            }
                                            this.settings_options.source_mode = index;
                                            cx.notify();
                                        },
                                    ))
                                }),
                            ))
                            .child(div().text_sm().text_color(rgb(MUTED)).child(
                                match self.settings_options.source_mode {
                                    1 => "只读取已有字幕，没有字幕时会提示。",
                                    2 => "根据视频声音生成转录。",
                                    _ => "有字幕时直接读取，没有字幕时自动识别。",
                                },
                            )),
                    )
                    .when(self.settings_options.source_mode != 1, |v| {
                        v.child(
                            group("识别位置")
                                .gap_0()
                                .child(div().pt_4().child(self.engine_panel(cx)))
                                .child(cloud),
                        )
                    });
            }
            2 => {
                let fields = v_flex()
                    .pt_4()
                    .gap_4()
                    .child(self.input(Field::LlmUrl, "服务地址", cx))
                    .child(self.input(Field::LlmModel, "模型名称", cx))
                    .child(self.input(Field::LlmKey, "API Key（可选）", cx));
                let fields = disclosure(
                    "settings-ai-fields",
                    self.settings_options.llm,
                    fields,
                    window,
                    cx,
                );
                view = view.child(
                    group("AI 整理")
                        .gap_0()
                        .child(preference(
                            "整理转录内容",
                            "启用后，转录文字会发送到你选择的服务。",
                            Switch::new("settings-ai")
                                .checked(self.settings_options.llm)
                                .on_click(cx.listener(|this, value: &bool, window, cx| {
                                    if !*value {
                                        this.blur_fields(
                                            &[Field::LlmUrl, Field::LlmModel, Field::LlmKey],
                                            window,
                                            cx,
                                        );
                                    }
                                    this.settings_options.llm = *value;
                                    cx.notify();
                                })),
                        ))
                        .child(fields),
                );
            }
            _ => {
                view = view.child(self.environment_page(window, cx));
            }
        }
        if cx.reduce_motion() || self.settings_transition == 0 {
            return view.into_any_element();
        }
        view.with_animation(
            ("settings-content", self.settings_transition),
            Animation::new(Duration::from_millis(140))
                .with_easing(gpui_component::animation::cubic_bezier(0.2, 0., 0., 1.)),
            |view, t| view.opacity(0.75 + 0.25 * t),
        )
        .into_any_element()
    }

    fn environment_page(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let mut capabilities = group("功能检查");
        if let Some(env) = &self.environment {
            for (name, detail, ready) in [
                ("视频处理", "读取视频、提取声音和截图", env.ready()),
                (
                    "在线链接",
                    "读取 YouTube 和 Bilibili 链接",
                    env.ytdlp && env.ready(),
                ),
                (
                    "本机识别",
                    "在这台电脑上转录声音",
                    env.engine && (env.apple || env.llama || env.npu),
                ),
            ] {
                capabilities = capabilities.child(
                    h_flex()
                        .w_full()
                        .gap_4()
                        .py_2()
                        .items_center()
                        .child(
                            Icon::new(if ready {
                                IconName::CircleCheck
                            } else {
                                IconName::TriangleAlert
                            })
                            .size_5()
                            .text_color(rgb(if ready {
                                SUCCESS
                            } else {
                                0x945000
                            })),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(name)
                                .child(div().text_sm().text_color(rgb(MUTED)).child(detail)),
                        )
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(if ready { SUCCESS } else { 0x945000 }))
                                .child(if ready { "可用" } else { "需要处理" }),
                        ),
                );
            }
        } else {
            capabilities = capabilities.child("正在检查这台电脑…");
        }
        capabilities = capabilities.child(
            Button::new("refresh-environment")
                .h(px(36.))
                .min_h(px(36.))
                .self_start()
                .label("重新检查")
                .icon(icons::refresh())
                .loading(self.environment.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.refresh_environment(cx))),
        );
        let needs_help = self
            .environment
            .as_ref()
            .is_some_and(|e| !e.ready() || !e.ytdlp || !(e.apple || e.llama || e.npu));
        let mut details = v_flex().pt_4().gap_4();
        if let Some(env) = &self.environment {
            details = details.children(
                [
                    ("转换程序", env.engine),
                    ("ffmpeg", env.ffmpeg),
                    ("ffprobe", env.ffprobe),
                    ("yt-dlp", env.ytdlp),
                    ("llama-server", env.llama),
                ]
                .into_iter()
                .map(|(name, ready)| {
                    h_flex()
                        .gap_4()
                        .child(div().flex_1().child(name))
                        .child(if ready {
                            "已检测到"
                        } else {
                            "未检测到"
                        })
                }),
            );
            if !env.engine {
                details = details.child(
                    v_flex()
                        .gap_2()
                        .child("转换程序无法运行，请重新安装完整应用。")
                        .child(
                            Button::new("repair-app")
                                .h(px(36.))
                                .min_h(px(36.))
                                .self_start()
                                .label("下载安装包")
                                .on_click(|_, _, cx| {
                                    cx.open_url("https://github.com/mizorewww/course2md/releases")
                                }),
                        ),
                );
            }
            if !(env.apple || env.llama || env.npu) {
                details=details.child(v_flex().gap_3().child("本机识别尚未配置。可以使用云端服务，或安装本机识别工具。")
                    .child(h_flex().gap_3().flex_wrap()
                        .child(Button::new("use-cloud").h(px(36.)).min_h(px(36.)).label("配置云端识别")
                            .on_click(cx.listener(|this,_,_,cx| { this.settings_options.provider=5;this.settings_options.source_mode=0;this.settings_tab=1;this.settings_transition=this.settings_transition.wrapping_add(1);cx.notify(); })))
                        .child(Button::new("local-help").h(px(36.)).min_h(px(36.)).label("本机识别安装说明")
                            .on_click(|_,_,cx| cx.open_url("https://github.com/mizorewww/course2md/blob/main/readme.zh.md")))));
            }
            if !env.ffmpeg || !env.ffprobe || !env.ytdlp {
                let command = if cfg!(target_os = "macos") {
                    "brew install ffmpeg yt-dlp"
                } else if cfg!(target_os = "windows") {
                    "winget install Gyan.FFmpeg yt-dlp.yt-dlp"
                } else {
                    "pipx install yt-dlp"
                };
                details = details.child(
                    v_flex()
                        .gap_2()
                        .child("安装视频工具")
                        .when(cfg!(target_os = "linux"), |v| {
                            v.child(
                                "先通过系统软件包管理器安装 ffmpeg；下载工具可使用以下命令安装。",
                            )
                        })
                        .child(div().text_sm().child(command))
                        .child(
                            Button::new("copy-install")
                                .h(px(36.))
                                .min_h(px(36.))
                                .self_start()
                                .label("复制安装命令")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(command.into()))
                                }),
                        ),
                );
            }
        }
        details = details
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        Button::new("doctor")
                            .h(px(36.))
                            .min_h(px(36.))
                            .label("运行完整诊断")
                            .disabled(self.job.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.start(Kind::Doctor, cx))),
                    )
                    .when(self.environment.as_ref().is_some_and(|e| e.llama), |v| {
                        v.child(
                            Button::new("models")
                                .h(px(36.))
                                .min_h(px(36.))
                                .label("下载 GPU / CPU 模型")
                                .disabled(self.job.is_some())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.start(Kind::Models, cx)),
                                ),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        Button::new("open-config")
                            .h(px(36.))
                            .min_h(px(36.))
                            .label("打开配置文件")
                            .on_click(|_, _, cx| {
                                cx.open_with_system(&course2md::settings::config_path())
                            }),
                    )
                    .child(
                        Button::new("reload-config")
                            .h(px(36.))
                            .min_h(px(36.))
                            .label("重新加载配置")
                            .on_click(cx.listener(|this, _, window, cx| {
                                match course2md::settings::load() {
                                    Ok(config) => {
                                        this.config = config;
                                        this.config_error = false;
                                        this.sync_settings(window, cx);
                                        this.message = Some("已重新加载配置".into());
                                    }
                                    Err(error) => {
                                        this.message = Some(format!("读取失败：{error:#}"))
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            );
        let details = disclosure(
            "diagnostics-content",
            self.show_diagnostics,
            details,
            window,
            cx,
        );
        v_flex().gap_6().child(capabilities).child(
            group(if needs_help {
                "解决问题"
            } else {
                "高级诊断"
            })
            .gap_0()
            .child(
                Button::new("toggle-diagnostics")
                    .ghost()
                    .h(px(36.))
                    .min_h(px(36.))
                    .self_start()
                    .label(if self.show_diagnostics {
                        "收起详细检查"
                    } else if needs_help {
                        "查看解决方法"
                    } else {
                        "查看详细检查"
                    })
                    .icon(if self.show_diagnostics {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_diagnostics = !this.show_diagnostics;
                        cx.notify();
                    })),
            )
            .child(details),
        )
    }
}
