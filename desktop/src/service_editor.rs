//! Service editor: picker, editor dialog, connection testing and publishing.
use super::*;

impl Desktop {
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

    /// The service a choice currently names, its saved alternatives with what changed in
    /// each, and the menu that picks among them. A superseded current version is listed
    /// beside its latest, so changing it stays an explicit choice.
    fn service_choices(
        &self,
        purpose: ServicePurpose,
        current_task: bool,
        cx: &mut Context<Self>,
    ) -> (Option<ServiceVersion>, Option<SharedString>, MaterialMenu) {
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
        let detail = current_version.as_ref().map(|version| {
            let state = if old_current.is_some() {
                course2md::tr!(" · 当前使用修改前的配置")
            } else {
                ""
            };
            SharedString::from(format!(
                "{} · {}{state}",
                version.config.protocol.choice_label(),
                version.config.model
            ))
        });
        let mut items = Vec::new();
        for version in latest.into_values() {
            let old = old_current
                .as_ref()
                .filter(|old| old.service_id == version.service_id);
            let state = old.map(|old| {
                let changed = version.changes_since(old);
                if changed.is_empty() {
                    course2md::tr!("配置内容相同").to_owned()
                } else {
                    course2md::tr!("已修改：{}", changed.join(course2md::tr!("、")))
                }
            });
            if let Some(old) = old {
                let label = course2md::tr!("修改前的配置").to_owned();
                items.push(self.service_choice_item(old, Some(label), &current, current_task, cx));
            }
            items.push(self.service_choice_item(&version, state, &current, current_task, cx));
        }
        let group = MenuGroup::new()
            .label(course2md::tr!("已添加的服务"))
            .items(items);
        let manage = MaterialMenuItem::new(match purpose {
            ServicePurpose::Speech => course2md::tr!("添加或管理语音服务…"),
            ServicePurpose::Ai => course2md::tr!("添加或管理 AI 服务…"),
        })
        .leading(Icon::from(symbols::add()))
        .on_click(cx.listener(|this, _, window, cx| this.manage_services(window, cx)));
        let menu = MaterialMenu::new(("service-choice-items", purpose as usize))
            .width(400.)
            .group(group)
            .group(MenuGroup::new().item(manage));
        (current_version, detail, menu)
    }

    fn service_choice_item(
        &self,
        version: &ServiceVersion,
        state: Option<String>,
        current: &Option<String>,
        current_task: bool,
        cx: &mut Context<Self>,
    ) -> MaterialMenuItem {
        let id = version.id.clone();
        let purpose = version.config.protocol.purpose();
        let mut detail = format!(
            "{} · {}",
            version.config.protocol.choice_label(),
            version.config.model
        );
        if let Some(state) = state {
            detail = format!("{detail} · {state}");
        }
        MaterialMenuItem::new(version.config.name.clone())
            .supporting_text(detail)
            .leading(Icon::from(service_form::protocol_icon(
                version.config.protocol,
            )))
            .selected(current.as_ref() == Some(&id))
            .on_click(cx.listener(move |this, _, _, cx| {
                let selectable = this.preferences.version(&id).is_some_and(|version| {
                    version.config.protocol.purpose() == purpose
                        && !this.preferences.is_service_retired(&version.service_id)
                });
                if selectable {
                    this.bind_service(purpose, Some(id.clone()), current_task, cx);
                }
            }))
    }

