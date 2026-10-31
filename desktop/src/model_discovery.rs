//! Read-only OpenAI-compatible model discovery, separate from service contract tests.
//! A listed model is a candidate, never evidence that a processing capability works.

use crate::credentials::{CredentialRef, CredentialVault, Secret};
use crate::preferences::{Authentication, ServiceDraft, ServiceProtocol, normalize_endpoint};
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const MAX_RESPONSE: u64 = 1024 * 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidAddress,
    MissingKey,
    InvalidKey,
    CredentialUnavailable,
    TimedOut,
    Network,
    AuthenticationRefused,
    Unsupported,
    Refused(u16),
    InvalidResponse,
    TooLarge,
    /// A provider-specific failure with an already sanitized, user-readable message.
    Custom(String),
    Cancelled,
}

impl Error {
    pub fn message(&self) -> String {
        match self {
            Self::InvalidAddress => course2md::tr!("请先填写有效的服务地址，再获取模型").into(),
            Self::MissingKey => course2md::tr!("请先填写 API Key，再获取模型").into(),
            Self::InvalidKey => course2md::tr!("API Key 含无效字符，请检查后重试").into(),
            Self::CredentialUnavailable => {
                course2md::tr!("暂时无法读取已保存的 API Key，可重新填写后重试").into()
            }
            Self::TimedOut => course2md::tr!("获取模型超时，可重试").into(),
            Self::Network => course2md::tr!("暂时无法获取模型，请检查地址或网络").into(),
            Self::AuthenticationRefused => {
                course2md::tr!("服务拒绝访问模型列表，请检查认证设置").into()
            }
            Self::Unsupported => course2md::tr!("服务未提供可读取的模型列表").into(),
            Self::Refused(status) => course2md::tr!("暂时无法获取模型（HTTP {}），可重试", status),
            Self::InvalidResponse => course2md::tr!("未能识别服务返回的模型列表").into(),
            Self::TooLarge => course2md::tr!("服务返回的模型列表过大").into(),
            Self::Custom(message) => message.clone(),
            Self::Cancelled => course2md::tr!("已取消获取模型").into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum AuthIdentity {
    None,
    Saved(CredentialRef),
    // Never retain plaintext input in a request identity or diagnostic string.
    Typed(String),
}

#[derive(Clone, PartialEq, Eq)]
pub struct RequestKey {
    endpoint: String,
    protocol: ServiceProtocol,
    auth: AuthIdentity,
}

impl RequestKey {
    /// Reads only public configuration and the currently entered key. The model ID
    /// and display name do not affect a service's model list.
    pub fn from_draft(draft: &ServiceDraft, typed_key: &str) -> Result<Self, Error> {
        let endpoint = models_endpoint(&draft.address, draft.protocol)?;
        let typed_key = typed_key.trim();
        let auth = if draft.authentication == Authentication::None {
            AuthIdentity::None
        } else if !typed_key.is_empty() {
            if typed_key.chars().any(char::is_control) {
                return Err(Error::InvalidKey);
            }
            AuthIdentity::Typed(course2md::execution::digest(typed_key.as_bytes()))
        } else {
            AuthIdentity::Saved(draft.credential.clone().ok_or(Error::MissingKey)?)
        };
        Ok(Self {
            endpoint,
            protocol: draft.protocol,
            auth,
        })
    }
}

fn models_endpoint(address: &str, protocol: ServiceProtocol) -> Result<String, Error> {
    let endpoint = normalize_endpoint(address, protocol).map_err(|_| Error::InvalidAddress)?;
    let mut url = url::Url::parse(&endpoint).map_err(|_| Error::InvalidAddress)?;
    let base = url
        .path()
        .strip_suffix(protocol.endpoint_suffix())
        .ok_or(Error::InvalidAddress)?;
    let path = format!("{base}/models");
    url.set_path(&path);
    Ok(url.into())
}

pub struct Request {
    key: RequestKey,
    typed_key: Option<Secret>,
    cancelled: Arc<AtomicBool>,
}

impl Request {
    /// Discovery does not stage a draft, store a key or require a model ID.
    pub fn from_draft(draft: &ServiceDraft, typed_key: Secret) -> Result<Self, Error> {
        let key = RequestKey::from_draft(draft, typed_key.expose())?;
        let typed_key = matches!(&key.auth, AuthIdentity::Typed(_))
            .then(|| Secret::new(typed_key.expose().trim()));
        Ok(Self {
            key,
            typed_key,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg(test)]
    pub fn key(&self) -> &RequestKey {
        &self.key
    }
}

#[derive(Clone)]
pub struct Ticket {
    id: uuid::Uuid,
    key: RequestKey,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
pub enum Status {
    #[default]
    Idle,
    Loading,
    Ready(Vec<String>),
    Failed(Error),
}

#[derive(Default)]
pub struct State {
    active: Option<Ticket>,
    status: Status,
    /// Whether the model field lists its candidates.
    pub choices_open: bool,
}

impl State {
    pub fn status(&self) -> &Status {
        &self.status
    }
    pub fn loading(&self) -> bool {
        matches!(self.status, Status::Loading)
    }
    pub fn models(&self) -> &[String] {
        match &self.status {
            Status::Ready(models) => models,
            _ => &[],
        }
    }
    pub fn invalidate(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancelled.store(true, Ordering::Release);
        }
        self.status = Status::Idle;
        self.choices_open = false;
    }
    /// Seed choices obtained without HTTP discovery (an account login's own catalog).
    /// There is no connection to invalidate; the next `invalidate` clears them.
    pub fn prime(&mut self, models: Vec<String>) {
        self.invalidate();
        self.status = Status::Ready(models);
    }
    /// Begin an externally driven fetch (no HTTP [`Request`]); complete with
    /// [`State::prime`] or [`State::reject`]. Gives the shared field its loading state.
    pub fn begin_external(&mut self) {
        self.invalidate();
        self.status = Status::Loading;
    }
    pub fn reject(&mut self, error: Error) {
        self.invalidate();
        self.status = Status::Failed(error);
    }
    pub fn begin(&mut self, request: &Request) -> Ticket {
        self.invalidate();
        let ticket = Ticket {
            id: uuid::Uuid::new_v4(),
            key: request.key.clone(),
            cancelled: request.cancelled.clone(),
        };
        self.active = Some(ticket.clone());
        self.status = Status::Loading;
        ticket
    }
    /// Recompute `current` from live inputs, not just an earlier change observer.
    /// This also rejects responses from a closed/reopened editor or a prior retry.
    pub fn complete(
        &mut self,
        ticket: Ticket,
        current: Option<&RequestKey>,
        result: Result<Vec<String>, Error>,
    ) -> bool {
        if !self.loading()
            || self
                .active
                .as_ref()
                .is_none_or(|active| active.id != ticket.id)
        {
            return false;
        }
        if current != Some(&ticket.key) || ticket.cancelled.load(Ordering::Acquire) {
            self.invalidate();
            return false;
        }
        // Keep the completed ticket as the lifetime of the displayed choices.
        // Invalidating the connection disables choices in an already-open menu.
        self.status = match result {
            Ok(models) => Status::Ready(models),
            Err(Error::Cancelled) => Status::Idle,
            Err(error) => Status::Failed(error),
        };
        true
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.invalidate();
    }
}

/// Uses the supplied draft's endpoint and authentication exactly once. Redirects
/// are disabled so authentication never follows a server to another endpoint.
/// The work runs on a blocking IO thread; the deadline also bounds waiting for
/// the Keychain and system DNS, which that thread cannot interrupt.
pub async fn discover(
    request: Request,
    vault: Arc<dyn CredentialVault>,
) -> Result<Vec<String>, Error> {
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    let work = crate::spawn_blocking_io(move || {
        run_discovery(
            request,
            vault.as_ref(),
            &HttpTransport { deadline },
            deadline,
            Instant::now,
        )
    });
    before_deadline(
        async move { work.recv().await.unwrap_or(Err(Error::Network)) },
        deadline,
    )
    .await
}

async fn before_deadline(
    work: impl Future<Output = Result<Vec<String>, Error>>,
    deadline: Instant,
) -> Result<Vec<String>, Error> {
    let result = smol::future::race(work, async {
        smol::Timer::at(deadline).await;
        Err(Error::TimedOut)
    })
    .await;
    // Both futures can become ready before this task is polled again. Never
    // accept a late result just because the worker won that poll's ordering.
    if Instant::now() >= deadline {
        return Err(Error::TimedOut);
    }
    // Do not cancel the UI ticket on timeout: it must accept the failure once.
    // A blocked OS call may finish later, but its dropped receiver cannot publish.
    result
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(Error::TimedOut)
}

struct Response {
    status: u16,
    body: Vec<u8>,
}
trait Transport {
    fn get(&self, endpoint: &str, authorization: Option<&Secret>) -> Result<Response, Error>;
}
struct HttpTransport {
    deadline: Instant,
}
impl Transport for HttpTransport {
    fn get(&self, endpoint: &str, authorization: Option<&Secret>) -> Result<Response, Error> {
        let budget = remaining(self.deadline)?;
        let agent = crate::bounded_http::json_agent(
            budget.min(Duration::from_secs(10)),
            budget.min(Duration::from_secs(15)),
            None,
            budget,
        );
        let call = crate::bounded_http::json_call(agent.get(endpoint), authorization);
        let response = match call.timeout(remaining(self.deadline)?).call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(_)) => return Err(Error::Network),
        };
        let status = response.status();
        let body =
            crate::bounded_http::read_bounded(response, MAX_RESPONSE).map_err(
                |error| match error {
                    crate::bounded_http::BoundedReadError::TooLarge => Error::TooLarge,
                    crate::bounded_http::BoundedReadError::Network => Error::Network,
                },
            )?;
        Ok(Response { status, body })
    }
}

fn run_discovery(
    request: Request,
    vault: &dyn CredentialVault,
    transport: &dyn Transport,
    deadline: Instant,
    now: impl Fn() -> Instant,
) -> Result<Vec<String>, Error> {
    let cancelled = request.cancelled.clone();
    let check = || {
        if cancelled.load(Ordering::Acquire) {
            Err(Error::Cancelled)
        } else if now() >= deadline {
            Err(Error::TimedOut)
        } else {
            Ok(())
        }
    };
    check()?;
    let authorization = match &request.key.auth {
        AuthIdentity::None => Ok(None),
        AuthIdentity::Typed(_) => Ok(request.typed_key),
        AuthIdentity::Saved(reference) => vault
            .resolve(reference)
            .map(Some)
            .map_err(|_| Error::CredentialUnavailable),
    };
    // A Keychain call cannot be interrupted while blocked. Once it returns,
    // an expired request must stop here before any network request is sent.
    check()?;
    let authorization = authorization?;
    if let Some(secret) = &authorization
        && (secret.is_empty() || secret.expose().chars().any(char::is_control))
    {
        return Err(Error::InvalidKey);
    }
    check()?;
    let response = transport.get(&request.key.endpoint, authorization.as_ref());
    check()?;
    let response = response?;
    match response.status {
        200..=299 => (),
        401 | 403 => return Err(Error::AuthenticationRefused),
        300..=399 | 404 | 405 | 501 => return Err(Error::Unsupported),
        status => return Err(Error::Refused(status)),
    }
    let models = parse_models(&response.body, authorization.as_ref());
    check()?;
    models
}

fn parse_models(body: &[u8], authorization: Option<&Secret>) -> Result<Vec<String>, Error> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or(Error::InvalidResponse)?;
    if value.get("error").is_some() {
        return Err(Error::InvalidResponse);
    }
    if data.len() > 10000 {
        return Err(Error::TooLarge);
    }
    let models: BTreeSet<String> = data
        .iter()
        .filter_map(|item| item.get("id")?.as_str())
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
        .filter(|id| authorization.is_none_or(|secret| !id.contains(secret.expose())))
        .map(str::to_owned)
        .collect();
    if !data.is_empty() && models.is_empty() {
        return Err(Error::InvalidResponse);
    }
    Ok(models.into_iter().collect())
}

/// Shared settings/setup model control. The input remains editable in every
/// discovery state; a chosen candidate fills it, emitting its ordinary change event.
/// Candidates list below the field, however many a service returns.
pub fn model_field(
    id: &'static str,
    input: &gpui::Entity<gpui_kit::component::input::InputState>,
    state: &State,
    disabled: bool,
    error: Option<&str>,
    on_fetch: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_show_choices: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    cx: &mut gpui::App,
) -> gpui::Div {
    use crate::{symbols, ui};
    use gpui::{SharedString, prelude::*};
    use gpui_base::v_flex;
    use gpui_kit_material::prelude::*;

    let child = |name: &str| SharedString::from(format!("{id}-{name}"));
    let fetch_label = if matches!(state.status(), Status::Ready(_)) {
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
                    .on_click(on_fetch),
            )
            .into_any_element()
    };
    let field = TextField::new(child("input"), input)
        .outlined()
        .w_full()
        .label(course2md::tr!("模型"))
        .placeholder(course2md::tr!("模型 ID，或从服务获取列表"))
        .disabled(disabled)
        .trailing_icon(trailing)
        .error(error.is_some())
        .when_some(error, |field, error| {
            field.supporting_text(error.to_owned())
        });
    let status = match state.status() {
        // Idle needs no words: the field and its button say what can be done.
        Status::Idle => ui::AsyncStatus::Idle,
        Status::Loading => ui::AsyncStatus::Busy(course2md::tr!("正在获取模型列表…").into()),
        Status::Ready(models) if models.is_empty() => {
            ui::AsyncStatus::Warning(course2md::tr!("服务未返回模型").into())
        }
        Status::Ready(models) => {
            ui::AsyncStatus::Success(course2md::tr!("找到 {} 个模型", models.len()).into())
        }
        Status::Failed(error) => ui::AsyncStatus::Failed(error.message().into()),
    };
    let toggle = (!state.models().is_empty()).then(|| {
        material_button(child("choose"), ButtonVariant::Text)
            .child(if state.choices_open {
                course2md::tr!("收起模型列表")
            } else {
                course2md::tr!("选择模型")
            })
            .disabled(disabled)
            .on_click(on_show_choices)
            .into_any_element()
    });
    let choices = (state.choices_open && !disabled && !state.models().is_empty()).then(|| {
        let models = state.models().to_vec();
        let count = models.len();
        let selected = input.read(cx).value().to_string();
        let field = input.clone();
        // A primed (account-bound) list has no ticket; a cancelled ticket disables
        // choices already on screen.
        let current = state.active.as_ref().map(|ticket| ticket.cancelled.clone());
        gpui::uniform_list(child("choices"), count, move |range, _, _| {
            range
                .map(|index| {
                    let model = models[index].clone();
                    let (field, current) = (field.clone(), current.clone());
                    ListItem::new(("model-choice", index), model.clone())
                        .density(ListDensity::Compact)
                        .selected(model == selected)
                        .on_click(move |_, window, cx| {
                            if current
                                .as_ref()
                                .is_some_and(|cancelled| cancelled.load(Ordering::Acquire))
                            {
                                return;
                            }
                            field
                                .update(cx, |input, cx| input.set_value(model.clone(), window, cx));
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

#[cfg(test)]
mod tests {
    use super::{
        DISCOVERY_TIMEOUT, Error, Request, RequestKey, Response, State, Status, Transport,
        before_deadline, models_endpoint, parse_models, run_discovery,
    };
    use crate::credentials::{CredentialVault, MemoryCredentialVault, Secret};
    use crate::preferences::{Authentication, ServiceDraft, ServiceProtocol, ServicePurpose};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    fn draft() -> ServiceDraft {
        let mut draft = ServiceDraft::new(ServicePurpose::Ai);
        draft.address = "https://example.test/gateway/v1".into();
        draft
    }

    fn run(
        request: Request,
        vault: &dyn CredentialVault,
        transport: &dyn Transport,
    ) -> Result<Vec<String>, Error> {
        run_discovery(
            request,
            vault,
            transport,
            Instant::now() + DISCOVERY_TIMEOUT,
            Instant::now,
        )
    }

    #[test]
    fn models_endpoint_preserves_the_configured_prefix_without_protocol_fallback() {
        for (address, protocol) in [
            ("https://example.test/gateway/v1/", ServiceProtocol::AiChat),
            (
                "https://example.test/gateway/v1/chat/completions",
                ServiceProtocol::AiChat,
            ),
            (
                "https://example.test/gateway/v1/audio/transcriptions",
                ServiceProtocol::SpeechTranscriptions,
            ),
        ] {
            assert_eq!(
                models_endpoint(address, protocol).unwrap(),
                "https://example.test/gateway/v1/models"
            );
        }
        for address in [
            "https://user:key@example.test/v1",
            "https://example.test/v1?key=secret",
            "https://example.test/v1#key",
            "file:///models",
            "https://example.test/v1/responses",
        ] {
            assert_eq!(
                models_endpoint(address, ServiceProtocol::AiChat),
                Err(Error::InvalidAddress)
            );
        }
    }

    #[test]
    fn response_accepts_only_bounded_model_ids_and_never_echoes_credentials() {
        let key = Secret::new("fixture-secret");
        let body = br#"{"data":[{"id":"z-model"},{"id":"a-model"},{"id":"a-model"},{"id":""},{"id":"debug-fixture-secret"},{"id":"bad\nmodel"}]}"#;
        assert_eq!(
            parse_models(body, Some(&key)).unwrap(),
            vec!["a-model", "z-model"]
        );
        assert!(parse_models(br#"{"data":[]}"#, None).unwrap().is_empty());
        for body in [
            br#"{"models":["model"]}"#.as_slice(),
            br#"{"data":[{}]}"#,
            br#"{"error":{"message":"fixture-secret"},"data":[]}"#,
        ] {
            let error = parse_models(body, Some(&key)).unwrap_err();
            assert_eq!(error, Error::InvalidResponse);
            assert!(!error.message().contains(key.expose()));
        }
    }

    #[test]
    fn stale_responses_cannot_survive_address_auth_key_or_editor_changes() {
        let draft = draft();
        let mut state = State::default();
        for change in 0..4 {
            let request = Request::from_draft(&draft, Secret::new("first-key")).unwrap();
            let ticket = state.begin(&request);
            let mut changed = draft.clone();
            let mut key = "first-key";
            match change {
                0 => changed.address = "https://other.test/v1".into(),
                1 => changed.authentication = Authentication::None,
                2 => key = "second-key",
                _ => state = State::default(),
            }
            let current = RequestKey::from_draft(&changed, key).unwrap();
            assert!(!state.complete(ticket, Some(&current), Ok(vec!["old-model".into()])));
            assert!(state.models().is_empty());
        }
        let first = Request::from_draft(&draft, Secret::new("first-key")).unwrap();
        let old = state.begin(&first);
        let second = Request::from_draft(&draft, Secret::new("first-key")).unwrap();
        let current = state.begin(&second);
        assert!(!state.complete(old, Some(second.key()), Ok(vec!["old-model".into()])));
        assert!(state.loading());
        assert!(state.complete(
            current,
            Some(second.key()),
            Ok(vec!["current-model".into()])
        ));
        assert_eq!(state.models(), &["current-model"]);
    }

    #[test]
    fn primed_catalog_behaves_like_a_ready_list_until_invalidated() {
        let mut state = State::default();
        state.prime(vec!["gpt-fixture".into()]);
        assert_eq!(state.models(), &["gpt-fixture"]);
        // 无连接票据：候选保持可选（菜单点击守卫只拒绝已取消的票据）
        assert!(state.active.is_none());
        state.begin_external();
        assert!(state.loading());
        assert!(state.models().is_empty());
        state.reject(Error::Custom("fixture failure".into()));
        assert!(matches!(state.status(), Status::Failed(Error::Custom(_))));
        assert_eq!(state.models().len(), 0, "失败状态不提供候选，仍可手动填写");
        state.prime(vec!["gpt-fixture".into()]);
        state.invalidate();
        assert!(state.models().is_empty());
        assert!(matches!(state.status(), Status::Idle));
    }

    struct FakeTransport {
        status: u16,
        calls: Cell<usize>,
        expected_key: Option<&'static str>,
    }
    impl Transport for FakeTransport {
        fn get(&self, endpoint: &str, authorization: Option<&Secret>) -> Result<Response, Error> {
            self.calls.set(self.calls.get() + 1);
            assert_eq!(endpoint, "https://example.test/gateway/v1/models");
            assert_eq!(authorization.map(Secret::expose), self.expected_key);
            Ok(Response {
                status: self.status,
                body: br#"{"data":[{"id":"candidate"}]}"#.to_vec(),
            })
        }
    }

    #[test]
    fn discovery_uses_unsaved_or_stored_credentials_without_staging_a_service() {
        let vault = MemoryCredentialVault::new();
        let mut draft = draft();
        let saved = vault.insert(Secret::new("saved-key")).unwrap();
        draft.credential = Some(saved);
        for (typed, expected) in [("", Some("saved-key")), ("typed-key", Some("typed-key"))] {
            let request = Request::from_draft(&draft, Secret::new(typed)).unwrap();
            let transport = FakeTransport {
                status: 200,
                calls: Cell::new(0),
                expected_key: expected,
            };
            assert_eq!(run(request, &vault, &transport).unwrap(), vec!["candidate"]);
            assert_eq!(transport.calls.get(), 1);
            assert!(
                draft.model.is_empty(),
                "discovery neither requires nor selects a model"
            );
        }
        draft.authentication = Authentication::None;
        let request = Request::from_draft(&draft, Secret::new("ignored-key")).unwrap();
        let transport = FakeTransport {
            status: 401,
            calls: Cell::new(0),
            expected_key: None,
        };
        assert_eq!(
            run(request, &vault, &transport),
            Err(Error::AuthenticationRefused)
        );
    }

    #[test]
    fn cancel_or_missing_credentials_do_not_send_and_empty_results_remain_distinct() {
        let vault = MemoryCredentialVault::new();
        let draft = draft();
        assert!(matches!(
            Request::from_draft(&draft, Secret::new("")),
            Err(Error::MissingKey)
        ));
        let request = Request::from_draft(&draft, Secret::new("key")).unwrap();
        let mut state = State::default();
        state.begin(&request);
        state.invalidate();
        let transport = FakeTransport {
            status: 200,
            calls: Cell::new(0),
            expected_key: Some("key"),
        };
        assert_eq!(run(request, &vault, &transport), Err(Error::Cancelled));
        assert_eq!(transport.calls.get(), 0);
        let request = Request::from_draft(&draft, Secret::new("key")).unwrap();
        let ticket = state.begin(&request);
        assert!(state.complete(ticket, Some(request.key()), Ok(vec![])));
        assert!(matches!(state.status(), Status::Ready(models) if models.is_empty()));
    }

    #[test]
    fn deadline_failure_is_visible_and_late_completion_cannot_replace_it_or_a_retry() {
        let draft = draft();
        let request = Request::from_draft(&draft, Secret::new("key")).unwrap();
        let current = request.key().clone();
        let mut state = State::default();
        let ticket = state.begin(&request);
        let (sender, receiver) = smol::channel::bounded(1);
        let result = smol::block_on(before_deadline(
            async { receiver.recv().await.unwrap() },
            Instant::now() + Duration::from_millis(5),
        ));
        assert_eq!(result, Err(Error::TimedOut));
        assert!(state.complete(ticket.clone(), Some(&current), result));
        assert!(matches!(state.status(), Status::Failed(Error::TimedOut)));
        assert!(!ticket.cancelled.load(Ordering::Acquire));

        // Model a blocking worker that finishes after its async receiver timed out.
        sender.try_send(Ok(vec!["late-model".into()])).unwrap();
        let late = receiver.try_recv().unwrap();
        assert!(!state.complete(ticket.clone(), Some(&current), late.clone()));
        assert!(matches!(state.status(), Status::Failed(Error::TimedOut)));

        let retry = Request::from_draft(&draft, Secret::new("key")).unwrap();
        let retry_ticket = state.begin(&retry);
        assert!(!state.complete(ticket, Some(&current), late));
        assert!(state.loading());
        assert!(state.complete(
            retry_ticket,
            Some(retry.key()),
            Ok(vec!["fresh-model".into()])
        ));
        assert_eq!(state.models(), &["fresh-model"]);

        // A ready worker must not win when the executor resumes after the deadline.
        assert_eq!(
            smol::block_on(before_deadline(
                async { Ok(vec!["already-late".into()]) },
                Instant::now(),
            )),
            Err(Error::TimedOut)
        );
    }

    struct ExpiringVault {
        resolved: AtomicBool,
    }

    impl CredentialVault for ExpiringVault {
        fn insert(&self, _secret: Secret) -> anyhow::Result<String> {
            panic!("discovery must not save credentials")
        }
        fn resolve(&self, _reference: &str) -> anyhow::Result<Secret> {
            self.resolved.store(true, Ordering::Release);
            Ok(Secret::new("saved-key"))
        }
        fn remove(&self, _reference: &str) -> anyhow::Result<()> {
            panic!("discovery must not remove credentials")
        }
    }

    #[test]
    fn credential_lookup_that_exhausts_the_deadline_never_sends_http() {
        let mut draft = draft();
        draft.credential = Some("saved-credential".into());
        let request = Request::from_draft(&draft, Secret::new("")).unwrap();
        let vault = ExpiringVault {
            resolved: AtomicBool::new(false),
        };
        let transport = FakeTransport {
            status: 200,
            calls: Cell::new(0),
            expected_key: Some("saved-key"),
        };
        let started = Instant::now();
        let deadline = started + DISCOVERY_TIMEOUT;
        // Advance a deterministic clock only when the credential call returns.
        // This exercises the blocking boundary without sleeping or using Keychain.
        let now = || {
            if vault.resolved.load(Ordering::Acquire) {
                deadline
            } else {
                started
            }
        };
        assert_eq!(
            run_discovery(request, &vault, &transport, deadline, now),
            Err(Error::TimedOut)
        );
        assert!(vault.resolved.load(Ordering::Acquire));
        assert_eq!(transport.calls.get(), 0);
    }

    #[test]
    fn response_arriving_after_deadline_cannot_be_accepted() {
        let request = Request::from_draft(&draft(), Secret::new("key")).unwrap();
        let vault = MemoryCredentialVault::new();
        let transport = FakeTransport {
            status: 200,
            calls: Cell::new(0),
            expected_key: Some("key"),
        };
        let started = Instant::now();
        let deadline = started + DISCOVERY_TIMEOUT;
        let now = || {
            if transport.calls.get() > 0 {
                deadline
            } else {
                started
            }
        };
        assert_eq!(
            run_discovery(request, &vault, &transport, deadline, now),
            Err(Error::TimedOut)
        );
        assert_eq!(transport.calls.get(), 1);
    }
}
