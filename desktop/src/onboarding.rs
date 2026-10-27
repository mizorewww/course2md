//! A resumable first-use guide. Configuration, contract tests and model jobs
//! use the same persistence and execution paths as Settings.
use crate::credentials::Secret;
use crate::preferences::{
    BindingScope, GenerationPreferences, PreferenceGroup, ServiceConfiguration, ServiceDraft,
    ServicePurpose, ServiceTestEvidence, ServiceVersion, TestOutcome,
};
use crate::service_test::{self, TestKind};
use crate::settings_ui::model_diagnostics::{
    LOCAL_ENGINES, Request as ModelRequest, engine_icon, engine_offered, job_busy, model_supported,
    unsupported_model,
};
use crate::settings_ui::service_form::{FormField, FormOwner, ServiceForm};
use crate::settings_ui::{settings_detail_row, settings_value};
use crate::theme::*;
use crate::*;
use course2md::{
    config::{AsrProvider, local_models, model_dir_from, model_label},
    models::status::CacheState,
};
use gpui_component::switch::Switch;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupAiUse {
    Proofread,
    Summary,
}

impl SetupAiUse {
    fn id(self) -> &'static str {
        match self {
            Self::Proofread => "proofread",
            Self::Summary => "summary",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Proofread => course2md::tr!("自动校对文字"),
            Self::Summary => course2md::tr!("生成课程摘要"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Engine,
    Ai,
    Account,
    Model,
}

struct ServiceSetup {
    form: ServiceForm,
    original: Option<ServiceVersion>,
    pending_version: Option<ServiceVersion>,
    evidence: Vec<(TestKind, ServiceTestEvidence)>,
    running: Option<Arc<AtomicBool>>,
    /// A key is being stored in, or checked against, the system vault.
    vault_busy: bool,
    test_serial: u64,
    details_open: bool,
}

impl State {
    /// Called from the model_preparation_finished event: adopt the result when it matches the request.
    pub(crate) fn apply_model_preparation_result(
        &mut self,
        request_key: &str,
        result: (String, bool),
        cancelled: bool,
    ) {
        if let Some(preparation) = &mut self.model_preparation
            && preparation.request.key() == request_key
        {
            preparation.result = Some(result);
            preparation.cancelled = cancelled;
        }
    }
}

struct ModelPreparation {
    request: ModelRequest,
    result: Option<(String, bool)>,
    cancelled: bool,
}

impl ServiceSetup {
    fn new(purpose: ServicePurpose, window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        Self {
            form: ServiceForm::new(
                FormOwner::Setup(purpose),
                &[FormField::Address, FormField::Model, FormField::Key],
                ServiceDraft::new(purpose),
                window,
                cx,
            ),
            original: None,
            pending_version: None,
            evidence: Vec::new(),
            running: None,
            vault_busy: false,
            test_serial: 0,
            details_open: false,
        }
    }
    fn cancel(&self) {
        if let Some(cancel) = &self.running {
            cancel.store(true, Ordering::Release);
        }
    }
    fn unchanged(&self, cx: &App) -> bool {
        self.form.typed_key(cx).is_empty()
            && self.original.as_ref().is_some_and(|version| {
                self.form
                    .current_draft(cx)
                    .configuration()
                    .ok()
                    .is_some_and(|config| {
                        config.fingerprint("configuration")
                            == version.config.fingerprint("configuration")
                    })
            })
    }
}

pub(crate) struct State {
    pub(crate) active: bool,
    step: Step,
    session: u64,
    provider: Option<AsrProvider>,
    local_provider: Option<AsrProvider>,
    model: String,
    ai_proofread: bool,
    ai_summary: bool,
    speech: ServiceSetup,
    ai: ServiceSetup,
    notice: Option<(String, bool)>,
    finish_failed: bool,
    model_details_open: bool,
    model_choices_open: bool,
    model_preparation: Option<ModelPreparation>,
    model_status_request: Option<ModelRequest>,
    model_return_page: Option<Page>,
    scroll: ScrollHandle,
}

impl State {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        Self {
            active: false,
            step: Step::Engine,
            session: 0,
            provider: None,
            local_provider: None,
            model: course2md::config::DEFAULT_ASR_MODEL.into(),
            ai_proofread: true,
            ai_summary: false,
            speech: ServiceSetup::new(ServicePurpose::Speech, window, cx),
            ai: ServiceSetup::new(ServicePurpose::Ai, window, cx),
            notice: None,
            finish_failed: false,
            model_details_open: false,
            model_choices_open: false,
            model_preparation: None,
            model_status_request: None,
            model_return_page: None,
            scroll: ScrollHandle::new(),
        }
    }
    fn service(&self, purpose: ServicePurpose) -> &ServiceSetup {
        match purpose {
            ServicePurpose::Speech => &self.speech,
            ServicePurpose::Ai => &self.ai,
        }
    }
    fn service_mut(&mut self, purpose: ServicePurpose) -> &mut ServiceSetup {
        match purpose {
            ServicePurpose::Speech => &mut self.speech,
            ServicePurpose::Ai => &mut self.ai,
        }
    }
}

/// The forward action of a service step.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceAction {
    Check,
    Recheck,
    Save,
    Keep,
    RetrySave,
}

impl ServiceAction {
    fn label(self) -> &'static str {
        match self {
            Self::Check => course2md::tr!("检查配置"),
            Self::Recheck => course2md::tr!("重新检查"),
            Self::Save => course2md::tr!("保存并继续"),
            Self::Keep => course2md::tr!("保留并继续"),
            Self::RetrySave => course2md::tr!("重试保存"),
        }
    }
    fn sends_sample(self) -> bool {
        matches!(self, Self::Check | Self::Recheck)
    }
}

/// Width of the guide's column for the window and text scale: the 760px
/// content column of settings, plus the 4px focus allowance on each side.
fn setup_column_width(window: &Window) -> f32 {
    let scale = f32::from(window.rem_size()) / 14.;
    let gutter = if is_compact(window) { 16. } else { 24. };
    (760. * scale.min(1.5) + 8.)
        .min(f32::from(window.viewport_size().width) - (gutter - 4.) * 2.)
        .max(280.)
}

#[derive(Clone, Copy)]
enum ProviderChoice {
    Local,
    Online,
    Engine(Option<AsrProvider>),
}

fn apply_provider_choice(
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
        crate::provider_label(Some(resolved)),
        previous,
        model_label(model)
    ))
}

/// One setup model choice; `id` is the value saved when it is chosen.
struct ModelChoice {
    id: String,
    label: String,
    description: &'static str,
}

