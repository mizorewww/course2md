//! Service editor: picker, editor dialog, connection testing and publishing.
use super::*;
use course2md::execution::Component;

impl Desktop {
    /// Ordinary service editing with explicit Save and Cancel actions.
    pub(super) fn service_editor_card(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let Some(editor) = &self.settings_ui.editor else {
            return v_flex();
        };
        let purpose = editor.form.draft.protocol.purpose();
        // A text-only title, worded like the 「添加语音服务」/「添加 AI 服务」 action that opened it.
        let editing = editor.form.draft.based_on.is_some();
        let title = match (purpose == ServicePurpose::Speech, editing) {
            (true, false) => course2md::tr!("添加语音服务"),
            (true, true) => course2md::tr!("编辑语音服务"),
            (false, false) => course2md::tr!("添加 AI 服务"),
            (false, true) => course2md::tr!("编辑 AI 服务"),
        };
        // A secondary page: the way back names where it leads.
        let parent = editor.origin_tab.unwrap_or(SettingsTab::Services).label();
        // As in the settings pane: a tonal round back button, then the title
        // in the page headline role under the category it returns to.
        let header = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(16. / 14.))
            .child(
                icon_button("service-editor-back", rems(48. / 14.), true, cx)
                    .icon(icons::arrow_left())
                    .accessibility_label(course2md::tr!("返回「{}」", parent))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_service_editor(window, cx)),
                    ),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        accessible_text("service-editor-parent", parent)
                            .text_size(TEXT_AUX)
                            .text_color(color(GRAY)),
                    )
                    .child(page_heading("service-editor-inline-title", title)),
            );
        v_flex()
            .w_full()
            .h_full()
            .min_h_0()
            .gap(rems(20. / 14.))
            .child(header)
            .child(self.service_editor_content(true, window, cx))
    }
    pub fn task_service_picker(&self, purpose: ServicePurpose, cx: &mut Context<Self>) -> Div {
        self.service_picker(purpose, true, cx)
    }
    pub fn selected_task_service(&self, purpose: ServicePurpose) -> Option<ServiceVersion> {
        let fixed = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.draft())
            .and_then(|draft| match purpose {
                ServicePurpose::Speech => draft.asr_service.clone(),
                ServicePurpose::Ai => draft.ai_service.clone(),
            });
        let defaults = self.preferences.default_refs();
        let id = fixed.or(match purpose {
            ServicePurpose::Speech => defaults.asr,
            ServicePurpose::Ai => defaults.llm,
        })?;
        self.preferences.version(&id).cloned()
    }
    pub(super) fn service_picker(
        &self,
        purpose: ServicePurpose,
        current_task: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let refs = self.preferences.default_refs();
        let current = if current_task {
            self.selected_task_service(purpose).map(|v| v.id)
        } else {
            match purpose {
                ServicePurpose::Speech => refs.asr,
                ServicePurpose::Ai => refs.llm,
            }
        };
        let latest: BTreeMap<String, ServiceVersion> = self
            .preferences
            .latest_versions()
            .into_iter()
            .filter(|(_, v)| {
                v.config.protocol.purpose() == purpose
                    && !self.preferences.service_retired_in_snapshot(&v.service_id)
            })
            .collect();
        let has_services = !latest.is_empty();
        let mut view = v_flex().w_full().min_w_0().gap_2();
        let current_version = current
            .as_deref()
            .and_then(|id| self.preferences.version(id))
            .filter(|version| {
                !self
                    .preferences
                    .service_retired_in_snapshot(&version.service_id)
            })
            .cloned();
        let old_current = current_version
            .as_ref()
            .filter(|version| {
                latest
                    .get(&version.service_id)
                    .is_some_and(|latest| latest.id != version.id)
            })
            .cloned();
        let mut choices = Vec::new();
        for version in latest.into_values() {
            if let Some(old) = old_current
                .as_ref()
                .filter(|old| old.service_id == version.service_id)
            {
                choices.push(old.clone());
            }
            choices.push(version);
        }
        let index = purpose as usize * 2 + current_task as usize;
        let controls = if choices.is_empty() {
            // Nothing to choose yet: the one action is adding a service.
            h_flex().w_full().justify_end().child(
                outline_pill(("configure-service", index), cx)
                    .settings_text()
                    .icon(icons::plus())
                    .label(match purpose {
                        ServicePurpose::Speech => course2md::tr!("添加语音服务"),
                        ServicePurpose::Ai => course2md::tr!("添加 AI 服务"),
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if current_task {
                            this.open_task_service_editor(purpose, window, cx)
                        } else {
                            this.open_settings_service_editor(purpose, None, window, cx);
                        }
                    })),
            )
        } else {
            let name = current_version
                .as_ref()
                .map(|version| version.config.name.clone())
                .unwrap_or_else(|| course2md::tr!("选择服务").into());
            let detail = current_version.as_ref().map(|version| {
                let state = if old_current.is_some() {
                    course2md::tr!(" · 当前使用修改前的配置")
                } else {
                    ""
                };
                format!(
                    "{} · {}{state}",
                    version.config.protocol.choice_label(),
                    version.config.model
                )
            });
            let menu_choices = choices
                .into_iter()
                .map(|version| {
                    let state = if let Some(old) = old_current
                        .as_ref()
                        .filter(|old| old.service_id == version.service_id)
                    {
                        if old.id == version.id {
                            course2md::tr!("修改前的配置").into()
                        } else {
                            let changed = version.changes_since(old);
                            if changed.is_empty() {
                                course2md::tr!("配置内容相同").into()
                            } else {
                                course2md::tr!("已修改：{}", changed.join(course2md::tr!("、")))
                            }
                        }
                    } else {
                        String::new()
                    };
                    (version, state)
                })
                .collect::<Vec<_>>();
            let entity = cx.entity().downgrade();
            let purpose_label = match purpose {
                ServicePurpose::Speech => course2md::tr!("语音服务"),
                ServicePurpose::Ai => course2md::tr!("AI 服务"),
            };
            let field = SelectField::new(("service-choice", index), name, move |menu, _, _| {
                let manage = entity.clone();
                menu_choices
                    .iter()
                    .fold(
                        menu.scrollable(true).label(course2md::tr!("已添加的服务")),
                        |menu, (version, state)| {
                            let id = version.id.clone();
                            let name = version.config.name.clone();
                            let mut detail = format!(
                                "{} · {}",
                                version.config.protocol.choice_label(),
                                version.config.model
                            );
                            if !state.is_empty() {
                                detail = format!("{detail} · {state}");
                            }
                            let entity = entity.clone();
                            menu.item(
                                PopupMenuItem::element(move |_, _| {
                                    crate::settings_ui::service_form::two_line_item(
                                        name.clone(),
                                        detail.clone(),
                                    )
                                })
                                .icon(crate::settings_ui::service_form::protocol_icon(
                                    version.config.protocol,
                                ))
                                .checked(current.as_ref() == Some(&id))
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |this, cx| {
                                        let selectable =
                                            this.preferences.version(&id).is_some_and(|version| {
                                                version.config.protocol.purpose() == purpose
                                                    && !this
                                                        .preferences
                                                        .is_service_retired(&version.service_id)
                                            });
                                        if selectable {
                                            this.bind_service(
                                                purpose,
                                                Some(id.clone()),
                                                current_task,
                                                cx,
                                            );
                                        }
                                    });
                                }),
                            )
                        },
                    )
                    .separator()
                    .item(
                        // The menu's way out is an action, drawn in the accent.
                        PopupMenuItem::element(move |_, _| {
                            h_flex()
                                .gap(rems(12. / 14.))
                                .text_color(color(ACCENT))
                                .font_weight(FontWeight::MEDIUM)
                                .child(icons::plus().size(rems(18. / 14.)))
                                .child(match purpose {
                                    ServicePurpose::Speech => course2md::tr!("添加或管理语音服务…"),
                                    ServicePurpose::Ai => course2md::tr!("添加或管理 AI 服务…"),
                                })
                        })
                        .on_click(move |_, window, cx| {
                            let _ = manage.update(cx, |this, cx| this.manage_services(window, cx));
                        }),
                    )
            })
            .when_some(detail, |field, detail| field.detail(detail))
            .when_some(current_version.as_ref(), |field, version| {
                field.icon(crate::settings_ui::service_form::protocol_icon(
                    version.config.protocol,
                ))
            })
            .when(current_task, |field| field.label(purpose_label));
            h_flex().w_full().min_w_0().child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(!current_task, |slot| slot.flex().justify_end())
                    .child(
                        self.reveal_setting(("service-choice-reveal", index), field)
                            .when(!current_task, |field| field.max_w(INLINE_SELECT_WIDTH)),
                    ),
            )
        };
        view = view.child(controls);
        if current_task
            && self
                .workspace
                .as_ref()
                .and_then(|w| w.state.draft())
                .is_some_and(|draft| match purpose {
                    ServicePurpose::Speech => draft.asr_service.is_some(),
                    ServicePurpose::Ai => draft.ai_service.is_some(),
                })
        {
            view = view.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        accessible_text(
                            ("service-only-this-note", purpose as usize),
                            course2md::tr!("仅用于这次笔记"),
                        )
                        .text_sm()
                        .text_color(color(GRAY)),
                    )
                    .child(
                        control(("inherit-default-service", purpose as usize))
                            .settings_text()
                            .ghost()
                            .icon(icons::refresh())
                            .label(course2md::tr!("恢复默认"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.bind_service(purpose, None, true, cx);
                            })),
                    ),
            );
        }
        if let Some((message, true)) = self.settings_ui.feedback.get(&PreferenceGroup::Services) {
            view = view.child(
                accessible_text(("task-service-error", purpose as usize), message.clone())
                    .text_sm()
                    .text_color(color(DANGER)),
            );
        }
        if current_task {
            view
        } else if !has_services {
            setting_surface().child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(accessible_text(
                                ("default-service-label", purpose as usize),
                                if purpose == ServicePurpose::Ai {
                                    course2md::tr!("AI 服务")
                                } else {
                                    course2md::tr!("语音服务")
                                },
                            ))
                            .child(
                                accessible_text(
                                    ("default-service-missing", purpose as usize),
                                    if purpose == ServicePurpose::Ai {
                                        course2md::tr!("尚未添加，校对和摘要需要先添加一个服务")
                                    } else {
                                        course2md::tr!("尚未添加，请先添加一个语音服务")
                                    },
                                )
                                .text_size(TEXT_SETTINGS_AUX)
                                .text_color(color(GRAY)),
                            ),
                    )
                    .child(div().flex_shrink_0().child(view)),
            )
        } else {
            settings_row(
                ("default-service-label", purpose as usize),
                if purpose == ServicePurpose::Speech {
                    course2md::tr!("语音服务")
                } else {
                    course2md::tr!("AI 服务")
                },
                // Nothing is sent anywhere until a service exists to choose.
                if purpose == ServicePurpose::Ai && has_services {
                    if self.preferences.generation().ai.enabled
                        && self.preferences.generation().ai.vision
                    {
                        course2md::tr!("文字及对应截图会发送到所选服务")
                    } else {
                        course2md::tr!("校对与摘要会把文字发送到所选服务")
                    }
                } else {
                    ""
                },
                view,
            )
        }
    }
    /// The services list is where services are added, tested and removed.
    fn manage_services(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == Page::Settings {
            self.select_settings_tab(SettingsTab::Services, window, cx);
        } else {
            self.settings_tab = SettingsTab::Services;
            self.open_settings(window, cx);
        }
    }
    fn bind_service(
        &mut self,
        purpose: ServicePurpose,
        id: Option<String>,
        current_task: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = if current_task {
            self.workspace
                .as_mut()
                .ok_or_else(|| anyhow!(course2md::tr!("当前笔记信息暂时不可用")))
                .and_then(|workspace| {
                    workspace.transaction(|state| {
                        let draft = state
                            .draft_mut()
                            .context(course2md::tr!("找不到当前笔记"))?;
                        match purpose {
                            ServicePurpose::Speech => draft.asr_service = id,
                            ServicePurpose::Ai => draft.ai_service = id,
                        };
                        Ok(())
                    })
                })
        } else {
            self.preferences.set_default_service(purpose, id.as_deref())
        };
        match result {
            Ok(()) => {
                self.set_settings_feedback(
                    PreferenceGroup::Services,
                    course2md::tr!("服务选择已保存").into(),
                    false,
                    cx,
                );
                self.advance_conversion_when_ready(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.set_settings_feedback(
                    PreferenceGroup::Services,
                    course2md::tr!("服务选择尚未保存：{}", format!("{error:#}")),
                    true,
                    cx,
                );
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn open_task_service_editor(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.save_current_draft(cx) {
            return;
        }
        let Some(target) = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.draft())
            .map(|d| d.id.clone())
        else {
            return;
        };
        let draft = self
            .selected_task_service(purpose)
            .filter(|version| {
                !self
                    .preferences
                    .service_retired_in_snapshot(&version.service_id)
            })
            .map(|v| ServiceDraft::from_version(&v))
            .unwrap_or_else(|| ServiceDraft::new(purpose));
        self.open_service_draft(draft, Some(target), None, window, cx);
    }
    pub(crate) fn open_reprocess_service_editor(
        &mut self,
        task_id: String,
        components: Vec<Component>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.state.tasks.iter().find(|task| task.id == task_id))
        else {
            self.workspace_error =
                Some(course2md::tr!("原任务已不可用，尚未修改服务或创建补做任务").into());
            cx.notify();
            return;
        };
        let draft = task
            .plan
            .ai_service
            .as_deref()
            .and_then(|id| self.preferences.version(id))
            .filter(|version| {
                !self
                    .preferences
                    .service_retired_in_snapshot(&version.service_id)
            })
            .map(ServiceDraft::from_version)
            .unwrap_or_else(|| ServiceDraft::new(ServicePurpose::Ai));
        self.open_service_draft(draft, None, Some((task_id, components)), window, cx);
    }
    pub(crate) fn open_settings_service_editor(
        &mut self,
        purpose: ServicePurpose,
        version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let draft = version
            .as_deref()
            .and_then(|id| self.preferences.version(id))
            .map(ServiceDraft::from_version)
            .unwrap_or_else(|| ServiceDraft::new(purpose));
        self.open_service_draft(draft, None, None, window, cx)
    }
    pub(crate) fn open_service_draft(
        &mut self,
        draft: ServiceDraft,
        target: Option<String>,
        repair_task: Option<(String, Vec<Component>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(editor) = &self.settings_ui.editor {
            if editor.target.is_some() || editor.repair_task.is_some() {
                return false;
            }
            if editor.form.draft.id == draft.id
                && editor.target == target
                && editor.repair_task == repair_task
            {
                self.settings_tab = SettingsTab::Services;
                self.navigate(Page::Settings, cx);
                self.prepare_settings_view(window, cx);
                self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(0.)));
                self.focus_service_field(FormField::Name, window, cx);
                return true;
            }
            // A second action must not reuse the previous editor's endpoint or scope.
            // Switching forms cancels the earlier edit; only Save can publish it.
            self.close_service_editor(window, cx);
        }
        let title = if repair_task.is_some() {
            course2md::tr!("修复服务并补做")
        } else {
            match (draft.protocol.purpose(), target.is_some()) {
                (ServicePurpose::Speech, true) => course2md::tr!("为这次笔记设置语音识别服务"),
                (ServicePurpose::Ai, true) => course2md::tr!("为这次笔记设置 AI 服务"),
                (ServicePurpose::Speech, false) => course2md::tr!("设置语音识别服务"),
                (ServicePurpose::Ai, false) => course2md::tr!("设置 AI 服务"),
            }
        };
        let kind = if draft.protocol.purpose() == ServicePurpose::Speech {
            TestKind::Speech
        } else {
            TestKind::Proofread
        };
        let evidence = draft.configuration().ok().and_then(|config| {
            self.preferences
                .test_evidence(&config, kind.contract())
                .cloned()
        });
        let inline = target.is_none() && repair_task.is_none();
        let defaults = self.preferences.default_refs();
        let default = match draft.protocol.purpose() {
            ServicePurpose::Speech => defaults.asr,
            ServicePurpose::Ai => defaults.llm,
        };
        // New notes follow a service when nothing else is the default yet, or
        // when it already is the default being edited.
        let also_default = default
            .as_deref()
            .and_then(|id| self.preferences.version(id))
            .is_none_or(|version| {
                version.service_id == draft.service_id
                    || self
                        .preferences
                        .service_retired_in_snapshot(&version.service_id)
            });
        let draft_protocol = draft.protocol;
        let origin_tab =
            (inline && self.page == Page::Settings && self.settings_tab != SettingsTab::Services)
                .then_some(self.settings_tab);
        let form = ServiceForm::new(
            FormOwner::Editor,
            &[
                FormField::Name,
                FormField::Address,
                FormField::Model,
                FormField::Key,
            ],
            draft,
            window,
            cx,
        );
        self.settings_ui.editor = Some(ServiceEditor {
            form,
            target,
            repair_task,
            scroll: ScrollHandle::new(),
            also_default,
            return_focus: window.focused(cx),
            status: None,
            test_kind: kind,
            test_running: None,
            evidence,
            received_tests: BTreeMap::new(),
            pending_binding: None,
            saved_configuration: None,
            test_details_open: false,
            save_failed: false,
            vault_busy: false,
            origin_tab,
        });
        match draft_protocol {
            ServiceProtocol::CodexResponses => {
                self.codex_refresh_status(crate::codex_ui::CodexSurface::Editor, cx)
            }
            ServiceProtocol::OllamaChat => self.ollama_prepare_form(cx),
            _ => {}
        }
        if inline {
            self.settings_tab = SettingsTab::Services;
            self.navigate(Page::Settings, cx);
            self.prepare_settings_view(window, cx);
            self.scrolls[Page::Settings as usize].set_offset(point(px(0.), px(0.)));
            self.focus_service_field(FormField::Name, window, cx);
            cx.notify();
            return true;
        }
        let desktop = cx.entity();
        let content = crate::views::DesktopView::observing(desktop, cx, |desktop, window, cx| {
            desktop
                .service_editor_content(false, window, cx)
                .into_any_element()
        });
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |sheet, window, _| {
            let closed = weak.clone();
            let cancel = weak.clone();
            let submit = weak.clone();
            sheet
                .material("service-editor-dialog-title", title)
                .w((window.rem_size() * (620. / 14.)).min(window.viewport_size().width - px(48.)))
                .margin_top(task_dialog_top(window))
                .overlay_closable(false)
                .close_button(false)
                .child(content.clone())
                .on_ok(move |_, window, cx| {
                    let _ = submit.update(cx, |this, cx| this.publish_open_service(window, cx));
                    false
                })
                .on_cancel(move |_, window, cx| {
                    let _ = cancel.update(cx, |this, cx| this.close_service_editor(window, cx));
                    false
                })
                .on_close(move |_, window, cx| {
                    let _ = closed.update(cx, |this, cx| this.restore_service_focus(window, cx));
                })
        });
        self.focus_service_field(FormField::Address, window, cx);
        cx.notify();
        true
    }
    /// Capture builds: `COURSE2MD_CAPTURE_SERVICE_EDITOR=ai|speech|dialog|confirm`
    /// opens the service editor once Settings › 服务与账号 shows (`ai` and
    /// `speech` as its settings page, `dialog` as the per-note dialog,
    /// `confirm` the delete confirmation for a placeholder service), so
    /// `PAGES=settings:services` captures it. Nothing is saved.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_service_editor(&mut self, cx: &mut Context<Self>) {
        thread_local! {
            static OPENED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if self.page != Page::Settings
            || self.settings_tab != SettingsTab::Services
            || self.settings_ui.editor.is_some()
            || OPENED.get()
        {
            return;
        }
        let Ok(kind) = std::env::var("COURSE2MD_CAPTURE_SERVICE_EDITOR") else {
            return;
        };
        OPENED.set(true);
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let _ = cx.update_window(window, |_, window, cx| {
                let _ = this.update(cx, |this, cx| match kind.as_str() {
                    "speech" => {
                        this.open_settings_service_editor(ServicePurpose::Speech, None, window, cx);
                    }
                    "confirm" => this.confirm_delete_service("capture".into(), window, cx),
                    "dialog" => {
                        this.open_service_draft(
                            ServiceDraft::new(ServicePurpose::Ai),
                            Some("capture".into()),
                            None,
                            window,
                            cx,
                        );
                    }
                    _ => {
                        this.open_settings_service_editor(ServicePurpose::Ai, None, window, cx);
                    }
                });
            });
        })
        .detach();
    }
    fn focus_service_field(&self, field: FormField, window: &mut Window, cx: &mut App) {
        if let Some(editor) = &self.settings_ui.editor {
            editor.form.focus(field, window, cx);
        }
    }
    pub(super) fn service_editor_content(
        &self,
        inline: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(editor) = &self.settings_ui.editor else {
            return div().into_any_element();
        };
        let protocol = editor.form.draft.protocol;
        let busy = editor.test_running.is_some() || editor.vault_busy;
        let awaiting_binding = editor.pending_binding.is_some();
        let configuration_saved = editor.saved_configuration.is_some() || awaiting_binding;
        let repair_submit_label = if awaiting_binding {
            course2md::tr!("重试补做")
        } else if configuration_saved {
            course2md::tr!("开始补做")
        } else {
            course2md::tr!("保存并补做")
        };
        let repair_refusal = self.repair_service_refusal(cx);
        let repair_refused = repair_refusal.is_some();
        let fixed_test_failure = if busy {
            None
        } else {
            repair_refusal.clone().or_else(|| {
                editor
                    .evidence
                    .as_ref()
                    .filter(|evidence| evidence.outcome != preferences::TestOutcome::Passed)
                    .map(|evidence| (editor.test_kind, evidence.clone()))
            })
        };
        let test_label = match editor.test_kind {
            TestKind::Speech => course2md::tr!("语音"),
            TestKind::Proofread => course2md::tr!("校对"),
            TestKind::Summary => course2md::tr!("摘要"),
            TestKind::Vision => course2md::tr!("截图校对"),
        };
        let test_outcome_unknown = editor
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.outcome == preferences::TestOutcome::OutcomeUnknown);
        let mut view = v_flex()
            .id("service-editor-body")
            .w_full()
            .min_w_0()
            .min_h_0()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&editor.scroll)
            .role(if inline { Role::Group } else { Role::Dialog })
            .aria_label(if protocol.purpose() == ServicePurpose::Speech {
                course2md::tr!("设置语音识别服务")
            } else {
                course2md::tr!("设置 AI 服务")
            })
            .gap_6()
            // The outward-paint allowance is cancelled outside so cards keep the pane edges.
            .mx(rems(-0.25))
            .px_1()
            // The scroll container leaves room for the first and last cards' shadow and focus paint (system.md: clipped containers reserve outward paint).
            .pt_1()
            .pb_3();
        use crate::settings_ui::settings_switch_row;
        // One card as on the canvas: the type select, the fields that type
        // needs, then whether new notes use this service.
        let hint = |id: &'static str, message: String| {
            accessible_text(id, message)
                .px(rems(16. / 14.))
                .text_size(TEXT_SETTINGS_AUX)
                .text_color(color(GRAY))
        };
        let mut fields = v_flex().w_full().min_w_0().gap(rems(24. / 14.));
        // Codex account connection: the login decides the model catalog and availability.
        if protocol == ServiceProtocol::CodexResponses {
            fields = fields.child(
                div()
                    .p(rems(16. / 14.))
                    .rounded(RADIUS_CARD)
                    .border_1()
                    .border_color(color(HAIRLINE))
                    .child(self.codex_account_section(
                        crate::codex_ui::CodexSurface::Editor,
                        awaiting_binding,
                        cx,
                    )),
            );
        }
        fields = fields.child(self.service_field(
            FormField::Name,
            course2md::tr!("名称"),
            None,
            window,
            cx,
        ));
        // Codex has a fixed endpoint and its login holds the credential: no address or key fields.
        if protocol != ServiceProtocol::CodexResponses {
            let address = editor.form.value(FormField::Address, cx);
            let endpoint = preferences::normalize_endpoint(&address, protocol)
                .ok()
                .filter(|endpoint| endpoint != address.trim());
            let ollama = protocol == ServiceProtocol::OllamaChat;
            fields = fields.child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(self.service_field(
                        FormField::Address,
                        if ollama {
                            course2md::tr!("地址")
                        } else {
                            course2md::tr!("接口地址")
                        },
                        None,
                        window,
                        cx,
                    ))
                    .when(ollama, |item| {
                        item.child(hint(
                            "service-ollama-address",
                            course2md::tr!("本机运行的 Ollama 会自动填好").into(),
                        ))
                    })
                    .when_some(endpoint.filter(|_| !ollama), |item, endpoint| {
                        item.child(hint(
                            "service-actual-endpoint",
                            course2md::tr!("实际地址：{}", endpoint),
                        ))
                    }),
            );
        }
        if !protocol.keyless() {
            let visibility = self
                .service_key_visibility(FormOwner::Editor, "toggle-service-key", cx)
                .settings_text()
                .into_any_element();
            let mut key = v_flex()
                .w_full()
                .gap_1()
                .child(self.service_field(FormField::Key, "API Key", Some(visibility), window, cx))
                .child(hint(
                    "service-key-storage",
                    if editor.form.draft.credential.is_some() {
                        course2md::tr!("已保存在系统钥匙串里，留空则保留").into()
                    } else {
                        course2md::tr!("保存在系统钥匙串里，不会写进配置文件").into()
                    },
                ));
            if let Some(source) = &editor.form.draft.credential_source {
                key = key.child(hint(
                    "service-credential-source",
                    course2md::tr!("密钥来源：{}", source),
                ));
            }
            for (index, name) in
                preferences::Store::available_environment_credentials(protocol.purpose())
                    .into_iter()
                    .enumerate()
            {
                key = key.child(
                    text_button(("capture-environment-key", index), cx)
                        .settings_text()
                        .icon(icons::content_copy())
                        .label(course2md::tr!("使用环境变量 {}", name))
                        .disabled(awaiting_binding || editor.vault_busy)
                        .self_start()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(draft) = this
                                .settings_ui
                                .editor
                                .as_ref()
                                .map(|editor| editor.form.current_draft(cx))
                            else {
                                return;
                            };
                            let secret = match preferences::Store::environment_secret(
                                draft.protocol.purpose(),
                                name,
                            ) {
                                Ok(secret) => secret,
                                Err(error) => {
                                    if let Some(editor) = &mut this.settings_ui.editor {
                                        editor.status = Some(course2md::tr!(
                                            "凭据尚未保存：{}",
                                            format!("{error:#}")
                                        ));
                                    }
                                    cx.notify();
                                    return;
                                }
                            };
                            if let Some(editor) = &mut this.settings_ui.editor {
                                editor.vault_busy = true;
                            }
                            this.run_vault_step(
                                cx,
                                move |vault| preferences::Store::insert_credential(vault, secret),
                                move |this, stored, window, cx| {
                                    this.finish_environment_credential(
                                        draft, name, stored, window, cx,
                                    )
                                },
                            );
                            cx.notify();
                        })),
                );
            }
            fields = fields.child(key);
        }
        let model_focused = editor
            .form
            .input(FormField::Model)
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        fields = fields.child(self.reveal_setting(
            ("setting-field-reveal", 8 + FormField::Model as usize),
            outlined_field_label(
                ("setting-field-label", 8 + FormField::Model as usize),
                course2md::tr!("模型"),
                TEXT_AUX,
                color(if model_focused { ACCENT } else { GRAY }),
                self.service_model_field(FormOwner::Editor, "service-models", awaiting_binding, cx),
            ),
        ));
        // Settings shows the form as connected tiles on the ground: the type,
        // the fields that type needs, then whether new notes use it. A dialog
        // is already one surface, so there the same parts are plain sections.
        let protocol_field = self.reveal_setting(
            "service-protocol-reveal",
            self.service_type_field(FormOwner::Editor, "service-protocol", awaiting_binding, cx)
                .label(course2md::tr!("服务类型")),
        );
        let default_switch = (editor.repair_task.is_none()).then(|| {
            settings_switch_row(
                course2md::tr!("设为默认服务"),
                course2md::tr!("新笔记默认使用这个服务"),
                Switch::new("service-also-default")
                    .checked(editor.also_default)
                    .disabled(awaiting_binding)
                    .on_click(cx.listener(|this, value, _, cx| {
                        if let Some(editor) = &mut this.settings_ui.editor {
                            editor.also_default = *value;
                        }
                        cx.notify();
                    })),
            )
        });
        let mut parts = vec![div().pt(rems(8. / 14.)).child(protocol_field), fields];
        parts.extend(default_switch.map(|row| div().child(row)));
        let count = parts.len();
        let connection = if inline {
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap(TILE_GAP)
                .children(parts.into_iter().enumerate().map(|(index, part)| {
                    list_tile(index, count)
                        .px(SETTING_ITEM_INSET)
                        .py(rems(16. / 14.))
                        .child(part)
                }))
        } else {
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap(rems(24. / 14.))
                .children(parts)
        };
        view = view.child(connection);
        if let Some(testing) = self.service_test_section(cx) {
            view = view.child(testing);
        }
        if let Some(status) = &editor.status
            && !busy
        {
            view = view.child(accessible_text("service-editor-status", status.clone()).text_sm());
        }
        // As on the canvas: the form's actions sit together at its trailing edge.
        let adding = inline && editor.form.draft.based_on.is_none();
        let actions = h_flex()
            .w_full()
            .flex_shrink_0()
            .gap_2()
            .flex_wrap()
            .justify_end()
            .child(
                text_button("close-service-editor", cx)
                    .settings_text()
                    .label(if configuration_saved {
                        course2md::tr!("关闭")
                    } else {
                        course2md::tr!("取消")
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_service_editor(window, cx);
                        if !inline {
                            window.close_dialog(cx);
                        }
                    })),
            )
            .child(
                outline_pill("run-service-test", cx)
                    .settings_text()
                    .icon(icons::science())
                    .label(if busy {
                        course2md::tr!("正在测试{}…", test_label)
                    } else {
                        match editor
                            .evidence
                            .as_ref()
                            .map(|evidence| evidence.outcome.clone())
                        {
                            Some(preferences::TestOutcome::Passed) => {
                                course2md::tr!("{}通过 · 重测", test_label)
                            }
                            Some(preferences::TestOutcome::OutcomeUnknown) => {
                                course2md::tr!("{}结果未确认 · 重试", test_label)
                            }
                            Some(
                                preferences::TestOutcome::AuthenticationRefused
                                | preferences::TestOutcome::ModelRefused
                                | preferences::TestOutcome::ContractMismatch
                                | preferences::TestOutcome::SampleMismatch,
                            ) => course2md::tr!("{}未通过 · 重试", test_label),
                            _ => course2md::tr!("测试连接").into(),
                        }
                    })
                    .loading(busy)
                    .disabled(busy || awaiting_binding)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.start_service_test(window, cx)),
                    ),
            )
            .when(editor.repair_task.is_some(), |row| {
                row.child(
                    text_button("save-repair-service-only", cx)
                        .settings_text()
                        .icon(icons::save())
                        .label(course2md::tr!("仅保存"))
                        .disabled(busy || awaiting_binding || editor.saved_configuration.is_some())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.publish_service_editor(false, window, cx)
                        })),
                )
            })
            .child(
                primary_pill("save-service")
                    .settings_text()
                    .label(if editor.repair_task.is_some() {
                        repair_submit_label
                    } else if awaiting_binding {
                        course2md::tr!("重试保存本次选择")
                    } else if adding {
                        course2md::tr!("添加")
                    } else {
                        course2md::tr!("保存")
                    })
                    .loading(editor.vault_busy)
                    .disabled(
                        editor.vault_busy
                            || editor.repair_task.is_some() && (busy || repair_refusal.is_some()),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.publish_open_service(window, cx)),
                    ),
            );
        let footer = self.service_editor_footer(
            actions,
            fixed_test_failure,
            repair_submit_label,
            test_outcome_unknown,
            repair_refused,
        );
        v_flex()
            .w_full()
            .min_w_0()
            .min_h_0()
            .gap_3()
            .when(inline, |view| view.h_full().flex_1())
            .when(!inline, |view| {
                view.max_h(
                    (window.viewport_size().height
                        - task_dialog_top(window)
                        - window.rem_size() * 4.
                        - px(64.))
                    .max(px(120.)),
                )
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(view)
                    .child(crate::backend::vertical_scrollbar(&editor.scroll)),
            )
            // The dialog's own 24px padding already ends the surface.
            .child(footer.when(!inline, |footer| footer.pb_0()))
            .into_any_element()
    }

    /// Test-kind choice, busy state, result card and expandable details.
    /// None when there is nothing to show yet (speech services before a test),
    /// so an empty 「检查服务」 heading never appears.
    fn service_test_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.settings_ui.editor.as_ref()?;
        let protocol = editor.form.draft.protocol;
        let busy = editor.test_running.is_some() || editor.vault_busy;
        let awaiting_binding = editor.pending_binding.is_some();
        if protocol.purpose() != ServicePurpose::Ai && !busy && editor.evidence.is_none() {
            return None;
        }
        let mut testing = crate::settings_ui::setting_surface();
        if protocol.purpose() == ServicePurpose::Ai {
            testing = testing.child(
                div().self_start().max_w_full().child(
                    self.setting_choices(
                        "service-test-purpose",
                        course2md::tr!("要测试的服务能力"),
                    )
                    .options(
                        [TestKind::Proofread, TestKind::Summary, TestKind::Vision]
                            .into_iter()
                            .map(|kind| (kind.contract(), kind.label())),
                    )
                    .icon(TestKind::Proofread.contract(), icons::edit())
                    .icon(TestKind::Summary.contract(), icons::summarize())
                    .icon(TestKind::Vision.contract(), icons::image())
                    .selected(editor.test_kind.contract())
                    .disabled(busy)
                    .on_change(cx.listener(
                        move |this, selected: &SharedString, _, cx| {
                            let Some(kind) =
                                [TestKind::Proofread, TestKind::Summary, TestKind::Vision]
                                    .into_iter()
                                    .find(|kind| kind.contract() == selected.as_ref())
                            else {
                                return;
                            };
                            let evidence = this
                                .settings_ui
                                .editor
                                .as_ref()
                                .filter(|editor| editor.form.typed_key(cx).is_empty())
                                .and_then(|editor| {
                                    let config = editor.form.draft.configuration().ok()?;
                                    editor
                                        .received_tests
                                        .get(&config.fingerprint(kind.contract()))
                                        .or_else(|| {
                                            this.preferences.test_evidence(&config, kind.contract())
                                        })
                                        .cloned()
                                });
                            if let Some(editor) = &mut this.settings_ui.editor {
                                editor.test_kind = kind;
                                editor.evidence = evidence;
                            }
                            cx.notify();
                        },
                    )),
                ),
            );
        }
        if busy {
            testing = testing.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(crate::motion::spinner("service-test-busy", cx))
                    .child(
                        accessible_text(
                            "service-test-busy-label",
                            editor
                                .status
                                .clone()
                                .unwrap_or_else(|| course2md::tr!("正在测试服务…").into()),
                        )
                        .text_sm(),
                    ),
            );
        }
        if let Some(evidence) = &editor.evidence {
            let (kind, label) = match evidence.outcome {
                preferences::TestOutcome::Passed => (
                    BadgeKind::Success,
                    course2md::tr!("{}测试通过", editor.test_kind.label()),
                ),
                preferences::TestOutcome::OutcomeUnknown => (
                    BadgeKind::Warning,
                    course2md::tr!("结果尚未确认").to_owned(),
                ),
                preferences::TestOutcome::NotSent => {
                    (BadgeKind::Neutral, course2md::tr!("测试未发送").to_owned())
                }
                _ => (BadgeKind::Danger, course2md::tr!("测试未通过").to_owned()),
            };
            let mut result = v_flex()
                .gap_3()
                .p_4()
                .rounded(RADIUS_CARD)
                .bg(color(match evidence.outcome {
                    preferences::TestOutcome::Passed => SUCCESS_BG,
                    preferences::TestOutcome::OutcomeUnknown => WARNING_BG,
                    preferences::TestOutcome::NotSent => INSET,
                    _ => DANGER_BG,
                }))
                .child(badge(kind).text_size(TEXT_SETTINGS_AUX).child(label))
                .when(
                    evidence.outcome != preferences::TestOutcome::Passed,
                    |view| {
                        view.child(
                            accessible_text("service-test-result", evidence.message.clone())
                                .text_sm(),
                        )
                    },
                );
            if evidence.outcome == preferences::TestOutcome::Passed
                && editor.repair_task.is_none()
                && awaiting_binding
            {
                result = result.child(
                    accessible_text(
                        "service-tested-save-state",
                        course2md::tr!("本次笔记的选择尚未保存"),
                    )
                    .text_sm(),
                );
            }
            result = result.child(
                accessible_text(
                    "service-test-time",
                    course2md::tr!(
                        "测试时间：{}",
                        crate::reader_navigation::timestamp_local(evidence.tested_at * 1000)
                    ),
                )
                .text_size(TEXT_SETTINGS_AUX)
                .text_color(color(GRAY)),
            );
            if !evidence.details.is_empty() {
                result = result.child(
                    text_button("service-test-details", cx)
                        .settings_text()
                        .icon(icons::info())
                        .self_start()
                        .label(if editor.test_details_open {
                            course2md::tr!("收起测试详情")
                        } else {
                            course2md::tr!("查看测试详情")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(editor) = &mut this.settings_ui.editor {
                                editor.test_details_open = !editor.test_details_open;
                            }
                            cx.notify();
                        })),
                );
                let details = v_flex()
                    .gap_2()
                    .children(evidence.details.iter().enumerate().map(|(index, detail)| {
                        accessible_text(("service-test-detail", index), detail.clone())
                            .text_sm()
                            .text_color(color(GRAY))
                    }));
                result = result.child(crate::motion::disclosure(
                    "service-test-details-content",
                    editor.test_details_open,
                    details,
                ));
            }
            testing = testing.child(crate::motion::enter(
                SharedString::from(format!(
                    "service-test-result-{}-{:?}",
                    evidence.tested_at, evidence.outcome
                )),
                result,
            ));
        }
        Some(
            crate::settings_ui::SettingsGroup::new(
                "service-test-heading",
                course2md::tr!("检查服务"),
            )
            .child(testing)
            .into_any_element(),
        )
    }

    /// Failure reason, saved-only hint, billing notice and the action row.
    fn service_editor_footer(
        &self,
        actions: Div,
        fixed_test_failure: Option<(TestKind, preferences::ServiceTestEvidence)>,
        repair_submit_label: &'static str,
        test_outcome_unknown: bool,
        repair_refused: bool,
    ) -> Div {
        let (configuration_saved, repair_task) = self
            .settings_ui
            .editor
            .as_ref()
            .map(|editor| {
                (
                    editor.saved_configuration.is_some() || editor.pending_binding.is_some(),
                    editor.repair_task.is_some(),
                )
            })
            .unwrap_or((false, false));
        let scrollable = self
            .settings_ui
            .editor
            .as_ref()
            .is_some_and(|editor| editor.scroll.max_offset().y > px(0.));
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_2()
            .pt_3()
            // Keep the bottom buttons off the window's lower edge.
            .pb_4()
            // The actions and the request notice stay put; a hairline marks
            // where a scrolling body passes under them.
            .when(scrollable, |footer| {
                footer.border_t_1().border_color(color(HAIRLINE))
            })
            .when_some(fixed_test_failure, |footer, (kind, evidence)| {
                footer.child(
                    accessible_text(
                        "service-test-fixed-reason",
                        course2md::tr!(
                            "{}：{}{}",
                            kind.label(),
                            evidence.message,
                            if repair_refused {
                                course2md::tr!("。修改配置或重新测试后再补做。")
                            } else {
                                ""
                            }
                        ),
                    )
                    .role(Role::Status)
                    .text_size(TEXT_SETTINGS_AUX)
                    .text_color(color(
                        if matches!(
                            evidence.outcome,
                            preferences::TestOutcome::OutcomeUnknown
                                | preferences::TestOutcome::NotSent
                        ) {
                            WARNING
                        } else {
                            DANGER
                        },
                    )),
                )
            })
            .when(configuration_saved && repair_task, |footer| {
                footer.child(
                    accessible_text(
                        "service-config-saved-only",
                        course2md::tr!("服务配置已保存，尚未开始补做"),
                    )
                    .text_size(TEXT_SETTINGS_AUX)
                    .text_color(color(GRAY)),
                )
            })
            .child(
                theme::supporting_info(
                    "service-test-notice",
                    if test_outcome_unknown && repair_task {
                        course2md::tr!(
                            "上次结果未确认；重测或「{}」都会再次发送请求，可能计费",
                            repair_submit_label
                        )
                    } else if test_outcome_unknown {
                        course2md::tr!("上次结果未确认；重测会再次发送，可能计费").to_owned()
                    } else if repair_task {
                        course2md::tr!("「{}」会重新发送失败部分，可能计费", repair_submit_label)
                    } else {
                        service_test::test_notice().to_owned()
                    },
                )
                .text_size(TEXT_SETTINGS_AUX),
            )
            .child(actions)
    }

    /// Records the editor and validates it. False while a typed key is still
    /// being stored; `after` then continues once it is.
    fn validate_service_editor(
        &mut self,
        after: AfterDraftSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.save_open_service_draft(true, after, window, cx) {
            return false;
        }
        let Some(editor) = &mut self.settings_ui.editor else {
            return false;
        };
        let valid = editor.form.validate(window, cx);
        cx.notify();
        valid
    }
    pub(super) fn start_service_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .settings_ui
            .editor
            .as_ref()
            .is_some_and(|editor| editor.test_running.is_some())
        {
            return;
        }
        if !self.validate_service_editor(AfterDraftSaved::Test, window, cx) {
            return;
        }
        let Some(editor) = &mut self.settings_ui.editor else {
            return;
        };
        let Ok(config) = editor.form.draft.configuration() else {
            return;
        };
        let id = editor.form.draft.id.clone();
        let kind = editor.test_kind;
        let cancel = Arc::new(AtomicBool::new(false));
        editor.test_running = Some(cancel.clone());
        editor.evidence = None;
        editor.status = Some(course2md::tr!("正在测试{}…", kind.label()));
        let vault = self.preferences.vault();
        let task = crate::spawn_blocking_io(move || {
            service_test::test_service_blocking(&config, kind, vault.as_ref(), &cancel)
        });
        cx.spawn(async move |this, cx| {
            let Ok(evidence) = task.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let persisted = this.preferences.record_test(evidence.clone()).is_ok();
                let key_unchanged = this
                    .settings_ui
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.form.typed_key(cx).is_empty());
                if let Some(editor) = &mut this.settings_ui.editor
                    && editor.form.draft.id == id
                {
                    editor.test_running = None;
                    editor.status = None;
                    if key_unchanged
                        && editor
                            .form
                            .draft
                            .configuration()
                            .ok()
                            .is_some_and(|config| {
                                config.fingerprint(&evidence.contract) == evidence.fingerprint
                            })
                    {
                        editor
                            .received_tests
                            .insert(evidence.fingerprint.clone(), evidence.clone());
                        editor.evidence = Some(evidence);
                        if !persisted {
                            editor.status = Some(course2md::tr!("测试结果尚未保存").into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn publish_open_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.publish_service_editor(true, window, cx);
    }
    fn repair_service_refusal(
        &self,
        cx: &App,
    ) -> Option<(TestKind, preferences::ServiceTestEvidence)> {
        let editor = self.settings_ui.editor.as_ref()?;
        let (task_id, components) = editor.repair_task.as_ref()?;
        if !editor.form.typed_key(cx).trim().is_empty() {
            return None;
        }
        let config = editor.form.current_draft(cx).configuration().ok()?;
        let uses_vision = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .state
                    .tasks
                    .iter()
                    .find(|task| task.id == *task_id)
            })
            .is_some_and(|task| task.plan.options().vision);
        let required: Vec<_> = [TestKind::Proofread, TestKind::Summary, TestKind::Vision]
            .into_iter()
            .filter(|kind| match kind {
                TestKind::Proofread => components.contains(&Component::Proofreading),
                TestKind::Vision => uses_vision && components.contains(&Component::Proofreading),
                TestKind::Summary => components.contains(&Component::Summary),
                TestKind::Speech => false,
            })
            .map(TestKind::contract)
            .collect();
        [TestKind::Proofread, TestKind::Summary, TestKind::Vision]
            .into_iter()
            .filter_map(|kind| {
                let evidence = editor
                    .received_tests
                    .get(&config.fingerprint(kind.contract()))
                    .or_else(|| self.preferences.test_evidence(&config, kind.contract()))?;
                service_test::blocks_reprocessing(evidence, &config, &required)
                    .then(|| (kind, evidence.clone()))
            })
            .max_by_key(|(_, evidence)| evidence.tested_at)
    }
    pub(super) fn publish_service_editor(
        &mut self,
        continue_repair: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.validate_service_editor(AfterDraftSaved::Publish { continue_repair }, window, cx) {
            return;
        }
        let Some(editor) = &self.settings_ui.editor else {
            return;
        };
        let target = editor.target.clone();
        let repair_task = editor.repair_task.clone();
        let repairing = repair_task.is_some();
        if repairing && editor.test_running.is_some() {
            return;
        }
        if repairing && continue_repair && self.repair_service_refusal(cx).is_some() {
            cx.notify();
            return;
        }
        let inline = target.is_none() && !repairing;
        let scope = publish_scope(repairing, target.is_some(), editor.also_default);
        let plan = PublishPlan {
            draft_id: editor.form.draft.id.clone(),
            continue_repair,
            target,
            repair_task,
            scope,
            inline,
        };
        if let Some(version) = editor
            .pending_binding
            .as_ref()
            .or(editor.saved_configuration.as_ref())
        {
            let version = version.clone();
            self.finish_service_publish(Ok(version), plan, window, cx);
            return;
        }
        // Publication first confirms the draft's credential is in the vault,
        // which can block; only that check leaves the UI thread.
        let reference = match self.preferences.publication_credential(&plan.draft_id) {
            Ok(reference) => reference,
            Err(error) => {
                self.finish_service_publish(Err(error), plan, window, cx);
                return;
            }
        };
        if reference.is_none() {
            let result =
                preferences::Store::verify_credential(self.preferences.vault().as_ref(), None)
                    .and_then(|verified| {
                        self.preferences
                            .commit_publication(&plan.draft_id, plan.scope, verified)
                    });
            self.finish_service_publish(result, plan, window, cx);
            return;
        }
        if let Some(editor) = &mut self.settings_ui.editor {
            editor.vault_busy = true;
        }
        self.run_vault_step(
            cx,
            move |vault| preferences::Store::verify_credential(vault, reference),
            move |this, verified, window, cx| {
                let open = this
                    .settings_ui
                    .editor
                    .as_mut()
                    .filter(|editor| editor.form.draft.id == plan.draft_id);
                let Some(editor) = open else {
                    return;
                };
                editor.vault_busy = false;
                let result = verified.and_then(|verified| {
                    this.preferences
                        .commit_publication(&plan.draft_id, plan.scope, verified)
                });
                this.finish_service_publish(result, plan, window, cx);
            },
        );
        cx.notify();
    }

    /// The commit step after an environment credential reached the vault.
    fn finish_environment_credential(
        &mut self,
        draft: ServiceDraft,
        name: &'static str,
        stored: Result<crate::credentials::CredentialRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self
            .settings_ui
            .editor
            .as_mut()
            .filter(|editor| editor.form.draft.id == draft.id);
        let Some(editor) = open else {
            if let Ok(reference) = stored {
                self.forget_credentials(vec![reference]);
            }
            return;
        };
        editor.vault_busy = false;
        let committed = stored.and_then(|reference| {
            self.preferences
                .commit_environment_credential(draft, name, reference.clone())
                .inspect_err(|_| self.forget_credentials(vec![reference]))
        });
        let Some(editor) = &mut self.settings_ui.editor else {
            return;
        };
        match committed {
            Ok(draft) => {
                editor.form.draft = draft;
                editor.form.models.invalidate();
                editor.form.clear_key(window, cx);
                editor.evidence = None;
                editor.saved_configuration = None;
                editor.save_failed = false;
                editor.status =
                    Some(course2md::tr!("已使用环境变量中的密钥，点击保存后生效").into());
            }
            Err(error) => {
                editor.status = Some(course2md::tr!("凭据尚未保存：{}", format!("{error:#}")));
            }
        }
        cx.notify();
    }

    /// Publication's commit continues here: bind the version where the
    /// editor was opened from, start a repair, report and close.
    fn finish_service_publish(
        &mut self,
        result: Result<ServiceVersion>,
        plan: PublishPlan,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PublishPlan {
            continue_repair,
            target,
            repair_task,
            scope,
            inline,
            ..
        } = plan;
        let repairing = repair_task.is_some();
        let version = match result {
            Ok(version) => version,
            Err(error) => {
                if let Some(editor) = &mut self.settings_ui.editor {
                    editor.status = Some(course2md::tr!("服务尚未保存：{}", format!("{error:#}")));
                    editor.save_failed = true;
                }
                cx.notify();
                return;
            }
        };
        if repairing && !continue_repair {
            if let Some(editor) = &mut self.settings_ui.editor {
                editor.form.draft = ServiceDraft::from_version(&version);
                editor.saved_configuration = Some(version);
                editor.pending_binding = None;
                editor.status = None;
                editor.save_failed = false;
            }
            self.refresh_dispatch_controls(cx);
            cx.notify();
            return;
        }
        if let Some(target) = target {
            let binding = self
                .workspace
                .as_mut()
                .ok_or_else(|| anyhow!(course2md::tr!("当前笔记信息暂时不可用")))
                .and_then(|workspace| {
                    workspace.transaction(|state| {
                        let draft = state
                            .drafts
                            .iter_mut()
                            .find(|draft| draft.id == target)
                            .context(course2md::tr!(
                                "原笔记已关闭；服务正在保存，但未用于其他笔记"
                            ))?;
                        let selected = draft_service(scope, &version.id);
                        match version.config.protocol.purpose() {
                            ServicePurpose::Speech => draft.asr_service = selected,
                            ServicePurpose::Ai => draft.ai_service = selected,
                        };
                        Ok(())
                    })
                });
            if let Err(error) = binding {
                if let Some(editor) = &mut self.settings_ui.editor {
                    editor.pending_binding = Some(version);
                    editor.status = Some(course2md::tr!(
                        "服务正在保存，本次选择尚未保存：{}",
                        format!("{error:#}")
                    ));
                }
                cx.notify();
                return;
            }
        }
        if let Some((task_id, components)) = repair_task
            && !self.reprocess_task_with_service(task_id, components, version.id.clone(), cx)
        {
            if let Some(editor) = &mut self.settings_ui.editor {
                editor.pending_binding = Some(version);
                editor.status =
                    Some(course2md::tr!("服务正在保存，补做任务尚未创建。请重试补做。").into());
            }
            cx.notify();
            return;
        }
        self.refresh_dispatch_controls(cx);
        self.set_settings_feedback(
            PreferenceGroup::Services,
            if self.preferences.is_pending(PreferenceGroup::Services) {
                course2md::tr!("正在保存…")
            } else {
                course2md::tr!("服务已保存")
            }
            .into(),
            false,
            cx,
        );
        self.restore_service_focus(window, cx);
        if !inline {
            window.close_dialog(cx);
        }
        if !repairing {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }
    pub(crate) fn close_service_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = &self.settings_ui.editor {
            // Tests can stage a private edit, but cancelling never publishes
            // it. A cleanup failure cannot make these hidden records active.
            let _ = self
                .preferences
                .discard_service_draft(&editor.form.draft.id);
        }
        self.restore_service_focus(window, cx);
    }
    fn restore_service_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.settings_ui.editor.take() {
            if let Some(cancel) = editor.test_running {
                cancel.store(true, Ordering::Release);
            }
            // Opened from another category's picker: go back there, where the
            // returning focus target lives.
            if let Some(tab) = editor.origin_tab
                && self.page == Page::Settings
                && self.settings_tab == SettingsTab::Services
            {
                self.settings_tab = tab;
                self.sync_settings_tab_stops();
            }
            if let Some(focus) = editor.return_focus {
                focus.focus(window, cx);
            }
        }
        cx.notify();
    }
    pub(super) fn confirm_default_service(
        &mut self,
        version_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(version) = self.preferences.version(&version_id) else {
            return;
        };
        let purpose = version.config.protocol.purpose();
        let defaults = self.preferences.default_refs();
        let previous = match purpose {
            ServicePurpose::Speech => defaults.asr.as_deref(),
            ServicePurpose::Ai => defaults.llm.as_deref(),
        }
        .and_then(|id| self.preferences.version(id))
        .map(|default| {
            course2md::tr!(
                "当前默认：{} · v{} · {}",
                default.config.name,
                default.number,
                default.config.model
            )
        })
        .unwrap_or_else(|| course2md::tr!("当前尚未设置默认服务").to_owned());
        let next = course2md::tr!(
            "更新为：{} · v{} · {}",
            version.config.name,
            version.number,
            version.config.model
        );
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let weak = weak.clone();
            let id = version_id.clone();
            confirm_dialog(
                dialog,
                course2md::tr!("更新默认服务"),
                course2md::tr!("保留原默认"),
                course2md::tr!("用于以后生成"),
                false,
                cx,
            )
            .child(accessible_text(
                "default-service-previous",
                previous.clone(),
            ))
            .child(accessible_text("default-service-next", next.clone()))
            .child(
                accessible_text(
                    "default-service-consequence",
                    course2md::tr!("仅影响以后生成的笔记"),
                )
                .text_size(TEXT_SETTINGS_AUX)
                .text_color(color(GRAY)),
            )
            .on_ok(move |_, _, cx| {
                weak.update(cx, |this, cx| {
                    match this.preferences.set_default_service(purpose, Some(&id)) {
                        Ok(()) => this.set_settings_feedback(
                            PreferenceGroup::Services,
                            course2md::tr!("已更新默认服务").into(),
                            false,
                            cx,
                        ),
                        Err(error) => {
                            this.set_settings_feedback(
                                PreferenceGroup::Services,
                                course2md::tr!("默认服务尚未更改，请重试").into(),
                                true,
                                cx,
                            );
                            this.settings_ui
                                .feedback_details
                                .insert(PreferenceGroup::Services, format!("{error:#}"));
                        }
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false)
            })
        });
    }
    pub(super) fn confirm_delete_service(
        &mut self,
        service_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let version_ids: Vec<_> = self
            .preferences
            .versions()
            .filter(|v| v.service_id == service_id)
            .map(|v| v.id.clone())
            .collect();
        let affected = self
            .workspace
            .as_ref()
            .map(|w| {
                w.state
                    .tasks
                    .iter()
                    .filter(|task| {
                        !task.state.finished()
                            && (task
                                .plan
                                .asr_service
                                .as_ref()
                                .is_some_and(|id| version_ids.contains(id))
                                || task
                                    .plan
                                    .ai_service
                                    .as_ref()
                                    .is_some_and(|id| version_ids.contains(id)))
                    })
                    .count()
            })
            .unwrap_or(0);
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let weak = weak.clone();
            let id = service_id.clone();
            confirm_dialog(
                dialog,
                course2md::tr!("删除此服务"),
                course2md::tr!("保留服务"),
                course2md::tr!("删除服务"),
                true,
                cx,
            )
            .child(accessible_text(
                "delete-service-consequence",
                course2md::tr!("将删除此服务配置，已有笔记不受影响"),
            ))
            .when(affected > 0, |dialog| {
                dialog.child(accessible_text(
                    "delete-service-affected",
                    course2md::tr!(
                        "有 {} 个未完成任务引用此服务；这些任务中用到此服务的步骤将不再继续",
                        affected
                    ),
                ))
            })
            .on_ok(move |_, _, cx| {
                weak.update(cx, |this, cx| match this.preferences.retire_service(&id) {
                    Ok(released) => {
                        this.forget_credentials(released);
                        this.refresh_dispatch_controls(cx);
                        this.set_settings_feedback(
                            PreferenceGroup::Services,
                            course2md::tr!("已删除此服务").into(),
                            false,
                            cx,
                        );
                        cx.notify();
                        true
                    }
                    Err(error) => {
                        // The retire marker is written before the group is persisted,
                        // so a mid-failure can leave dispatch blocked without a visible
                        // deletion; say so instead of claiming nothing happened.
                        let blocked = this.preferences.is_service_retired(&id);
                        this.set_settings_feedback(
                            PreferenceGroup::Services,
                            course2md::tr!(
                                "{}：{}",
                                if blocked {
                                    course2md::tr!("已停止使用此服务，但删除尚未完整保存")
                                } else {
                                    course2md::tr!("尚未删除此服务")
                                },
                                format!("{error:#}")
                            ),
                            true,
                            cx,
                        );
                        this.refresh_dispatch_controls(cx);
                        cx.notify();
                        true
                    }
                })
                .unwrap_or(false)
            })
        });
    }

    pub(crate) fn commit_generation(
        &mut self,
        next: GenerationPreferences,
        cx: &mut Context<Self>,
    ) -> bool {
        self.discard_staged_generation_text();
        let previous = self
            .ordinary_preferences_submit_issue()
            .map(|issue| issue.message);
        match self.preferences.save_generation(next.clone()) {
            Ok(()) => {
                self.settings_ui.pending_generation = None;
                self.set_settings_feedback(
                    PreferenceGroup::Generation,
                    if self.preferences.is_pending(PreferenceGroup::Generation) {
                        course2md::tr!("正在保存…")
                    } else {
                        course2md::tr!("已保存")
                    }
                    .into(),
                    false,
                    cx,
                );
                self.refresh_preference_defaults(cx);
                self.clear_resolved_generation_errors(previous);
                if !self.preference_defaults_pending {
                    self.advance_conversion_when_ready(cx);
                }
                true
            }
            Err(error) => {
                self.settings_ui.pending_generation = Some(next);
                self.set_settings_save_failure(PreferenceGroup::Generation, &error, cx);
                cx.notify();
                false
            }
        }
    }
    pub(crate) fn refresh_preference_defaults(&mut self, cx: &mut Context<Self>) {
        let out = self.config.defaults.out.clone();
        self.config = self.preferences.defaults_config();
        self.config.defaults.out = out;
        self.settings_options = ConversionOptions::from_config(&self.config);
        self.desktop_settings = self.preferences.application().desktop.clone();
        let defaults = self.settings_options.clone();
        if let Some(workspace) = &mut self.workspace {
            match workspace.transaction(|state| {
                for draft in &mut state.drafts {
                    if draft.submitted_task.is_none() {
                        draft.inherit(&defaults);
                    }
                }
                Ok(())
            }) {
                Ok(()) => {
                    if self.preference_defaults_pending {
                        self.preference_defaults_pending = false;
                        self.workspace_error = None;
                    }
                }
                Err(error) => {
                    self.preference_defaults_pending = true;
                    self.workspace_error = Some(course2md::tr!(
                        "默认设置已保存，当前视频的选项尚未同步：{}",
                        format!("{error:#}")
                    ))
                }
            }
        }
        cx.notify();
    }
}

