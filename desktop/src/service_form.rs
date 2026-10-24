//! The service connection form shared by the settings editor and first-run setup:
//! draft, inputs, field errors, model discovery and key visibility. Each surface
//! lays the controls out in its own form; behavior and choices live here once.
use super::*;
use crate::codex_ui::CodexSurface;
use crate::model_discovery;
use crate::preferences::FieldError;

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
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(match field {
                            FormField::Address => "https://api.example.com/v1",
                            FormField::Name => course2md::tr!("例如：我的 API"),
                            FormField::Model => course2md::tr!("模型 ID，或从服务获取列表"),
                            _ => "",
                        })
                        .masked(field == FormField::Key)
                });
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
        let placeholder = if draft.credential.is_some() {
            "••••••••"
        } else {
            ""
        };
        self.inputs[&FormField::Key].update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_masked(true, window, cx);
        });
        self.draft = draft;
        self.errors.clear();
        self.models.invalidate();
        self.show_key = false;
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

/// A menu entry with a name and a quieter line saying what it means.
pub(crate) fn two_line_item(
    name: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .py(rems(8. / 14.))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .line_height(rems(20. / 14.))
                .child(name.into()),
        )
        .child(
            div()
                .text_size(TEXT_AUX)
                .line_height(rems(16. / 14.))
                .opacity(0.8)
                .whitespace_normal()
                .child(detail.into()),
        )
}

pub(crate) fn protocol_icon(protocol: ServiceProtocol) -> Icon {
    match protocol {
        ServiceProtocol::SpeechTranscriptions => icons::mic(),
        ServiceProtocol::SpeechChat => icons::article(),
        ServiceProtocol::AiChat => icons::cloud(),
        ServiceProtocol::OllamaChat => icons::server(),
        ServiceProtocol::CodexResponses => icons::sparkles(),
    }
}

impl Desktop {
    pub(crate) fn service_form(&self, owner: FormOwner) -> Option<&ServiceForm> {
        match owner {
            FormOwner::Editor => self.settings_ui.editor.as_ref().map(|editor| &editor.form),
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

    /// The service type select for this form's purpose: one menu grouped by
    /// how each type is reached, each entry saying what choosing it means.
    /// Each form labels it the way it labels its other fields.
    pub(crate) fn service_type_field(
        &self,
        owner: FormOwner,
        id: impl Into<ElementId>,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> SelectField {
        let current = self
            .service_form(owner)
            .map_or(ServiceProtocol::AiChat, |form| form.draft.protocol);
        let ollama = self.ollama.status_line();
        let detail = move |protocol: ServiceProtocol| -> SharedString {
            match (&ollama, protocol) {
                (Some(line), ServiceProtocol::OllamaChat) => line.clone().into(),
                _ => protocol.choice_detail().into(),
            }
        };
        let entity = cx.weak_entity();
        SelectField::new(id, current.choice_label(), {
            let detail = detail.clone();
            move |menu, _, _| {
                let mut group = None;
                ServiceProtocol::for_purpose(current.purpose()).fold(menu, |mut menu, protocol| {
                    if group != Some(protocol.choice_group()) {
                        group = Some(protocol.choice_group());
                        menu = menu.label(protocol.choice_group());
                    }
                    let entity = entity.clone();
                    let detail = detail(protocol);
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            two_line_item(protocol.choice_label(), detail.clone())
                        })
                        .icon(protocol_icon(protocol))
                        .checked(protocol == current)
                        .on_click(move |_, window, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.switch_service_protocol(owner, protocol, window, cx)
                            });
                        }),
                    )
                })
            }
        })
        .icon(protocol_icon(current))
        .detail(detail(current))
        .disabled(disabled)
    }

    /// The trailing eye of the key input: it shows or hides the key inside
    /// the field's own boundary, as the model field's actions do.
    pub(crate) fn service_key_visibility(
        &self,
        owner: FormOwner,
        id: impl Into<ElementId>,
        cx: &mut Context<Self>,
    ) -> Button {
        let shown = self.service_form(owner).is_some_and(|form| form.show_key);
        input_action(id)
            .icon(if shown {
                icons::eye_off()
            } else {
                icons::eye()
            })
            .accessibility_label(if shown {
                course2md::tr!("隐藏密钥")
            } else {
                course2md::tr!("显示密钥")
            })
            .tooltip(if shown {
                course2md::tr!("隐藏密钥")
            } else {
                course2md::tr!("显示密钥")
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(form) = this.service_form_mut(owner) {
                    form.show_key = !form.show_key;
                    let masked = !form.show_key;
                    form.inputs[&FormField::Key]
                        .update(cx, |input, cx| input.set_masked(masked, window, cx));
                }
                cx.notify();
            }))
    }

    /// The model input with its discovery menu and field error.
    pub(crate) fn service_model_field(
        &self,
        owner: FormOwner,
        id: &'static str,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(form) = self.service_form(owner) else {
            return div();
        };
        model_discovery::model_field(
            id,
            form.input(FormField::Model),
            &form.models,
            disabled,
            form.error(FormField::Model),
            cx.listener(move |this, _, _, cx| this.fetch_service_models(owner, cx)),
            cx,
        )
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
                // A reloaded or closed form has dropped this ticket.
                if let Some(form) = this.service_form_mut(owner) {
                    form.models.complete(ticket, current.as_ref(), result);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The form a Codex result belongs to, while it still edits a Codex service.
    fn codex_form(&mut self, surface: CodexSurface) -> Option<&mut ServiceForm> {
        let owner = match surface {
            CodexSurface::Editor => FormOwner::Editor,
            CodexSurface::Onboarding => FormOwner::Setup(ServicePurpose::Ai),
            CodexSurface::Settings => return None,
        };
        self.service_form_mut(owner).filter(|form| form.is_codex())
    }
    pub(crate) fn codex_form_needs_catalog(&mut self, surface: CodexSurface) -> bool {
        self.codex_form(surface)
            .is_some_and(|form| form.models.models().is_empty() && !form.models.loading())
    }
    pub(crate) fn codex_form_models_loading(&mut self, surface: CodexSurface) -> bool {
        self.codex_form(surface)
            .map(|form| form.models.begin_external())
            .is_some()
    }
    pub(crate) fn codex_form_models_failed(&mut self, surface: CodexSurface, message: String) {
        if let Some(form) = self.codex_form(surface) {
            form.models.reject(model_discovery::Error::Custom(message));
        }
    }
    /// Candidates only: a suggestion fills an empty model input and changes nothing saved.
    pub(crate) fn codex_form_catalog(
        &mut self,
        surface: CodexSurface,
        models: Vec<String>,
        suggested: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let window = self.window;
        if let Some(form) = self.codex_form(surface) {
            form.models.prime(models);
            let input = form.input(FormField::Model).clone();
            crate::codex_ui::autofill_input(window, input, suggested, cx);
        }
    }
    pub(crate) fn codex_forms_logged_out(&mut self) {
        for surface in [CodexSurface::Editor, CodexSurface::Onboarding] {
            if let Some(form) = self.codex_form(surface) {
                form.models.invalidate();
            }
        }
    }
}