/// The engine's models, plus a saved alias or repository as its own visible choice.
fn local_model_choices(provider: AsrProvider, current: &str) -> Vec<ModelChoice> {
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

fn engine_preferences(
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

fn requested_tests(
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

fn enabled_ai_tests(proofread: bool, summary: bool, vision: bool) -> Vec<TestKind> {
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

fn newly_enabled_ai_tests(
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

fn evidence_covers(
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

impl Desktop {
    pub(crate) fn start_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup_model_job_running() {
            self.open_setup_model_status(window, cx);
            return;
        }
        if self.page == Page::New && !self.save_current_draft(cx) {
            return;
        }
        self.onboarding.speech.cancel();
        self.onboarding.ai.cancel();
        for purpose in [ServicePurpose::Speech, ServicePurpose::Ai] {
            let id = self.onboarding.service(purpose).form.draft.id.clone();
            let _ = self.preferences.discard_service_draft(&id);
        }
        self.onboarding.active = false;
        self.onboarding.session += 1;
        let generation = self.preferences.generation();
        self.onboarding.provider = generation.options.provider;
        self.onboarding.local_provider = if generation.options.provider == Some(AsrProvider::Api) {
            generation.last_local_provider
        } else {
            generation.options.provider
        };
        let engine = self
            .onboarding
            .local_provider
            .unwrap_or_else(|| self.recommended_local_provider());
        self.onboarding.model = generation
            .options
            .asr_model
            .clone()
            .unwrap_or_else(|| course2md::config::default_model(engine).into());
        self.onboarding.ai_proofread = generation.ai.enabled;
        self.onboarding.ai_summary = generation.ai.summarize;
        let refs = self.preferences.default_refs();
        self.hydrate_setup_service(ServicePurpose::Speech, refs.asr.as_deref(), window, cx);
        self.hydrate_setup_service(ServicePurpose::Ai, refs.llm.as_deref(), window, cx);
        if self.onboarding.ai.original.is_none() {
            self.onboarding.ai_proofread = true;
        }
        self.onboarding.model_details_open = false;
        self.onboarding.model_choices_open = false;
        self.onboarding.finish_failed = false;
        self.onboarding.model_status_request = None;
        self.onboarding.model_return_page = None;
        self.onboarding.active = true;
        self.setup_step(Step::Engine, cx);
        // Capture builds can open a later step for review:
        // COURSE2MD_CAPTURE_SETUP_STEP=ai|account|model (cloud for the engine
        // step's online route). Nothing is saved by jumping.
        #[cfg(feature = "capture")]
        if let Ok(step) = std::env::var("COURSE2MD_CAPTURE_SETUP_STEP") {
            self.capture_setup_step(&step, cx);
        }
        self.root_focus.focus(window, cx);
    }

    /// Capture builds: show a later first-use step (`ai`, `account`,
    /// `model`) or the engine step's online route (`cloud`); any other name
    /// shows the first step, and an empty one keeps the current step.
    /// Nothing is saved by jumping.
    #[cfg(feature = "capture")]
    pub(crate) fn capture_setup_step(&mut self, step: &str, cx: &mut Context<Self>) {
        match step {
            "cloud" => self.select_setup_route(false, cx),
            "ai" => self.setup_step(Step::Ai, cx),
            "account" => self.setup_step(Step::Account, cx),
            "model" => self.setup_step(Step::Model, cx),
            "" => {}
            _ => self.setup_step(Step::Engine, cx),
        }
    }

    fn select_setup_route(&mut self, local: bool, cx: &mut Context<Self>) {
        self.apply_setup_provider_choice(
            if local {
                ProviderChoice::Local
            } else {
                ProviderChoice::Online
            },
            cx,
        );
    }

    fn select_setup_provider(&mut self, provider: Option<AsrProvider>, cx: &mut Context<Self>) {
        self.apply_setup_provider_choice(ProviderChoice::Engine(provider), cx);
    }

    fn apply_setup_provider_choice(&mut self, choice: ProviderChoice, cx: &mut Context<Self>) {
        let recommended = self.recommended_local_provider();
        let state = &mut self.onboarding;
        let notice = apply_provider_choice(
            &mut state.provider,
            &mut state.local_provider,
            &mut state.model,
            choice,
            recommended,
        );
        state.notice = notice.map(|message| (message, false));
        if self.onboarding.step == Step::Model {
            let request = self.setup_model_request();
            self.ensure_model_diagnostic(request.provider, Some(&request.model), &request.root, cx);
        }
        cx.notify();
    }

    fn select_setup_model(&mut self, model: &str, cx: &mut Context<Self>) {
        let provider = self
            .onboarding
            .provider
            .unwrap_or_else(|| self.recommended_local_provider());
        if model_supported(provider, model) {
            self.onboarding.model = model.to_owned();
            self.onboarding.notice = None;
        } else {
            self.onboarding.notice = Some((unsupported_model().into(), true));
        }
        if self.onboarding.step == Step::Model {
            let request = self.setup_model_request();
            self.ensure_model_diagnostic(request.provider, Some(&request.model), &request.root, cx);
        }
        cx.notify();
    }

    fn hydrate_setup_service(
        &mut self,
        purpose: ServicePurpose,
        reference: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let original = reference
            .and_then(|id| self.preferences.version(id))
            .cloned();
        let draft = original
            .as_ref()
            .map(ServiceDraft::from_version)
            .unwrap_or_else(|| ServiceDraft::new(purpose));
        let service = self.onboarding.service_mut(purpose);
        service.cancel();
        service.running = None;
        service.form.load(draft, window, cx);
        service.original = original;
        service.pending_version = None;
        service.evidence.clear();
        service.details_open = false;
    }

    fn setup_step(&mut self, step: Step, cx: &mut Context<Self>) {
        if step == Step::Engine && self.setup_model_job_running() {
            return;
        }
        if step != Step::Model {
            self.onboarding.model_status_request = None;
            self.onboarding.model_return_page = None;
        }
        if step == Step::Account {
            self.refresh_account(cx);
        }
        if step == Step::Ai && self.onboarding.ai.form.is_codex() {
            self.codex_refresh_status(crate::codex_ui::CodexSurface::Onboarding, cx);
        }
        self.onboarding.step = step;
        self.onboarding.notice = None;
        self.onboarding.scroll.set_offset(point(px(0.), px(0.)));
        if step == Step::Model {
            let request = self.setup_model_request();
            self.ensure_model_diagnostic(request.provider, Some(&request.model), &request.root, cx);
        }
        cx.notify();
    }

    /// The setup step's service form while the guide is open.
    pub(crate) fn setup_form(&self, purpose: ServicePurpose) -> Option<&ServiceForm> {
        self.onboarding
            .active
            .then(|| &self.onboarding.service(purpose).form)
    }
    pub(crate) fn setup_form_mut(&mut self, purpose: ServicePurpose) -> Option<&mut ServiceForm> {
        self.onboarding
            .active
            .then(|| &mut self.onboarding.service_mut(purpose).form)
    }
    /// An edited form needs a new check; the old notice no longer applies.
    pub(crate) fn setup_service_edited(&mut self, purpose: ServicePurpose) {
        self.onboarding.service_mut(purpose).evidence.clear();
        self.onboarding.notice = None;
    }

    fn setup_tests(&self, purpose: ServicePurpose) -> Vec<TestKind> {
        requested_tests(
            purpose,
            self.onboarding.ai_proofread,
            self.onboarding.ai_summary,
            self.preferences.generation().ai.vision,
        )
    }

    fn setup_required_tests(&self, purpose: ServicePurpose, cx: &App) -> Vec<TestKind> {
        if !self.onboarding.service(purpose).unchanged(cx) {
            return self.setup_tests(purpose);
        }
        match purpose {
            ServicePurpose::Speech => Vec::new(),
            ServicePurpose::Ai => newly_enabled_ai_tests(
                self.preferences.generation(),
                self.onboarding.ai_proofread,
                self.onboarding.ai_summary,
            ),
        }
    }

    fn setup_service_passed(&self, purpose: ServicePurpose, cx: &App) -> bool {
        let service = self.onboarding.service(purpose);
        service.form.typed_key(cx).is_empty()
            && service
                .form
                .current_draft(cx)
                .configuration()
                .ok()
                .is_some_and(|config| {
                    evidence_covers(
                        &config,
                        &self.setup_required_tests(purpose, cx),
                        &service.evidence,
                    )
                })
    }

    /// Records the step's service form. A typed key is first stored in the
    /// vault off the UI thread: this returns None and the test starts again
    /// once the draft carries the stored key.
    fn stage_setup_service(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ServiceConfiguration> {
        let service = self.onboarding.service(purpose);
        if service.vault_busy {
            return None;
        }
        let draft = service.form.current_draft(cx);
        let key = service.form.typed_key(cx);
        if !key.trim().is_empty() {
            self.onboarding.service_mut(purpose).vault_busy = true;
            let secret = Secret::new(key);
            self.run_vault_step(
                cx,
                move |vault| preferences::Store::insert_credential(vault, secret),
                move |this, stored, window, cx| {
                    this.finish_setup_key(purpose, draft, stored, window, cx)
                },
            );
            cx.notify();
            return None;
        }
        let saved = self.preferences.save_service_draft(draft, None);
        self.commit_setup_draft(purpose, saved, window, cx)
    }

    fn commit_setup_draft(
        &mut self,
        purpose: ServicePurpose,
        saved: Result<ServiceDraft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ServiceConfiguration> {
        match saved {
            Ok(draft) => {
                let form = &mut self.onboarding.service_mut(purpose).form;
                form.draft = draft;
                if !form.validate(window, cx) {
                    cx.notify();
                    return None;
                }
                form.draft.configuration().ok()
            }
            Err(error) => {
                self.onboarding.notice = Some((
                    preferences::save_failure_message(PreferenceGroup::Services, &error),
                    true,
                ));
                cx.notify();
                None
            }
        }
    }

    /// The commit step after a typed key reached the vault; the test the
    /// person asked for then starts.
    fn finish_setup_key(
        &mut self,
        purpose: ServicePurpose,
        draft: ServiceDraft,
        stored: Result<crate::credentials::CredentialRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.onboarding.service_mut(purpose).vault_busy = false;
        if self.onboarding.service(purpose).form.draft.id != draft.id {
            if let Ok(reference) = stored {
                self.forget_credentials(vec![reference]);
            }
            return;
        }
        let committed = stored.and_then(|reference| {
            self.preferences
                .commit_service_draft(draft, Some(reference.clone()))
                .inspect_err(|_| self.forget_credentials(vec![reference]))
        });
        if committed.is_ok() {
            self.onboarding
                .service_mut(purpose)
                .form
                .clear_key(window, cx);
        }
        if self
            .commit_setup_draft(purpose, committed, window, cx)
            .is_some()
        {
            self.start_setup_test(purpose, window, cx);
        }
        cx.notify();
    }

    fn start_setup_test(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.service(purpose).running.is_some() {
            return;
        }
        let Some(config) = self.stage_setup_service(purpose, window, cx) else {
            return;
        };
        let required = self.setup_required_tests(purpose, cx);
        let required = if required.is_empty() {
            self.setup_tests(purpose)
        } else {
            required
        };
        self.root_focus.focus(window, cx);
        let cancel = Arc::new(AtomicBool::new(false));
        let session = self.onboarding.session;
        let service = self.onboarding.service_mut(purpose);
        service.test_serial += 1;
        let serial = service.test_serial;
        service.running = Some(cancel.clone());
        service.evidence.clear();
        self.onboarding.notice = None;
        let vault = self.preferences.vault();
        let task = crate::spawn_blocking_io(move || {
            service_test::test_services_blocking(&config, &required, vault.as_ref(), &cancel)
        });
        cx.spawn(async move |this, cx| {
            let Ok(results) = task.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let mut persisted = true;
                for (_, evidence) in &results {
                    persisted &= this.preferences.record_test(evidence.clone()).is_ok();
                }
                if this.onboarding.session != session
                    || this.onboarding.service(purpose).test_serial != serial
                {
                    return;
                }
                let service = this.onboarding.service(purpose);
                let unchanged = service.form.typed_key(cx).is_empty()
                    && service
                        .form
                        .current_draft(cx)
                        .configuration()
                        .ok()
                        .is_some_and(|config| {
                            results.iter().all(|(_, evidence)| {
                                evidence.fingerprint == config.fingerprint(&evidence.contract)
                            })
                        });
                let service = this.onboarding.service_mut(purpose);
                service.running = None;
                if unchanged {
                    service.evidence = results;
                    if !persisted {
                        this.onboarding.notice =
                            Some((course2md::tr!("检查结果尚未保存").into(), true));
                    }
                } else {
                    this.onboarding.notice = Some((
                        course2md::tr!("配置已改变，请检查当前填写的配置。").into(),
                        false,
                    ));
                }
                // Keep newly arrived feedback above the fixed action row.
                if this.onboarding.active
                    && matches!(
                        (this.onboarding.step, purpose),
                        (Step::Ai, ServicePurpose::Ai) | (Step::Engine, ServicePurpose::Speech)
                    )
                {
                    this.onboarding.scroll.scroll_to_bottom();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn continue_setup_service(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let unchanged = self.onboarding.service(purpose).unchanged(cx);
        let pending = self.onboarding.service(purpose).pending_version.clone();
        if pending.is_none() && !self.setup_service_passed(purpose, cx) {
            self.start_setup_test(purpose, window, cx);
            return false;
        }
        let version = if let Some(version) = pending {
            version
        } else if unchanged {
            let Some(version) = self.onboarding.service(purpose).original.clone() else {
                return false;
            };
            if let Err(error) = self.preferences.check_dispatch(&version.id) {
                self.onboarding.notice = Some((
                    course2md::tr!("当前服务暂不可用：{}", format!("{error:#}")),
                    true,
                ));
                cx.notify();
                return false;
            }
            version
        } else {
            if self.stage_setup_service(purpose, window, cx).is_none() {
                return false;
            }
            let id = self.onboarding.service(purpose).form.draft.id.clone();
            // Publication confirms the credential is in the vault, which can
            // block: that check runs off the UI thread and this step continues
            // with the published version once it returns.
            let reference = match self.preferences.publication_credential(&id) {
                Ok(reference) => reference,
                Err(error) => {
                    self.setup_publish_failed(&error, cx);
                    return false;
                }
            };
            if reference.is_some() {
                self.onboarding.service_mut(purpose).vault_busy = true;
                self.run_vault_step(
                    cx,
                    move |vault| preferences::Store::verify_credential(vault, reference),
                    move |this, verified, window, cx| {
                        this.onboarding.service_mut(purpose).vault_busy = false;
                        let published = verified.and_then(|verified| {
                            this.preferences.commit_publication(
                                &id,
                                BindingScope::Defaults,
                                verified,
                            )
                        });
                        match published {
                            Ok(version) => {
                                this.onboarding.service_mut(purpose).pending_version =
                                    Some(version);
                                this.continue_setup_service(purpose, window, cx);
                            }
                            Err(error) => this.setup_publish_failed(&error, cx),
                        }
                        cx.notify();
                    },
                );
                cx.notify();
                return false;
            }
            let published =
                preferences::Store::verify_credential(self.preferences.vault().as_ref(), None)
                    .and_then(|verified| {
                        self.preferences
                            .commit_publication(&id, BindingScope::Defaults, verified)
                    });
            match published {
                Ok(version) => {
                    self.onboarding.service_mut(purpose).pending_version = Some(version.clone());
                    version
                }
                Err(error) => {
                    self.setup_publish_failed(&error, cx);
                    return false;
                }
            }
        };
        let mut next = self.preferences.generation().clone();
        match purpose {
            ServicePurpose::Speech => next.select_provider(Some(AsrProvider::Api)),
            ServicePurpose::Ai => {
                next.ai.enabled = self.onboarding.ai_proofread;
                next.ai.summarize = self.onboarding.ai_summary;
            }
        }
        if !self.commit_generation(next, cx) {
            self.onboarding.notice =
                Some((course2md::tr!("默认选项尚未保存，可重试").into(), true));
            return false;
        }
        self.refresh_dispatch_controls(cx);
        self.hydrate_setup_service(purpose, Some(&version.id), window, cx);
        self.setup_step(
            if purpose == ServicePurpose::Speech {
                Step::Ai
            } else {
                Step::Account
            },
            cx,
        );
        true
    }

    fn setup_publish_failed(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        self.onboarding.notice = Some((
            preferences::save_failure_message(PreferenceGroup::Services, error),
            true,
        ));
        cx.notify();
    }

    /// Saves the first step; true when the guide moved on to the AI step.
    fn save_setup_engine(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.onboarding.provider == Some(AsrProvider::Api) {
            return self.continue_setup_service(ServicePurpose::Speech, window, cx);
        }
        let provider = self
            .onboarding
            .provider
            .unwrap_or_else(|| self.recommended_local_provider());
        if !model_supported(provider, &self.onboarding.model) {
            self.onboarding.notice = Some((unsupported_model().into(), true));
            cx.notify();
            return false;
        }
        let next = engine_preferences(
            self.preferences.generation(),
            self.onboarding.provider,
            &self.onboarding.model,
        );
        if !self.commit_generation(next, cx) {
            self.onboarding.notice = Some((course2md::tr!("选择未保存，请重试").into(), true));
            return false;
        }
        self.setup_step(Step::Ai, cx);
        true
    }

    fn skip_setup_ai(&mut self, cx: &mut Context<Self>) {
        self.onboarding.ai.cancel();
        // Keep this private edit valid when the user returns within the guide.
        // Only finishing the guide discards unpublished service drafts.
        self.setup_step(Step::Account, cx);
    }

    fn setup_model_request(&self) -> ModelRequest {
        self.onboarding
            .model_status_request
            .clone()
            .unwrap_or_else(|| {
                ModelRequest::new(
                    self.onboarding
                        .provider
                        .unwrap_or_else(|| self.recommended_local_provider()),
                    Some(&self.onboarding.model),
                    &model_dir_from(self.preferences.generation().options.model_dir.as_deref()),
                )
            })
    }

    fn setup_model_job_running(&self) -> bool {
        self.onboarding
            .model_preparation
            .as_ref()
            .is_some_and(|preparation| {
                let request = &preparation.request;
                self.setup_model_snapshot(request.provider, Some(&request.model), &request.root)
                    .preparing
            })
    }

    fn open_setup_model_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.onboarding.active && self.page == Page::New && !self.save_current_draft(cx) {
            return;
        }
        let Some(preparation) = &self.onboarding.model_preparation else {
            return;
        };
        if !self.onboarding.active {
            self.onboarding.model_return_page = Some(self.page);
        }
        self.onboarding.model_status_request = Some(preparation.request.clone());
        self.onboarding.active = true;
        self.setup_step(Step::Model, cx);
        self.root_focus.focus(window, cx);
    }

    fn finish_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut application = self.preferences.application().clone();
        application.desktop.setup_completed = true;
        if let Err(error) = self.preferences.save_application(application) {
            self.onboarding.finish_failed = true;
            self.onboarding.notice = Some((
                preferences::save_failure_message(PreferenceGroup::Application, &error),
                true,
            ));
            cx.notify();
            return;
        }
        self.leave_onboarding(window, cx);
    }

    fn leave_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for purpose in [ServicePurpose::Speech, ServicePurpose::Ai] {
            let service = self.onboarding.service_mut(purpose);
            service.cancel();
            service.form.models.invalidate();
            service.form.clear_key(window, cx);
            let id = service.form.draft.id.clone();
            let _ = self.preferences.discard_service_draft(&id);
        }
        self.onboarding.active = false;
        self.onboarding.finish_failed = false;
        self.refresh_preference_defaults(cx);
        let return_page = self
            .onboarding
            .model_return_page
            .take()
            .unwrap_or(Page::New);
        self.navigate(return_page, cx);
        self.root_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn onboarding_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Rendering does not settle state: model_preparation results are adopted in model_preparation_finished.
        // Capture builds only: hold the step at a scroll offset to render its lower part.
        #[cfg(feature = "capture")]
        if let Some(offset) = std::env::var("COURSE2MD_CAPTURE_SCROLL")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
        {
            self.onboarding
                .scroll
                .set_offset(point(px(0.), px(-offset)));
        }
        let step = self.onboarding.step;
        let (number, title, description) = match step {
            Step::Engine => (
                1,
                course2md::tr!("默认识别方式"),
                Some(course2md::tr!("仅用于没有字幕的视频")),
            ),
            Step::Ai => (2, course2md::tr!("连接 AI 服务"), None),
            Step::Account => (3, course2md::tr!("连接 Bilibili"), None),
            Step::Model if self.onboarding.provider == Some(AsrProvider::Api) => {
                (4, course2md::tr!("准备就绪"), None)
            }
            Step::Model => (4, course2md::tr!("准备识别模型"), None),
        };
        let content = match step {
            Step::Engine => self.setup_engine_content(window, cx),
            Step::Ai => self.setup_service_content(ServicePurpose::Ai, window, cx),
            Step::Account => setup_tile(true, true).child(self.account_onboarding_page(cx)),
            Step::Model => self.setup_model_content(window, cx),
        };
        let scale = f32::from(window.rem_size()) / 14.;
        let narrow = is_compact(window);
        let gutter = if narrow { 16. } else { 24. };
        let width = setup_column_width(window);
        let available_height = (f32::from(window.viewport_size().height)
            - f32::from(title_bar_height(window)))
        .max(160.);
        let compact = available_height < 560. * scale;
        let steps = [
            course2md::tr!("识别方式"),
            course2md::tr!("AI 服务"),
            "Bilibili",
            course2md::tr!("模型准备"),
        ];
        // As on the settings canvas: no frame around the guide, a 30px
        // regular headline, and the step's choices as tiles on the ground.
        let heading = v_flex()
            .w_full()
            .min_w_0()
            .gap(px(if compact { 12. } else { 20. }))
            .when(!compact, |heading| {
                heading.child(setup_steps(&steps, number))
            })
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .child(
                                page_heading("setup-title", title)
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal(),
                            )
                            .when(compact, |row| {
                                row.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(TEXT_BODY)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color(GRAY))
                                        .child(format!("{number} / 4")),
                                )
                            }),
                    )
                    .when_some(description, |view, description| {
                        view.child(
                            accessible_text("setup-description", description)
                                .text_size(TEXT_SETTINGS_AUX)
                                .line_height(rems(20. / 14.))
                                .text_color(color(GRAY)),
                        )
                    }),
            );
        let scrolling_content = v_flex()
            .w_full()
            .min_w_0()
            .gap(px(if compact { 16. } else { 24. }))
            .child(content)
            .when_some(self.onboarding.notice.clone(), |view, (message, error)| {
                view.child(
                    settings_value("setup-notice", message)
                        .role(Role::Status)
                        .px(SETTING_ITEM_INSET)
                        .text_size(TEXT_SETTINGS_AUX)
                        .text_color(color(if error { DANGER } else { GRAY })),
                )
            });
        // Natural height keeps the actions next to the step; the body scrolls
        // only when the window is too short, and then a hairline marks where
        // it passes under the actions.
        let scrollable = self.onboarding.scroll.max_offset().y > px(0.);
        let footer = self.setup_footer(cx);
        let column = v_flex()
            .relative()
            .w(px(width))
            .max_w_full()
            .min_w_0()
            .max_h(px(available_height))
            .child(
                heading
                    .flex_shrink_0()
                    .px(px(4.))
                    .pt(px(if compact { 16. } else { 32. }))
                    .pb(px(if compact { 16. } else { 24. })),
            )
            .child(
                v_flex()
                    .relative()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("setup-scroll")
                            .flex_1()
                            .w_full()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.onboarding.scroll)
                            // Room for focus rings painted outside a tile.
                            .px(px(4.))
                            .pb(px(4.))
                            .child(motion::state_enter(
                                ("setup-step", number),
                                scrolling_content,
                            )),
                    )
                    .child(crate::backend::vertical_scrollbar(&self.onboarding.scroll)),
            )
            .child(
                footer
                    .px(px(4.))
                    .pt(px(if compact { 12. } else { 20. }))
                    .pb(px(if compact { 16. } else { 24. }))
                    .when(scrollable, |footer| {
                        footer.border_t_1().border_color(color(HAIRLINE))
                    }),
            );
        v_flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .items_center()
            .justify_start()
            .bg(color(CANVAS))
            .px(px(gutter - 4.))
            .child(column)
            .into_any_element()
    }

    fn setup_engine_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let selected = self.onboarding.provider;
        let recommended = self.recommended_local_provider();
        let local = selected != Some(AsrProvider::Api);
        // Local recognition is recommended only where an engine can run;
        // when none can, its obstacle and repair are shown below the engines.
        let local_blocked = self.environment.is_some() && self.engine_issue(recommended).is_some();
        // Tinted icons follow the interface text size, like their labels.
        let icon_size = rems(40. / 14.).to_pixels(window.rem_size());
        // Where audio is processed is the outcome-level choice: two tiles.
        let route = |index: usize,
                     id: &'static str,
                     title: &'static str,
                     description: &'static str,
                     icon: Icon,
                     tint: Tint,
                     chosen: bool| {
            ChoiceTile::new(id, index, 2, title, chosen)
                .tinted_icon(icon, tint, icon_size)
                .description(description)
        };
        let routes = v_flex()
            .w_full()
            .min_w_0()
            .gap(TILE_GAP)
            .child(
                self.setup_reveal(
                    "setup-local-route-reveal",
                    route(
                        0,
                        "setup-local-route",
                        course2md::tr!("本机识别"),
                        if local_blocked {
                            course2md::tr!("音频不离开本机")
                        } else {
                            course2md::tr!("推荐 · 音频不离开本机")
                        },
                        icons::computer(),
                        Tint::Blue,
                        local,
                    )
                    .render(window, cx)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_setup_route(true, cx);
                    })),
                ),
            )
            .child(
                self.setup_reveal(
                    "setup-cloud-route-reveal",
                    route(
                        1,
                        "setup-cloud-route",
                        course2md::tr!("在线语音服务"),
                        course2md::tr!("音频会发送到所选服务"),
                        icons::cloud(),
                        Tint::Teal,
                        !local,
                    )
                    .render(window, cx)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_setup_route(false, cx);
                    })),
                ),
            );
        let mut body = v_flex().w_full().min_w_0().gap(px(24.)).child(routes);
        if !local {
            return body.child(self.setup_service_content(ServicePurpose::Speech, window, cx));
        }
        // Automatic first, then the engines it chooses between: one connected
        // group, each engine's real availability as its status chip.
        let engines: Vec<_> = LOCAL_ENGINES
            .into_iter()
            .filter(|engine| engine_offered(*engine, selected))
            .collect();
        let count = engines.len() + 1;
        // Automatic falls back to CPU even when CPU cannot run; then no engine
        // is usable, and that one obstacle is said once below the group.
        let obstacle = self
            .environment
            .is_some()
            .then(|| self.engine_issue(recommended))
            .flatten();
        let automatic = if self.environment.is_none() {
            course2md::tr!("正在检测").to_owned()
        } else if obstacle.is_some() {
            course2md::tr!("暂无可用的本机引擎").to_owned()
        } else {
            course2md::tr!("目前使用 {}", crate::provider_label(Some(recommended)))
        };
        // Engines are local resources: tinted circles like the routes above,
        // one family so they read as one group.
        let engine_tile = |tile: ChoiceTile, provider: Option<AsrProvider>, enabled: bool| {
            if enabled {
                tile.tinted_icon(engine_icon(provider), Tint::Blue, icon_size)
            } else {
                tile.icon(tinted_icon(engine_icon(provider), Tint::Blue, icon_size).opacity(0.38))
            }
        };
        let mut engine_group = v_flex().w_full().min_w_0().gap(TILE_GAP).child(
            self.setup_reveal(
                "setup-engine-auto-reveal",
                engine_tile(
                    ChoiceTile::new(
                        "setup-engine-auto",
                        0,
                        count,
                        course2md::tr!("自动选择引擎"),
                        selected.is_none(),
                    ),
                    None,
                    true,
                )
                .description(automatic)
                .render(window, cx)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_setup_provider(None, cx);
                })),
            ),
        );
        // An engine's own obstacle that something here can repair (a missing
        // component) is stated below the group with that repair, after the
        // shared one; hardware this Mac lacks is only its chip.
        let mut repairable = Vec::new();
        for (index, engine) in engines.into_iter().enumerate() {
            let provider = Some(engine);
            if let Some(issue) = self.engine_issue(engine)
                && Some(issue) != obstacle
                && matches!(
                    issue,
                    crate::settings_ui::model_diagnostics::DeviceIssue::AppleRuntime
                        | crate::settings_ui::model_diagnostics::DeviceIssue::LlamaMissing
                        | crate::settings_ui::model_diagnostics::DeviceIssue::VcRuntime
                        | crate::settings_ui::model_diagnostics::DeviceIssue::NpuRuntime
                )
                && !repairable.contains(&issue)
            {
                repairable.push(issue);
            }
            // A ready engine needs no chip; only checking or unavailable is status.
            let (status, unavailable) = match &self.environment {
                None => (
                    Some(badge(BadgeKind::Progress).child(course2md::tr!("正在检测"))),
                    false,
                ),
                Some(_) => match self.engine_issue(engine) {
                    // The shared obstacle is stated once, not on every tile.
                    Some(issue) if Some(issue) == obstacle => (None, true),
                    Some(issue) => (Some(badge(BadgeKind::Warning).child(issue.short())), true),
                    None => (None, false),
                },
            };
            engine_group = engine_group.child(
                self.setup_reveal(
                    ("setup-engine-choice-reveal", index),
                    engine_tile(
                        ChoiceTile::new(
                            ("setup-engine-choice", index),
                            index + 1,
                            count,
                            crate::provider_label(provider),
                            selected == provider,
                        ),
                        provider,
                        !unavailable,
                    )
                    .status(status)
                    .enabled(!unavailable)
                    .render(window, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_setup_provider(provider, cx);
                    })),
                ),
            );
        }
        body = body.child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(px(8.))
                .child(
                    section_title("setup-fixed-engine-label", course2md::tr!("本机引擎"))
                        .pl(SETTING_ITEM_INSET),
                )
                .child(engine_group)
                .when_some(obstacle, |group, issue| {
                    group.child(
                        div()
                            .pt(px(4.))
                            .px(SETTING_ITEM_INSET)
                            .child(self.engine_obstacle("setup-engine-obstacle", issue, cx)),
                    )
                })
                .children(repairable.into_iter().map(|issue| {
                    div()
                        .pt(px(4.))
                        .px(SETTING_ITEM_INSET)
                        .child(self.engine_obstacle(
                            &format!("setup-engine-obstacle-{issue:?}"),
                            issue,
                            cx,
                        ))
                })),
        );
        let provider = selected.unwrap_or(recommended);
        let models = local_model_choices(provider, &self.onboarding.model);
        let current_model = models
            .iter()
            .find(|choice| choice.id == self.onboarding.model)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| self.onboarding.model.clone());
        let request = self.setup_model_request();
        let snapshot =
            self.setup_model_snapshot(request.provider, Some(&request.model), &request.root);
        let cache_state = snapshot
            .result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|status| status.state);
        let model_ready = !snapshot.checking
            && snapshot.device_issue.is_none()
            && matches!(cache_state, Some(CacheState::Loaded | CacheState::Cached));
        let (status_kind, model_status) = if snapshot.checking {
            (
                BadgeKind::Progress,
                course2md::tr!("正在检查模型").to_owned(),
            )
        } else if snapshot.device_issue.is_some() {
            (
                BadgeKind::Warning,
                course2md::tr!("缺少识别组件").to_owned(),
            )
        } else {
            match cache_state {
                Some(state) => (
                    if model_ready {
                        BadgeKind::Success
                    } else if state == CacheState::Unsupported {
                        BadgeKind::Warning
                    } else {
                        BadgeKind::Neutral
                    },
                    crate::settings_ui::model_diagnostics::cache_state_label(state)
                        .1
                        .into(),
                ),
                None => (BadgeKind::Neutral, course2md::tr!("检查未完成").into()),
            }
        };
        let choices_open = self.onboarding.model_choices_open;
        let model_count = models.len();
        let mut model_group = v_flex().w_full().min_w_0().gap(TILE_GAP);
        for (
            index,
            ModelChoice {
                id,
                label,
                description,
            },
        ) in models.into_iter().enumerate()
        {
            // The choices hang below the model tile, so only the last keeps
            // an outside corner.
            let last = index + 1 == model_count;
            let choice = ChoiceTile::new(
                ("setup-model-choice", index),
                index,
                model_count,
                label,
                self.onboarding.model == id,
            )
            .description(description)
            .corners([false, false, last, last]);
            model_group = model_group.child(
                self.setup_reveal(
                    ("setup-model-choice-reveal", index),
                    choice
                        .render(window, cx)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_setup_model(&id, cx);
                        })),
                ),
            );
        }
        let summary = tile([true, true, !choices_open, !choices_open])
            .flex()
            .items_center()
            .flex_wrap()
            .gap_x(rems(16. / 14.))
            .gap_y(rems(8. / 14.))
            .min_h(rems(72. / 14.))
            .pl(SETTING_ITEM_INSET)
            .pr(rems(12. / 14.))
            .py(rems(12. / 14.))
            .child(tinted_icon(icons::mic(), Tint::Purple, icon_size))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap_x(rems(12. / 14.))
                    .gap_y(rems(4. / 14.))
                    .child(tile_label("setup-current-model", current_model))
                    // Readiness reads as the model's status, next to its name.
                    .child(badge(status_kind).child(model_status)),
            )
            .child(
                text_button("setup-change-model", cx)
                    .icon(if choices_open {
                        icons::chevron_up()
                    } else {
                        icons::tune()
                    })
                    .label(if choices_open {
                        course2md::tr!("收起模型")
                    } else {
                        course2md::tr!("更换模型")
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.onboarding.model_choices_open = !this.onboarding.model_choices_open;
                        cx.notify();
                    })),
            );
        body = body.child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(px(8.))
                .child(
                    section_title("setup-model-label", course2md::tr!("识别模型"))
                        .pl(SETTING_ITEM_INSET),
                )
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap(TILE_GAP)
                        .child(summary)
                        .child(motion::disclosure(
                            "setup-model-choices",
                            choices_open,
                            model_group,
                        )),
                ),
        );
        body
    }

    fn setup_reveal(&self, id: impl Into<ElementId>, child: impl IntoElement) -> AnyElement {
        crate::focus_scroll::RevealFocus::new(id, child, self.onboarding.scroll.clone())
            .into_any_element()
    }

    fn setup_input_row(
        &self,
        purpose: ServicePurpose,
        field: FormField,
        label: &'static str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let owner = FormOwner::Setup(purpose);
        let service = self.onboarding.service(purpose);
        let form = &service.form;
        let busy =
            service.running.is_some() || service.vault_busy || service.pending_version.is_some();
        let id = format!("setup-{}-{field:?}", purpose as usize);
        let state = form.input(field);
        let focused = state.read(cx).focus_handle(cx).is_focused(window);
        // Outlined fields with their labels on the outline, as in the
        // service editor; status and errors stay in flow below.
        let column = if field == FormField::Model {
            v_flex().w_full().min_w_0().child(outlined_field_label(
                SharedString::from(id.clone()),
                label,
                TEXT_AUX,
                color(if focused { ACCENT } else { GRAY }),
                self.service_model_field(
                    owner,
                    if purpose == ServicePurpose::Ai {
                        "setup-ai-model"
                    } else {
                        "setup-speech-model"
                    },
                    busy,
                    cx,
                ),
            ))
        } else {
            let error = form.error(field);
            let input = text_input(state)
                .settings_text()
                .w_full()
                .aria_label(
                    error
                        .map(|error| course2md::tr!("{}，{}", label, error))
                        .unwrap_or_else(|| label.to_owned()),
                )
                .disabled(busy)
                .when(error.is_some(), |input| input.border_color(color(DANGER)))
                .when(field == FormField::Key, |input| {
                    input.suffix(self.service_key_visibility(
                        owner,
                        format!("setup-key-visibility-{}", purpose as usize),
                        cx,
                    ))
                });
            let input = labeled_input(
                SharedString::from(id.clone()),
                label,
                state,
                input,
                window,
                cx,
            );
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rems(4. / 14.))
                .child(input)
                .when(
                    field == FormField::Key && form.draft.credential.is_some(),
                    |column| {
                        column.child(
                            tile_support(
                                format!("setup-kept-key-{}", purpose as usize),
                                course2md::tr!("已保存密钥，留空则保留"),
                            )
                            .px(rems(16. / 14.)),
                        )
                    },
                )
                .when_some(error, |column, error| {
                    column.child(
                        error_text(SharedString::from(format!("{id}-error")), error.to_owned())
                            .px(rems(16. / 14.))
                            .text_size(TEXT_SETTINGS_AUX)
                            .whitespace_normal(),
                    )
                })
        };
        crate::focus_scroll::RevealFocus::new(
            SharedString::from(format!("{id}-reveal")),
            column,
            self.onboarding.scroll.clone(),
        )
        .into_any_element()
    }

    fn setup_service_content(
        &self,
        purpose: ServicePurpose,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let owner = FormOwner::Setup(purpose);
        let service = self.onboarding.service(purpose);
        let draft = &service.form.draft;
        let busy =
            service.running.is_some() || service.vault_busy || service.pending_version.is_some();
        // The connection form is one tile; what the service is used for,
        // the running check and its result follow as connected tiles.
        let mut items: Vec<AnyElement> = Vec::new();
        let mut form = v_flex()
            .w_full()
            .min_w_0()
            .gap(rems(20. / 14.))
            .py(rems(8. / 14.))
            .child(
                self.setup_reveal(
                    format!("setup-protocol-reveal-{}", purpose as usize),
                    self.service_type_field(
                        owner,
                        SharedString::from(format!("setup-protocol-{}", purpose as usize)),
                        busy,
                        cx,
                    )
                    .label(course2md::tr!("服务类型")),
                ),
            );
        let codex = draft.protocol == preferences::ServiceProtocol::CodexResponses;
        if !codex {
            form = form.child(self.setup_input_row(
                purpose,
                FormField::Address,
                course2md::tr!("服务地址"),
                window,
                cx,
            ));
        }
        if !draft.protocol.keyless() {
            form = form.child(self.setup_input_row(purpose, FormField::Key, "API Key", window, cx));
        }
        if codex {
            form = form.child(self.codex_account_section(
                crate::codex_ui::CodexSurface::Onboarding,
                busy,
                cx,
            ));
        }
        form = form.child(self.setup_input_row(
            purpose,
            FormField::Model,
            course2md::tr!("模型"),
            window,
            cx,
        ));
        items.push(form.into_any_element());
        if purpose == ServicePurpose::Ai {
            let mut uses = v_flex().w_full().min_w_0().gap(rems(4. / 14.));
            for use_kind in [SetupAiUse::Proofread, SetupAiUse::Summary] {
                let id = use_kind.id();
                let checked = match use_kind {
                    SetupAiUse::Proofread => self.onboarding.ai_proofread,
                    SetupAiUse::Summary => self.onboarding.ai_summary,
                };
                let icon = match use_kind {
                    SetupAiUse::Proofread => icons::auto_fix(),
                    SetupAiUse::Summary => icons::summarize(),
                };
                uses = uses.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .min_h(rems(48. / 14.))
                        .items_center()
                        .gap(rems(16. / 14.))
                        .child(
                            icon.size(rems(22. / 14.))
                                .flex_shrink_0()
                                .text_color(color(GRAY)),
                        )
                        .child(
                            tile_label(format!("setup-ai-{id}-label"), use_kind.label())
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(crate::focus_scroll::FocusRing::new(
                            format!("setup-ai-{id}-focus"),
                            preference_switch(
                                Switch::new(format!("setup-ai-{id}"))
                                    .checked(checked)
                                    .disabled(busy)
                                    .accessibility_label(use_kind.label())
                                    .on_click(cx.listener(move |this, enabled, _, cx| {
                                        match use_kind {
                                            SetupAiUse::Proofread => {
                                                this.onboarding.ai_proofread = *enabled
                                            }
                                            SetupAiUse::Summary => {
                                                this.onboarding.ai_summary = *enabled
                                            }
                                        }
                                        cx.notify();
                                    })),
                            ),
                        )),
                );
            }
            items.push(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rems(4. / 14.))
                    .child(self.setup_reveal("setup-ai-uses-reveal", uses))
                    .child(tile_info(
                        "setup-ai-use-scope",
                        course2md::tr!("开启后转录文字会发送到所选 AI 服务"),
                    ))
                    .into_any_element(),
            );
        }
        if let Some(cancel) = &service.running {
            let stopping = cancel.load(Ordering::Acquire);
            items.push(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .flex_wrap()
                    .gap(rems(12. / 14.))
                    .child(motion::spinner(
                        format!("setup-service-check-{}", purpose as usize),
                        cx,
                    ))
                    .child(
                        settings_value(
                            format!("setup-service-check-label-{}", purpose as usize),
                            if stopping {
                                course2md::tr!("正在停止检查")
                            } else {
                                course2md::tr!("正在检查配置")
                            },
                        )
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::MEDIUM),
                    )
                    .child(
                        text_button(format!("setup-cancel-check-{}", purpose as usize), cx)
                            .icon(icons::close())
                            .label(course2md::tr!("停止检查"))
                            .disabled(stopping)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.onboarding.service(purpose).cancel();
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        if !service.evidence.is_empty() {
            let mut feedback = v_flex().w_full().min_w_0().gap(rems(8. / 14.));
            for (index, (kind, evidence)) in service.evidence.iter().enumerate() {
                let passed = evidence.outcome == TestOutcome::Passed;
                feedback =
                    feedback.child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_start()
                            .gap(rems(12. / 14.))
                            .child(
                                if passed {
                                    icons::check_circle()
                                } else {
                                    icons::warning()
                                }
                                .size(rems(20. / 14.))
                                .mt(rems(1. / 14.))
                                .flex_shrink_0()
                                .text_color(color(if passed { SUCCESS } else { DANGER })),
                            )
                            .child(
                                settings_value(
                                    format!("setup-test-message-{}-{index}", purpose as usize),
                                    if passed {
                                        course2md::tr!("{}测试通过", kind.label())
                                    } else {
                                        evidence.message.clone()
                                    },
                                )
                                .flex_1()
                                .min_w_0()
                                .text_color(color(if passed { INK } else { DANGER })),
                            ),
                    );
            }
            let details = service
                .evidence
                .iter()
                .flat_map(|(_, evidence)| evidence.details.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n");
            feedback = feedback
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .flex_wrap()
                        .ml(rems(-12. / 14.))
                        .when(self.setup_service_passed(purpose, cx), |row| {
                            row.child(
                                text_button(
                                    format!("setup-recheck-service-{}", purpose as usize),
                                    cx,
                                )
                                .icon(icons::refresh())
                                .label(course2md::tr!("重新检查"))
                                .disabled(busy)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.start_setup_test(purpose, window, cx)
                                    },
                                )),
                            )
                        })
                        .child(
                            text_button(format!("setup-test-details-{}", purpose as usize), cx)
                                .icon(icons::info())
                                .label(if service.details_open {
                                    course2md::tr!("收起检查详情")
                                } else {
                                    course2md::tr!("检查详情")
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let service = this.onboarding.service_mut(purpose);
                                    service.details_open = !service.details_open;
                                    cx.notify();
                                })),
                        ),
                )
                .child(motion::disclosure(
                    format!("setup-test-detail-content-{}", purpose as usize),
                    service.details_open,
                    v_flex().w_full().min_w_0().child(tile_support(
                        format!("setup-test-details-text-{}", purpose as usize),
                        details,
                    )),
                ));
            items.push(motion::state_enter(
                ("setup-service-feedback", service.test_serial as usize),
                feedback,
            ));
        }
        let count = items.len();
        v_flex().w_full().min_w_0().gap(TILE_GAP).children(
            items
                .into_iter()
                .enumerate()
                .map(|(index, item)| setup_tile(index == 0, index + 1 == count).child(item)),
        )
    }

    fn setup_model_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let request = self.setup_model_request();
        let provider = request.provider;
        if provider == AsrProvider::Api {
            let version = self
                .preferences
                .default_refs()
                .asr
                .and_then(|id| self.preferences.version(&id))
                .cloned();
            return setup_tile(true, true).child(tile_info(
                "setup-cloud-model-description",
                version
                    .map(|version| {
                        course2md::tr!(
                            "识别时使用 {} · {}",
                            version.config.name,
                            version.config.model
                        )
                    })
                    .unwrap_or_else(|| {
                        course2md::tr!("当前没有默认语音服务，请返回第一步完成配置。").into()
                    }),
            ));
        }
        let preparation_result = self
            .onboarding
            .model_preparation
            .as_ref()
            .filter(|preparation| preparation.request == request)
            .and_then(|preparation| preparation.result.clone());
        let previously_cancelled = self
            .onboarding
            .model_preparation
            .as_ref()
            .is_some_and(|preparation| preparation.request == request && preparation.cancelled);
        let model = request.model;
        let root = request.root;
        let snapshot = self.setup_model_snapshot(provider, Some(&model), &root);
        let cancelled = if snapshot.notice.is_some() {
            snapshot.cancelled
        } else {
            previously_cancelled
        };
        let status = snapshot
            .result
            .as_ref()
            .and_then(|result| result.as_ref().ok());
        let mut body = setup_tile(true, true).gap(rems(16. / 14.));
        let mut actions = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .flex_wrap();
        let (label, hint) = if snapshot.preparing {
            (course2md::tr!("正在准备模型"), "")
        } else if cancelled {
            (course2md::tr!("模型准备已暂停"), "")
        } else if snapshot.checking {
            (course2md::tr!("正在检查模型"), "")
        } else if snapshot.device_issue.is_some() {
            (
                course2md::tr!("缺少识别组件"),
                course2md::tr!("可返回第一步更换引擎"),
            )
        } else {
            match status.map(|status| &status.state) {
                Some(
                    state @ (CacheState::Loaded
                    | CacheState::Cached
                    | CacheState::Missing
                    | CacheState::Partial),
                ) => (
                    crate::settings_ui::model_diagnostics::cache_state_label(*state).1,
                    "",
                ),
                Some(CacheState::Unsupported) => {
                    (unsupported_model(), course2md::tr!("可返回第一步更换模型"))
                }
                None => (course2md::tr!("模型检查未完成"), ""),
            }
        };
        let model_title = course2md::config::model_label(&model);
        // The state is the model's status chip, next to its name; a live
        // check or preparation adds its spinner.
        let active = (snapshot.checking && !cancelled) || snapshot.preparing;
        let state_kind = if active {
            BadgeKind::Progress
        } else if cancelled {
            BadgeKind::Neutral
        } else if snapshot.device_issue.is_some() {
            BadgeKind::Warning
        } else {
            match status.map(|status| status.state) {
                Some(CacheState::Loaded | CacheState::Cached) => BadgeKind::Success,
                Some(CacheState::Unsupported) => BadgeKind::Warning,
                _ => BadgeKind::Neutral,
            }
        };
        body = body.child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_center()
                        .gap(rems(16. / 14.))
                        .child(tinted_icon(
                            icons::mic(),
                            Tint::Purple,
                            rems(40. / 14.).to_pixels(window.rem_size()),
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(rems(2. / 14.))
                                .child(
                                    h_flex()
                                        .min_w_0()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_x(rems(12. / 14.))
                                        .gap_y(rems(4. / 14.))
                                        .child(tile_label("setup-model-identity", model_title))
                                        .child(
                                            badge(state_kind).child(
                                                accessible_text("setup-model-state", label)
                                                    .role(Role::Status),
                                            ),
                                        )
                                        .when(active, |row| {
                                            row.child(motion::spinner("setup-model-spinner", cx))
                                        }),
                                )
                                .child(tile_support(
                                    "setup-model-engine",
                                    crate::provider_label(Some(provider)),
                                )),
                        ),
                )
                .when(!hint.is_empty(), |v| {
                    v.child(tile_info("setup-model-state-hint", hint))
                }),
        );
        // Text actions start on the tile's content edge; a leading pill keeps its own.
        let mut leading_pill = snapshot.preparing;
        if snapshot.preparing {
            body = body.child(self.setup_model_progress(window, cx));
            actions = actions.child(
                outline_pill("setup-pause-model", cx)
                    .self_start()
                    .icon(icons::pause())
                    .label(if self.cancelling {
                        course2md::tr!("正在暂停…")
                    } else {
                        course2md::tr!("暂停准备")
                    })
                    .disabled(self.cancelling)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(job) = &this.job {
                            job.cancel();
                            this.cancelling = true;
                            cx.notify();
                        }
                    })),
            );
        } else {
            let allowed = self.environment.is_some()
                && snapshot.device_issue.is_none()
                && status.is_some_and(|status| status.can_prepare);
            let needs_download = status.is_some_and(|status| {
                matches!(status.state, CacheState::Missing | CacheState::Partial)
            });
            if allowed {
                let loaded = status.is_some_and(|status| status.state == CacheState::Loaded);
                leading_pill = !loaded;
                let button = if needs_download {
                    primary_pill("setup-prepare-model")
                } else if status.is_some_and(|status| status.state == CacheState::Loaded) {
                    text_button("setup-prepare-model", cx)
                } else {
                    outline_pill("setup-prepare-model", cx)
                };
                let target = root.clone();
                actions = actions.child(
                    button
                        .self_start()
                        .icon(
                            if status.is_some_and(|status| status.state == CacheState::Loaded) {
                                icons::refresh()
                            } else {
                                icons::download()
                            },
                        )
                        .label(if cancelled {
                            course2md::tr!("继续准备")
                        } else {
                            crate::settings_ui::model_diagnostics::model_action_label(
                                provider,
                                status.map(|status| &status.state),
                            )
                        })
                        .disabled(self.job.is_some() || snapshot.checking)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.job.is_some() {
                                return;
                            }
                            let request = ModelRequest::new(provider, Some(&model), &target);
                            this.onboarding.model_status_request = Some(request.clone());
                            this.onboarding.model_preparation = Some(ModelPreparation {
                                request,
                                result: None,
                                cancelled: false,
                            });
                            this.prepare_setup_model(provider, Some(&model), &target, cx);
                        })),
                );
            }
            actions = actions.child(
                text_button("setup-recheck-model", cx)
                    .self_start()
                    .icon(icons::refresh())
                    .label(course2md::tr!("重新检查"))
                    .disabled(snapshot.checking || self.job.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_environment(cx))),
            );
            if self.job.is_some() {
                body = body.child(tile_info("setup-model-job-busy", job_busy()));
            }
        }
        let mut details = v_flex().w_full().min_w_0().gap_3();
        if let Some((message, error)) = snapshot.notice.or(preparation_result) {
            if error && !cancelled && !snapshot.preparing {
                body = body.child(
                    settings_value(
                        "setup-model-result",
                        course2md::tr!("准备未完成，原因见模型详情"),
                    )
                    .text_color(color(DANGER))
                    .role(Role::Status),
                );
            }
            details = details.child(tile_info("setup-model-preparation-detail", message));
        }
        if let Some(issue) = snapshot.device_issue {
            details = details.child(tile_info("setup-model-device-detail", issue));
        }
        if let Some(Err(error)) = &snapshot.result {
            details = details.child(tile_info("setup-model-error-detail", error.clone()));
        }
        if let Some(status) = status {
            if status.bytes > 0 {
                details = details.child(settings_detail_row(
                    "setup-model-size-label",
                    course2md::tr!("占用空间"),
                    settings_value("setup-model-size", crate::activity::bytes(status.bytes)),
                ));
            }
            for (index, part) in status.parts.iter().enumerate() {
                details = details
                    .child(
                        settings_value(("setup-model-part", index), part.name.clone())
                            .font_weight(FontWeight::MEDIUM),
                    )
                    .child(
                        tile_info(("setup-model-path", index), part.path.display().to_string())
                            .text_size(TEXT_AUX),
                    );
            }
            if let Some(error) = &status.last_error {
                details = details.child(tile_info(
                    "setup-model-last-error",
                    course2md::tr!("上次准备记录：{}", error),
                ));
            }
        }
        actions = actions.child(
            text_button("setup-model-details", cx)
                .icon(icons::info())
                .label(if self.onboarding.model_details_open {
                    course2md::tr!("收起详情")
                } else {
                    course2md::tr!("模型详情")
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.onboarding.model_details_open = !this.onboarding.model_details_open;
                    cx.notify();
                })),
        );
        body.child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(self.setup_reveal(
                    "setup-model-actions-reveal",
                    actions.when(!leading_pill, |row| row.ml(rems(-12. / 14.))),
                ))
                .child(motion::disclosure(
                    "setup-model-detail-content",
                    self.onboarding.model_details_open,
                    details,
                )),
        )
    }

    fn setup_model_progress(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // Reserve the phase, detail and bar region before the first worker sample.
        // Incoming progress must not move the pause or continue actions.
        let mut view = v_flex().w_full().min_w_0().min_h(rems(80. / 14.)).gap_3();
        let mut has_phase = false;
        for (index, (stage, progress)) in self
            .progress
            .iter()
            .filter(|(stage, progress)| {
                stage.is_model_transfer() && progress.has_samples() && !progress.done
            })
            .enumerate()
        {
            has_phase = true;
            let phase = activity::model_transfer_phase(stage, progress.phase);
            view = view.child(motion::transfer_status(
                ("setup-model-progress", index),
                phase,
                &progress.transfer_metrics(stage, true),
                progress.fraction(),
                window,
                cx,
            ));
        }
        if !has_phase {
            view = view.child(semantic_label(
                "setup-model-awaiting-progress",
                course2md::tr!("等待准备进度"),
                icons::download(),
            ));
        }
        view
    }

    fn setup_service_action(&self, purpose: ServicePurpose, cx: &App) -> ServiceAction {
        let service = self.onboarding.service(purpose);
        if service.running.is_some() {
            ServiceAction::Save
        } else if service.pending_version.is_some() {
            ServiceAction::RetrySave
        } else if service.unchanged(cx) && self.setup_required_tests(purpose, cx).is_empty() {
            ServiceAction::Keep
        } else if self.setup_service_passed(purpose, cx) {
            ServiceAction::Save
        } else if service.evidence.is_empty() {
            ServiceAction::Check
        } else {
            ServiceAction::Recheck
        }
    }

    /// Which service the footer's service action targets: the speech service when the engine step chose
    /// cloud recognition, the AI service on the AI step; other steps have no service action.
    fn footer_service_purpose(&self) -> Option<ServicePurpose> {
        if self.onboarding.step == Step::Engine
            && self.onboarding.provider == Some(AsrProvider::Api)
        {
            Some(ServicePurpose::Speech)
        } else if self.onboarding.step == Step::Ai {
            Some(ServicePurpose::Ai)
        } else {
            None
        }
    }

    fn setup_footer(&self, cx: &mut Context<Self>) -> Div {
        if self.onboarding.finish_failed {
            return v_flex()
                .w_full()
                .flex_shrink_0()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            text_button("setup-enter-temporarily", cx)
                                .ml(rems(-12. / 14.))
                                .label(course2md::tr!("暂时进入应用"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.leave_onboarding(window, cx)
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            primary_pill("setup-retry-finish")
                                .label(course2md::tr!("重试保存"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.finish_onboarding(window, cx)
                                })),
                        ),
                )
                .child(
                    supporting_info(
                        "setup-finish-failure-hint",
                        course2md::tr!("暂时进入不会保存未完成的选择"),
                    )
                    .text_size(TEXT_AUX),
                );
        }
        let step = self.onboarding.step;
        let model_review = self.onboarding.model_return_page.is_some();
        let model_running = self.setup_model_job_running();
        let emphasize_model_preparation = if step == Step::Model
            && !model_running
            && self.job.is_none()
        {
            let request = self.setup_model_request();
            let snapshot =
                self.setup_model_snapshot(request.provider, Some(&request.model), &request.root);
            request.provider != AsrProvider::Api
                && self.environment.is_some()
                && !snapshot.checking
                && snapshot.device_issue.is_none()
                && snapshot.result.as_ref().is_some_and(|result| {
                    result.as_ref().is_ok_and(|status| {
                        status.can_prepare
                            && matches!(status.state, CacheState::Missing | CacheState::Partial)
                    })
                })
        } else {
            false
        };
        let service_purpose = self.footer_service_purpose();
        let service_action = service_purpose.map(|purpose| self.setup_service_action(purpose, cx));
        let busy = service_purpose.is_some_and(|purpose| {
            let service = self.onboarding.service(purpose);
            service.running.is_some() || service.vault_busy
        });
        let label = match service_action {
            Some(action) => action.label(),
            None => match step {
                Step::Account => {
                    if self.account_connected() {
                        course2md::tr!("继续")
                    } else {
                        course2md::tr!("跳过登录")
                    }
                }
                Step::Model if model_review => course2md::tr!("返回应用"),
                Step::Model if model_running => course2md::tr!("继续使用"),
                Step::Model if emphasize_model_preparation => course2md::tr!("稍后准备"),
                Step::Model => course2md::tr!("开始使用"),
                _ => course2md::tr!("保存并继续"),
            },
        };
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .flex_wrap();
        if step == Step::Engine && !model_review {
            row = row.child(
                // Skipping is not the way forward, so it carries no arrow.
                text_button("setup-later", cx)
                    .ml(rems(-12. / 14.))
                    .label(course2md::tr!("跳过设置"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.save_setup_engine(window, cx) {
                            this.finish_onboarding(window, cx);
                        }
                    })),
            );
        }
        if step != Step::Engine && !model_review {
            row = row.child(
                text_button("setup-back", cx)
                    .ml(rems(-12. / 14.))
                    .icon(icons::arrow_left())
                    .label(course2md::tr!("上一步"))
                    .disabled(step == Step::Model && model_running)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.setup_step(
                            match step {
                                Step::Ai => Step::Engine,
                                Step::Account => Step::Ai,
                                _ => Step::Account,
                            },
                            cx,
                        )
                    })),
            );
        }
        row = row.child(div().flex_1());
        if step == Step::Ai {
            row = row.child(
                outline_pill("setup-skip-ai", cx)
                    .label(course2md::tr!("稍后配置 AI"))
                    .on_click(cx.listener(|this, _, _, cx| this.skip_setup_ai(cx))),
            );
        }
        let next = if emphasize_model_preparation {
            outline_pill("setup-next", cx)
        } else {
            primary_pill("setup-next")
        };
        row = row.child(
            next.label(label)
                .icon(icons::arrow_forward())
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| match step {
                    Step::Engine => {
                        this.save_setup_engine(window, cx);
                    }
                    Step::Ai => {
                        this.continue_setup_service(ServicePurpose::Ai, window, cx);
                    }
                    Step::Account => this.setup_step(Step::Model, cx),
                    Step::Model if model_review => this.leave_onboarding(window, cx),
                    Step::Model => this.finish_onboarding(window, cx),
                })),
        );
        v_flex()
            .w_full()
            .flex_shrink_0()
            .gap_2()
            // The cost notice appears only with the action that sends the sample.
            .when(
                service_action.is_some_and(ServiceAction::sends_sample),
                |v| {
                    v.child(
                        supporting_info("setup-test-notice", service_test::test_notice())
                            .text_size(TEXT_AUX),
                    )
                },
            )
            .child(row)
            .when(model_running, |v| {
                v.child(
                    h_flex().w_full().justify_end().child(
                        supporting_info(
                            "setup-return-hint",
                            course2md::tr!("模型会继续在后台准备"),
                        )
                        .text_size(TEXT_AUX)
                        .text_right(),
                    ),
                )
            })
    }

    pub(crate) fn onboarding_background_notice(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if self.onboarding.active {
            return None;
        }
        let preparation = self.onboarding.model_preparation.as_ref()?;
        let running = self.setup_model_job_running();
        let result_error = preparation.result.as_ref().is_some_and(|(_, error)| *error);
        let label = if running {
            if self.cancelling {
                course2md::tr!("正在暂停").to_owned()
            } else {
                self.progress
                    .iter()
                    .find(|(stage, progress)| {
                        stage.is_model_transfer() && progress.has_samples() && !progress.done
                    })
                    .map(|(stage, progress)| {
                        let phase = activity::model_transfer_phase(stage, progress.phase);
                        progress
                            .fraction()
                            .map(|fraction| {
                                format!("{phase} · {:.0}%", fraction.clamp(0., 1.) * 100.)
                            })
                            .unwrap_or(phase)
                    })
                    .unwrap_or_else(|| course2md::tr!("正在准备").into())
            }
        } else if preparation.cancelled {
            course2md::tr!("准备已暂停").into()
        } else if result_error {
            course2md::tr!("准备未完成").into()
        } else {
            course2md::tr!("已准备完成").into()
        };
        Some(
            v_flex()
                .w_full()
                .min_w_0()
                .px_6()
                .py_2()
                .gap_2()
                .bg(color(INSET))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(
                            h_flex()
                                .flex_1()
                                .flex_basis(rems(240. / 14.))
                                .min_w_0()
                                .gap_2()
                                .items_center()
                                .when(running, |row| {
                                    row.child(div().flex_shrink_0().child(motion::spinner(
                                        "setup-background-model-spinner",
                                        cx,
                                    )))
                                })
                                .child(
                                    settings_value(
                                        "setup-background-model-status",
                                        format!(
                                            "{} · {label}",
                                            model_label(&preparation.request.model)
                                        ),
                                    )
                                    .w_auto()
                                    .flex_1()
                                    .text_color(color(
                                        if result_error && !preparation.cancelled && !running {
                                            DANGER
                                        } else {
                                            INK
                                        },
                                    )),
                                ),
                        )
                        .child(
                            quiet("setup-background-model-open")
                                .label(if running {
                                    course2md::tr!("查看进度")
                                } else {
                                    course2md::tr!("查看结果")
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_setup_model_status(window, cx)
                                })),
                        )
                        .when(!running, |row| {
                            row.child(
                                text_button("setup-background-model-dismiss", cx)
                                    .label(course2md::tr!("关闭提示"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.setup_model_job_running() {
                                            return;
                                        }
                                        this.onboarding.model_preparation = None;
                                        cx.notify();
                                    })),
                            )
                        }),
                ),
        )
    }
}

