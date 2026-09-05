//! First-run setup and the same capability explanations used by Settings.
use super::*;
use crate::theme::*;
use gpui_component::button::*;

struct Setup {
    desktop: Entity<Desktop>,
    _observation: Subscription,
}
impl Render for Setup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.desktop
            .update(cx, |desktop, cx| desktop.setup_content(window, cx))
    }
}

pub struct EngineChoice {
    pub index: usize,
    pub name: &'static str,
    pub status: &'static str,
    pub detail: String,
    pub selectable: bool,
}

impl Desktop {
    pub fn engine_choices(&self, cx: &App) -> Vec<EngineChoice> {
        let Some(env) = &self.environment else {
            return Vec::new();
        };
        let mut choices = Vec::new();
        if cfg!(target_os = "macos") {
            choices.push(EngineChoice {
                index: 1,
                name: "Apple 原生",
                selectable: env.apple && env.engine,
                status: if env.apple && env.engine {
                    "可用"
                } else {
                    "不可用"
                },
                detail: if env.apple {
                    "在本机识别；缺少模型时自动下载"
                } else {
                    "需要 Apple Silicon 和包含原生识别的完整安装包"
                }
                .into(),
            });
        }
        choices.push(EngineChoice {
            index: 2,
            name: "GPU 加速",
            selectable: env.gpu.is_some() && env.engine,
            status: if env.gpu.is_some() && env.engine {
                "已检测到设备"
            } else {
                "不可用"
            },
            detail: env
                .gpu
                .clone()
                .map(|name| format!("{name} · 缺少模型时自动下载"))
                .unwrap_or_else(|| {
                    if env.llama {
                        "llama-server 未报告可用的 GPU 设备"
                    } else {
                        "需要安装支持 GPU 的 llama-server"
                    }
                    .into()
                }),
        });
        choices.push(EngineChoice {
            index: 3,
            name: "CPU 识别",
            selectable: env.llama && env.engine,
            status: if env.llama && env.engine {
                "可用"
            } else {
                "未安装"
            },
            detail: if env.llama {
                "在本机识别，速度通常较慢；缺少模型时自动下载"
            } else {
                "需要安装 llama-server"
            }
            .into(),
        });
        if cfg!(any(target_os = "linux", target_os = "windows")) {
            choices.push(EngineChoice {
                index: 4,
                name: "Intel NPU",
                selectable: env.npu && env.engine,
                status: if env.npu {
                    "已检测到设备"
                } else {
                    "尚未验证"
                },
                detail: if env.npu {
                    "已检测到 Intel 加速设备与 Python；首次使用需准备运行环境"
                } else {
                    "需要 Intel NPU、驱动及 Python；可在运行环境中执行完整诊断"
                }
                .into(),
            });
        }
        let api_configured = (!self.value(Field::AsrKey, cx).is_empty()
            || course2md::config::asr_api_key_from_env().is_some())
            && !self.value(Field::AsrUrl, cx).is_empty()
            && !self.value(Field::AsrModel, cx).is_empty();
        choices.push(EngineChoice {
            index: 5,
            name: "使用云端服务",
            selectable: true,
            status: if api_configured {
                "已填写配置"
            } else {
                "需要配置"
            },
            detail: "音频会发送到你选择的云端服务".into(),
        });
        choices.push(EngineChoice {
            index: 6,
            name: "仅使用字幕",
            selectable: true,
            status: "无需识别模型",
            detail: "读取视频字幕；没有字幕时会提示，不自动转写".into(),
        });
        choices
    }

