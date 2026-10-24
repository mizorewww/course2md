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
    fn translation_menu(
        &self,
        current: Option<&'static str>,
        pick: impl Fn(&mut Self, Option<&'static str>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let entity = cx.weak_entity();
        let pick = std::rc::Rc::new(pick);
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
        }
    }

    /// The bilingual target select of the per-task options.
    pub(crate) fn translation_select(
        &self,
        id: &'static str,
        current: Option<&'static str>,
        pick: impl Fn(&mut Self, Option<&'static str>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> SelectField {
        SelectField::new(
            id,
            current
                .map(course2md::language::native_name)
                .unwrap_or(course2md::tr!("不翻译")),
            self.translation_menu(current, pick, cx),
        )
        .label(course2md::tr!("双语翻译"))
    }

    /// Bilingual notes: the language each paragraph is also translated into.
    fn translation_setting(
        &self,
        value: &crate::preferences::GenerationPreferences,
        outer: [bool; 4],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = value
            .ai
            .translate
            .as_deref()
            .and_then(course2md::language::target);
        let shown = current
            .map(course2md::language::native_name)
            .unwrap_or(course2md::tr!("不翻译"));
        let menu = self.translation_menu(
            current,
            |this, code, cx| {
                let mut next = this.generation_edit_base();
                next.ai.translate = code.map(str::to_owned);
                this.commit_generation(next, cx);
            },
            cx,
        );
        let button = tile_button("default-ai-translate", outer, cx)
            .min_h(rems(76. / 14.))
            .accessibility_label(course2md::tr!("{}：{}", course2md::tr!("双语翻译"), shown))
            .child(
                tile_text(
                    "default-ai-translate-label",
                    course2md::tr!("双语翻译"),
                    Some(shown.into()),
                )
                .when(current.is_some(), |text| {
                    text.child(tile_support(
                        "default-ai-translate-row-hint",
                        course2md::tr!("每段文字下方附上译文；已是该语言的视频不翻译"),
                    ))
                }),
            )
            .child(tile_chevron())
            .dropdown_menu_with_anchor(Anchor::TopRight, menu);
        self.reveal_setting("default-ai-translate-reveal", button)
            .into_any_element()
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
        let subtitle_value = if custom_language {
            course2md::tr!("自定义优先级")
        } else {
            subtitle_language_label(language)
        };
        let subtitle_menu = {
            let current = language.to_owned();
            let entity = cx.weak_entity();
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                SUBTITLE_LANGUAGES
                    .into_iter()
                    .map(|(id, label)| (id, course2md::tr!(label)))
                    .fold(menu.min_w(px(240.)), |menu, (id, label)| {
                        let entity = entity.clone();
                        menu.item(PopupMenuItem::new(label).checked(id == current).on_click(
                            move |_, window, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    let mut next = this.generation_edit_base();
                                    next.preferred_subtitle_languages = if id.is_empty() {
                                        Vec::new()
                                    } else {
                                        vec![id.to_owned()]
                                    };
                                    next.subtitle_languages_draft = None;
                                    if this.commit_generation(next, cx) {
                                        this.settings_ui.language_details_open = false;
                                        this.settings_ui.inputs[&EditField::Languages]
                                            .update(cx, |input, cx| {
                                                input.set_value(id, window, cx)
                                            });
                                    }
                                });
                            },
                        ))
                    })
            }
        };
        let subtitle_language = self.reveal_setting(
            "default-subtitle-language-reveal",
            tile_button("default-subtitle-language", [false; 4], cx)
                .min_h(rems(76. / 14.))
                .accessibility_label(course2md::tr!(
                    "{}：{}",
                    course2md::tr!("字幕语言"),
                    subtitle_value
                ))
                .child(tile_text(
                    "subtitle-language-label",
                    course2md::tr!("字幕语言"),
                    Some(subtitle_value.into()),
                ))
                .child(tile_chevron())
                .dropdown_menu_with_anchor(Anchor::TopRight, subtitle_menu),
        );
        let languages_open =
            self.settings_ui.language_details_open || value.subtitle_languages_draft.is_some();
        let priority = tile_disclosure(
            "toggle-language-details",
            [false, false, !languages_open, !languages_open],
            languages_open,
            cx,
        )
        .accessibility_label(course2md::tr!("自定义语言优先级"))
        .child(tile_text(
            "subtitle-custom-label",
            course2md::tr!("自定义语言优先级"),
            custom_language.then(|| value.preferred_subtitle_languages.join(" → ").into()),
        ))
        .child(disclosure_chevron(languages_open))
        .on_click(cx.listener(|this, _, _, cx| {
            this.settings_ui.language_details_open = !this.settings_ui.language_details_open;
            cx.notify();
        }));
        let mut languages = group("language-settings", course2md::tr!("字幕与识别"))
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
            .tile(move |_| subtitle_language.into_any_element())
            .tile(move |_| priority.into_any_element());
        if languages_open {
            languages = languages.detail(crate::motion::disclosure(
                "subtitle-language-details",
                true,
                h_flex()
                    .w_full()
                    .min_w_0()
                    .px(SETTING_ITEM_INSET)
                    .pb(rems(16. / 14.))
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
                        outline_pill("apply-languages", cx)
                            .settings_text()
                            .icon(icons::check_circle())
                            .label(course2md::tr!("应用"))
                            .on_click(
                                cx.listener(|this, _, _, cx| this.apply_language_preferences(cx)),
                            ),
                    ),
            ));
        }
        // 识别方式: where speech is recognized; the local engine shows as a chip.
        let method_value = if online {
            course2md::tr!("在线语音服务")
        } else {
            course2md::tr!("本机识别")
        };
        let method_menu = {
            let entity = cx.weak_entity();
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                [
                    (false, course2md::tr!("本机识别")),
                    (true, course2md::tr!("在线语音服务")),
                ]
                .into_iter()
                .fold(menu.min_w(px(240.)), |menu, (use_online, label)| {
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
            }
        };
        let method = self.reveal_setting(
            "default-asr-device-reveal",
            tile_button("default-asr-device", [true, true, false, false], cx)
                .min_h(rems(76. / 14.))
                .accessibility_label(course2md::tr!(
                    "{}：{}",
                    course2md::tr!("识别方式"),
                    method_value
                ))
                .child(
                    tile_text(
                        "asr-method-label",
                        course2md::tr!("识别方式"),
                        Some(method_value.into()),
                    )
                    .when(online, |text| {
                        text.child(tile_support(
                            "asr-method-label-hint",
                            course2md::tr!("音频会发送到所选服务"),
                        ))
                    }),
                )
                .when(!online, |row| {
                    row.child(outlined_badge(
                        "asr-method-engine",
                        crate::provider_label(provider),
                    ))
                })
                .child(tile_chevron())
                .dropdown_menu_with_anchor(Anchor::TopRight, method_menu),
        );
        let mut recognition = group("asr-default-settings", course2md::tr!("语音识别"))
            .tile(move |_| method.into_any_element());
        if online {
            recognition = recognition.child(self.service_picker(ServicePurpose::Speech, false, cx));
        } else {
            recognition = self.local_model_tiles(recognition, window, cx);
        }
        let ai_open = self.settings_ui.show_prompt;
        let mut ai = group("ai-default-settings", course2md::tr!("AI 处理"))
            .child(
                setting_surface().child(settings_switch_row(
                    course2md::tr!("校对文字和标点"),
                    "",
                    Switch::new("default-ai-proofread")
                        .checked(value.ai.enabled)
                        .on_click(cx.listener(|this, enabled, _, cx| {
                            let mut next = this.generation_edit_base();
                            next.ai.enabled = *enabled;
                            this.commit_generation(next, cx);
                        })),
                )),
            )
            .child(
                setting_surface().child(settings_switch_row(
                    course2md::tr!("在开头生成摘要"),
                    "",
                    Switch::new("default-ai-summary")
                        .checked(value.ai.summarize)
                        .on_click(cx.listener(|this, enabled, _, cx| {
                            let mut next = this.generation_edit_base();
                            next.ai.summarize = *enabled;
                            this.commit_generation(next, cx);
                        })),
                )),
            )
            .child(
                setting_surface().child(settings_switch_row(
                    course2md::tr!("校对时附带对应截图"),
                    "",
                    Switch::new("default-ai-vision")
                        .checked(value.ai.vision)
                        .disabled(!value.ai.enabled)
                        .on_click(cx.listener(|this, enabled, _, cx| {
                            let mut next = this.generation_edit_base();
                            next.ai.vision = *enabled;
                            this.commit_generation(next, cx);
                        })),
                )),
            );
        let translation = self.translation_setting(value, [false; 4], cx);
        ai =
            ai.tile(move |_| translation)
                .child(self.service_picker(ServicePurpose::Ai, false, cx));
        // The rule only applies to proofreading, so it appears with it.
        if value.ai.enabled {
            let rules = tile_disclosure(
                "show-proofread-rules",
                [false, false, !ai_open, !ai_open],
                ai_open,
                cx,
            )
            .min_h(rems(76. / 14.))
            .accessibility_label(course2md::tr!("自定义校对规则"))
            .child(tile_text(
                "proofread-rules-label",
                course2md::tr!("自定义校对规则"),
                Some(
                    if value.ai.prompt.is_some() {
                        course2md::tr!("已自定义")
                    } else {
                        course2md::tr!("使用标准规则")
                    }
                    .into(),
                ),
            ))
            .child(disclosure_chevron(ai_open))
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
            }));
            ai = ai.tile(move |_| rules.into_any_element());
            if ai_open {
                ai = ai.detail(crate::motion::disclosure(
                    "proofread-rules-content",
                    true,
                    v_flex()
                        .px(SETTING_ITEM_INSET)
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
                                    outline_pill("apply-proofread-rules", cx)
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
                                            next.ai.prompt = (!prompt.is_empty()
                                                && prompt != course2md::llm::DEFAULT_PROMPT.trim())
                                            .then_some(prompt);
                                            next.prompt_draft = None;
                                            this.commit_generation(next, cx);
                                        })),
                                )
                                .child(
                                    text_button("restore-proofread-rules", cx)
                                        .settings_text()
                                        .icon(icons::refresh())
                                        .label(course2md::tr!("恢复标准规则"))
                                        .disabled(value.ai.prompt.is_none())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            let mut next = this.generation_edit_base();
                                            next.ai.prompt = None;
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
        }
        let selected = value.options.formats.clone().unwrap_or_default();
        let location = self
            .workspace
            .as_ref()
            .and_then(|workspace| {
                workspace
                    .state
                    .libraries
                    .iter()
                    .find(|library| library.id == workspace.state.default_library)
            })
            .map(|library| library.root.display().to_string());
        let manage_storage = tile_button("settings-manage-storage", [true, true, false, false], cx)
            .min_h(rems(76. / 14.))
            .accessibility_label(course2md::tr!("保存位置"))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(2. / 14.))
                    .child(tile_label(
                        "settings-save-location",
                        course2md::tr!("保存位置"),
                    ))
                    .child(
                        tile_support(
                            "settings-current-location",
                            location.unwrap_or_else(|| course2md::tr!("尚未设置").into()),
                        )
                        .text_size(rems(13. / 14.))
                        .font_family(gpui_component::Theme::global(cx).mono_font_family.clone()),
                    ),
            )
            .child(tile_chevron())
            .on_click(cx.listener(|this, _, window, cx| {
                this.select_settings_tab(SettingsTab::Storage, window, cx);
            }));
        v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(px(28.))
            .child(languages)
            .child(recognition)
            .child(ai)
            .child(
                group("export-default-settings", course2md::tr!("保存与导出"))
                    .tile(move |_| manage_storage.into_any_element())
                    .child(
                        setting_surface().child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .gap(rems(1.))
                                .child(tile_label(
                                    "export-format-label",
                                    course2md::tr!("同时导出"),
                                ))
                                .child(
                                    h_flex().w_full().gap_2().flex_wrap().children(
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
                                        .map(
                                            |(index, (format, label))| {
                                                filter_chip(
                                                    ("default-export", index),
                                                    selected.contains(&format),
                                                    cx,
                                                )
                                                .settings_text()
                                                .label(label)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    let mut next = this.generation_edit_base();
                                                    let formats = next
                                                        .options
                                                        .formats
                                                        .get_or_insert_with(Vec::new);
                                                    if formats.contains(&format) {
                                                        formats.retain(|value| *value != format);
                                                    } else {
                                                        formats.push(format);
                                                    }
                                                    this.commit_generation(next, cx);
                                                }))
                                            },
                                        ),
                                    ),
                                ),
                        ),
                    )
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

    /// 识别模型, 管理模型与下载 and 高级识别设置 as tiles of the 语音识别 group.
    fn local_model_tiles(
        &self,
        mut recognition: SettingsGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> SettingsGroup {
        let preferences = self.preferences.generation();
        let selected_provider = preferences.options.provider;
        let provider = selected_provider.unwrap_or_else(|| self.recommended_local_provider());
        let selected = preferences
            .options
            .asr_model
            .as_deref()
            .unwrap_or(course2md::config::DEFAULT_ASR_MODEL);
        let canonical = course2md::config::canonical_model(provider, selected).ok();
        let models = local_models(provider);
        let model_name: SharedString = if canonical.is_some() {
            model_label(canonical.as_deref().unwrap_or(selected)).into()
        } else if let [only] = models {
            only.label.into()
        } else {
            course2md::tr!("自定义模型").into()
        };
        let (settled, conclusion) = self.default_model_conclusion();
        let state = self.default_model_state();
        // A finished check on a ready model is said by the badge alone.
        let ready = matches!(
            state,
            Some(course2md::models::status::CacheState::Loaded)
                | Some(course2md::models::status::CacheState::Cached)
        );
        let status_badge = state.map(|state| {
            let (kind, label) = super::model_diagnostics::cache_state_label(state);
            badge(kind).child(label)
        });
        let model_text = tile_text(
            "local-model-heading",
            course2md::tr!("识别模型"),
            Some(model_name.clone()),
        )
        .when(!(settled && ready), |text| {
            text.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rems(6. / 14.))
                    .items_start()
                    .when(!settled, |line| {
                        line.child(
                            div().h(rems(20. / 14.)).flex().items_center().child(
                                icons::warning()
                                    .size(rems(16. / 14.))
                                    .text_color(color(WARNING)),
                            ),
                        )
                    })
                    .child(
                        tile_support("model-readiness-conclusion", conclusion)
                            .flex_1()
                            .text_color(color(if settled { GRAY } else { WARNING })),
                    ),
            )
        });
        recognition = if models.len() > 1 {
            let current = canonical.clone();
            let entity = cx.entity().downgrade();
            let menu = move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                models.iter().fold(menu.min_w(px(240.)), |menu, model| {
                    let id = model.id;
                    let entity = entity.clone();
                    menu.item(
                        PopupMenuItem::new(model.label)
                            .checked(current.as_deref() == Some(id))
                            .on_click(move |_, window, cx| {
                                let _ = entity
                                    .update(cx, |this, cx| this.select_local_model(id, window, cx));
                            }),
                    )
                })
            };
            let model_tile = self.reveal_setting(
                "default-local-model-menu-reveal",
                tile_button("default-local-model-menu", [false; 4], cx)
                    .min_h(rems(76. / 14.))
                    .accessibility_label(course2md::tr!(
                        "{}：{}",
                        course2md::tr!("识别模型"),
                        model_name
                    ))
                    .child(model_text)
                    .children(status_badge)
                    .child(tile_chevron())
                    .dropdown_menu_with_anchor(Anchor::TopRight, menu),
            );
            recognition.tile(move |_| model_tile.into_any_element())
        } else {
            recognition.child(
                setting_surface().child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(rems(16. / 14.))
                        .child(model_text)
                        .children(status_badge),
                ),
            )
        };
        if !model_supported(provider, selected) {
            recognition = recognition.detail(
                tile_support("unavailable-fixed-model", unsupported_model())
                    .px(SETTING_ITEM_INSET)
                    .pb(rems(16. / 14.))
                    .text_color(color(DANGER)),
            );
        }
        if provider == AsrProvider::Npu {
            recognition = recognition.child(
                setting_surface()
                    .child(self.setting_field(
                        EditField::LocalModel,
                        course2md::tr!("模型名称或 Hugging Face 仓库（如 org/model）"),
                    ))
                    .child(
                        outline_pill("apply-custom-npu-model", cx)
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
                    ),
            );
        }
        let manage_open = self.settings_ui.model_details_open;
        let manage = tile_disclosure("toggle-model-details", [false; 4], manage_open, cx)
            .accessibility_label(course2md::tr!("管理模型与下载"))
            .child(tile_text(
                "model-management-label",
                course2md::tr!("管理模型与下载"),
                None,
            ))
            .child(disclosure_chevron(manage_open))
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings_ui.model_details_open = !this.settings_ui.model_details_open;
                cx.notify();
            }));
        recognition = recognition.tile(move |_| manage.into_any_element());
        if manage_open {
            recognition = recognition.detail(crate::motion::disclosure(
                "default-model-management",
                true,
                setting_surface()
                    .pt_0()
                    .child(self.model_management_panel(window, cx)),
            ));
        }
        let advanced_open = self.settings_ui.asr_details_open;
        let advanced = tile_disclosure(
            "toggle-asr-advanced",
            [false, false, !advanced_open, !advanced_open],
            advanced_open,
            cx,
        )
        .accessibility_label(course2md::tr!("高级识别设置"))
        .child(tile_text(
            "asr-advanced-label",
            course2md::tr!("高级识别设置"),
            None,
        ))
        .child(disclosure_chevron(advanced_open))
        .on_click(cx.listener(|this, _, _, cx| {
            this.settings_ui.asr_details_open = !this.settings_ui.asr_details_open;
            cx.notify();
        }));
        recognition = recognition.tile(move |_| advanced.into_any_element());
        if advanced_open {
            let engines: Vec<Option<AsrProvider>> = std::iter::once(None)
                .chain(
                    LOCAL_ENGINES
                        .into_iter()
                        .filter(|engine| engine_offered(*engine, selected_provider))
                        .map(Some),
                )
                .collect();
            recognition =
                recognition.detail(crate::motion::disclosure(
                    "asr-advanced-settings",
                    true,
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
                                .options(engines.iter().map(|engine| {
                                    (engine_id(*engine), crate::provider_label(*engine))
                                })),
                                |choices, engine| {
                                    choices.icon(engine_id(*engine), engine_icon(*engine))
                                },
                            )
                            .full_width()
                            .selected(engine_id(selected_provider))
                            .on_change(cx.listener(|this, id: &SharedString, _, cx| {
                                let mut next = this.generation_edit_base();
                                next.select_provider(engine_from_id(id));
                                this.commit_generation(next, cx);
                            })),
                    )
                    .pt_0(),
                ));
        }
        recognition
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