/// The guide's progress: numbered steps joined by a hairline. The current
/// step is the accent, finished ones sit on the secondary container.
fn setup_steps(labels: &[&'static str], current: usize) -> Div {
    let count = labels.len();
    h_flex()
        .w_full()
        .min_w_0()
        .items_center()
        .gap(px(8.))
        .children(labels.iter().enumerate().map(|(index, label)| {
            let number = index + 1;
            let is_current = number == current;
            let finished = number < current;
            h_flex()
                .when(number < count, |item| item.flex_1())
                .min_w_0()
                .gap(px(8.))
                .items_center()
                .child(
                    div()
                        .size(rems(24. / 14.))
                        .flex_shrink_0()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(TEXT_AUX)
                        .font_weight(FontWeight::MEDIUM)
                        .map(|dot| {
                            if is_current {
                                dot.bg(color(ACCENT)).text_color(color(ON_PRIMARY))
                            } else if finished {
                                dot.bg(color(SECONDARY_CONTAINER))
                                    .text_color(color(ON_SECONDARY_CONTAINER))
                            } else {
                                dot.border_1()
                                    .border_color(color(CONTROL))
                                    .text_color(color(GRAY))
                            }
                        })
                        .when(finished, |dot| dot.child(icons::check().size(rems(1.))))
                        .when(!finished, |dot| dot.child(number.to_string())),
                )
                .child(
                    accessible_text(("setup-step-label", index), *label)
                        .flex_shrink_0()
                        .text_size(TEXT_BODY)
                        .line_height(rems(20. / 14.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color(if is_current { INK } else { GRAY })),
                )
                .when(number < count, |item| {
                    item.child(div().flex_1().min_w(px(8.)).h(px(1.)).bg(color(HAIRLINE)))
                })
        }))
}

/// A tile of the guide on the canvas ground: the settings tile surface and
/// insets. `first`/`last` give its outside corners in a connected group.
fn setup_tile(first: bool, last: bool) -> Div {
    tile([first, first, last, last])
        .flex()
        .flex_col()
        .gap(rems(12. / 14.))
        .px(SETTING_ITEM_INSET)
        .py(rems(16. / 14.))
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
                super::ModelRequest::new(provider, Some(current), root).model,
                cache
            );
            let saved =
                engine_preferences(&GenerationPreferences::default(), Some(provider), current);
            assert_eq!(saved.options.asr_model.as_deref(), Some(current));
        }
        // An automatic engine that resolves to the NPU runs its default, not
        // the Qwen3 choice saved for another engine.
        assert_eq!(
            super::ModelRequest::new(AsrProvider::Npu, Some("qwen3-1.7b"), root).model,
            "whisper"
        );
        // An NPU-only model must not be silently substituted on Apple silicon.
        assert!(!model_supported(AsrProvider::Coreml, "whisper-tiny"));
        assert_eq!(
            super::ModelRequest::new(AsrProvider::Coreml, Some("whisper-tiny"), root).model,
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