    pub fn engine_panel(&self, cx: &mut Context<Self>) -> Div {
        let selected = if self.editing_options().source_mode == 1 {
            6
        } else {
            self.editing_options().provider
        };
        let choices = self.engine_choices(cx);
        let local = choices
            .iter()
            .find(|c| (1..=4).contains(&c.index) && c.selectable && c.index == selected)
            .or_else(|| {
                choices
                    .iter()
                    .find(|c| (1..=4).contains(&c.index) && c.selectable)
            });
        let local_index = local.map(|c| c.index);
        let mut visible = Vec::new();
        if let Some(local) = local
            && !self.show_engine_details
        {
            visible.push(EngineChoice {
                index: local.index,
                name: "在本机识别",
                status: "推荐",
                detail: "音频留在这台电脑上".into(),
                selectable: true,
            });
        }
        if self.show_engine_details {
            visible.extend(
                self.engine_choices(cx)
                    .into_iter()
                    .filter(|c| {
                        c.selectable
                            && ((1..=4).contains(&c.index)
                                || (self.setup_open && local_index.is_some() && c.index == 6))
                    })
                    .map(|mut c| {
                        c.status = "";
                        c.detail = match c.index {
                            1 => "适合这台 Mac",
                            2 => "使用显卡加速",
                            3 => "使用处理器识别",
                            4 => "使用神经处理器",
                            _ => "视频没有字幕时不转写",
                        }
                        .into();
                        c
                    }),
            );
        }
        visible.extend(choices.into_iter().filter(|c| {
            c.index == 5 || (self.setup_open && local_index.is_none() && c.index == 6)
        }));
        v_flex()
            .gap_2()
            .when(self.environment.is_none(), |v| {
                v.child("正在检测本机识别方式…")
            })
            .children(visible.into_iter().enumerate().map(|(row, choice)| {
                let index = choice.index;
                let selected = selected == index;
                let content = v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(choice.name),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(MUTED))
                                    .child(choice.status),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(MUTED))
                            .whitespace_normal()
                            .child(choice.detail),
                    );
                if choice.selectable {
                    Button::new(("engine-choice", row))
                        .w_full()
                        .h_auto()
                        .p_3()
                        .selected(selected)
                        .toggled(selected)
                        .accessibility_label(format!(
                            "{}{}{}",
                            choice.name,
                            if choice.status.is_empty() {
                                String::new()
                            } else {
                                format!("，{}", choice.status)
                            },
                            if selected { "，已选择" } else { "" }
                        ))
                        .child(
                            h_flex()
                                .w_full()
                                .gap_3()
                                .child(
                                    div()
                                        .size(px(16.))
                                        .flex_shrink_0()
                                        .rounded_full()
                                        .border_2()
                                        .border_color(rgb(if selected { BLUE } else { CONTROL }))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .when(selected, |v| {
                                            v.child(div().size(px(8.)).rounded_full().bg(rgb(BLUE)))
                                        }),
                                )
                                .child(content),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let options = this.editing_options_mut();
                            if index == 6 {
                                options.source_mode = 1;
                            } else {
                                options.provider = index;
                                if options.source_mode == 1 {
                                    options.source_mode = 0;
                                }
                            }
                            if this.setup_open {
                                this.settings_status.clear();
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                } else {
                    h_flex()
                        .w_full()
                        .p_3()
                        .gap_3()
                        .rounded_md()
                        .bg(rgb(SIDEBAR))
                        .child(Icon::new(IconName::CircleX).size_4().text_color(rgb(MUTED)))
                        .child(content)
                        .into_any_element()
                }
            }))
            .child(
                Button::new("engine-details")
                    .ghost()
                    .self_start()
                    .label(if self.show_engine_details {
                        "收起高级设置"
                    } else {
                        "高级设置"
                    })
                    .icon(if self.show_engine_details {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_engine_details = !this.show_engine_details;
                        cx.notify();
                    })),
            )
            .when(self.show_engine_details, |v| {
                v.child(
                    Button::new("engine-help")
                        .label("识别诊断…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.setup_open && !this.finish_setup(false, window, cx) {
                                return;
                            }
                            window.close_dialog(cx);
                            this.settings_tab = 3;
                            this.navigate(Page::Settings, cx);
                        })),
                )
            })
    }

    pub fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup_open {
            return;
        }
        self.settings_deadline = None;
        self.setup_open = true;
        self.show_engine_details = false;
        if self.settings_options.provider == 0
            && let Some(choice) = self
                .engine_choices(cx)
                .into_iter()
                .find(|c| (1..=4).contains(&c.index) && c.selectable)
        {
            self.settings_options.provider = choice.index;
        }
        self.settings_status.clear();
        let desktop = cx.entity();
        let setup = cx.new(|cx| Setup {
            _observation: cx.observe(&desktop, |_, _, cx| cx.notify()),
            desktop,
        });
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            let confirm = weak.clone();
            dialog
                .title("设置 course2md")
                .w(px(620.))
                .margin_top(px(24.))
                .close_button(false)
                .overlay_closable(false)
                .child(setup.clone())
                .on_ok(move |_, window, cx| {
                    confirm
                        .update(cx, |this, cx| this.finish_setup(true, window, cx))
                        .unwrap_or(true)
                })
                .on_cancel(move |_, window, cx| {
                    weak.update(cx, |this, cx| this.finish_setup(false, window, cx))
                        .unwrap_or(true)
                })
        });
        cx.notify();
    }

    fn finish_setup(&mut self, apply: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Missing prerequisites lead to installation help, without claiming that
        // a speech configuration has been completed or saving its draft.
        let apply = apply && self.environment.as_ref().is_none_or(|env| env.ready());
        if apply {
            if self.settings_options.source_mode != 1 {
                let selected = self.settings_options.provider;
                if !self
                    .engine_choices(cx)
                    .iter()
                    .any(|c| c.index == selected && c.selectable)
                {
                    self.settings_status = "请选择一种识别方式，或选择仅使用字幕".into();
                    cx.notify();
                    return false;
                }
            }
            self.desktop_settings.setup_completed = true;
            self.save_settings(cx);
            if self.settings_status != "已自动保存" {
                self.desktop_settings.setup_completed = false;
                if let Some((field, _)) = self.missing_setting(cx) {
                    self.inputs[&field].update(cx, |input, cx| input.focus(window, cx));
                }
                return false;
            }
        } else {
            let mut config = self.config.clone();
            config.desktop.setup_completed = true;
            if !self.config_error {
                if let Err(error) = course2md::settings::save(&config) {
                    self.settings_status = format!("保存失败：{error:#}");
                    cx.notify();
                    return false;
                }
                self.config = config;
            }
            self.sync_settings(window, cx);
        }
        self.setup_open = false;
        self.settings_deadline = None;
        self.settings_snapshot = self.edited_settings(cx);
        self.settings_status.clear();
        if self.environment.as_ref().is_some_and(|env| !env.ready()) {
            self.settings_tab = 3;
            self.navigate(Page::Settings, cx);
        } else {
            self.navigate(Page::New, cx);
            cx.defer_in(window, |this, window, cx| {
                if this.page == Page::New {
                    this.begin_add(window, cx);
                }
            });
        }
        true
    }

    fn setup_content(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let form =
            v_flex()
                .gap_4()
                .child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("以后可随时在左下角「设置」中修改。"),
                )
                .when_some(self.environment.as_ref().filter(|e| !e.ready()), |v, _| {
                    v.child(
                        div()
                            .p_3()
                            .bg(rgb(0xfff0db))
                            .text_color(rgb(0x784600))
                            .child("还需要完成安装，才能开始转换。"),
                    )
                })
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("1. 选择识别方式"),
                )
                .child(self.engine_panel(cx))
                .when(
                    self.settings_options.provider == 5 && self.settings_options.source_mode != 1,
                    |v| {
                        v.child(self.input(Field::AsrUrl, "API 服务地址"))
                            .child(self.input(Field::AsrModel, "识别模型"))
                            .child(self.input(Field::AsrKey, "API Key"))
                    },
                )
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("2. 笔记保存位置"),
                )
                .child(
                    h_flex()
                        .items_end()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(self.input(Field::Output, "保存目录")),
                        )
                        .child(Button::new("setup-directory").label("选择目录…").on_click(
                            cx.listener(|this, _, window, cx| this.pick(true, window, cx)),
                        )),
                );
        v_flex()
            .gap_4()
            .text_size(px(14.))
            .text_color(rgb(INK))
            .child(
                div()
                    .id("setup-scroll")
                    .max_h((window.viewport_size().height - px(205.)).max(px(220.)))
                    .overflow_y_scroll()
                    .child(form),
            )
            .when(!self.settings_status.is_empty(), |v| {
                v.child(
                    div()
                        .text_color(rgb(0xa32626))
                        .child(self.settings_status.clone()),
                )
            })
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        Button::new("setup-later")
                            .label("稍后设置")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.finish_setup(false, window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("setup-done")
                            .primary()
                            .label(
                                if self.environment.as_ref().is_some_and(|env| !env.ready()) {
                                    "查看安装方法"
                                } else {
                                    "完成设置，添加课程"
                                },
                            )
                            .disabled(self.environment.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.finish_setup(true, window, cx) {
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}
