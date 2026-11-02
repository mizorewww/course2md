//! First use's decisions: which engine and models to offer, and which service tests a
//! configuration still needs.
use crate::conversion::provider_label;
use crate::model_status::{model_supported, unsupported_model};
use crate::preferences::{
    GenerationPreferences, ServiceConfiguration, ServicePurpose, ServiceTestEvidence, TestOutcome,
};
use crate::service_test::TestKind;
use course2md::config::{AsrProvider, local_models, model_label};

#[derive(Clone, Copy)]
pub enum ProviderChoice {
    Local,
    Online,
    Engine(Option<AsrProvider>),
}

pub fn apply_provider_choice(
    provider: &mut Option<AsrProvider>,
    local_provider: &mut Option<AsrProvider>,
    model: &mut String,
    choice: ProviderChoice,
    recommended: AsrProvider,
) -> Option<String> {
    let next = match choice {
        ProviderChoice::Online | ProviderChoice::Engine(Some(AsrProvider::Api)) => {
            if *provider != Some(AsrProvider::Api) {
                *local_provider = *provider;
            }
            Some(AsrProvider::Api)
        }
        ProviderChoice::Local => *local_provider,
        ProviderChoice::Engine(selected) => {
            *local_provider = selected;
            selected
        }
    };
    *provider = next;
    if next == Some(AsrProvider::Api) {
        return None;
    }
    let resolved = next.unwrap_or(recommended);
    if model_supported(resolved, model) {
        return None;
    }
    let previous = std::mem::replace(model, course2md::config::default_model(resolved).into());
    Some(course2md::tr!(
        "{} 不支持「{}」，已改用 {}。",
        provider_label(Some(resolved)),
        previous,
        model_label(model)
    ))
}

/// One setup model choice; `id` is the value saved when it is chosen.
pub struct ModelChoice {
    pub id: String,
    pub label: String,
    pub description: &'static str,
}

/// The engine's models, plus a saved alias or repository as its own visible choice.
pub fn local_model_choices(provider: AsrProvider, current: &str) -> Vec<ModelChoice> {
    let mut choices: Vec<ModelChoice> = local_models(provider)
        .iter()
        .map(|model| ModelChoice {
            id: model.id.into(),
            label: model.label.into(),
            description: match model.id {
                course2md::config::DEFAULT_ASR_MODEL => course2md::tr!("默认 · 更准确，占内存较多"),
                "qwen3-0.6b" => course2md::tr!("更快，占内存较少"),
                _ => "",
            },
        })
        .collect();
    if !choices.iter().any(|choice| choice.id == current) {
        choices.push(ModelChoice {
            id: current.into(),
            label: current.into(),
            description: if model_supported(provider, current) {
                course2md::tr!("当前配置")
            } else {
                unsupported_model()
            },
        });
    }
    choices
}

pub fn engine_preferences(
    current: &GenerationPreferences,
    provider: Option<AsrProvider>,
    model: &str,
) -> GenerationPreferences {
    let mut next = current.clone();
    next.select_provider(provider);
    if provider != Some(AsrProvider::Api) {
        next.options.asr_model = Some(model.to_owned());
        next.local_model_draft = None;
    }
    next
}

pub fn requested_tests(
    purpose: ServicePurpose,
    proofread: bool,
    summary: bool,
    vision: bool,
) -> Vec<TestKind> {
    if purpose == ServicePurpose::Speech {
        return vec![TestKind::Speech];
    }
    let mut tests = enabled_ai_tests(proofread, summary, vision);
    if tests.is_empty() {
        tests.push(TestKind::Proofread);
    }
    tests
}

pub fn enabled_ai_tests(proofread: bool, summary: bool, vision: bool) -> Vec<TestKind> {
    let mut tests = Vec::new();
    if proofread {
        tests.push(TestKind::Proofread);
    }
    if summary {
        tests.push(TestKind::Summary);
    }
    if proofread && vision {
        tests.push(TestKind::Vision);
    }
    tests
}

pub fn newly_enabled_ai_tests(
    current: &GenerationPreferences,
    proofread: bool,
    summary: bool,
) -> Vec<TestKind> {
    let existing = enabled_ai_tests(current.ai.enabled, current.ai.summarize, current.ai.vision);
    enabled_ai_tests(proofread, summary, current.ai.vision)
        .into_iter()
        .filter(|kind| !existing.contains(kind))
        .collect()
}

