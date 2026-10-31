//! A resumable first-use guide. Configuration, contract tests and model jobs
//! use the same persistence and execution paths as Settings.
use crate::codex_ui::CodexSurface;
use crate::credentials::Secret;
use crate::preferences::{
    self, BindingScope, GenerationPreferences, PreferenceGroup, ServiceConfiguration,
    ServiceDraft, ServiceProtocol, ServicePurpose, ServiceTestEvidence, ServiceVersion,
    TestOutcome,
};
use crate::service_test::{self, TestKind};
use crate::settings_ui::model_diagnostics::{
    LOCAL_ENGINES, Request as ModelRequest, engine_icon, engine_offered, model_supported,
    unsupported_model,
};
use crate::settings_ui::service_form::{FormField, FormOwner, ServiceForm};
use crate::ui::{self, AsyncStatus};
use crate::{Desktop, Page, activity, provider_label, symbols};
use anyhow::Result;
use course2md::config::{AsrProvider, local_models, model_dir_from, model_label};
use gpui::{prelude::*, *};
use gpui_base::{h_flex, v_flex};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit_material::{icons::Icon, motion, prelude::*};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// The guide's content column.
const COLUMN: Rems = rem(760.);
const STEPS: usize = 4;

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
    /// Editing is closed while a check, a vault step or a retried save is open.
    fn busy(&self) -> bool {
        self.running.is_some() || self.vault_busy || self.pending_version.is_some()
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
    /// The model the guide started preparing; the shell reports on it after the guide closes.
    background_model: Option<ModelRequest>,
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
            background_model: None,
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
        provider_label(Some(resolved)),
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
            "cloud" => self.apply_setup_provider_choice(ProviderChoice::Online, cx),
            "ai" => self.setup_step(Step::Ai, cx),
            "account" => self.setup_step(Step::Account, cx),
            "model" => self.setup_step(Step::Model, cx),
            "" => {}
            _ => self.setup_step(Step::Engine, cx),
        }
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
        self.ensure_setup_model(cx);
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
        self.ensure_setup_model(cx);
        cx.notify();
    }

    fn ensure_setup_model(&mut self, cx: &mut Context<Self>) {
        let request = self.setup_model_request();
        self.ensure_model_diagnostic(request.provider, Some(&request.model), &request.root, cx);
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
            self.codex_refresh_status(CodexSurface::Onboarding, cx);
        }
        self.onboarding.step = step;
        self.onboarding.notice = None;
        self.onboarding.scroll.set_offset(point(px(0.), px(0.)));
        if matches!(step, Step::Engine | Step::Model) {
            self.ensure_setup_model(cx);
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

    /// Adopts a committed draft and validates it; `None` reports why not.
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
                self.setup_publish_failed(&error, cx);
                None
            }
        }
    }

    fn commit_setup_form(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<ServiceConfiguration> {
        let draft = self.onboarding.service(purpose).form.current_draft(cx);
        let saved = self.preferences.commit_service_draft(draft, None);
        self.commit_setup_draft(purpose, saved, window, cx)
    }

    /// Checks the form against the service. A typed key first reaches the vault
    /// off the UI thread, and the check starts again once the draft carries it.
    fn start_setup_test(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.onboarding.service(purpose);
        if service.running.is_some() || service.vault_busy {
            return;
        }
        let key = service.form.typed_key(cx);
        if !key.trim().is_empty() {
            let draft = service.form.current_draft(cx);
            self.onboarding.service_mut(purpose).vault_busy = true;
            let secret = Secret::new(key);
            self.run_vault_step(
                cx,
                move |vault| preferences::Store::insert_credential(vault, secret),
                move |this, stored, window, cx| {
                    let service = this.onboarding.service_mut(purpose);
                    service.vault_busy = false;
                    if service.form.draft.id != draft.id {
                        if let Ok(reference) = stored {
                            this.forget_credentials(vec![reference]);
                        }
                        return;
                    }
                    let committed = stored.and_then(|reference| {
                        this.preferences
                            .commit_service_draft(draft, Some(reference.clone()))
                            .inspect_err(|_| this.forget_credentials(vec![reference]))
                    });
                    if committed.is_ok() {
                        let service = this.onboarding.service_mut(purpose);
                        service.form.clear_key(window, cx);
                    }
                    if this
                        .commit_setup_draft(purpose, committed, window, cx)
                        .is_some()
                    {
                        this.start_setup_test(purpose, window, cx);
                    }
                    cx.notify();
                },
            );
            cx.notify();
            return;
        }
        let Some(config) = self.commit_setup_form(purpose, window, cx) else {
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
                if this.onboarding.active {
                    this.onboarding.scroll.scroll_to_bottom();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The forward action of a service step: check what has not passed,
    /// publish what has, then save the defaults and move on.
    fn continue_setup_service(
        &mut self,
        purpose: ServicePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let service = self.onboarding.service(purpose);
        if let Some(version) = service.pending_version.clone() {
            return self.finish_setup_service(purpose, version, window, cx);
        }
        if !self.setup_service_passed(purpose, cx) {
            return self.start_setup_test(purpose, window, cx);
        }
        if service.unchanged(cx) {
            let Some(version) = service.original.clone() else {
                return;
            };
            if let Err(error) = self.preferences.check_dispatch(&version.id) {
                self.onboarding.notice = Some((
                    course2md::tr!("当前服务暂不可用：{}", format!("{error:#}")),
                    true,
                ));
                cx.notify();
                return;
            }
            return self.finish_setup_service(purpose, version, window, cx);
        }
        if self.commit_setup_form(purpose, window, cx).is_none() {
            return;
        }
        let id = self.onboarding.service(purpose).form.draft.id.clone();
        // Publication confirms the credential is in the vault, which can
        // block: that check runs off the UI thread.
        let reference = match self.preferences.publication_credential(&id) {
            Ok(reference) => reference,
            Err(error) => return self.setup_publish_failed(&error, cx),
        };
        self.onboarding.service_mut(purpose).vault_busy = true;
        self.run_vault_step(
            cx,
            move |vault| preferences::Store::verify_credential(vault, reference),
            move |this, verified, window, cx| {
                this.onboarding.service_mut(purpose).vault_busy = false;
                let published = verified.and_then(|verified| {
                    this.preferences
                        .commit_publication(&id, BindingScope::Defaults, verified)
                });
                match published {
                    Ok(version) => {
                        this.onboarding.service_mut(purpose).pending_version =
                            Some(version.clone());
                        this.finish_setup_service(purpose, version, window, cx);
                    }
                    Err(error) => this.setup_publish_failed(&error, cx),
                }
            },
        );
        cx.notify();
    }

    fn finish_setup_service(
        &mut self,
        purpose: ServicePurpose,
        version: ServiceVersion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            cx.notify();
            return;
        }
        self.refresh_dispatch_controls(cx);
        self.hydrate_setup_service(purpose, Some(&version.id), window, cx);
        self.setup_step(
            match purpose {
                ServicePurpose::Speech => Step::Ai,
                ServicePurpose::Ai => Step::Account,
            },
            cx,
        );
    }

    fn setup_publish_failed(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        self.onboarding.notice = Some((
            preferences::save_failure_message(PreferenceGroup::Services, error),
            true,
        ));
        cx.notify();
    }

    /// Saves a local engine choice; true when it was saved.
    fn save_setup_engine(&mut self, cx: &mut Context<Self>) -> bool {
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
            cx.notify();
            return false;
        }
        true
    }

    /// Skipping keeps a usable local choice. An online service that has not passed
    /// stays unsaved, and skipping never sends it a sample.
    fn skip_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.onboarding.provider == Some(AsrProvider::Api) || self.save_setup_engine(cx) {
            self.finish_onboarding(window, cx);
        }
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

    /// A preparation started in the guide stays its own after the guide closes.
    pub(crate) fn follow_setup_model(&mut self, request: &ModelRequest) {
        if self.onboarding.active {
            self.onboarding.model_status_request = Some(request.clone());
            self.onboarding.background_model = Some(request.clone());
        }
    }

    fn setup_model_job_running(&self) -> bool {
        self.onboarding
            .background_model
            .as_ref()
            .is_some_and(|request| self.is_preparing(request))
    }

    fn open_setup_model_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.onboarding.active && self.page == Page::New && !self.save_current_draft(cx) {
            return;
        }
        let Some(request) = self.onboarding.background_model.clone() else {
            return;
        };
        if !self.onboarding.active {
            self.onboarding.model_return_page = Some(self.page);
        }
        self.onboarding.model_status_request = Some(request);
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
        let (number, title, description): (usize, _, _) = match step {
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
            Step::Account => setup_tile().child(self.account_onboarding_page(cx)),
            Step::Model => self.setup_model_content(window, cx),
        };
        let position = course2md::tr!("第 {} 步，共 {} 步", number, STEPS);
        let heading = v_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap(rem(16.))
            .pt(rem(24.))
            .pb(rem(20.))
            .child(
                v_flex()
                    .w_full()
                    .gap(rem(8.))
                    .child(TileScale::Regular.support("setup-position", position.clone()))
                    .child(progress_with_label(
                        "setup-progress",
                        position,
                        number as f32 / STEPS as f32,
                        window,
                        cx,
                    )),
            )
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(rem(4.))
                    .child(page_heading("setup-title", title))
                    .when_some(description, |view, description| {
                        view.child(TileScale::Regular.support("setup-description", description))
                    }),
            );
        let body = v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(24.))
            .child(content)
            .children(self.onboarding.notice.as_ref().and_then(|(message, error)| {
                let status = if *error {
                    AsyncStatus::Failed(message.clone().into())
                } else {
                    AsyncStatus::Warning(message.clone().into())
                };
                ui::async_status("setup-notice", &status, None, cx)
            }));
        // Natural height keeps the actions next to the step; the body scrolls
        // only when the window is too short, and then a hairline marks where
        // it passes under the actions.
        let scrollable = self.onboarding.scroll.max_offset().y > px(0.);
        let footer = self.setup_footer(cx);
        let column = v_flex()
            .w(COLUMN)
            .max_w_full()
            .min_w_0()
            .max_h_full()
            .child(heading)
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
                            .pb(rem(4.))
                            .child(motion::state_enter(("setup-step", number), body)),
                    )
                    .child(Scrollbar::vertical(&self.onboarding.scroll)),
            )
            .child(
                footer
                    .pt(rem(16.))
                    .pb(rem(20.))
                    .when(scrollable, |footer| {
                        footer
                            .border_t_1()
                            .border_color(color(ColorRole::OUTLINE_VARIANT))
                    }),
            );
        v_flex()
            .flex_1()
            .w_full()
            .min_h_0()
            .items_center()
            .bg(color(ColorRole::SURFACE_CONTAINER))
            .px(rem(if is_compact(window) { 16. } else { 24. }))
            .child(column)
            .into_any_element()
    }

    fn setup_reveal(&self, id: impl Into<ElementId>, child: impl IntoElement) -> RevealFocus {
        RevealFocus::new(id, child, self.onboarding.scroll.clone())
    }

    fn setup_engine_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let selected = self.onboarding.provider;
        let recommended = self.recommended_local_provider();
        let local = selected != Some(AsrProvider::Api);
        // Automatic falls back to CPU even when CPU cannot run; then no engine
        // is usable, and that one obstacle is said once below the group.
        let obstacle = self.engine_issue(recommended);
        let icon_size = rem(40.).to_pixels(window.rem_size());
        // Where audio is processed is the outcome-level choice.
        let routes = ChoiceGroup::new("setup-route", course2md::tr!("音频的处理位置"))
            .tile(
                ChoiceTile::new("setup-local-route", 0, 2, course2md::tr!("本机识别"), local)
                    .tinted_icon(symbols::computer().into(), Tint::Blue, icon_size)
                    .description(if obstacle.is_some() {
                        course2md::tr!("音频不离开本机")
                    } else {
                        course2md::tr!("推荐 · 音频不离开本机")
                    }),
            )
            .tile(
                ChoiceTile::new(
                    "setup-cloud-route",
                    1,
                    2,
                    course2md::tr!("在线语音服务"),
                    !local,
                )
                .tinted_icon(symbols::cloud().into(), Tint::Teal, icon_size)
                .description(course2md::tr!("音频会发送到所选服务")),
            )
            .on_change(cx.listener(|this, index: &usize, _, cx| {
                let choice = if *index == 0 {
                    ProviderChoice::Local
                } else {
                    ProviderChoice::Online
                };
                this.apply_setup_provider_choice(choice, cx);
            }));
        let body = v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(24.))
            .child(self.setup_reveal("setup-route-reveal", routes));
        if !local {
            return body.child(self.setup_service_content(ServicePurpose::Speech, window, cx));
        }

        let detected = self.environment.is_some();
        let engines: Vec<_> = LOCAL_ENGINES
            .into_iter()
            .filter(|engine| engine_offered(*engine, selected))
            .collect();
        let automatic = if !detected {
            course2md::tr!("正在检测").to_owned()
        } else if obstacle.is_some() {
            course2md::tr!("暂无可用的本机引擎").to_owned()
        } else {
            course2md::tr!("目前使用 {}", provider_label(Some(recommended)))
        };
        // An engine's own obstacle that something here can repair is stated below
        // the group, after the shared one; hardware this Mac lacks is only its status.
        let mut issues: Vec<_> = obstacle.into_iter().collect();
        for issue in engines.iter().filter_map(|engine| self.engine_issue(*engine)) {
            if issue.installable() && !issues.contains(&issue) {
                issues.push(issue);
            }
        }
        let engine_item = |provider: Option<AsrProvider>| {
            let item = MaterialMenuItem::new(match provider {
                Some(_) => provider_label(provider),
                None => course2md::tr!("自动选择引擎"),
            })
            .leading(Icon::from(engine_icon(provider)))
            .selected(selected == provider);
            item.on_click(cx.listener(move |this, _, _, cx| {
                this.apply_setup_provider_choice(ProviderChoice::Engine(provider), cx);
            }))
        };
        let engine_menu = MaterialMenu::new("setup-engine-items")
            .item(engine_item(None).supporting_text(automatic.clone()))
            .items(engines.iter().map(|engine| {
                let item = engine_item(Some(*engine));
                if !detected {
                    return item.supporting_text(course2md::tr!("正在检测"));
                }
                match self.engine_issue(*engine) {
                    Some(issue) => item.supporting_text(issue.short()).disabled(true),
                    None => item,
                }
            }));
        let engine_value = match selected {
            None => course2md::tr!("自动选择引擎"),
            provider => provider_label(provider),
        };

        let provider = selected.unwrap_or(recommended);
        let models = local_model_choices(provider, &self.onboarding.model);
        let current_model = models
            .iter()
            .find(|choice| choice.id == self.onboarding.model)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| self.onboarding.model.clone());
        let chosen = self.onboarding.model.clone();
        let model_menu = MaterialMenu::new("setup-model-items").items(models.into_iter().map(
            |ModelChoice {
                 id,
                 label,
                 description,
             }| {
                let selected = id == chosen;
                let item = MaterialMenuItem::new(label);
                if description.is_empty() {
                    item
                } else {
                    item.supporting_text(description)
                }
                .selected(selected)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_setup_model(&id, cx);
                    }))
            },
        ));
        let (_, model_status) = self.model_reading(&self.setup_model_request());
        let fields = ui::field_table(
            "setup-local-engine",
            course2md::tr!("本机引擎和模型"),
            [
                ui::Field::menu(
                    "setup-engine",
                    course2md::tr!("本机引擎"),
                    engine_value,
                    engine_menu,
                )
                .hint(if selected.is_none() {
                    automatic
                } else {
                    String::new()
                }),
                ui::Field::menu(
                    "setup-model",
                    course2md::tr!("识别模型"),
                    current_model,
                    model_menu,
                )
                .hint(model_status),
            ],
        );
        body.child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(self.setup_reveal("setup-engine-reveal", fields))
                .children(issues.into_iter().map(|issue| {
                    div().px(ITEM_INSET).child(self.engine_obstacle(
                        &format!("setup-engine-obstacle-{issue:?}"),
                        issue,
                        cx,
                    ))
                })),
        )
    }

    /// One labeled input of a service form, with its error or kept-key note below.
    fn setup_field(
        &self,
        purpose: ServicePurpose,
        field: FormField,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> RevealFocus {
        let service = self.onboarding.service(purpose);
        let form = &service.form;
        let id = format!("setup-{}-{field:?}", purpose as usize);
        let error = form.error(field);
        let kept = field == FormField::Key && form.draft.credential.is_some();
        let input = TextField::new(SharedString::from(id.clone()), form.input(field))
            .outlined()
            .label(label)
            // The input owns its placeholder: the label stays clear of it.
            .float_label(true)
            .w_full()
            .disabled(service.busy())
            .error(error.is_some())
            .when_some(
                error
                    .map(str::to_owned)
                    .or(kept.then(|| course2md::tr!("已保存密钥，留空则保留").to_owned())),
                |input, text| input.supporting_text(text),
            )
            .when(field == FormField::Key, |input| {
                input.trailing_icon(self.service_key_visibility(
                    FormOwner::Setup(purpose),
                    format!("setup-key-visibility-{}", purpose as usize),
                    cx,
                ))
            });
        self.setup_reveal(SharedString::from(format!("{id}-reveal")), input)
    }

    fn setup_service_content(
        &self,
        purpose: ServicePurpose,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let owner = FormOwner::Setup(purpose);
        let service = self.onboarding.service(purpose);
        let draft = &service.form.draft;
        let busy = service.busy();
        let suffix = purpose as usize;
        let codex = draft.protocol == ServiceProtocol::CodexResponses;
        let keyless = draft.protocol.keyless();
        let form = setup_tile()
            .gap(rem(20.))
            .child(
                self.setup_reveal(
                    SharedString::from(format!("setup-protocol-reveal-{suffix}")),
                    self.service_type_field(
                        owner,
                        SharedString::from(format!("setup-protocol-{suffix}")),
                        busy,
                        cx,
                    )
                    .label(course2md::tr!("服务类型")),
                ),
            )
            .when(!codex, |form| {
                form.child(self.setup_field(
                    purpose,
                    FormField::Address,
                    course2md::tr!("服务地址"),
                    cx,
                ))
            })
            .when(!keyless, |form| {
                form.child(self.setup_field(purpose, FormField::Key, "API Key", cx))
            })
            .when(codex, |form| {
                form.child(self.codex_account_section(CodexSurface::Onboarding, busy, cx))
            })
            .child(
                self.setup_reveal(
                    SharedString::from(format!("setup-model-reveal-{suffix}")),
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap(rem(4.))
                        .child(TileScale::Regular.support(
                            SharedString::from(format!("setup-model-label-{suffix}")),
                            course2md::tr!("模型"),
                        ))
                        .child(self.service_model_field(
                            owner,
                            if purpose == ServicePurpose::Ai {
                                "setup-ai-model"
                            } else {
                                "setup-speech-model"
                            },
                            busy,
                            cx,
                        )),
                ),
            );
        let uses = (purpose == ServicePurpose::Ai).then(|| {
            let (proofread, summary) = (self.onboarding.ai_proofread, self.onboarding.ai_summary);
            v_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .child(ui::field_table(
                    "setup-ai-uses",
                    course2md::tr!("AI 用途"),
                    [
                        ui::Field::switch(
                            "setup-ai-proofread",
                            course2md::tr!("自动校对文字"),
                            proofread,
                            cx.listener(|this, on: &bool, _, cx| {
                                this.onboarding.ai_proofread = *on;
                                cx.notify();
                            }),
                        )
                        .disabled(busy),
                        ui::Field::switch(
                            "setup-ai-summary",
                            course2md::tr!("生成课程摘要"),
                            summary,
                            cx.listener(|this, on: &bool, _, cx| {
                                this.onboarding.ai_summary = *on;
                                cx.notify();
                            }),
                        )
                        .disabled(busy),
                    ],
                ))
                .child(div().px(ITEM_INSET).child(supporting_info(
                    "setup-ai-use-scope",
                    course2md::tr!("开启后转录文字会发送到所选 AI 服务"),
                )))
        });
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(24.))
            .child(form)
            .children(uses)
            .children(self.setup_check_status(purpose, cx))
    }

    /// The running check, or what the last one found, with its details.
    fn setup_check_status(&self, purpose: ServicePurpose, cx: &mut Context<Self>) -> Option<Div> {
        let service = self.onboarding.service(purpose);
        let suffix = purpose as usize;
        let id = |part: &str| SharedString::from(format!("setup-check-{part}-{suffix}"));
        let view = v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .px(ITEM_INSET);
        if let Some(cancel) = &service.running {
            let stopping = cancel.load(Ordering::Acquire);
            let status = AsyncStatus::Busy(
                if stopping {
                    course2md::tr!("正在停止检查")
                } else {
                    course2md::tr!("正在检查配置")
                }
                .into(),
            );
            return Some(view.children(ui::async_status(id("running"), &status, None, cx)).child(
                h_flex().child(
                    material_button(id("stop"), ButtonVariant::Text)
                        .icon(symbols::close())
                        .child(course2md::tr!("停止检查"))
                        .disabled(stopping)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.onboarding.service(purpose).cancel();
                            cx.notify();
                        })),
                ),
            ));
        }
        if service.evidence.is_empty() {
            return None;
        }
        let results = service.evidence.iter().enumerate().filter_map(|(index, (kind, evidence))| {
            let status = if evidence.outcome == TestOutcome::Passed {
                AsyncStatus::Success(course2md::tr!("{}测试通过", kind.label()).into())
            } else {
                AsyncStatus::Failed(evidence.message.clone().into())
            };
            ui::async_status(id(&format!("result-{index}")), &status, None, cx)
        });
        let results: Vec<_> = results.collect();
        let details = service
            .evidence
            .iter()
            .flat_map(|(_, evidence)| evidence.details.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        let open = service.details_open;
        let view = view
            .children(results)
            .child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .gap(rem(8.))
                    .when(self.setup_service_passed(purpose, cx), |row| {
                        row.child(
                            material_button(id("recheck"), ButtonVariant::Text)
                                .icon(symbols::refresh())
                                .child(course2md::tr!("重新检查"))
                                .disabled(service.busy())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_setup_test(purpose, window, cx)
                                })),
                        )
                    })
                    .child(
                        material_button(id("details"), ButtonVariant::Text)
                            .icon(if open {
                                symbols::keyboard_arrow_up()
                            } else {
                                symbols::info()
                            })
                            .child(if open {
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
                id("details-content"),
                open,
                v_flex()
                    .w_full()
                    .min_w_0()
                    .child(TileScale::Regular.support(id("details-text"), details)),
            ));
        Some(view)
    }

    fn setup_model_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let request = self.setup_model_request();
        if request.provider == AsrProvider::Api {
            let version = self
                .preferences
                .default_refs()
                .asr
                .and_then(|id| self.preferences.version(&id))
                .cloned();
            return setup_tile().child(tile_info(
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
        setup_tile().child(self.model_surface(&request, true, window, cx))
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

    /// Which service the forward action targets, on a step that has one.
    fn footer_service_purpose(&self) -> Option<ServicePurpose> {
        match self.onboarding.step {
            Step::Engine if self.onboarding.provider == Some(AsrProvider::Api) => {
                Some(ServicePurpose::Speech)
            }
            Step::Ai => Some(ServicePurpose::Ai),
            _ => None,
        }
    }

    fn setup_footer(&self, cx: &mut Context<Self>) -> Div {
        let row = || {
            h_flex()
                .w_full()
                .min_w_0()
                .gap(rem(8.))
                .items_center()
                .flex_wrap()
        };
        let footer = v_flex().w_full().flex_shrink_0().gap(rem(8.));
        if self.onboarding.finish_failed {
            return footer
                .child(
                    row()
                        .child(
                            material_button("setup-enter-temporarily", ButtonVariant::Text)
                                .child(course2md::tr!("暂时进入应用"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.leave_onboarding(window, cx)
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            material_button("setup-retry-finish", ButtonVariant::Filled)
                                .child(course2md::tr!("重试保存"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.finish_onboarding(window, cx)
                                })),
                        ),
                )
                .child(supporting_info(
                    "setup-finish-failure-hint",
                    course2md::tr!("暂时进入不会保存未完成的选择"),
                ));
        }
        let step = self.onboarding.step;
        let model_review = self.onboarding.model_return_page.is_some();
        let model_running = self.setup_model_job_running();
        // While the model still needs downloading, that is the step's main action.
        let prepare_first = step == Step::Model && {
            let request = self.setup_model_request();
            request.provider != AsrProvider::Api
                && !model_running
                && self.model_awaits_download(&request)
        };
        let purpose = self.footer_service_purpose();
        let service_action = purpose.map(|purpose| self.setup_service_action(purpose, cx));
        let busy = purpose.is_some_and(|purpose| {
            let service = self.onboarding.service(purpose);
            service.running.is_some() || service.vault_busy
        });
        let label = match (service_action, step) {
            (Some(action), _) => action.label(),
            (_, Step::Account) if self.account_connected() => course2md::tr!("继续"),
            (_, Step::Account) => course2md::tr!("跳过登录"),
            (_, Step::Model) if model_review => course2md::tr!("返回应用"),
            (_, Step::Model) if model_running => course2md::tr!("继续使用"),
            (_, Step::Model) if prepare_first => course2md::tr!("稍后准备"),
            (_, Step::Model) => course2md::tr!("开始使用"),
            _ => course2md::tr!("保存并继续"),
        };
        let variant = if prepare_first {
            ButtonVariant::Outlined
        } else {
            ButtonVariant::Filled
        };
        footer
            // The cost notice appears only with the action that sends the sample.
            .when(
                service_action.is_some_and(ServiceAction::sends_sample),
                |footer| {
                    footer.child(supporting_info(
                        "setup-test-notice",
                        service_test::test_notice(),
                    ))
                },
            )
            .child(
                row()
                    .when(step == Step::Engine, |row| {
                        // Skipping is not the way forward, so it carries no arrow.
                        row.child(
                            material_button("setup-later", ButtonVariant::Text)
                                .child(course2md::tr!("跳过设置"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.skip_setup(window, cx)
                                })),
                        )
                    })
                    .when(step != Step::Engine && !model_review, |row| {
                        row.child(
                            material_button("setup-back", ButtonVariant::Text)
                                .icon(symbols::arrow_back())
                                .child(course2md::tr!("上一步"))
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
                        )
                    })
                    .child(div().flex_1())
                    .when(step == Step::Ai, |row| {
                        row.child(
                            material_button("setup-skip-ai", ButtonVariant::Outlined)
                                .child(course2md::tr!("稍后配置 AI"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    // Keep this private edit valid when the person returns
                                    // within the guide; finishing it discards the draft.
                                    this.onboarding.ai.cancel();
                                    this.setup_step(Step::Account, cx);
                                })),
                        )
                    })
                    .child(
                        material_button("setup-next", variant)
                            .icon(symbols::arrow_forward())
                            .child(label)
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| match step {
                                Step::Engine
                                    if this.onboarding.provider == Some(AsrProvider::Api) =>
                                {
                                    this.continue_setup_service(ServicePurpose::Speech, window, cx)
                                }
                                Step::Engine => {
                                    if this.save_setup_engine(cx) {
                                        this.setup_step(Step::Ai, cx);
                                    }
                                }
                                Step::Ai => {
                                    this.continue_setup_service(ServicePurpose::Ai, window, cx)
                                }
                                Step::Account => this.setup_step(Step::Model, cx),
                                Step::Model if model_review => this.leave_onboarding(window, cx),
                                Step::Model => this.finish_onboarding(window, cx),
                            })),
                    ),
            )
            .when(model_running, |footer| {
                footer.child(supporting_info(
                    "setup-return-hint",
                    course2md::tr!("模型会继续在后台准备"),
                ))
            })
    }

    /// After the guide closes: the model it started preparing, until dismissed.
    pub(crate) fn onboarding_background_notice(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if self.onboarding.active {
            return None;
        }
        let request = self.onboarding.background_model.as_ref()?;
        let running = self.is_preparing(request);
        let outcome = self.model_outcome(request);
        let label = if running && self.cancelling {
            course2md::tr!("正在暂停").to_owned()
        } else if running {
            self.job_events
                .stages
                .iter()
                .find(|(stage, progress)| {
                    stage.is_model_transfer() && progress.has_samples() && !progress.done
                })
                .map(|(stage, progress)| {
                    let phase = activity::model_transfer_phase(stage, progress.phase);
                    progress
                        .fraction()
                        .map(|fraction| format!("{phase} · {:.0}%", fraction.clamp(0., 1.) * 100.))
                        .unwrap_or(phase)
                })
                .unwrap_or_else(|| course2md::tr!("正在准备").into())
        } else {
            // Another preparation replaced this one's result: nothing left to report.
            let outcome = outcome?;
            if outcome.cancelled {
                course2md::tr!("准备已暂停").into()
            } else if outcome.error {
                course2md::tr!("准备未完成").into()
            } else {
                course2md::tr!("已准备完成").into()
            }
        };
        let failed = !running && outcome.is_some_and(|outcome| outcome.error);
        Some(
            notice_banner(
                "setup-background-model",
                if failed {
                    NoticeKind::Danger
                } else {
                    NoticeKind::Info
                },
                format!("{} · {label}", model_label(&request.model)),
            )
            .child(
                material_button("setup-background-model-open", ButtonVariant::Text)
                    .child(if running {
                        course2md::tr!("查看进度")
                    } else {
                        course2md::tr!("查看结果")
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_setup_model_status(window, cx)
                    })),
            )
            .when(!running, |banner| {
                banner.child(
                    material_button("setup-background-model-dismiss", ButtonVariant::Text)
                        .child(course2md::tr!("关闭提示"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.onboarding.background_model = None;
                            cx.notify();
                        })),
                )
            }),
        )
    }
}

/// A tile of the guide on the canvas ground.
fn setup_tile() -> Div {
    TileScale::Regular
        .surface([true; 4])
        .gap(rem(12.))
        .p(ITEM_INSET)
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
