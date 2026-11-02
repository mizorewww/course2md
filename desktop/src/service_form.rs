//! The service connection form shared by the settings editor and first-run setup:
//! draft, inputs, field errors, model discovery and key visibility. Each surface
//! lays the controls out in its own form; behavior and choices live here once.
use super::*;
use crate::credentials::CredentialRef;
use crate::model_discovery;
use crate::preferences::{BindingScope, FieldError, ServiceTestEvidence};
use desktop_accounts::codex::CodexSurface;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FormField {
    Name,
    Address,
    Model,
    Key,
}

impl FormField {
    /// The `FieldError::field` reported for this input.
    fn error_key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Address => "address",
            Self::Model => "model",
            Self::Key => "api_key",
        }
    }
    fn from_error_key(key: &str) -> Self {
        match key {
            "name" => Self::Name,
            "address" => Self::Address,
            "model" => Self::Model,
            _ => Self::Key,
        }
    }
}

/// Where a form lives. Asynchronous results land only on the form that asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormOwner {
    Editor,
    Setup(ServicePurpose),
}

impl FormOwner {
    fn index(self) -> usize {
        match self {
            Self::Editor => 0,
            Self::Setup(purpose) => 1 + purpose as usize,
        }
    }
    fn codex_surface(self) -> CodexSurface {
        match self {
            Self::Editor => CodexSurface::Editor,
            Self::Setup(_) => CodexSurface::Onboarding,
        }
    }
}

pub(crate) struct ServiceForm {
    inputs: BTreeMap<FormField, Entity<InputState>>,
    pub(crate) draft: ServiceDraft,
    pub(crate) errors: Vec<FieldError>,
    pub(crate) models: model_discovery::State,
    pub(crate) show_key: bool,
    /// A key is being stored in, or checked against, the system vault.
    pub(crate) vault_busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl ServiceForm {
    pub(crate) fn new(
        owner: FormOwner,
        fields: &[FormField],
        draft: ServiceDraft,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> Self {
        let inputs: BTreeMap<_, _> = fields
            .iter()
            .map(|&field| {
                let input =
                    cx.new(|cx| InputState::new(window, cx).masked(field == FormField::Key));
                (field, input)
            })
            .collect();
        let subscriptions = inputs
            .iter()
            .map(|(&field, input)| {
                cx.subscribe_in(input, window, move |this, _, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.service_form_input_changed(owner, field, cx);
                    }
                })
            })
            .collect();
        let mut form = Self {
            inputs,
            draft: ServiceDraft::new(draft.protocol.purpose()),
            errors: Vec::new(),
            models: Default::default(),
            show_key: false,
            vault_busy: false,
            _subscriptions: subscriptions,
        };
        form.load(draft, window, cx);
        form
    }

    pub(crate) fn input(&self, field: FormField) -> &Entity<InputState> {
        &self.inputs[&field]
    }
    pub(crate) fn value(&self, field: FormField, cx: &App) -> String {
        self.inputs
            .get(&field)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }
    /// The key typed in this session; empty keeps the saved one.
    pub(crate) fn typed_key(&self, cx: &App) -> String {
        self.value(FormField::Key, cx)
    }
    /// The draft with what the inputs show now. Inputs set programmatically emit no
    /// change event, so the inputs, not the draft, are authoritative for text.
    pub(crate) fn current_draft(&self, cx: &App) -> ServiceDraft {
        let mut draft = self.draft.clone();
        if self.inputs.contains_key(&FormField::Name) {
            draft.name = self.value(FormField::Name, cx);
        }
        draft.address = self.value(FormField::Address, cx);
        draft.model = self.value(FormField::Model, cx);
        draft.authentication = self.authentication(cx);
        draft
    }

    /// The result was for what the form shows now, with no new key typed.
    pub(crate) fn fits(&self, evidence: &ServiceTestEvidence, cx: &App) -> bool {
        self.typed_key(cx).is_empty()
            && self
                .current_draft(cx)
                .configuration()
                .is_ok_and(|config| config.fingerprint(&evidence.contract) == evidence.fingerprint)
    }