pub fn evidence_covers(
    config: &ServiceConfiguration,
    required: &[TestKind],
    evidence: &[(TestKind, ServiceTestEvidence)],
) -> bool {
    required.iter().all(|kind| {
        evidence.iter().any(|(tested, evidence)| {
            tested == kind
                && evidence.outcome == TestOutcome::Passed
                && evidence.contract == kind.contract()
                && evidence.fingerprint == config.fingerprint(kind.contract())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderChoice, apply_provider_choice, engine_preferences, evidence_covers,
        local_model_choices, model_supported, newly_enabled_ai_tests, requested_tests,
    };
    use crate::preferences::{
        Authentication, GenerationPreferences, ServiceConfiguration, ServiceProtocol,
        ServicePurpose, ServiceTestEvidence, TestOutcome,
    };
    use crate::service_test::TestKind;
    use course2md::config::AsrProvider;

    #[test]
    fn online_route_round_trip_preserves_explicit_local_engine_and_alias() {
        for (engine, initial_model) in [
            (Some(AsrProvider::Cpu), "QWEN3-ASR-1.7B"),
            (Some(AsrProvider::Gpu), "qwen3-asr-1.7b-q8_0.gguf"),
            (Some(AsrProvider::Coreml), "whisper-large-v3-turbo"),
            (Some(AsrProvider::Npu), "Fixture/Custom-Whisper"),
            (None, "qwen3-0.6b"),
        ] {
            let mut provider = engine;
            let mut local = engine;
            let mut model = initial_model.to_owned();
            // Re-clicking the online route must not replace the remembered engine with Api.
            for _ in 0..2 {
                assert!(
                    apply_provider_choice(
                        &mut provider,
                        &mut local,
                        &mut model,
                        ProviderChoice::Online,
                        AsrProvider::Coreml,
                    )
                    .is_none()
                );
                assert_eq!(provider, Some(AsrProvider::Api));
                assert_eq!(local, engine);
            }
            assert!(
                apply_provider_choice(
                    &mut provider,
                    &mut local,
                    &mut model,
                    ProviderChoice::Local,
                    AsrProvider::Coreml,
                )
                .is_none()
            );
            assert_eq!(provider, engine);
            assert_eq!(model, initial_model);
            let saved = engine_preferences(&GenerationPreferences::default(), provider, &model);
            assert_eq!(saved.options.provider, engine);
            assert_eq!(saved.options.asr_model.as_deref(), Some(initial_model));
        }
    }

    #[test]
    fn switching_to_an_incompatible_engine_selects_a_supported_model_with_notice() {
        let mut provider = Some(AsrProvider::Npu);
        let mut local = provider;
        let mut model = "whisper-tiny".to_owned();
        let notice = apply_provider_choice(
            &mut provider,
            &mut local,
            &mut model,
            ProviderChoice::Engine(Some(AsrProvider::Coreml)),
            AsrProvider::Coreml,
        )
        .unwrap();
        assert_eq!(provider, Some(AsrProvider::Coreml));
        assert_eq!(local, provider);
        assert_eq!(model, "qwen3-1.7b");
        assert!(notice.contains("whisper-tiny"));
        assert!(model_supported(AsrProvider::Coreml, &model));
        assert_eq!(
            local_model_choices(AsrProvider::Coreml, &model)
                .iter()
                .filter(|choice| choice.id == model)
                .count(),
            1
        );

        model = "qwen3-0.6b".into();
        assert!(
            apply_provider_choice(
                &mut provider,
                &mut local,
                &mut model,
                ProviderChoice::Engine(Some(AsrProvider::Cpu)),
                AsrProvider::Coreml,
            )
            .is_some()
        );
        assert_eq!(model, "qwen3-1.7b");
        assert!(model_supported(AsrProvider::Cpu, &model));
    }

    #[test]
    fn current_alias_repository_and_invalid_value_remain_visible_in_model_choices() {
        for (provider, current, supported) in [
            (AsrProvider::Coreml, "whisper-large-v3-turbo", true),
            (AsrProvider::Npu, "Fixture/Custom-Whisper", true),
            (AsrProvider::Npu, "whisper-large", true),
            (AsrProvider::Coreml, "whisper-tiny", false),
        ] {
            let choices = local_model_choices(provider, current);
            let selected: Vec<_> = choices
                .iter()
                .filter(|choice| choice.id == current)
                .collect();
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].label, current);
            assert_eq!(model_supported(provider, current), supported);
        }
    }

    #[test]
    fn cache_names_normalize_compatible_aliases_without_rewriting_saved_values() {
        let root = std::path::Path::new("/models");
        for (provider, current, cache) in [
            (AsrProvider::Coreml, "whisper-large-v3-turbo", "whisper"),
            (AsrProvider::Coreml, "qwen3-asr-0.6b", "qwen3-0.6b"),
            (AsrProvider::Cpu, "qwen3-asr-1.7b-q8_0.gguf", "qwen3-1.7b"),
            (
                AsrProvider::Npu,
                "Fixture/Custom-Whisper",
                "Fixture/Custom-Whisper",
            ),
        ] {
            assert_eq!(
                crate::model_status::Request::new(provider, Some(current), root).model,
                cache
            );
            let saved =
                engine_preferences(&GenerationPreferences::default(), Some(provider), current);
            assert_eq!(saved.options.asr_model.as_deref(), Some(current));
        }
        // An automatic engine that resolves to the NPU runs its default, not
        // the Qwen3 choice saved for another engine.
        assert_eq!(
            crate::model_status::Request::new(AsrProvider::Npu, Some("qwen3-1.7b"), root).model,
            "whisper"
        );
        // An NPU-only model must not be silently substituted on Apple silicon.
        assert!(!model_supported(AsrProvider::Coreml, "whisper-tiny"));
        assert_eq!(
            crate::model_status::Request::new(AsrProvider::Coreml, Some("whisper-tiny"), root)
                .model,
            "whisper-tiny"
        );
        for provider in [
            AsrProvider::Cpu,
            AsrProvider::Gpu,
            AsrProvider::Coreml,
            AsrProvider::Npu,
        ] {
            let default = course2md::config::default_model(provider);
            for choice in local_model_choices(provider, default) {
                assert!(
                    model_supported(provider, &choice.id),
                    "{provider:?}: {}",
                    choice.id
                );
            }
        }
    }

    #[test]
    fn existing_ai_configuration_checks_only_new_capabilities_and_never_disabled_ones() {
        let mut existing = GenerationPreferences::default();
        existing.ai.enabled = true;
        existing.ai.summarize = false;
        existing.ai.vision = true;

        assert!(newly_enabled_ai_tests(&existing, true, false).is_empty());
        assert!(newly_enabled_ai_tests(&existing, false, false).is_empty());
        assert_eq!(
            newly_enabled_ai_tests(&existing, true, true),
            vec![TestKind::Summary]
        );
        assert_eq!(
            newly_enabled_ai_tests(&existing, false, true),
            vec![TestKind::Summary]
        );

        existing.ai.enabled = false;
        existing.ai.summarize = true;
        assert_eq!(
            newly_enabled_ai_tests(&existing, true, true),
            vec![TestKind::Proofread, TestKind::Vision]
        );
        assert!(newly_enabled_ai_tests(&existing, false, false).is_empty());

        existing.ai.vision = false;
        assert_eq!(
            newly_enabled_ai_tests(&existing, true, true),
            vec![TestKind::Proofread]
        );
    }

    #[test]
    fn engine_choice_changes_only_engine_fields_and_preserves_other_preferences() {
        let mut current = GenerationPreferences::default();
        current.ai.enabled = true;
        current.ai.summarize = true;
        current.ai.concurrency = 5;
        current.ai.prompt = Some("Keep technical terms".into());
        current.options.keep_video = Some(true);
        current.options.model_dir = Some("/existing/cache".into());
        let next = engine_preferences(&current, Some(AsrProvider::Gpu), "qwen3-1.7b");
        let mut expected = current.clone();
        expected.options.provider = Some(AsrProvider::Gpu);
        expected.last_local_provider = Some(AsrProvider::Gpu);
        expected.options.asr_model = Some("qwen3-1.7b".into());
        assert_eq!(next, expected);
        let cloud = engine_preferences(&next, Some(AsrProvider::Api), "ignored");
        assert_eq!(cloud.last_local_provider, Some(AsrProvider::Gpu));
        assert_eq!(cloud.options.asr_model, next.options.asr_model);
        assert_eq!(cloud.options.model_dir, next.options.model_dir);
    }

    #[test]
    fn checks_cover_selected_capabilities_and_never_treat_other_contracts_as_passes() {
        let config = ServiceConfiguration {
            name: "Test".into(),
            protocol: ServiceProtocol::AiChat,
            endpoint: "https://example.test/v1/chat/completions".into(),
            model: "model-a".into(),
            authentication: Authentication::None,
            credential: None,
            credential_source: None,
        };
        let passed = |kind: TestKind| {
            (
                kind,
                ServiceTestEvidence {
                    fingerprint: config.fingerprint(kind.contract()),
                    contract: kind.contract().into(),
                    tested_at: 0,
                    outcome: TestOutcome::Passed,
                    message: "样例通过".into(),
                    details: Vec::new(),
                    http_status: None,
                },
            )
        };
        let required = requested_tests(ServicePurpose::Ai, true, true, false);
        assert!(!evidence_covers(
            &config,
            &required,
            &[passed(TestKind::Proofread)]
        ));
        let evidence = vec![passed(TestKind::Proofread), passed(TestKind::Summary)];
        assert!(evidence_covers(&config, &required, &evidence));
        let mut changed = config.clone();
        changed.model = "model-b".into();
        assert!(!evidence_covers(&changed, &required, &evidence));
        let mut cancelled = evidence.clone();
        cancelled[0].1.outcome = TestOutcome::OutcomeUnknown;
        assert!(!evidence_covers(&config, &required, &cancelled));
        assert_eq!(
            requested_tests(ServicePurpose::Speech, true, true, true),
            vec![TestKind::Speech]
        );
        assert_eq!(
            requested_tests(ServicePurpose::Ai, false, true, true),
            vec![TestKind::Summary]
        );
        assert_eq!(
            requested_tests(ServicePurpose::Ai, true, false, true),
            vec![TestKind::Proofread, TestKind::Vision]
        );
    }
}
