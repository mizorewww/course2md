//! Ollama 本地服务发现：探测本机服务与已下载模型，一键预填 AI 服务编辑器。
//! Ollama 的 OpenAI 兼容端点无需凭据，「登录」即服务发现 + 配置（见 core login/ollama）。
use super::*;
use crate::preferences::{Authentication, ServiceDraft, ServiceProtocol, ServicePurpose};
use crate::theme::*;

#[derive(Default)]
pub(crate) struct OllamaUi {
    generation: u64,
    working: bool,
    /// Ok((地址, 模型列表)) 已检测到；Err(说明) 未检测到；None 尚未检测
    status: Option<Result<(String, Vec<String>), String>>,
}

impl OllamaUi {
    /// 已经有过检测结果（或正在检测）：进入设置页时不重复发起
    pub(crate) fn checked(&self) -> bool {
        self.working || self.status.is_some()
    }
}

impl Desktop {
    /// 探测本机 Ollama 服务。阻塞网络请求不进 GPUI 执行器（见 spawn_blocking_io）。
    pub(crate) fn ollama_refresh(&mut self, cx: &mut Context<Self>) {
        if self.ollama.working {
            return;
        }
        self.ollama.working = true;
        self.ollama.generation = self.ollama.generation.wrapping_add(1);
        let generation = self.ollama.generation;
        let task = crate::spawn_blocking_io(|| {
            course2md::login::ollama::discover_for_desktop().map_err(|error| format!("{error:#}"))
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = task.recv().await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.ollama.generation != generation {
                    return;
                }
                this.ollama.working = false;
                this.ollama.status = Some(result);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 检测到的服务地址是否已有对应的 Ollama AI 服务条目；返回其版本 id
    fn ollama_added_service(&self) -> Option<String> {
        let Some(Ok((host, _))) = &self.ollama.status else {
            return None;
        };
        let detected = url::Url::parse(host).ok()?;
        let detected = (
            detected.host_str()?.to_owned(),
            detected.port_or_known_default(),
        );
        self.preferences
            .latest_versions()
            .values()
            .find(|version| {
                version.config.protocol == ServiceProtocol::OllamaChat
                    && !self
                        .preferences
                        .service_retired_in_snapshot(&version.service_id)
                    && url::Url::parse(&version.config.endpoint)
                        .ok()
                        .and_then(|url| {
                            Some((url.host_str()?.to_owned(), url.port_or_known_default()))
                        })
                        .as_ref()
                        == Some(&detected)
            })
            .map(|version| version.id.clone())
    }

    /// 用检测结果预填 AI 服务编辑器：协议、地址与首个模型都是受支持的默认值，
    /// 保存前仍可修改（编辑器是发布动作的唯一入口）。
    fn ollama_add_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft = ServiceDraft::new(ServicePurpose::Ai);
        draft.protocol = ServiceProtocol::OllamaChat;
        draft.authentication = Authentication::None;
        draft.name = "Ollama 本地服务".into();
        if let Some(Ok((host, models))) = &self.ollama.status {
            draft.address = host.clone();
            draft.model = models.first().cloned().unwrap_or_default();
        }
        self.open_service_draft(draft, None, None, window, cx);
    }

    /// 设置「服务与账号」页的 Ollama 卡片：状态、说明与动作（与 Codex 卡片同一组合）。
    pub(crate) fn ollama_account_section(&self, cx: &mut Context<Self>) -> Div {
        let id = "settings-ollama-account";
        let working = self.ollama.working;
        let (kind, label) = if working {
            (BadgeKind::Progress, "正在检测")
        } else {
            match &self.ollama.status {
                Some(Ok(_)) => (BadgeKind::Success, "已检测到本地服务"),
                Some(Err(_)) => (BadgeKind::Neutral, "未检测到本地服务"),
                None => (BadgeKind::Neutral, "尚未检测"),
            }
        };
        let detail = match &self.ollama.status {
            Some(Ok((host, models))) => {
                let preview: Vec<&str> = models
                    .iter()
                    .take(3)
                    .map(|model| model.as_str())
                    .collect();
                Some(if models.len() > preview.len() {
                    format!("{host} · {} 个模型：{} 等", models.len(), preview.join("、"))
                } else if preview.is_empty() {
                    format!("{host} · 还没有已下载的模型")
                } else {
                    format!("{host} · {} 个模型：{}", models.len(), preview.join("、"))
                })
            }
            _ => None,
        };
        let failure = match &self.ollama.status {
            Some(Err(message)) => Some(message.clone()),
            _ => None,
        };
        let added_version = self.ollama_added_service();
        let added = added_version.is_some();
        crate::settings_ui::settings_detail_group(
            SharedString::from(format!("{id}-heading")),
            icons::computer(),
            "Ollama 本地服务",
        )
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .items_center()
                .flex_wrap()
                .child(badge(kind).child(label))
                .when_some(detail, |row, detail| {
                    row.child(
                        accessible_text(SharedString::from(format!("{id}-detail")), detail)
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(TEXT_AUX)
                            .text_color(color(MUTED)),
                    )
                })
                .when(working, |row| {
                    row.child(crate::motion::spinner(
                        SharedString::from(format!("{id}-busy")),
                        cx,
                    ))
                }),
        )
        .child(
            supporting_info(
                SharedString::from(format!("{id}-policy")),
                "Ollama 在本机运行开源模型，无需账号与 API Key；检测到服务后可一键添加为 AI 服务。",
            )
            .w_full()
            .min_w_0()
            .whitespace_normal(),
        )
        .when_some(failure, |view, message| {
            view.child(
                accessible_text(SharedString::from(format!("{id}-failure")), message)
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .text_size(TEXT_AUX)
                    .text_color(color(MUTED)),
            )
        })
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .flex_wrap()
                .justify_end()
                // 已有对应服务条目时不再邀请重复添加：给状态与直达入口，
                // 服务列表仍是唯一管理面（review#1）
                .when_some(added_version, |row, version_id| {
                    row.child(badge(BadgeKind::Success).child("已添加为 AI 服务"))
                        .child(
                            quiet(SharedString::from(format!("{id}-open-added")))
                                .icon(icons::edit())
                                .label("查看服务")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_settings_service_editor(
                                        ServicePurpose::Ai,
                                        Some(version_id.clone()),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                })
                .when(!added, |row| {
                    row.child(
                        outline_pill(SharedString::from(format!("{id}-add")))
                            .icon(icons::add())
                            .label("添加 Ollama 服务")
                            .disabled(working)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.ollama_add_service(window, cx);
                            })),
                    )
                })
                .child(
                    quiet(SharedString::from(format!("{id}-recheck")))
                        .icon(icons::refresh())
                        .label("重新检测")
                        .disabled(working)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ollama_refresh(cx);
                        })),
                ),
        )
    }
}