    /// A service is called with a key exactly when it has one: a saved key or
    /// one typed now. Keyless types never send one.
    fn authentication(&self, cx: &App) -> Authentication {
        if !self.draft.protocol.keyless()
            && (self.draft.credential.is_some() || !self.typed_key(cx).trim().is_empty())
        {
            Authentication::ApiKey
        } else {
            Authentication::None
        }
    }

    /// Shows `draft` with an empty, masked key field.
    pub(crate) fn load(&mut self, draft: ServiceDraft, window: &mut Window, cx: &mut App) {
        for (field, value) in [
            (FormField::Name, draft.name.clone()),
            (FormField::Address, draft.address.clone()),
            (FormField::Model, draft.model.clone()),
            (FormField::Key, String::new()),
        ] {
            if let Some(input) = self.inputs.get(&field) {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        self.inputs[&FormField::Key].update(cx, |input, cx| input.set_masked(true, window, cx));
        self.draft = draft;
        self.errors.clear();
        self.models.invalidate();
        self.show_key = false;
        self.vault_busy = false;
    }
    pub(crate) fn clear_key(&self, window: &mut Window, cx: &mut App) {
        self.inputs[&FormField::Key].update(cx, |input, cx| input.set_value("", window, cx));
    }
    pub(crate) fn focus(&self, field: FormField, window: &mut Window, cx: &mut App) {
        if let Some(input) = self.inputs.get(&field) {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    pub(crate) fn error(&self, field: FormField) -> Option<&str> {
        self.errors
            .iter()
            .find(|error| error.field == field.error_key())
            .map(|error| error.message.as_str())
    }
    /// Validates the staged draft and focuses the first invalid field.
    pub(crate) fn validate(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.draft.authentication = self.authentication(cx);
        self.errors = self.draft.validate();
        match self.errors.first() {
            Some(error) => {
                self.focus(FormField::from_error_key(error.field), window, cx);
                false
            }
            None => true,
        }
    }

    pub(crate) fn is_codex(&self) -> bool {
        self.draft.protocol == ServiceProtocol::CodexResponses
    }

    /// A keyless type drops authentication; an AI type change drops the model,
    /// which does not carry across services. A name derived from the old address
    /// follows the new type, and fixed Codex/Ollama addresses are filled or cleared.
    fn switch_protocol(&mut self, protocol: ServiceProtocol, window: &mut Window, cx: &mut App) {
        let current = self.value(FormField::Address, cx);
        let current = current.trim().trim_end_matches('/').to_owned();
        let name = self.current_draft(cx).name;
        let previous_host = preferences::service_host(&current, self.draft.protocol);
        // A name the app supplied follows the type: an account or local service
        // is named after its type, any other after its address once saved.
        let derived = name.trim().is_empty()
            || Some(name.trim()) == previous_host.as_deref()
            || name.trim() == self.draft.protocol.choice_label();
        if derived {
            let name = if protocol.keyless() {
                protocol.choice_label()
            } else {
                ""
            };
            self.draft.name = name.into();
            if let Some(input) = self.inputs.get(&FormField::Name) {
                input.update(cx, |input, cx| input.set_value(name, window, cx));
            }
        }
        let kind_changed =
            protocol.purpose() == ServicePurpose::Ai && self.draft.protocol != protocol;
        self.draft.protocol = protocol;
        if kind_changed {
            self.draft.model.clear();
            self.inputs[&FormField::Model].update(cx, |input, cx| input.set_value("", window, cx));
        }
        if protocol.keyless() {
            self.draft.authentication = Authentication::None;
            self.clear_key(window, cx);
        }
        let codex_address = [
            preferences::CODEX_ADDRESS,
            course2md::provider::CODEX_RESPONSES_URL,
        ]
        .contains(&current.as_str());
        let address = match protocol {
            ServiceProtocol::CodexResponses => Some(preferences::CODEX_ADDRESS),
            // Ollama's address comes from detecting the local service.
            // The fixed Codex address is not the user's; other input stays.
            _ if codex_address => Some(""),
            _ => None,
        };
        if let Some(address) = address {
            self.inputs[&FormField::Address]
                .update(cx, |input, cx| input.set_value(address, window, cx));
            self.draft.address = address.into();
        }
        self.errors.clear();
        self.models.invalidate();
    }
}

pub(crate) fn protocol_icon(protocol: ServiceProtocol) -> SymbolIcon {
    match protocol {
        ServiceProtocol::SpeechTranscriptions => symbols::mic(),
        ServiceProtocol::SpeechChat => symbols::article(),
        ServiceProtocol::AiChat => symbols::cloud(),
        ServiceProtocol::OllamaChat => symbols::dns(),
        ServiceProtocol::CodexResponses => symbols::auto_awesome(),
    }
}

impl Desktop {
    pub(crate) fn service_form(&self, owner: FormOwner) -> Option<&ServiceForm> {
        match owner {
            // The closed editor still draws while its dialog plays the exit.
            FormOwner::Editor => self
                .settings_ui
                .editor
                .as_ref()
                .or(self.settings_ui.closed_editor.as_ref())
                .map(|editor| &editor.form),
            FormOwner::Setup(purpose) => self.setup_form(purpose),
        }
    }
    pub(crate) fn service_form_mut(&mut self, owner: FormOwner) -> Option<&mut ServiceForm> {
        match owner {
            FormOwner::Editor => self
                .settings_ui
                .editor
                .as_mut()
                .map(|editor| &mut editor.form),
            FormOwner::Setup(purpose) => self.setup_form_mut(purpose),
        }
    }

    fn form_of_draft(&mut self, owner: FormOwner, id: &str) -> Option<&mut ServiceForm> {
        self.service_form_mut(owner)
            .filter(|form| form.draft.id == id)
    }

    /// Stores a secret in the vault off the UI thread and commits `draft` with it. If the
    /// form moved to another draft or closed meanwhile, or the commit fails, nothing
    /// refers to the stored key.
    pub(crate) fn store_form_secret(
        &mut self,
        owner: FormOwner,
        draft: ServiceDraft,
        secret: Secret,
        commit: impl FnOnce(&mut Self, ServiceDraft, CredentialRef) -> Result<ServiceDraft> + 'static,
        then: impl FnOnce(&mut Self, Result<ServiceDraft>, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let id = draft.id.clone();
        if let Some(form) = self.service_form_mut(owner) {
            form.vault_busy = true;
        }
        self.run_vault_step(
            cx,
            move |vault| preferences::Store::insert_credential(vault, secret),
            move |this, stored, window, cx| {
                let Some(form) = this.form_of_draft(owner, &id) else {
                    if let Ok(reference) = stored {
                        this.forget_credentials(vec![reference]);
                    }
                    return;
                };
                form.vault_busy = false;
                let committed = stored.and_then(|reference| {
                    commit(this, draft, reference.clone())
                        .inspect_err(|_| this.forget_credentials(vec![reference]))
                });
                then(this, committed, window, cx);
            },
        );
    }

    /// Publishes the form's committed draft. Publication first confirms the draft's
    /// credential is in the vault, which can block; only that check leaves the UI thread.
    pub(crate) fn publish_form_draft(
        &mut self,
        owner: FormOwner,
        scope: BindingScope,
        then: impl FnOnce(&mut Self, Result<ServiceVersion>, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.service_form(owner).map(|form| form.draft.id.clone()) else {
            return;
        };
        let reference = match self.preferences.publication_credential(&id) {
            Ok(reference) => reference,
            Err(error) => return then(self, Err(error), window, cx),
        };
        if let Some(form) = self.service_form_mut(owner) {
            form.vault_busy = true;
        }
        self.run_vault_step(
            cx,
            move |vault| preferences::Store::verify_credential(vault, reference),
            move |this, verified, window, cx| {
                let Some(form) = this.form_of_draft(owner, &id) else {
                    return;
                };
                form.vault_busy = false;
                let published = verified
                    .and_then(|verified| this.preferences.commit_publication(&id, scope, verified));
                then(this, published, window, cx);
            },
        );
        cx.notify();
    }

    /// An edit invalidates what the surface derived from the old values.
    /// A name change does not affect test results.
    fn service_form_edited(&mut self, owner: FormOwner, affects_tests: bool) {
        match owner {
            FormOwner::Editor => {
                if let Some(editor) = &mut self.settings_ui.editor {
                    editor.saved_configuration = None;
                    editor.status = None;
                    if affects_tests {
                        editor.evidence = None;
                    }
                }
            }
            FormOwner::Setup(purpose) => self.setup_service_edited(purpose),
        }
    }

    fn service_form_input_changed(
        &mut self,
        owner: FormOwner,
        field: FormField,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.service_form_mut(owner) else {
            return;
        };
        form.errors.clear();
        if matches!(field, FormField::Address | FormField::Key) {
            form.models.invalidate();
        }
        self.service_form_edited(owner, field != FormField::Name);
        cx.notify();
    }

    fn switch_service_protocol(
        &mut self,
        owner: FormOwner,
        protocol: ServiceProtocol,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.service_form_mut(owner) else {
            return;
        };
        form.switch_protocol(protocol, window, cx);
        self.service_form_edited(owner, true);
        match protocol {
            ServiceProtocol::CodexResponses => self.codex_refresh_status(owner.codex_surface(), cx),
            ServiceProtocol::OllamaChat => self.ollama_prepare_form(cx),
            _ => {}
        }
        cx.notify();
    }

    /// The service type: a few choices that each need a word of explanation, so they
    /// stand as tiles rather than behind a menu.
    pub(crate) fn service_type_field(
        &self,
        owner: FormOwner,
        id: impl Into<ElementId>,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> ChoiceGroup {
        let current = self
            .service_form(owner)
            .map_or(ServiceProtocol::AiChat, |form| form.draft.protocol);
        let protocols: Vec<_> = ServiceProtocol::for_purpose(current.purpose()).collect();
        let count = protocols.len();
        let id = id.into();
        let group = protocols.iter().enumerate().fold(
            ChoiceGroup::new(id.clone(), course2md::tr!("服务类型")),
            |group, (index, protocol)| {
                let detail = match (&self.ollama.status_line(), protocol) {
                    (Some(line), ServiceProtocol::OllamaChat) => line.clone().into(),
                    _ => SharedString::from(protocol.choice_detail()),
                };
                group.tile(
                    ChoiceTile::new(
                        (id.clone(), protocol.choice_label()),
                        index,
                        count,
                        protocol.choice_label(),
                        *protocol == current,
                    )
                    .icon(Icon::from(protocol_icon(*protocol)).size(rem(24.)))
                    .description(detail)
                    .enabled(!disabled),
                )
            },
        );
        group.on_change(cx.listener(move |this, index: &usize, window, cx| {
            this.switch_service_protocol(owner, protocols[*index], window, cx);
        }))
    }

    /// A text input of a service form: labelled, with its error or `hint` below.
    /// While `locked` the editor's value can be selected but not changed, and
    /// setup's is disabled; setup's inputs show no placeholder.
    pub(crate) fn service_text_field(
        &self,
        owner: FormOwner,
        field: FormField,
        label: &'static str,
        hint: Option<String>,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> Option<TextField> {
        let form = self.service_form(owner)?;
        let error = form.error(field).map(str::to_owned);
        let saved_key = field == FormField::Key && form.draft.credential.is_some();
        let placeholder = match field {
            FormField::Address => "https://api.example.com/v1",
            FormField::Name => course2md::tr!("例如：我的 API"),
            _ if saved_key => "••••••••",
            _ => "",
        };
        let trailing = (field == FormField::Key)
            .then(|| self.service_key_visibility(owner, ("service-key", owner.index()), cx));
        let id = ElementId::NamedChild(
            Arc::new(ElementId::from(("service-field", owner.index()))),
            field.error_key().into(),
        );
        let support = error.clone().or(hint);
        Some(
            TextField::new(id, form.input(field))
                .outlined()
                .shape(TextFieldShape::Rounded)
                .w_full()
                .label(label)
                .float_label(true)
                .map(|field| match owner {
                    FormOwner::Editor => field.placeholder(placeholder).read_only(locked),
                    FormOwner::Setup(_) => field.disabled(locked),
                })
                .error(error.is_some())
                .when_some(support, |field, support| field.supporting_text(support))
                .when_some(trailing, |field, trailing| field.trailing_icon(trailing)),
        )
    }

    /// The trailing eye of the key input: it shows or hides the key inside the field.
    pub(crate) fn service_key_visibility(
        &self,
        owner: FormOwner,
        id: impl Into<ElementId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = id.into();
        let shown = self.service_form(owner).is_some_and(|form| form.show_key);
        let label = if shown {
            course2md::tr!("隐藏密钥")
        } else {
            course2md::tr!("显示密钥")
        };
        MaterialTooltip::plain((id.clone(), "tip"), label)
            .trigger(
                IconButton::new(
                    (id, "button"),
                    if shown {
                        symbols::visibility_off()
                    } else {
                        symbols::visibility()
                    },
                    label,
                )
                .button_size(ButtonSize::Small)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(form) = this.service_form_mut(owner) {
                        form.show_key = !form.show_key;
                        let masked = !form.show_key;
                        form.inputs[&FormField::Key]
                            .update(cx, |input, cx| input.set_masked(masked, window, cx));
                    }
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    /// The model input with its discovery and field error. The input remains editable
    /// in every discovery state; candidates list below it, however many a service returns.
    pub(crate) fn service_model_field(
        &self,
        owner: FormOwner,
        id: &'static str,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        use model_discovery::ModelFieldEvent::{self, *};
        let Some(form) = self.service_form(owner) else {
            return div();
        };
        let (state, input, error) = (
            &form.models,
            form.input(FormField::Model),
            form.error(FormField::Model),
        );
        let child = |name: &str| SharedString::from(format!("{id}-{name}"));
        let this = cx.weak_entity();
        let send = |event: ModelFieldEvent| {
            let this = this.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| {
                    this.service_model_event(owner, event.clone(), window, cx)
                });
            }
        };
        let fetch_label = if matches!(state.status(), model_discovery::Status::Ready(_)) {
            course2md::tr!("刷新模型列表")
        } else {
            course2md::tr!("获取模型列表")
        };
        let trailing = if state.loading() {
            icon_spinner(child("fetching"), fetch_label, rem(20.), cx)
        } else {
            MaterialTooltip::plain(child("fetch-tip"), fetch_label)
                .trigger(
                    IconButton::new(child("fetch"), symbols::refresh(), fetch_label)
                        .button_size(ButtonSize::Small)
                        .disabled(disabled)
                        .on_click(send(Fetch)),
                )
                .into_any_element()
        };
        let field = TextField::new(child("input"), input)
            .outlined()
            .shape(TextFieldShape::Rounded)
            .w_full()
            .label(course2md::tr!("模型"))
            .float_label(true)
            .placeholder(course2md::tr!("模型 ID，或从服务获取列表"))
            .disabled(disabled)
            .trailing_icon(trailing)
            .error(error.is_some())
            .when_some(error, |field, error| {
                field.supporting_text(error.to_owned())
            });
        let status = match state.status() {
            // Idle needs no words: the field and its button say what can be done.
            model_discovery::Status::Idle => ui::AsyncStatus::Idle,
            model_discovery::Status::Loading => {
                ui::AsyncStatus::Busy(course2md::tr!("正在获取模型列表…").into())
            }
            model_discovery::Status::Ready(models) if models.is_empty() => {
                ui::AsyncStatus::Warning(course2md::tr!("服务未返回模型").into())
            }
            model_discovery::Status::Ready(models) => {
                ui::AsyncStatus::Success(course2md::tr!("找到 {} 个模型", models.len()).into())
            }
            model_discovery::Status::Failed(error) => {
                ui::AsyncStatus::Failed(error.message().into())
            }
        };
        let toggle = (!state.models().is_empty()).then(|| {
            material_button(child("choose"), ButtonVariant::Text)
                .child(if state.choices_open {
                    course2md::tr!("收起模型列表")
                } else {
                    course2md::tr!("选择模型")
                })
                .disabled(disabled)
                .on_click(send(ToggleChoices))
                .into_any_element()
        });
        let choices = (state.choices_open && !disabled && !state.models().is_empty()).then(|| {
            let models = state.models().to_vec();
            let count = models.len();
            let selected = input.read(cx).value().to_string();
            let this = this.clone();
            uniform_list(child("choices"), count, move |range, _, _| {
                range
                    .map(|index| {
                        let (model, this) = (models[index].clone(), this.clone());
                        ListItem::new(("model-choice", index), model.clone())
                            .density(ListDensity::Compact)
                            .selected(model == selected)
                            .on_click(move |_, window, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.service_model_event(
                                        owner,
                                        Choose(model.clone()),
                                        window,
                                        cx,
                                    )
                                });
                            })
                    })
                    .collect()
            })
            .h(rem(200.))
            .w_full()
        });
        v_flex()
            .w_full()
            .min_w_0()
            .gap(rem(8.))
            .child(field)
            .children(ui::async_status(child("status"), &status, toggle, cx))
            .children(choices)
    }

    fn service_model_event(
        &mut self,
        owner: FormOwner,
        event: model_discovery::ModelFieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use model_discovery::ModelFieldEvent::*;
        match event {
            Fetch => self.fetch_service_models(owner, cx),
            ToggleChoices => {
                if let Some(form) = self.service_form_mut(owner) {
                    form.models.choices_open = !form.models.choices_open;
                }
            }
            // A value set from here emits no change event, so this is the edit.
            Choose(model) => {
                if let Some(form) = self.service_form_mut(owner) {
                    form.models.choices_open = false;
                    form.errors.clear();
                    form.input(FormField::Model)
                        .update(cx, |input, cx| input.set_value(model, window, cx));
                }
                self.service_form_edited(owner, true);
            }
        }
        cx.notify();
    }

    fn fetch_service_models(&mut self, owner: FormOwner, cx: &mut Context<Self>) {
        let Some(form) = self.service_form(owner) else {
            return;
        };
        if form.is_codex() {
            self.codex_refresh_models(owner.codex_surface(), cx);
            return;
        }
        let request = model_discovery::Request::from_draft(
            &form.current_draft(cx),
            Secret::new(form.typed_key(cx)),
        );
        let vault = self.preferences.vault();
        let Some(form) = self.service_form_mut(owner) else {
            return;
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                form.models.reject(error);
                cx.notify();
                return;
            }
        };
        let ticket = form.models.begin(&request);
        cx.spawn(async move |this, cx| {
            let result = model_discovery::discover(request, vault).await;
            let _ = this.update(cx, |this, cx| {
                let current = this.service_form(owner).and_then(|form| {
                    model_discovery::RequestKey::from_draft(
                        &form.current_draft(cx),
                        &form.typed_key(cx),
                    )
                    .ok()
                });
                // A reloaded or closed form has dropped this ticket. A list the person
                // just asked for shows at once.
                if let Some(form) = this.service_form_mut(owner)
                    && form.models.complete(ticket, current.as_ref(), result)
                {
                    form.models.choices_open = !form.models.models().is_empty();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The form a Codex result belongs to, while it still edits a Codex service.
    pub(crate) fn codex_form(&mut self, surface: CodexSurface) -> Option<&mut ServiceForm> {
        let owner = match surface {
            CodexSurface::Editor => FormOwner::Editor,
            CodexSurface::Onboarding => FormOwner::Setup(ServicePurpose::Ai),
            CodexSurface::Settings => return None,
        };
        self.service_form_mut(owner).filter(|form| form.is_codex())
    }
    pub(crate) fn codex_form_models_failed(&mut self, surface: CodexSurface, message: String) {
        if let Some(form) = self.codex_form(surface) {
            form.models.reject(model_discovery::Error::Custom(message));
        }
    }
}
