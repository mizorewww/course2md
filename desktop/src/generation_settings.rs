//! Generation defaults page: speech/AI model, languages and vision switches.
use super::model_diagnostics::{
    LOCAL_ENGINES, engine_from_id, engine_icon, engine_id, engine_offered, model_supported,
    unsupported_model,
};
use super::*;

const SUBTITLE_LANGUAGES: [(&str, &str); 5] = [
    ("", "自动"),
    ("zh-Hans", "简体中文"),
    ("zh-Hant", "繁體中文"),
    ("en", "英语"),
    ("ja", "日语"),
];

fn subtitle_language_label(id: &str) -> &'static str {
    SUBTITLE_LANGUAGES
        .iter()
        .find(|(language, _)| *language == id)
        .map(|(_, label)| course2md::tr!(label))
        .unwrap_or_else(|| course2md::tr!("自动"))
}
use course2md::config::{AsrProvider, local_models, model_label};

impl Desktop {
    /// The bilingual target menu shared by Settings and the per-task options.
    pub(crate) fn translation_select(
        &self,
        id: &'static str,
        current: Option<&'static str>,
        pick: impl Fn(&mut Self, Option<&'static str>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> SelectField {
        let entity = cx.weak_entity();
        let pick = std::rc::Rc::new(pick);
        SelectField::new(
            id,
            current
                .map(course2md::language::native_name)
                .unwrap_or(course2md::tr!("不翻译")),
            move |menu, _, _| {
                std::iter::once((None, course2md::tr!("不翻译")))
                    .chain(
                        course2md::language::TARGETS
                            .iter()
                            .map(|(code, _)| (Some(*code), course2md::language::native_name(code))),
                    )
                    .fold(menu, |menu, (code, label)| {
                        let entity = entity.clone();
                        let pick = pick.clone();
                        menu.item(PopupMenuItem::new(label).checked(code == current).on_click(
                            move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| pick(this, code, cx));
                            },
                        ))
                    })
            },
        )
        .label(course2md::tr!("双语翻译"))
    }

    /// Bilingual notes: the language each paragraph is also translated into.
    fn translation_setting(
        &self,
        value: &crate::preferences::GenerationPreferences,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = value
            .translate
            .as_deref()
            .and_then(course2md::language::target);
        settings_outlined_row(
            "default-ai-translate-row",
            if current.is_some() {
                course2md::tr!("每段文字下方附上译文；已是该语言的视频不翻译")
            } else {
                ""
            },
            self.translation_select(
                "default-ai-translate",
                current,
                |this, code, cx| {
                    let mut next = this.generation_edit_base();
                    next.translate = code.map(str::to_owned);
                    this.commit_generation(next, cx);
                },
                cx,
            ),
        )
    }

    pub(super) fn generation_settings_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.preferences.generation();
        let provider = value.options.provider;
        let online = provider == Some(AsrProvider::Api);
        let language = match value.preferred_subtitle_languages.as_slice() {
            [one] if SUBTITLE_LANGUAGES.iter().any(|(id, _)| id == one) => one.as_str(),
            _ => "",
        };
        let source_labels =
            if f32::from(window.bounds().size.width) / (f32::from(window.rem_size()) / 14.) < 700.0
            {
                (
                    course2md::tr!("自动（紧凑）"),
                    course2md::tr!("仅字幕（紧凑）"),
                    course2md::tr!("语音识别（紧凑）"),
                )
            } else {
                (
                    course2md::tr!("自动"),
                    course2md::tr!("仅字幕"),
                    course2md::tr!("语音识别"),
                )
            };
        let custom_language = value.preferred_subtitle_languages.len() > 1
            || value
                .preferred_subtitle_languages
                .first()
                .is_some_and(|one| !SUBTITLE_LANGUAGES.iter().any(|(id, _)| id == one));
        let languages = group("language-settings", course2md::tr!("字幕与识别"))
            .child(settings_stacked_row(
                "default-text-source-label",
                course2md::tr!("文字来源"),
                match value.options.transcript_source {
                    Some(course2md::config::TranscriptSource::Subtitle) => {
                        course2md::tr!("没有字幕时会提示选择字幕文件或改用语音识别")
                    }
                    Some(course2md::config::TranscriptSource::Asr) => {
                        course2md::tr!("语音识别，不使用现有字幕")
                    }
                    _ => course2md::tr!("有字幕时直接用字幕，没有字幕时使用语音识别"),
                },
                self.setting_choices("default-text-source", course2md::tr!("文字来源"))
                    .options([
                        ("auto", source_labels.0),
                        ("subtitle", source_labels.1),
                        ("asr", source_labels.2),
                    ])
                    .icon("auto", icons::auto_fix())
                    .icon("subtitle", icons::subtitles())
                    .icon("asr", icons::mic())
                    .full_width()
                    .selected(match value.options.transcript_source {
                        Some(course2md::config::TranscriptSource::Subtitle) => "subtitle",
                        Some(course2md::config::TranscriptSource::Asr) => "asr",
                        _ => "auto",
                    })
                    .on_change(cx.listener(|this, selected: &SharedString, _, cx| {
                        let mut next = this.generation_edit_base();
                        next.options.transcript_source = Some(match selected.as_ref() {
                            "subtitle" => course2md::config::TranscriptSource::Subtitle,
                            "asr" => course2md::config::TranscriptSource::Asr,
                            _ => course2md::config::TranscriptSource::Auto,
                        });
                        this.commit_generation(next, cx);
                    })),
            ))
            .child(settings_outlined_row(
                "subtitle-language-label",
                "",
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .child(self.reveal_setting("default-subtitle-language-reveal", {
                        let current = language.to_owned();
                        let entity = cx.weak_entity();
                        SelectField::new(
                            "default-subtitle-language",
                            if custom_language {
                                course2md::tr!("自定义优先级")
                            } else {
                                subtitle_language_label(language)
                            },
                            move |menu, _, _| {
                                SUBTITLE_LANGUAGES
                                    .into_iter()
                                    .map(|(id, label)| (id, course2md::tr!(label)))
                                    .fold(menu, |menu, (id, label)| {
                                        let entity = entity.clone();
                                        menu.item(
                                            PopupMenuItem::new(label)
                                                .checked(id == current)
                                                .on_click(move |_, window, cx| {
                                                    let _ = entity.update(cx, |this, cx| {
                                                        let mut next = this.generation_edit_base();
                                                        next.preferred_subtitle_languages =
                                                            if id.is_empty() {
                                                                Vec::new()
                                                            } else {
                                                                vec![id.to_owned()]
                                                            };
                                                        next.subtitle_languages_draft = None;
                                                        if this.commit_generation(next, cx) {
                                                            this.settings_ui
                                                                .language_details_open = false;
                                                            this.settings_ui.inputs
                                                                [&EditField::Languages]
                                                                .update(cx, |input, cx| {
                                                                    input.set_value(id, window, cx)
                                                                });
                                                        }
                                                    });
                                                }),
                                        )
                                    })
                            },
                        )
                        .label(course2md::tr!("字幕语言"))
                    }))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .items_center()
                            .flex_wrap()
                            .child(
                                crate::theme::item_disclosure(
                                    "toggle-language-details",
                                    course2md::tr!("自定义语言优先级"),
                                    self.settings_ui.language_details_open,
                                )
                                .settings_text()
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.settings_ui.language_details_open =
                                            !this.settings_ui.language_details_open;
                                        cx.notify();
                                    },
                                )),
                            )
                            .when(custom_language, |row| {
                                row.child(
                                    accessible_text(
                                        "subtitle-custom-summary",
                                        value.preferred_subtitle_languages.join(" → "),
                                    )
                                    .text_size(TEXT_SETTINGS_AUX)
                                    .text_color(color(GRAY)),
                                )
                            }),
                    )
                    .child(crate::motion::disclosure(
                        "subtitle-language-details",
                        self.settings_ui.language_details_open
                            || value.subtitle_languages_draft.is_some(),
                        v_flex().child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .flex_wrap()
                                .child(
                                    div().flex_1().min_w(rems(16.)).child(
                                        text_input(&self.settings_ui.inputs[&EditField::Languages])
                                            .settings_text()
                                            .w_full()
                                            .aria_label(course2md::tr!("语言优先级")),
                                    ),
                                )
                                .child(
                                    outline_pill("apply-languages")
                                        .settings_text()
                                        .icon(icons::check_circle())
                                        .label(course2md::tr!("应用"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.apply_language_preferences(cx)
                                        })),
                                ),
                        ),
                    )),
            ));
        let mut recognition =
            group("asr-default-settings", course2md::tr!("语音识别")).child(settings_outlined_row(
                "asr-method-label",
                if online {
                    course2md::tr!("音频会发送到所选服务")
                } else {
                    ""
                },
                self.reveal_setting("default-asr-device-reveal", {
                    let entity = cx.weak_entity();
                    SelectField::new(
                        "default-asr-device",
                        if online {
                            course2md::tr!("在线语音服务")
                        } else {
                            course2md::tr!("本机识别")
                        },
                        move |menu, _, _| {
                            [
                                (false, course2md::tr!("本机识别")),
                                (true, course2md::tr!("在线语音服务")),
                            ]
                            .into_iter()
                            .fold(menu, |menu, (use_online, label)| {
                                let entity = entity.clone();
                                menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(online == use_online)
                                        .on_click(move |_, _, cx| {
                                            let _ = entity.update(cx, |this, cx| {
                                                let mut next = this.generation_edit_base();
                                                next.select_recognition_location(use_online);
                                                this.commit_generation(next, cx);
                                            });
                                        }),
                                )
                            })
                        },
                    )
                    .label(course2md::tr!("识别方式"))
                    .when(!online, |field| {
                        field.trailing(crate::provider_label(provider))
                    })
                }),
            ));
        if online {
            recognition = recognition.child(self.service_picker(ServicePurpose::Speech, false, cx));
        } else {
            let engines: Vec<Option<AsrProvider>> = std::iter::once(None)
                .chain(
                    LOCAL_ENGINES
                        .into_iter()
                        .filter(|engine| engine_offered(*engine, provider))
                        .map(Some),
                )
                .collect();
            recognition =
                recognition
                    .child(self.local_model_picker(cx))
                    .detail(crate::motion::disclosure(
                        "asr-advanced-settings",
                        self.settings_ui.asr_details_open,
                        settings_stacked_row(
                            "asr-hardware-heading",
                            course2md::tr!("本机引擎"),
                            "",
                            engines
                                .iter()
                                .fold(
                                    self.setting_choices(
                                        "default-asr-hardware",
                                        course2md::tr!("本机引擎"),
                                    )
                                    .options(
                                        engines.iter().map(|engine| {
                                            (engine_id(*engine), crate::provider_label(*engine))
                                        }),
                                    ),
                                    |choices, engine| {
                                        choices.icon(engine_id(*engine), engine_icon(*engine))
                                    },
                                )
                                .full_width()
                                .selected(engine_id(provider))
                                .on_change(cx.listener(|this, id: &SharedString, _, cx| {
                                    let mut next = this.generation_edit_base();
                                    next.select_provider(engine_from_id(id));
                                    this.commit_generation(next, cx);
                                })),
                        ),
                    ));
            // The panel belongs to the model item above and shares its insets.
            let model_panel = setting_surface()
                .pt_0()
                .child(self.model_management_panel(window, cx));
            recognition = recognition.detail(crate::motion::disclosure(
                "default-model-management",
                self.settings_ui.model_details_open,
                model_panel,
            ));
        }
        let mut ai = group("ai-default-settings", course2md::tr!("AI 处理"))
            .child(
                setting_surface()
                    .child(settings_switch_row(
                        course2md::tr!("校对文字和标点"),
                        "",
                        Switch::new("default-ai-proofread")
                            .checked(value.ai_proofread)
                            .on_click(cx.listener(|this, enabled, _, cx| {
                                let mut next = this.generation_edit_base();
                                next.ai_proofread = *enabled;
                                this.commit_generation(next, cx);
                            })),
                    ))
                    .child(settings_switch_row(
                        course2md::tr!("在开头生成摘要"),
                        "",
                        Switch::new("default-ai-summary")
                            .checked(value.ai_summary)
                            .on_click(cx.listener(|this, enabled, _, cx| {
                                let mut next = this.generation_edit_base();
                                next.ai_summary = *enabled;
                                this.commit_generation(next, cx);
                            })),
                    ))
                    .child(settings_switch_row(
                        course2md::tr!("校对时附带对应截图"),
                        "",
                        Switch::new("default-ai-vision")
                            .checked(value.vision)
                            .disabled(!value.ai_proofread)
                            .on_click(cx.listener(|this, enabled, _, cx| {
                                let mut next = this.generation_edit_base();
                                next.vision = *enabled;
                                this.commit_generation(next, cx);
                            })),
                    )),
            )
            .child(self.translation_setting(value, cx))
            .child(self.service_picker(ServicePurpose::Ai, false, cx));
        // The rule only applies to proofreading, so it appears with it.
        if value.ai_proofread {
            ai = ai
                // As on the canvas, the rule is a list item showing its current value.
                .child(settings_row(
                    "proofread-rules-label",
                    course2md::tr!("自定义校对规则"),
                    "",
                    quiet("show-proofread-rules")
                        .settings_text()
                        .mr(-crate::theme::QUIET_INSET)
                        .label(if value.prompt.is_some() {
                            course2md::tr!("已自定义")
                        } else {
                            course2md::tr!("使用标准规则")
                        })
                        .icon(if self.settings_ui.show_prompt {
                            icons::chevron_up()
                        } else {
                            icons::chevron_down()
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
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
                        })),
                ))
                .detail(crate::motion::disclosure(
                    "proofread-rules-content",
                    self.settings_ui.show_prompt,
                    v_flex()
                        .px(rems(20. / 14.))
                        .pb(rems(16. / 14.))
                        .gap_3()
                        .child(
                            Textarea::new(&self.settings_ui.prompt)
                                .h(px(160.))
                                .w_full()
                                .aria_label(course2md::tr!("自定义校对规则")),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(
                                    outline_pill("apply-proofread-rules")
                                        .settings_text()
                                        .icon(icons::save())
                                        .label(course2md::tr!("应用校对规则"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let prompt = this
                                                .settings_ui
                                                .prompt
                                                .read(cx)
                                                .value()
                                                .trim()
                                                .to_owned();
                                            let mut next = this.generation_edit_base();
                                            next.prompt = (!prompt.is_empty()
                                                && prompt != course2md::llm::DEFAULT_PROMPT.trim())
                                            .then_some(prompt);
                                            next.prompt_draft = None;
                                            this.commit_generation(next, cx);
                                        })),
                                )
                                .child(
                                    quiet("restore-proofread-rules")
                                        .settings_text()
                                        .icon(icons::refresh())
                                        .label(course2md::tr!("恢复标准规则"))
                                        .disabled(value.prompt.is_none())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            let mut next = this.generation_edit_base();
                                            next.prompt = None;
                                            next.prompt_draft = None;
                                            if this.commit_generation(next, cx) {
                                                this.settings_ui.prompt.update(cx, |input, cx| {
                                                    input.set_value(
                                                        course2md::llm::DEFAULT_PROMPT,
                                                        window,
                                                        cx,
                                                    )
                                                });
                                            }
                                        })),
                                ),
                        ),
                ));
        }
        let selected = value.options.formats.clone().unwrap_or_default();
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(px(32.))
            .child(languages)
            .child(recognition)
            .child(ai)
            .child(
                group("export-default-settings", course2md::tr!("保存与导出"))
                    .child(
                        setting_surface().child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_center()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(accessible_text(
                                            "settings-save-location",
                                            course2md::tr!("保存位置"),
                                        ))
                                        .child(
                                            accessible_text(
                                                "settings-current-location",
                                                self.workspace
                                                    .as_ref()
                                                    .and_then(|workspace| {
                                                        workspace.state.libraries.iter().find(
                                                            |library| {
                                                                library.id
                                                                    == workspace
                                                                        .state
                                                                        .default_library
                                                            },
                                                        )
                                                    })
                                                    .map(|library| {
                                                        library.root.display().to_string()
                                                    })
                                                    .unwrap_or_else(|| {
                                                        course2md::tr!("尚未设置").into()
                                                    }),
                                            )
                                            .text_size(TEXT_SETTINGS_AUX)
                                            .text_color(color(GRAY))
                                            .font_family(
                                                gpui_component::Theme::global(cx)
                                                    .mono_font_family
                                                    .clone(),
                                            )
                                            .min_w_0(),
                                        ),
                                )
                                .child(
                                    outline_pill("settings-manage-storage")
                                        .settings_text()
                                        .label(course2md::tr!("更改"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.select_settings_tab(
                                                SettingsTab::Storage,
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                        ),
                    )
                    .child(settings_row(
                        "export-format-label",
                        course2md::tr!("同时导出"),
                        "",
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_3()
                            .flex_wrap()
                            .children(
                                [
                                    (course2md::config::OutputFormat::Md, "Markdown"),
                                    (
                                        course2md::config::OutputFormat::Html,
                                        course2md::tr!("网页"),
                                    ),
                                    (course2md::config::OutputFormat::Json, "JSON"),
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(index, (format, label))| {
                                    filter_chip(
                                        ("default-export", index),
                                        selected.contains(&format),
                                        cx,
                                    )
                                    .settings_text()
                                    .label(label)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let mut next = this.generation_edit_base();
                                            let formats =
                                                next.options.formats.get_or_insert_with(Vec::new);
                                            if formats.contains(&format) {
                                                formats.retain(|value| *value != format);
                                            } else {
                                                formats.push(format);
                                            }
                                            this.commit_generation(next, cx);
                                        },
                                    ))
                                }),
                            ),
                    ))
                    .child(
                        setting_surface().child(settings_switch_row(
                            course2md::tr!("保留下载的视频"),
                            course2md::tr!("可离线播放，会占用更多磁盘空间"),
                            Switch::new("default-keep-video")
                                .checked(value.options.keep_video.unwrap_or(false))
                                .on_click(cx.listener(|this, enabled, _, cx| {
                                    let mut next = this.generation_edit_base();
                                    next.options.keep_video = Some(*enabled);
                                    this.commit_generation(next, cx);
                                })),
                        )),
                    ),
            )
            .into_any_element()
    }

    fn local_model_picker(&self, cx: &mut Context<Self>) -> Div {
        let preferences = self.preferences.generation();
        let provider = preferences
            .options
            .provider
            .unwrap_or_else(|| self.recommended_local_provider());
        let selected = preferences
            .options
            .asr_model
            .as_deref()
            .unwrap_or(course2md::config::DEFAULT_ASR_MODEL);
        let canonical = course2md::config::canonical_model(provider, selected).ok();
        let models = local_models(provider);
        let picker = if let [only] = models {
            settings_value("default-local-model-single", only.label).into_any_element()
        } else if models.len() > 3 {
            let current = canonical.clone();
            let label = if canonical.is_some() {
                model_label(canonical.as_deref().unwrap_or(selected))
            } else {
                course2md::tr!("自定义模型")
            };
            let entity = cx.entity().downgrade();
            self.reveal_setting(
                "default-local-model-menu-reveal",
                SelectField::new(
                    "default-local-model-menu",
                    label.to_owned(),
                    move |menu, _, _| {
                        models.iter().fold(menu, |menu, model| {
                            let id = model.id;
                            let entity = entity.clone();
                            menu.item(
                                PopupMenuItem::new(model.label)
                                    .checked(current.as_deref() == Some(id))
                                    .on_click(move |_, window, cx| {
                                        let _ = entity.update(cx, |this, cx| {
                                            this.select_local_model(id, window, cx)
                                        });
                                    }),
                            )
                        })
                    },
                ),
            )
            .into_any_element()
        } else {
            models
                .iter()
                .fold(
                    self.setting_choices("default-local-model", course2md::tr!("默认识别模型"))
                        .options(models.iter().map(|model| (model.id, model.label))),
                    |choices, _| choices,
                )
                .full_width()
                .selected(canonical.clone().unwrap_or_default())
                .on_change(cx.listener(move |this, id: &SharedString, window, cx| {
                    this.select_local_model(id, window, cx)
                }))
                .into_any_element()
        };
        let (settled, conclusion) = self.default_model_conclusion();
        // Keep cached readiness and recovery actions on the model control ruler.
        let model_footer = h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .items_center()
            .flex_wrap()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .when(!settled, |status| {
                        status.child(
                            icons::warning()
                                .size(rems(16. / 14.))
                                .text_color(color(WARNING)),
                        )
                    })
                    .child(
                        accessible_text("model-readiness-conclusion", conclusion)
                            .text_size(TEXT_SETTINGS_AUX)
                            .text_color(color(if settled { GRAY } else { WARNING })),
                    ),
            )
            .child(
                quiet("toggle-model-details")
                    .settings_text()
                    .icon(if self.settings_ui.model_details_open {
                        icons::chevron_up()
                    } else {
                        icons::download()
                    })
                    .label(if self.settings_ui.model_details_open {
                        course2md::tr!("收起模型管理")
                    } else {
                        course2md::tr!("管理模型与下载")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_ui.model_details_open = !this.settings_ui.model_details_open;
                        cx.notify();
                    })),
            )
            .child(
                quiet("toggle-asr-advanced")
                    .settings_text()
                    .mr(-crate::theme::QUIET_INSET)
                    .icon(icons::tune())
                    .label(if self.settings_ui.asr_details_open {
                        course2md::tr!("收起高级设置")
                    } else {
                        course2md::tr!("高级识别设置")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_ui.asr_details_open = !this.settings_ui.asr_details_open;
                        cx.notify();
                    })),
            );
        let mut view = v_flex().w_full().min_w_0().child(settings_stacked_row(
            "local-model-heading",
            course2md::tr!("识别模型"),
            "",
            v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(picker)
                .child(model_footer),
        ));
        if !model_supported(provider, selected) {
            view = view.child(
                accessible_text("unavailable-fixed-model", unsupported_model())
                    .text_size(TEXT_SETTINGS_AUX)
                    .text_color(color(DANGER)),
            );
        }
        if provider == AsrProvider::Npu {
            view = view
                .child(self.setting_field(
                    EditField::LocalModel,
                    course2md::tr!("模型名称或 Hugging Face 仓库（如 org/model）"),
                ))
                .child(
                    control("apply-custom-npu-model")
                        .settings_text()
                        .icon(icons::check_circle())
                        .label(course2md::tr!("应用模型选择"))
                        .self_start()
                        .on_click(cx.listener(|this, _, _, cx| {
                            let model = this
                                .setting_value(EditField::LocalModel, cx)
                                .trim()
                                .to_owned();
                            let mut next = this.generation_edit_base();
                            next.options.asr_model = (!model.is_empty()).then_some(model);
                            next.local_model_draft = None;
                            this.commit_generation(next, cx);
                        })),
                );
        }
        view
    }

    fn select_local_model(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut next = self.generation_edit_base();
        next.options.asr_model = Some(id.to_owned());
        next.local_model_draft = None;
        if self.commit_generation(next, cx) {
            self.settings_ui.inputs[&EditField::LocalModel]
                .update(cx, |input, cx| input.set_value(id.to_owned(), window, cx));
        }
    }
}
