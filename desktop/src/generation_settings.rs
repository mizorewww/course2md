//! Generation defaults page: speech/AI model, languages and vision switches.
use super::model_diagnostics::{
    LOCAL_ENGINES, cache_state_label, engine_icon, engine_offered, model_supported,
    unsupported_model,
};
use super::*;
use course2md::config::{AsrProvider, OutputFormat, TranscriptSource, local_models, model_label};
use desktop_tasks::import::{text_source_choices, text_source_id};

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

    fn subtitle_fields(&self, window: &Window, cx: &mut Context<Self>) -> Vec<ui::Field> {
        let value = self.preferences.generation();
        let source = value.options.transcript_source;
        let labels: Vec<&str> = text_source_choices()
            .iter()
            .map(|(_, label)| *label)
            .collect();
        let fits = super::segmented_fits(&labels, true, window);
        let group = ConnectedButtonGroup::new("default-text-source", course2md::tr!("文字来源"))
            .tone(ButtonTone::Neutral)
            .segmented()
            // As wide as its options while they fit the row; else they stack whole.
            .when(fits, |group| group.fit_content())
            .vertical(!fits)
            .button_size(ButtonSize::Small)
            .options(text_source_choices().map(|(mode, label)| (text_source_id(mode), label)))
            .icon("auto", symbols::auto_fix_high())
            .icon("subtitle", symbols::subtitles())
            .icon("asr", symbols::mic())
            .selected(text_source_id(source.unwrap_or_default()))
            .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                let mut next = this.generation_edit_base();
                next.options.transcript_source = text_source_choices()
                    .into_iter()
                    .map(|(mode, _)| mode)
                    .find(|mode| text_source_id(*mode) == selected.as_ref());
                this.commit_generation(next, cx);
            }));
        let source_hint = match source {
            Some(TranscriptSource::Subtitle) => {
                course2md::tr!(
                    "视频没有字幕时，视频卡片会说明；本地视频可选择字幕文件，否则需更改文字来源"
                )
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
            // The group, then what its choice means, as on home's options.
            ui::Field::below(
                "default-text-source-row",
                course2md::tr!("文字来源"),
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(8.))
                    .child(div().when(fits, |row| row.flex()).child(group))
                    .child(supporting_info("default-text-source-hint", source_hint)),
            ),
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
                        .shape(TextFieldShape::Rounded)
                        .w_full()
                        .label(course2md::tr!("语言优先级"))
                        .float_label(true)
                        .placeholder(course2md::tr!("按优先顺序填写语言代码，如 zh-Hans, en")),
                    material_button("apply-languages", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
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
        let methods = [course2md::tr!("本机识别"), course2md::tr!("在线语音服务")];
        let fits = super::segmented_fits(&methods, true, window);
        let group = ConnectedButtonGroup::new("default-asr-device", course2md::tr!("识别方式"))
            .tone(ButtonTone::Neutral)
            .segmented()
            .when(fits, |group| group.fit_content())
            .vertical(!fits)
            .button_size(ButtonSize::Small)
            .options([("local", methods[0]), ("online", methods[1])])
            // The same icons the choice has in first use.
            .icon("local", symbols::computer())
            .icon("online", symbols::cloud())
            .selected(if online { "online" } else { "local" })
            .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                let mut next = this.generation_edit_base();
                next.select_recognition_location(selected.as_ref() == "online");
                this.commit_generation(next, cx);
            }));
        let method = ui::Field::below(
            "default-asr-method",
            course2md::tr!("识别方式"),
            div().when(fits, |row| row.flex()).child(group),
        )
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
        // The value is the model; its state is the row's supporting text, so the
        // field shows the name whole.
        let model_value = model_name;
        // A model not on disk says, with its state, what happens when it is needed.
        let missing = matches!(
            state,
            Some(course2md::models::status::CacheState::Missing)
                | Some(course2md::models::status::CacheState::Partial)
        );
        let model_hint: SharedString = if missing && engine_issue.is_none() && settled {
            format!("{state_label} · {conclusion}").into()
        } else {
            state_label.into()
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
                    .items(
                        ui::choice_items(
                            models.iter().map(|model| (model.id, model.label)),
                            &current_model,
                            cx.listener(|this, id: &&'static str, window, cx| {
                                this.select_local_model(id, window, cx)
                            }),
                        )
                        .zip(models.iter())
                        // Each model says what it is good for and what it downloads.
                        .map(|(item, model)| {
                            let tradeoff = desktop_prefs::setup::model_tradeoff(provider, model.id);
                            if tradeoff.is_empty() {
                                item
                            } else {
                                item.supporting_text(tradeoff)
                            }
                        }),
                    ),
            )
            .hint(model_hint.clone())
        } else {
            ui::Field::inline(
                "default-local-model",
                course2md::tr!("识别模型"),
                TileScale::Regular.support("default-local-model-name", model_value),
            )
            .hint(model_hint)
        });
        // An engine that cannot run here says why with its repair; otherwise an unsettled
        // check says what is still open. A finished check on a ready model says nothing.
        if let Some(issue) = engine_issue {
            fields.push(ui::Field::content(
                "generation-engine-obstacle",
                self.engine_obstacle("generation-engine-obstacle-detail", issue, true, cx),
            ));
        } else if !(settled && (ready || missing)) {
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
                        .shape(TextFieldShape::Rounded)
                        .w_full()
                        .label(course2md::tr!(
                            "模型名称或 Hugging Face 仓库（如 org/model）"
                        ))
                        .float_label(true),
                    material_button("apply-custom-npu-model", ButtonVariant::Outlined)
                        .tone(ButtonTone::Accent)
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
        .map(|(item, engine)| item.leading(Icon::from(engine_icon(*engine))));
        fields.push(ui::Field::menu(
            "default-asr-hardware",
            course2md::tr!("本机引擎"),
            crate::provider_label(selected_provider),
            MaterialMenu::new("asr-hardware-items")
                .width(MENU_WIDTH)
                .items(engine_items),
        ));
        // The model and its engine read together; managing downloads closes the group.
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
                .items(ui::choice_items(
                    std::iter::once((None, course2md::tr!("不翻译"))).chain(
                        course2md::language::TARGETS
                            .iter()
                            .map(|(code, _)| (Some(*code), course2md::language::native_name(code))),
                    ),
                    &current,
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
            // Off, it says why it cannot be turned on.
            .hint(if value.ai.enabled {
                ""
            } else {
                course2md::tr!("打开「校对文字和标点」后可用")
            })
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
                    .shape(TextFieldShape::Rounded)
                    .label(course2md::tr!("自定义校对规则"))
                    .w_full(),
            )
            .child(
                h_flex()
                    .gap(rem(8.))
                    .flex_wrap()
                    .child(
                        material_button("apply-proofread-rules", ButtonVariant::Outlined)
                            .tone(ButtonTone::Accent)
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
        let location = self.workspace(cx).and_then(|workspace| {
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
                        ("default-export-format-item", index),
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
                self.subtitle_fields(window, cx),
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
