//! Generation defaults page: speech/AI model, languages and vision switches.
use super::model_diagnostics::{
    LOCAL_ENGINES, cache_state_label, engine_icon, engine_offered, model_supported,
    unsupported_model,
};
use super::*;
use course2md::config::{AsrProvider, OutputFormat, TranscriptSource, local_models, model_label};

const SUBTITLE_LANGUAGES: [(&str, &str); 5] = [
    ("", "自动"),
    ("zh-Hans", "简体中文"),
    ("zh-Hant", "繁體中文"),
    ("en", "英语"),
    ("ja", "日语"),
];
/// The language menu's entry for a typed priority list.
const CUSTOM_LANGUAGES: &str = "custom";

impl Desktop {
    /// The bilingual targets, shared by settings and the per-task options.
    fn translation_items(
        current: Option<&'static str>,
        pick: impl Fn(&Option<&'static str>, &mut Window, &mut App) + 'static,
    ) -> Vec<MaterialMenuItem> {
        ui::choice_items(
            std::iter::once((None, course2md::tr!("不翻译"))).chain(
                course2md::language::TARGETS
                    .iter()
                    .map(|(code, _)| (Some(*code), course2md::language::native_name(code))),
            ),
            &current,
            pick,
        )
        .collect()
    }

    /// One AI, recognition or saving default that turns on or off.
    fn generation_switch(
        id: &'static str,
        label: &'static str,
        on: bool,
        set: fn(&mut GenerationPreferences, bool),
        cx: &mut Context<Self>,
    ) -> ui::Field {
        ui::Field::switch(
            id,
            label,
            on,
            cx.listener(move |this, on: &bool, _, cx| {
                let mut next = this.generation_edit_base();
                set(&mut next, *on);
                this.commit_generation(next, cx);
            }),
        )
    }

    fn subtitle_fields(&self, cx: &mut Context<Self>) -> Vec<ui::Field> {
        let value = self.preferences.generation();
        let source = value.options.transcript_source;
        let group = ConnectedButtonGroup::new("default-text-source", course2md::tr!("文字来源"))
            .segmented()
            .button_size(ButtonSize::Small)
            .options([
                ("auto", course2md::tr!("自动")),
                ("subtitle", course2md::tr!("仅字幕")),
                ("asr", course2md::tr!("语音识别")),
            ])
            .icon("auto", symbols::auto_fix_high())
            .icon("subtitle", symbols::subtitles())
            .icon("asr", symbols::mic())
            .selected(match source {
                Some(TranscriptSource::Subtitle) => "subtitle",
                Some(TranscriptSource::Asr) => "asr",
                _ => "auto",
            })
            .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                let mut next = this.generation_edit_base();
                next.options.transcript_source = Some(match selected.as_ref() {
                    "subtitle" => TranscriptSource::Subtitle,
                    "asr" => TranscriptSource::Asr,
                    _ => TranscriptSource::Auto,
                });
                this.commit_generation(next, cx);
            }));
        let source_hint = match source {
            Some(TranscriptSource::Subtitle) => {
                course2md::tr!("没有字幕时会提示选择字幕文件或改用语音识别")
            }
            Some(TranscriptSource::Asr) => course2md::tr!("语音识别，不使用现有字幕"),
            _ => course2md::tr!("有字幕时直接用字幕，没有字幕时使用语音识别"),
        };
        let languages = &value.preferred_subtitle_languages;
        let known = |id: &str| SUBTITLE_LANGUAGES.iter().any(|(known, _)| *known == id);
        let customized = languages.len() > 1 || languages.first().is_some_and(|one| !known(one));
        let custom = customized
            || self.settings_ui.custom_languages
            || value.subtitle_languages_draft.is_some();
        let (current, shown) = if custom {
            (CUSTOM_LANGUAGES, course2md::tr!("自定义优先级"))
        } else {
            let first = languages.first().map_or("", String::as_str);
            SUBTITLE_LANGUAGES
                .iter()
                .find(|(id, _)| *id == first)
                .map_or(("", course2md::tr!("自动")), |(id, label)| {
                    (*id, course2md::tr!(label))
                })
        };
        let items = ui::choice_items(
            SUBTITLE_LANGUAGES
                .into_iter()
                .map(|(id, label)| (id, course2md::tr!(label)))
                .chain([(CUSTOM_LANGUAGES, course2md::tr!("自定义优先级"))]),
            &current,
            cx.listener(|this, id: &&'static str, window, cx| {
                this.pick_subtitle_language(id, window, cx)
            }),
        );
        let mut fields = vec![
            ui::Field::below("default-text-source-row", course2md::tr!("文字来源"), group)
                .hint(source_hint),
            ui::Field::menu(
                "default-subtitle-language",
                course2md::tr!("字幕语言"),
                shown,
                MaterialMenu::new("subtitle-language-items")
                    .width(MENU_WIDTH)
                    .items(items),
            )
            .hint(if customized {
                languages.join(" → ")
            } else {
                String::new()
            }),
        ];
        if custom {
            fields.push(ui::Field::content(
                "subtitle-language-details",
                Self::field_with_apply(
                    TextField::new("subtitle-languages", &self.settings_ui.languages)
                        .outlined()
                        .w_full()
                        .label(course2md::tr!("语言优先级"))
                        .placeholder(course2md::tr!("按优先顺序填写语言代码，如 zh-Hans, en")),
                    material_button("apply-languages", ButtonVariant::Outlined)
                        .icon(symbols::check_circle())
                        .child(course2md::tr!("应用"))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.apply_language_preferences(cx)),
                        ),
                ),
            ));
        }
        fields
    }

    fn pick_subtitle_language(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if id == CUSTOM_LANGUAGES {
            self.settings_ui.custom_languages = true;
            cx.notify();
            return;
        }
        let mut next = self.generation_edit_base();
        next.preferred_subtitle_languages = if id.is_empty() {
            Vec::new()
        } else {
            vec![id.to_owned()]
        };
        next.subtitle_languages_draft = None;
        if self.commit_generation(next, cx) {
            self.settings_ui.custom_languages = false;
            self.settings_ui
                .languages
                .update(cx, |input, cx| input.set_value(id.to_owned(), window, cx));
        }
    }

    /// A text field and the action that applies what was typed.
    fn field_with_apply(field: TextField, apply: MaterialButton) -> Div {
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap(rem(12.))
            .items_center()
            .child(div().flex_1().flex_basis(rem(240.)).min_w_0().child(field))
            .child(apply)
    }

    fn recognition_fields(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<ui::Field> {
        let preferences = self.preferences.generation();
        let selected_provider = preferences.options.provider;
        let online = selected_provider == Some(AsrProvider::Api);
        let group = ConnectedButtonGroup::new("default-asr-device", course2md::tr!("识别方式"))
            .segmented()
            .button_size(ButtonSize::Small)
            .options([
                ("local", course2md::tr!("本机识别")),
                ("online", course2md::tr!("在线语音服务")),
            ])
            .selected(if online { "online" } else { "local" })
            .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                let mut next = this.generation_edit_base();
                next.select_recognition_location(selected.as_ref() == "online");
                this.commit_generation(next, cx);
            }));
        let method = ui::Field::below("default-asr-method", course2md::tr!("识别方式"), group)
            .hint(if online {
                course2md::tr!("音频会发送到所选服务")
            } else {
                ""
            });
        if online {
            return vec![method, self.service_field(ServicePurpose::Speech, cx)];
        }
        let provider = selected_provider.unwrap_or_else(|| self.recommended_local_provider());
        // Automatic selection may resolve to the NPU, which runs its default
        // in place of a Qwen3 choice saved for another engine.
        let selected =
            course2md::config::effective_model(provider, preferences.options.asr_model.as_deref());
        let canonical = course2md::config::canonical_model(provider, selected).ok();
        let models = local_models(provider);
        let model_name: SharedString = if canonical.is_some() {
            model_label(canonical.as_deref().unwrap_or(selected)).into()
        } else if let [only] = models {
            only.label.into()
        } else {
            course2md::tr!("自定义模型").into()
        };
        let state = self.default_model_state();
        let state_label = state.map(|state| cache_state_label(state).1).unwrap_or("");
        let (settled, conclusion) = self.default_model_conclusion();
        let engine_issue = self.engine_issue(provider);
        let ready = matches!(
            state,
            Some(course2md::models::status::CacheState::Loaded)
                | Some(course2md::models::status::CacheState::Cached)
        );
        let model_value: SharedString = if state_label.is_empty() {
            model_name
        } else {
            format!("{model_name} · {state_label}").into()
        };
        let current_model = models
            .iter()
            .map(|model| model.id)
            .find(|id| canonical.as_deref() == Some(*id))
            .unwrap_or_default();
        let mut fields = vec![method];
        fields.push(if models.len() > 1 {
            ui::Field::menu(
                "default-local-model-menu",
                course2md::tr!("识别模型"),
                model_value,
                MaterialMenu::new("local-model-items")
                    .width(MENU_WIDTH)
                    .items(ui::choice_items(
                        models.iter().map(|model| (model.id, model.label)),
                        &current_model,
                        cx.listener(|this, id: &&'static str, window, cx| {
                            this.select_local_model(id, window, cx)
                        }),
                    )),
            )
        } else {
            ui::Field::inline(
                "default-local-model",
                course2md::tr!("识别模型"),
                TileScale::Regular.support("default-local-model-name", model_value),
            )
        });
        // An engine that cannot run here says why with its repair; otherwise an unsettled
        // check says what is still open. A finished check on a ready model says nothing.
        if let Some(issue) = engine_issue {
            fields.push(ui::Field::content(
                "generation-engine-obstacle",
                self.engine_obstacle("generation-engine-obstacle-detail", issue, cx),
            ));
        } else if !(settled && ready) {
            let status = if settled {
                ui::AsyncStatus::Success(conclusion.into())
            } else {
                ui::AsyncStatus::Warning(conclusion.into())
            };
            fields.push(ui::Field::content(
                "model-readiness-conclusion",
                div().children(ui::async_status("model-readiness", &status, None, cx)),
            ));
        }
        if !model_supported(provider, selected) {
            fields.push(ui::Field::content(
                "unavailable-fixed-model",
                error_text("unavailable-fixed-model-detail", unsupported_model()),
            ));
        }
        if provider == AsrProvider::Npu {
            fields.push(ui::Field::content(
                "custom-npu-model",
                Self::field_with_apply(
                    TextField::new("custom-npu-model-input", &self.settings_ui.local_model)
                        .outlined()
                        .w_full()
                        .label(course2md::tr!(
                            "模型名称或 Hugging Face 仓库（如 org/model）"
                        )),
                    material_button("apply-custom-npu-model", ButtonVariant::Outlined)
                        .icon(symbols::check_circle())
                        .child(course2md::tr!("应用"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let model = this.setting_input(EditField::LocalModel, cx);
                            let model = model.trim().to_owned();
                            let mut next = this.generation_edit_base();
                            next.options.asr_model = (!model.is_empty()).then_some(model);
                            next.local_model_draft = None;
                            this.commit_generation(next, cx);
                        })),
                ),
            ));
        }
        fields.push(ui::Field::disclosure(
            "toggle-model-details",
            course2md::tr!("管理模型与下载"),
            self.settings_ui.model_details_open,
            cx.listener(|this, _, _, cx| {
                this.settings_ui.model_details_open = !this.settings_ui.model_details_open;
                cx.notify();
            }),
            div().child(self.model_management_panel(window, cx)),
        ));
        let engines: Vec<_> = std::iter::once(None)
            .chain(
                LOCAL_ENGINES
                    .into_iter()
                    .filter(|engine| engine_offered(*engine, selected_provider))
                    .map(Some),
            )
            .collect();
        let engine_items = ui::choice_items(
            engines
                .iter()
                .map(|engine| (*engine, crate::provider_label(*engine))),
            &selected_provider,
            cx.listener(|this, engine: &Option<AsrProvider>, _, cx| {
                let mut next = this.generation_edit_base();
                next.select_provider(*engine);
                this.commit_generation(next, cx);
            }),
        )
        .zip(engines.iter())
        .map(|(item, engine)| item.leading(engine_icon(*engine)));
        fields.push(ui::Field::menu(
            "default-asr-hardware",
            course2md::tr!("本机引擎"),
            crate::provider_label(selected_provider),
            MaterialMenu::new("asr-hardware-items")
                .width(MENU_WIDTH)
                .items(engine_items),
        ));
        fields
    }

    fn select_local_model(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut next = self.generation_edit_base();
        next.options.asr_model = Some(id.to_owned());
        next.local_model_draft = None;
        if self.commit_generation(next, cx) {
            self.settings_ui
                .local_model
                .update(cx, |input, cx| input.set_value(id.to_owned(), window, cx));
        }
    }

    fn ai_fields(&self, cx: &mut Context<Self>) -> Vec<ui::Field> {
        let value = self.preferences.generation();
        let current = value
            .ai
            .translate
            .as_deref()
            .and_then(course2md::language::target);
        let shown = current
            .map(course2md::language::native_name)
            .unwrap_or(course2md::tr!("不翻译"));
        let translation = ui::Field::menu(
            "default-ai-translate",
            course2md::tr!("双语翻译"),
            shown,
            MaterialMenu::new("default-ai-translate-items")
                .width(MENU_WIDTH)
                .items(Self::translation_items(
                    current,
                    cx.listener(|this, code: &Option<&'static str>, _, cx| {
                        let mut next = this.generation_edit_base();
                        next.ai.translate = code.map(str::to_owned);
                        this.commit_generation(next, cx);
                    }),
                )),
        )
        .hint(if current.is_some() {
            course2md::tr!("每段文字下方附上译文；已是该语言的视频不翻译")
        } else {
            ""
        });
        let mut fields = vec![
            Self::generation_switch(
                "default-ai-proofread",
                course2md::tr!("校对文字和标点"),
                value.ai.enabled,
                |next, on| next.ai.enabled = on,
                cx,
            ),
            Self::generation_switch(
                "default-ai-summary",
                course2md::tr!("在开头生成摘要"),
                value.ai.summarize,
                |next, on| next.ai.summarize = on,
                cx,
            ),
            Self::generation_switch(
                "default-ai-vision",
                course2md::tr!("校对时附带对应截图"),
                value.ai.vision,
                |next, on| next.ai.vision = on,
                cx,
            )
            .disabled(!value.ai.enabled),
            translation,
            self.service_field(ServicePurpose::Ai, cx),
        ];
        // The rule only applies to proofreading, so it appears with it.
        if value.ai.enabled {
            fields.push(self.proofread_rules_field(cx));
        }
        fields
    }

    fn proofread_rules_field(&self, cx: &mut Context<Self>) -> ui::Field {
        let customized = self.preferences.generation().ai.prompt.is_some();
        let detail = v_flex()
            .gap(rem(12.))
            .child(
                TextField::multi_line("proofread-rules", &self.settings_ui.prompt)
                    .outlined()
                    .label(course2md::tr!("自定义校对规则"))
                    .w_full(),
            )
            .child(
                h_flex()
                    .gap(rem(8.))
                    .flex_wrap()
                    .child(
                        material_button("apply-proofread-rules", ButtonVariant::Outlined)
                            .icon(symbols::save())
                            .child(course2md::tr!("应用校对规则"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let prompt = this.settings_ui.prompt.read(cx).value();
                                let prompt = prompt.trim().to_owned();
                                let mut next = this.generation_edit_base();
                                next.ai.prompt = (!prompt.is_empty()
                                    && prompt != course2md::llm::DEFAULT_PROMPT.trim())
                                .then_some(prompt);
                                next.prompt_draft = None;
                                this.commit_generation(next, cx);
                            })),
                    )
                    .child(
                        material_button("restore-proofread-rules", ButtonVariant::Text)
                            .icon(symbols::refresh())
                            .child(course2md::tr!("恢复标准规则"))
                            .disabled(!customized)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let mut next = this.generation_edit_base();
                                next.ai.prompt = None;
                                next.prompt_draft = None;
                                if this.commit_generation(next, cx) {
                                    this.settings_ui.prompt.update(cx, |input, cx| {
                                        input.set_value(course2md::llm::DEFAULT_PROMPT, window, cx)
                                    });
                                }
                            })),
                    ),
            );
        ui::Field::disclosure(
            "show-proofread-rules",
            course2md::tr!("自定义校对规则"),
            self.settings_ui.show_prompt,
            cx.listener(|this, _, window, cx| {
                this.settings_ui.show_prompt = !this.settings_ui.show_prompt;
                // The standard rule is edited in place rather than printed beside an empty box.
                if this.settings_ui.show_prompt
                    && this.settings_ui.prompt.read(cx).value().trim().is_empty()
                {
                    this.settings_ui.prompt.update(cx, |input, cx| {
                        input.set_value(course2md::llm::DEFAULT_PROMPT, window, cx)
                    });
                }
                cx.notify();
            }),
            detail,
        )
        .hint(if customized {
            course2md::tr!("已自定义")
        } else {
            course2md::tr!("使用标准规则")
        })
    }

    fn saving_fields(&self, cx: &mut Context<Self>) -> Vec<ui::Field> {
        let value = self.preferences.generation();
        let selected = value.options.formats.clone().unwrap_or_default();
        let location = self.workspace.as_ref().and_then(|workspace| {
            let state = &workspace.state;
            state
                .libraries
                .iter()
                .find(|library| library.id == state.default_library)
                .map(|library| {
                    course2md::config::simplify_verbatim(library.root.clone())
                        .display()
                        .to_string()
                })
        });
        let formats = [
            (OutputFormat::Md, "Markdown"),
            (OutputFormat::Html, course2md::tr!("网页")),
            (OutputFormat::Json, "JSON"),
        ];
        let formats = ChipSet::new("default-export", course2md::tr!("同时导出")).items(
            formats
                .into_iter()
                .enumerate()
                .map(|(index, (format, label))| {
                    ChipSetItem::new(
                        FilterChip::new(("default-export-format", index), label)
                            .selected(selected.contains(&format))
                            .on_toggle(cx.listener(move |this, _: &bool, _, cx| {
                                let mut next = this.generation_edit_base();
                                let formats = next.options.formats.get_or_insert_with(Vec::new);
                                if formats.contains(&format) {
                                    formats.retain(|value| *value != format);
                                } else {
                                    formats.push(format);
                                }
                                this.commit_generation(next, cx);
                            })),
                    )
                }),
        );
        vec![
            ui::Field::link(
                "settings-manage-storage",
                course2md::tr!("保存位置"),
                cx.listener(|this, _, window, cx| {
                    this.select_settings_tab(SettingsTab::Storage, window, cx);
                }),
            )
            .hint(location.unwrap_or_else(|| course2md::tr!("尚未设置").into())),
            ui::Field::below("export-format", course2md::tr!("同时导出"), formats),
            Self::generation_switch(
                "default-keep-video",
                course2md::tr!("保留下载的视频"),
                value.options.keep_video.unwrap_or(false),
                |next, on| next.options.keep_video = Some(on),
                cx,
            )
            .hint(course2md::tr!("可离线播放，会占用更多磁盘空间")),
        ]
    }

    pub(super) fn generation_settings_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(28.))
            .child(self.settings_fields(
                "language-settings",
                course2md::tr!("字幕与识别"),
                self.subtitle_fields(cx),
            ))
            .child(self.settings_fields(
                "asr-default-settings",
                course2md::tr!("语音识别"),
                self.recognition_fields(window, cx),
            ))
            .child(self.settings_fields(
                "ai-default-settings",
                course2md::tr!("AI 处理"),
                self.ai_fields(cx),
            ))
            .child(self.settings_fields(
                "export-default-settings",
                course2md::tr!("保存与导出"),
                self.saving_fields(cx),
            ))
            .into_any_element()
    }
}