    /// The default service of a purpose, as a row of the generation page.
    pub(super) fn service_field(
        &self,
        purpose: ServicePurpose,
        cx: &mut Context<Self>,
    ) -> ui::Field {
        let index = purpose as usize;
        let label = match purpose {
            ServicePurpose::Speech => course2md::tr!("语音服务"),
            ServicePurpose::Ai => course2md::tr!("AI 服务"),
        };
        let has_services = self.preferences.latest_versions().values().any(|version| {
            version.config.protocol.purpose() == purpose
                && !self
                    .preferences
                    .service_retired_in_snapshot(&version.service_id)
        });
        if !has_services {
            // Nothing to choose yet: the one action is adding a service.
            return ui::Field::inline(
                format!("default-service-{index}"),
                label,
                material_button(("configure-service", index), ButtonVariant::Outlined)
                    .icon(symbols::add())
                    .child(match purpose {
                        ServicePurpose::Speech => course2md::tr!("添加语音服务"),
                        ServicePurpose::Ai => course2md::tr!("添加 AI 服务"),
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_settings_service_editor(purpose, None, window, cx);
                    })),
            )
            .hint(match purpose {
                ServicePurpose::Ai => course2md::tr!("尚未添加，校对和摘要需要先添加一个服务"),
                ServicePurpose::Speech => course2md::tr!("尚未添加，请先添加一个语音服务"),
            });
        }
        let (version, detail, menu) = self.service_choices(purpose, false, cx);
        let name: SharedString = version.as_ref().map_or_else(
            || course2md::tr!("选择服务").into(),
            |version| version.config.name.clone().into(),
        );
        let generation = self.preferences.generation();
        // What leaves the machine stays beside the choice that sends it.
        let consequence = match (purpose, generation.ai.enabled && generation.ai.vision) {
            (ServicePurpose::Ai, true) => course2md::tr!("文字及对应截图会发送到所选服务"),
            (ServicePurpose::Ai, false) => course2md::tr!("校对与摘要会把文字发送到所选服务"),
            (ServicePurpose::Speech, _) => "",
        };
        let hint = [detail.as_deref().unwrap_or_default(), consequence]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let failure = self
            .settings_ui
            .failures
            .get(&PreferenceGroup::Services)
            .cloned();
        ui::Field::menu(format!("default-service-{index}"), label, name, menu)
            .hint(failure.unwrap_or(hint))
    }

    /// The service of the current input, with a way back to the default.
    pub fn task_service_picker(
        &self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let index = purpose as usize;
        let has_services = self.preferences.latest_versions().values().any(|version| {
            version.config.protocol.purpose() == purpose
                && !self
                    .preferences
                    .service_retired_in_snapshot(&version.service_id)
        });
        let mut view = v_flex().w_full().min_w_0().gap(rem(8.));
        if has_services {
            let (version, detail, menu) = self.service_choices(purpose, true, cx);
            let scale = TileScale::Compact;
            let popup = ElementId::from(("task-service-choice", index));
            let name: SharedString = version.as_ref().map_or_else(
                || course2md::tr!("选择服务").into(),
                |version| version.config.name.clone().into(),
            );
            let support: SharedString = match detail {
                Some(detail) => format!("{name} · {detail}").into(),
                None => name,
            };
            let label = match purpose {
                ServicePurpose::Speech => course2md::tr!("语音服务"),
                ServicePurpose::Ai => course2md::tr!("AI 服务"),
            };
            let trigger = scale
                .button(("task-service-trigger", index), [true; 4])
                .accessibility_label(label)
                .on_click(ui::menu_key_toggle(&popup, window, cx))
                .child(
                    scale
                        .row()
                        .children(version.map(|version| {
                            Icon::from(service_form::protocol_icon(version.config.protocol))
                        }))
                        .child(scale.text(("task-service-text", index), label, Some(support)))
                        .child(tile_chevron()),
                );
            view = view.child(ui::menu(popup, trigger, menu, window, cx));
        } else {
            view = view.child(
                material_button(("configure-task-service", index), ButtonVariant::Outlined)
                    .icon(symbols::add())
                    .child(match purpose {
                        ServicePurpose::Speech => course2md::tr!("添加语音服务"),
                        ServicePurpose::Ai => course2md::tr!("添加 AI 服务"),
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_task_service_editor(purpose, window, cx)
                    })),
            );
        }
        let bound = self
            .workspace
            .as_ref()
            .and_then(|w| w.state.draft())
            .is_some_and(|draft| match purpose {
                ServicePurpose::Speech => draft.asr_service.is_some(),
                ServicePurpose::Ai => draft.ai_service.is_some(),
            });
        view.when(bound, |view| {
            view.child(
                h_flex()
                    .gap(rem(8.))
                    .items_center()
                    .child(TileScale::Compact.support(
                        ("service-only-this-note", index),
                        course2md::tr!("仅用于这次笔记"),
                    ))
                    .child(
                        material_button(("inherit-default-service", index), ButtonVariant::Text)
                            .icon(symbols::refresh())
                            .child(course2md::tr!("恢复默认"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.bind_service(purpose, None, true, cx);
                            })),
                    ),
            )
        })
        .when_some(
            self.settings_ui.failures.get(&PreferenceGroup::Services),
            |view, message| view.child(error_text(("task-service-error", index), message.clone())),
        )
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
                self.clear_settings_failure(PreferenceGroup::Services);
                self.advance_conversion_when_ready(cx);
                cx.notify();
                true
            }
            Err(error) => {
                self.set_settings_failure(
                    PreferenceGroup::Services,
                    course2md::tr!("服务选择尚未保存：{}", format!("{error:#}")),
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
    /// Opens the editor over whatever the person is doing. It closes back to that place,
    /// and only its save action publishes anything.
    pub(crate) fn open_service_draft(
        &mut self,
        draft: ServiceDraft,
        target: Option<String>,
        repair_task: Option<(String, Vec<Component>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.settings_ui.editor.is_some() {
            return false;
        }
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
        self.settings_ui.closed_editor = None;
        self.settings_ui.editor = Some(ServiceEditor {
            form,
            target,
            repair_task,
            scroll: ScrollHandle::new(),
            also_default,
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
        });
        match draft_protocol {
            ServiceProtocol::CodexResponses => {
                self.codex_refresh_status(crate::codex_ui::CodexSurface::Editor, cx)
            }
            ServiceProtocol::OllamaChat => self.ollama_prepare_form(cx),
            _ => {}
        }
        cx.notify();
        true
    }
    /// Capture builds: opens the service editor over Settings › 服务与账号 (`speech`, or
    /// `dialog` for one note's service; `confirm` the delete confirmation for a
    /// placeholder service). Nothing is saved.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_service_editor(
        &mut self,
        kind: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            "speech" => {
                self.open_settings_service_editor(ServicePurpose::Speech, None, window, cx);
            }
            "confirm" => self.confirm_delete_service("capture".into(), window, cx),
            "dialog" => {
                self.open_service_draft(
                    ServiceDraft::new(ServicePurpose::Ai),
                    Some("capture".into()),
                    None,
                    window,
                    cx,
                );
            }
            _ => {
                self.open_settings_service_editor(ServicePurpose::Ai, None, window, cx);
            }
        }
    }
    /// Capture builds: one dialog per shot.
    #[cfg(feature = "capture")]
    pub(crate) fn close_settings_dialogs(&mut self, cx: &mut Context<Self>) {
        self.close_service_editor(cx);
        self.close_palette_picker(cx);
        self.close_account_dialog(cx);
    }
    /// The editor's dialog, while it is open or playing its exit.
    pub(crate) fn service_editor_dialog(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.settings_ui.editor.is_some();
        let Some(editor) = self
            .settings_ui
            .editor
            .as_ref()
            .or(self.settings_ui.closed_editor.as_ref())
        else {
            return div().into_any_element();
        };
        let speech = editor.form.draft.protocol.purpose() == ServicePurpose::Speech;
        let editing = editor.form.draft.based_on.is_some();
        let title = if editor.repair_task.is_some() {
            course2md::tr!("修复服务并补做")
        } else if editor.target.is_some() {
            if speech {
                course2md::tr!("为这次笔记设置语音识别服务")
            } else {
                course2md::tr!("为这次笔记设置 AI 服务")
            }
        } else {
            match (speech, editing) {
                (true, false) => course2md::tr!("添加语音服务"),
                (true, true) => course2md::tr!("编辑语音服务"),
                (false, false) => course2md::tr!("添加 AI 服务"),
                (false, true) => course2md::tr!("编辑 AI 服务"),
            }
        };
        let busy = editor.test_running.is_some() || editor.vault_busy;
        let awaiting_binding = editor.pending_binding.is_some();
        let configuration_saved = editor.saved_configuration.is_some() || awaiting_binding;
        let repairing = editor.repair_task.is_some();
        let repair_submit_label = if awaiting_binding {
            course2md::tr!("重试补做")
        } else if configuration_saved {
            course2md::tr!("开始补做")
        } else {
            course2md::tr!("保存并补做")
        };
        let repair_refusal = self.repair_service_refusal(cx);
        let cancel = material_button("close-service-editor", ButtonVariant::Text)
            .child(if configuration_saved {
                course2md::tr!("关闭")
            } else {
                course2md::tr!("取消")
            })
            .on_click(cx.listener(|this, _, _, cx| this.close_service_editor(cx)));
        let test = material_button("run-service-test", ButtonVariant::Outlined)
            .icon(symbols::science())
            .loading(busy, course2md::tr!("正在测试服务…"))
            .disabled(busy || awaiting_binding)
            .child(if editor.evidence.is_some() {
                course2md::tr!("重新测试")
            } else {
                course2md::tr!("测试连接")
            })
            .on_click(cx.listener(|this, _, window, cx| this.start_service_test(window, cx)));
        let save_only =
            repairing.then(|| {
                material_button("save-repair-service-only", ButtonVariant::Text)
                    .icon(symbols::save())
                    .child(course2md::tr!("仅保存"))
                    .disabled(busy || awaiting_binding || editor.saved_configuration.is_some())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.publish_service_editor(false, window, cx)
                    }))
            });
        let adding = editor.target.is_none() && !repairing && !editing;
        let save = material_button("save-service", ButtonVariant::Filled)
            .icon(symbols::check())
            .child(if repairing {
                repair_submit_label
            } else if awaiting_binding {
                course2md::tr!("重试保存本次选择")
            } else if adding {
                course2md::tr!("添加")
            } else {
                course2md::tr!("保存")
            })
            .loading(editor.vault_busy, course2md::tr!("正在保存密钥…"))
            .disabled(editor.vault_busy || repairing && (busy || repair_refusal.is_some()))
            .on_click(
                cx.listener(|this, _, window, cx| this.publish_service_editor(true, window, cx)),
            );
        MaterialDialog::new("service-editor-dialog")
            .headline(title)
            .size(DialogSize::Medium)
            .content(self.service_editor_content(
                editor,
                repair_refusal,
                repair_submit_label,
                window,
                cx,
            ))
            .action(cancel)
            .action(test)
            .when_some(save_only, |dialog, save_only| dialog.action(save_only))
            .action(save)
            .dismiss_on_scrim(false)
            .modal(open)
            .on_dismiss({
                let this = cx.weak_entity();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| this.close_service_editor(cx));
                }
            })
            .into_any_element()
    }

    /// The form scrolls; the failure, what was saved and what a request may cost stay
    /// beside the actions.
    fn service_editor_content(
        &self,
        editor: &ServiceEditor,
        repair_refusal: Option<(TestKind, preferences::ServiceTestEvidence)>,
        repair_submit_label: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let busy = editor.test_running.is_some() || editor.vault_busy;
        let awaiting_binding = editor.pending_binding.is_some();
        let protocol = editor.form.draft.protocol;
        let reveal = |id: &'static str, part: AnyElement| {
            RevealFocus::new(id, part, editor.scroll.clone()).into_any_element()
        };
        let mut parts = vec![reveal(
            "service-protocol-reveal",
            self.service_type_field(FormOwner::Editor, "service-protocol", awaiting_binding, cx)
                .into_any_element(),
        )];
        // Codex account connection: the login decides the model catalog and availability.
        if protocol == ServiceProtocol::CodexResponses {
            parts.push(
                self.codex_account_section(
                    crate::codex_ui::CodexSurface::Editor,
                    awaiting_binding,
                    cx,
                )
                .into_any_element(),
            );
        }
        let text_field = |this: &Self,
                          id: &'static str,
                          field: FormField,
                          label: &'static str,
                          hint: Option<String>,
                          cx: &mut Context<Self>| {
            this.service_text_field(FormOwner::Editor, field, label, hint, awaiting_binding, cx)
                .map(|field| reveal(id, field.into_any_element()))
        };
        parts.extend(text_field(
            self,
            "service-name-reveal",
            FormField::Name,
            course2md::tr!("名称"),
            None,
            cx,
        ));
        // Codex has a fixed endpoint and its login holds the credential: no address or key fields.
        if protocol != ServiceProtocol::CodexResponses {
            let address = editor.form.value(FormField::Address, cx);
            let ollama = protocol == ServiceProtocol::OllamaChat;
            let hint = if ollama {
                Some(course2md::tr!("本机运行的 Ollama 会自动填好").to_owned())
            } else {
                preferences::normalize_endpoint(&address, protocol)
                    .ok()
                    .filter(|endpoint| endpoint != address.trim())
                    .map(|endpoint| course2md::tr!("实际地址：{}", endpoint))
            };
            parts.extend(text_field(
                self,
                "service-address-reveal",
                FormField::Address,
                if ollama {
                    course2md::tr!("地址")
                } else {
                    course2md::tr!("接口地址")
                },
                hint,
                cx,
            ));
        }
        if !protocol.keyless() {
            let mut hint = if editor.form.draft.credential.is_some() {
                course2md::tr!("已保存在系统钥匙串里，留空则保留").to_owned()
            } else {
                course2md::tr!("保存在系统钥匙串里，不会写进配置文件").to_owned()
            };
            if let Some(source) = &editor.form.draft.credential_source {
                hint = format!("{hint} · {}", course2md::tr!("密钥来源：{}", source));
            }
            parts.extend(text_field(
                self,
                "service-key-reveal",
                FormField::Key,
                "API Key",
                Some(hint),
                cx,
            ));
            parts.extend(
                preferences::Store::available_environment_credentials(protocol.purpose())
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| {
                        material_button(("capture-environment-key", index), ButtonVariant::Text)
                            .icon(symbols::content_copy())
                            .child(course2md::tr!("使用环境变量 {}", name))
                            .disabled(awaiting_binding || editor.vault_busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.use_environment_credential(name, cx)
                            }))
                            .into_any_element()
                    }),
            );
        }
        parts.push(reveal(
            "service-model-reveal",
            self.service_model_field(FormOwner::Editor, "service-models", awaiting_binding, cx)
                .into_any_element(),
        ));
        if editor.repair_task.is_none() {
            parts.push(
                ListItem::new("service-also-default", course2md::tr!("设为默认服务"))
                    .supporting(course2md::tr!("新笔记默认使用这个服务"))
                    .trailing(ListTrailing::Switch(editor.also_default))
                    .segment([true; 4])
                    .disabled(awaiting_binding)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(editor) = &mut this.settings_ui.editor {
                            editor.also_default = !editor.also_default;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        parts.extend(self.service_test_section(editor, cx));
        let fixed_failure = if busy {
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
        let repairing = editor.repair_task.is_some();
        let outcome_unknown = editor
            .evidence
            .as_ref()
            .is_some_and(|evidence| evidence.outcome == preferences::TestOutcome::OutcomeUnknown);
        let notice = if outcome_unknown && repairing {
            course2md::tr!(
                "上次结果未确认；重测或「{}」都会再次发送请求，可能计费",
                repair_submit_label
            )
        } else if outcome_unknown {
            course2md::tr!("上次结果未确认；重测会再次发送，可能计费").to_owned()
        } else if repairing {
            course2md::tr!("「{}」会重新发送失败部分，可能计费", repair_submit_label)
        } else {
            service_test::test_notice().to_owned()
        };
        let saved_only =
            repairing && (editor.saved_configuration.is_some() || editor.pending_binding.is_some());
        let refused = repair_refusal.is_some();
        v_flex()
            .w_full()
            .min_w_0()
            .max_h(super::dialog_body_height(window))
            .gap(rem(12.))
            .child(
                div()
                    .relative()
                    .flex()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        v_flex()
                            .id("service-editor-body")
                            .w_full()
                            .min_w_0()
                            .overflow_y_scroll()
                            .track_scroll(&editor.scroll)
                            .px(rem(24.))
                            .pt(rem(8.))
                            .gap(rem(20.))
                            .children(parts),
                    )
                    .child(Scrollbar::vertical(&editor.scroll)),
            )
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .px(rem(24.))
                    .gap(rem(8.))
                    .when_some(fixed_failure, |view, (kind, evidence)| {
                        let message = course2md::tr!(
                            "{}：{}{}",
                            kind.label(),
                            evidence.message,
                            if refused {
                                course2md::tr!("。修改配置或重新测试后再补做。")
                            } else {
                                ""
                            }
                        );
                        view.children(ui::async_status(
                            "service-test-fixed-reason",
                            &if matches!(
                                evidence.outcome,
                                preferences::TestOutcome::OutcomeUnknown
                                    | preferences::TestOutcome::NotSent
                            ) {
                                ui::AsyncStatus::Warning(message.into())
                            } else {
                                ui::AsyncStatus::Failed(message.into())
                            },
                            None,
                            cx,
                        ))
                    })
                    .when_some(editor.status.clone().filter(|_| !busy), |view, status| {
                        view.child(error_text("service-editor-status", status))
                    })
                    .when(saved_only, |view| {
                        view.child(TileScale::Regular.support(
                            "service-config-saved-only",
                            course2md::tr!("服务配置已保存，尚未开始补做"),
                        ))
                    })
                    .child(supporting_info("service-test-notice", notice)),
            )
    }

    /// Which capability to check, the check running and its result. Nothing shows for a
    /// speech service before its first test, so no empty section ever appears.
    fn service_test_section(
        &self,
        editor: &ServiceEditor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ai = editor.form.draft.protocol.purpose() == ServicePurpose::Ai;
        let busy = editor.test_running.is_some() || editor.vault_busy;
        if !ai && !busy && editor.evidence.is_none() {
            return None;
        }
        let kinds = [TestKind::Proofread, TestKind::Summary, TestKind::Vision];
        let picker = ai.then(|| {
            ConnectedButtonGroup::new("service-test-purpose", course2md::tr!("要测试的服务能力"))
                .segmented()
                .button_size(ButtonSize::Small)
                .options(kinds.map(|kind| (kind.contract(), kind.label())))
                .icon(TestKind::Proofread.contract(), symbols::edit())
                .icon(TestKind::Summary.contract(), symbols::summarize())
                .icon(TestKind::Vision.contract(), symbols::image())
                .selected(editor.test_kind.contract())
                .disabled(busy)
                .on_change(cx.listener(move |this, selected: &SharedString, _, cx| {
                    if let Some(kind) = kinds
                        .into_iter()
                        .find(|kind| kind.contract() == selected.as_ref())
                    {
                        this.select_service_test(kind, cx);
                    }
                }))
        });
        let status = if busy {
            ui::AsyncStatus::Busy(
                editor
                    .status
                    .clone()
                    .unwrap_or_else(|| course2md::tr!("正在测试服务…").to_owned())
                    .into(),
            )
        } else {
            match &editor.evidence {
                Some(evidence) => {
                    let time = course2md::tr!(
                        "测试时间：{}",
                        crate::reader_navigation::timestamp_local(evidence.tested_at * 1000)
                    );
                    let line = |label: String| format!("{label} · {time}").into();
                    match evidence.outcome {
                        preferences::TestOutcome::Passed => ui::AsyncStatus::Success(line(
                            course2md::tr!("{}测试通过", editor.test_kind.label()),
                        )),
                        preferences::TestOutcome::OutcomeUnknown => ui::AsyncStatus::Warning(line(
                            course2md::tr!("结果尚未确认").to_owned(),
                        )),
                        preferences::TestOutcome::NotSent => {
                            ui::AsyncStatus::Warning(line(course2md::tr!("测试未发送").to_owned()))
                        }
                        _ => ui::AsyncStatus::Failed(line(course2md::tr!("测试未通过").to_owned())),
                    }
                }
                None => ui::AsyncStatus::Idle,
            }
        };
        let details = editor
            .evidence
            .as_ref()
            .filter(|evidence| !evidence.details.is_empty());
        let toggle = details.map(|_| {
            material_button("service-test-details", ButtonVariant::Text)
                .icon(symbols::info())
                .child(if editor.test_details_open {
                    course2md::tr!("收起测试详情")
                } else {
                    course2md::tr!("查看测试详情")
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(editor) = &mut this.settings_ui.editor {
                        editor.test_details_open = !editor.test_details_open;
                    }
                    cx.notify();
                }))
                .into_any_element()
        });
        let binding_unsaved = editor.repair_task.is_none()
            && editor.pending_binding.is_some()
            && editor
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.outcome == preferences::TestOutcome::Passed);
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(12.))
                .child(section_title(
                    "service-test-heading",
                    course2md::tr!("检查服务"),
                ))
                .children(picker)
                .children(ui::async_status("service-test-status", &status, toggle, cx))
                .when(binding_unsaved, |view| {
                    view.child(TileScale::Regular.support(
                        "service-tested-save-state",
                        course2md::tr!("本次笔记的选择尚未保存"),
                    ))
                })
                .when_some(details, |view, evidence| {
                    view.child(motion::disclosure(
                        "service-test-details-content",
                        editor.test_details_open,
                        v_flex()
                            .gap(rem(8.))
                            .children(evidence.details.iter().enumerate().map(
                                |(index, detail)| {
                                    TileScale::Regular
                                        .support(("service-test-detail", index), detail.clone())
                                },
                            )),
                    ))
                })
                .into_any_element(),
        )
    }

    fn select_service_test(&mut self, kind: TestKind, cx: &mut Context<Self>) {
        let evidence = self
            .settings_ui
            .editor
            .as_ref()
            .filter(|editor| editor.form.typed_key(cx).is_empty())
            .and_then(|editor| {
                let config = editor.form.draft.configuration().ok()?;
                editor
                    .received_tests
                    .get(&config.fingerprint(kind.contract()))
                    .or_else(|| self.preferences.test_evidence(&config, kind.contract()))
                    .cloned()
            });
        if let Some(editor) = &mut self.settings_ui.editor {
            editor.test_kind = kind;
            editor.evidence = evidence;
        }
        cx.notify();
    }

    /// Records the open editor's fields, then continues with `after`. A typed key is
    /// first stored in the vault off the UI thread, which can wait on a prompt.
    fn save_service_draft_then(
        &mut self,
        after: AfterDraftSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &self.settings_ui.editor else {
            return;
        };
        if editor.vault_busy {
            return;
        }
        if editor.pending_binding.is_some() || editor.saved_configuration.is_some() {
            self.continue_after_draft(after, window, cx);
            return;
        }
        let draft = editor.form.current_draft(cx);
        let key = Some(editor.form.typed_key(cx)).filter(|key| !key.trim().is_empty());
        let empty = draft.revision == 0
            && draft.name.is_empty()
            && draft.address.is_empty()
            && draft.model.is_empty()
            && draft.credential.is_none();
        if key.is_none() && (empty || self.preferences.draft(&draft.id) == Some(&draft)) {
            self.continue_after_draft(after, window, cx);
            return;
        }
        let Some(key) = key else {
            let committed = self.preferences.commit_service_draft(draft, None);
            self.finish_service_draft(committed, after, window, cx);
            return;
        };
        if let Some(editor) = &mut self.settings_ui.editor {
            editor.vault_busy = true;
            editor.status = Some(course2md::tr!("正在保存密钥…").into());
        }
        self.store_editor_secret(
            draft,
            Secret::new(key),
            |this, draft, reference| {
                this.preferences
                    .commit_service_draft(draft, Some(reference))
            },
            move |this, committed, window, cx| {
                this.finish_service_draft(committed, after, window, cx)
            },
            cx,
        );
        cx.notify();
    }

    /// Stores a secret in the vault off the UI thread and commits `draft` with it. If the
    /// editor closed meanwhile, or the commit fails, nothing refers to the stored key.
    fn store_editor_secret(
        &self,
        draft: ServiceDraft,
        secret: Secret,
        commit: impl FnOnce(
            &mut Self,
            ServiceDraft,
            crate::credentials::CredentialRef,
        ) -> Result<ServiceDraft>
        + 'static,
        then: impl FnOnce(&mut Self, Result<ServiceDraft>, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let id = draft.id.clone();
        self.run_vault_step(
            cx,
            move |vault| preferences::Store::insert_credential(vault, secret),
            move |this, stored, window, cx| {
                let open = this
                    .settings_ui
                    .editor
                    .as_mut()
                    .filter(|editor| editor.form.draft.id == id);
                let Some(editor) = open else {
                    if let Ok(reference) = stored {
                        this.forget_credentials(vec![reference]);
                    }
                    return;
                };
                editor.vault_busy = false;
                let committed = stored.and_then(|reference| {
                    commit(this, draft, reference.clone())
                        .inspect_err(|_| this.forget_credentials(vec![reference]))
                });
                then(this, committed, window, cx);
            },
        );
    }

    fn finish_service_draft(
        &mut self,
        committed: Result<ServiceDraft>,
        after: AfterDraftSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &mut self.settings_ui.editor else {
            return;
        };
        match committed {
            Ok(draft) => {
                editor.form.draft = draft;
                editor.save_failed = false;
                editor.status = None;
                editor.form.clear_key(window, cx);
                self.continue_after_draft(after, window, cx);
            }
            Err(error) => {
                editor.status = Some(course2md::tr!("这些修改尚未保存：{}", format!("{error:#}")));
                editor.save_failed = true;
            }
        }
        cx.notify();
    }

    /// The draft is recorded: validate it and go on with what was asked.
    fn continue_after_draft(
        &mut self,
        after: AfterDraftSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &mut self.settings_ui.editor else {
            return;
        };
        let valid = editor.form.validate(window, cx);
        cx.notify();
        if valid {
            match after {
                AfterDraftSaved::Test => self.run_service_test(cx),
                AfterDraftSaved::Publish { continue_repair } => {
                    self.publish_saved_draft(continue_repair, cx)
                }
            }
        }
    }

    pub(super) fn start_service_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .settings_ui
            .editor
            .as_ref()
            .is_none_or(|editor| editor.test_running.is_some())
        {
            return;
        }
        self.save_service_draft_then(AfterDraftSaved::Test, window, cx);
    }

    fn run_service_test(&mut self, cx: &mut Context<Self>) {
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
        self.save_service_draft_then(AfterDraftSaved::Publish { continue_repair }, window, cx);
    }

    fn publish_saved_draft(&mut self, continue_repair: bool, cx: &mut Context<Self>) {
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
        let scope = publish_scope(repairing, target.is_some(), editor.also_default);
        let plan = PublishPlan {
            draft_id: editor.form.draft.id.clone(),
            continue_repair,
            target,
            repair_task,
            scope,
        };
        if let Some(version) = editor
            .pending_binding
            .as_ref()
            .or(editor.saved_configuration.as_ref())
        {
            let version = version.clone();
            self.finish_service_publish(Ok(version), plan, cx);
            return;
        }
        // Publication first confirms the draft's credential is in the vault,
        // which can block; only that check leaves the UI thread.
        let reference = match self.preferences.publication_credential(&plan.draft_id) {
            Ok(reference) => reference,
            Err(error) => {
                self.finish_service_publish(Err(error), plan, cx);
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
            self.finish_service_publish(result, plan, cx);
            return;
        }
        if let Some(editor) = &mut self.settings_ui.editor {
            editor.vault_busy = true;
        }
        self.run_vault_step(
            cx,
            move |vault| preferences::Store::verify_credential(vault, reference),
            move |this, verified, _, cx| {
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
                this.finish_service_publish(result, plan, cx);
            },
        );
        cx.notify();
    }

    /// Takes a key from an environment variable into the vault for this draft.
    fn use_environment_credential(&mut self, name: &'static str, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.settings_ui.editor else {
            return;
        };
        let draft = editor.form.current_draft(cx);
        let secret = match preferences::Store::environment_secret(draft.protocol.purpose(), name) {
            Ok(secret) => secret,
            Err(error) => {
                editor.status = Some(course2md::tr!("凭据尚未保存：{}", format!("{error:#}")));
                cx.notify();
                return;
            }
        };
        editor.vault_busy = true;
        self.store_editor_secret(
            draft,
            secret,
            move |this, draft, reference| {
                this.preferences
                    .commit_environment_credential(draft, name, reference)
            },
            |this, committed, window, cx| {
                let Some(editor) = &mut this.settings_ui.editor else {
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
                        editor.status =
                            Some(course2md::tr!("凭据尚未保存：{}", format!("{error:#}")));
                    }
                }
                cx.notify();
            },
            cx,
        );
        cx.notify();
    }

    /// Publication's commit continues here: bind the version where the
    /// editor was opened from, start a repair, report and close.
    fn finish_service_publish(
        &mut self,
        result: Result<ServiceVersion>,
        plan: PublishPlan,
        cx: &mut Context<Self>,
    ) {
        let PublishPlan {
            continue_repair,
            target,
            repair_task,
            scope,
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
        self.clear_settings_failure(PreferenceGroup::Services);
        self.dismiss_service_editor(cx);
        if !repairing {
            self.advance_conversion_when_ready(cx);
        }
        cx.notify();
    }
    pub(crate) fn close_service_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = &self.settings_ui.editor {
            // Tests can stage a private edit, but cancelling never publishes
            // it. A cleanup failure cannot make these hidden records active.
            let _ = self
                .preferences
                .discard_service_draft(&editor.form.draft.id);
        }
        self.dismiss_service_editor(cx);
    }
    fn dismiss_service_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.settings_ui.editor.take() {
            if let Some(cancel) = &editor.test_running {
                cancel.store(true, Ordering::Release);
            }
            self.settings_ui.closed_editor = Some(editor);
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
        ui::Confirm::new(
            course2md::tr!("更新默认服务"),
            course2md::tr!("保留原默认"),
            course2md::tr!("用于以后生成"),
        )
        .text(previous)
        .text(next)
        .text(course2md::tr!("仅影响以后生成的笔记"))
        .ask(
            cx.listener(move |this, _, _, cx| {
                match this
                    .preferences
                    .set_default_service(purpose, Some(&version_id))
                {
                    Ok(()) => this.clear_settings_failure(PreferenceGroup::Services),
                    Err(error) => {
                        this.set_settings_failure(
                            PreferenceGroup::Services,
                            course2md::tr!("默认服务尚未更改，请重试").into(),
                        );
                        this.settings_ui
                            .failure_details
                            .insert(PreferenceGroup::Services, format!("{error:#}"));
                    }
                }
                cx.notify();
            }),
            window,
            cx,
        );
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
        let uses = |id: &Option<String>| id.as_ref().is_some_and(|id| version_ids.contains(id));
        let affected = self.workspace.as_ref().map_or(0, |w| {
            w.state
                .tasks
                .iter()
                .filter(|task| {
                    !task.state.finished()
                        && (uses(&task.plan.asr_service) || uses(&task.plan.ai_service))
                })
                .count()
        });
        let mut confirm = ui::Confirm::new(
            course2md::tr!("删除此服务"),
            course2md::tr!("保留服务"),
            course2md::tr!("删除服务"),
        )
        .danger()
        .text(course2md::tr!("将删除此服务配置，已有笔记不受影响。"));
        if affected > 0 {
            confirm = confirm.text(course2md::tr!(
                "有 {} 个未完成任务引用此服务；这些任务中用到此服务的步骤将不再继续。",
                affected
            ));
        }
        confirm.ask(
            cx.listener(move |this, _, _, cx| {
                match this.preferences.retire_service(&service_id) {
                    Ok(released) => {
                        this.forget_credentials(released);
                        this.clear_settings_failure(PreferenceGroup::Services);
                    }
                    Err(error) => {
                        // The retire marker is written before the group is persisted,
                        // so a mid-failure can leave dispatch blocked without a visible
                        // deletion; say so instead of claiming nothing happened.
                        let blocked = this.preferences.is_service_retired(&service_id);
                        this.set_settings_failure(
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
                        );
                    }
                }
                this.refresh_dispatch_controls(cx);
                cx.notify();
            }),
            window,
            cx,
        );
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
                self.clear_settings_failure(PreferenceGroup::Generation);
                self.refresh_preference_defaults(cx);
                self.clear_resolved_generation_errors(previous);
                if !self.preference_defaults_pending {
                    self.advance_conversion_when_ready(cx);
                }
                true
            }
            Err(error) => {
                self.settings_ui.pending_generation = Some(next);
                self.set_settings_save_failure(PreferenceGroup::Generation, &error);
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