/// What a service publication continues with once its credential is checked.
struct PublishPlan {
    draft_id: String,
    continue_repair: bool,
    target: Option<String>,
    repair_task: Option<(String, Vec<Component>)>,
    scope: BindingScope,
    inline: bool,
}

/// Where a published service applies. A repair, or a service chosen while
/// editing one input, binds to that task alone unless the person also made it
/// the default; a service saved in settings is the default only when chosen so.
fn publish_scope(repairing: bool, from_input: bool, also_default: bool) -> BindingScope {
    if repairing || (from_input && !also_default) {
        BindingScope::CurrentTask
    } else if also_default {
        BindingScope::Defaults
    } else {
        BindingScope::Saved
    }
}

/// The service the input records after a publish: its own binding, or none so
/// that it follows the defaults the publish just changed.
fn draft_service(scope: BindingScope, id: &str) -> Option<String> {
    (scope == BindingScope::CurrentTask).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{BindingScope, draft_service, publish_scope};

    #[test]
    fn a_repair_binds_only_its_task_whatever_the_default_choice() {
        for (from_input, also_default) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            assert_eq!(
                publish_scope(true, from_input, also_default),
                BindingScope::CurrentTask
            );
        }
    }

    #[test]
    fn a_service_chosen_for_an_input_binds_it_unless_it_becomes_the_default() {
        let scope = publish_scope(false, true, false);
        assert_eq!(scope, BindingScope::CurrentTask);
        assert_eq!(draft_service(scope, "svc").as_deref(), Some("svc"));

        // Made the default as well: the input follows the new default instead
        // of pinning a copy of it.
        let scope = publish_scope(false, true, true);
        assert_eq!(scope, BindingScope::Defaults);
        assert_eq!(draft_service(scope, "svc"), None);
    }

    #[test]
    fn a_service_saved_in_settings_changes_defaults_only_when_asked() {
        assert_eq!(publish_scope(false, false, false), BindingScope::Saved);
        assert_eq!(publish_scope(false, false, true), BindingScope::Defaults);
    }
}
