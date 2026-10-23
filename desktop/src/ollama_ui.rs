//! Local Ollama discovery: probe the local service and downloaded models as address and model candidates for the Ollama service form.
//! Ollama's OpenAI-compatible endpoint needs no credential; "signing in" is discovery plus configuration (see core login/ollama).
use super::*;
use crate::preferences::{ServiceProtocol, ServicePurpose};
use crate::settings_ui::service_form::{FormField, FormOwner};

#[derive(Default)]
pub(crate) struct OllamaUi {
    generation: u64,
    working: bool,
    /// Ok((address, models)) when found; Err(explanation) when not; None before the first check.
    status: Option<Result<(String, Vec<String>), String>>,
}

impl OllamaUi {
    /// A found service is reused; a failed check is retried on the next form.
    fn found(&self) -> bool {
        matches!(self.status, Some(Ok(_)))
    }

    /// What the check established, for the Ollama entry of the type select.
    pub(crate) fn status_line(&self) -> Option<String> {
        if self.working {
            return Some(course2md::tr!("正在检查本机的 Ollama…").into());
        }
        match &self.status {
            Some(Ok((host, models))) => Some(course2md::tr!(
                "已在 {} 检测到 {} 个模型",
                host,
                models.len()
            )),
            Some(Err(message)) => Some(message.clone()),
            None => None,
        }
    }
}

impl Desktop {
    /// Probe the local Ollama service. Blocking requests stay off the GPUI executor (see spawn_blocking_io).
    pub(crate) fn ollama_refresh(&mut self, cx: &mut Context<Self>) {
        if self.ollama.working {
            return;
        }
        self.ollama.working = true;
        self.ollama.generation = self.ollama.generation.wrapping_add(1);
        let generation = self.ollama.generation;
        // The engine error names `ollama serve`; the page offers its own action.
        let task = crate::spawn_blocking_io(|| {
            course2md::login::ollama::discover_for_desktop()
                .map_err(|_| course2md::tr!("未检测到 Ollama，请安装并打开 Ollama").to_owned())
        });
        cx.spawn(async move |this, cx| {
            let result = task
                .recv()
                .await
                .unwrap_or_else(|_| Err(crate::interrupted().to_owned()));
            let _ = this.update(cx, |this, cx| {
                if this.ollama.generation != generation {
                    return;
                }
                this.ollama.working = false;
                this.ollama.status = Some(result);
                this.ollama_apply_catalog(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Ollama forms read the local service: a found service is reused, a
    /// missing one is checked again.
    pub(crate) fn ollama_prepare_form(&mut self, cx: &mut Context<Self>) {
        if self.ollama.found() {
            self.ollama_apply_catalog(cx);
        } else {
            self.ollama_refresh(cx);
        }
    }

    /// Detected models become candidates on every form still editing an Ollama
    /// service; an empty address or model is filled, typed values stay. Without
    /// a detected service the address falls back to Ollama's default.
    pub(crate) fn ollama_apply_catalog(&mut self, cx: &mut Context<Self>) {
        let (host, models) = match self.ollama.status.clone() {
            Some(Ok(found)) => found,
            _ => (
                crate::preferences::OLLAMA_DEFAULT_ADDRESS.to_owned(),
                Vec::new(),
            ),
        };
        let window = self.window;
        for owner in [FormOwner::Editor, FormOwner::Setup(ServicePurpose::Ai)] {
            let Some(form) = self
                .service_form_mut(owner)
                .filter(|form| form.draft.protocol == ServiceProtocol::OllamaChat)
            else {
                continue;
            };
            if !models.is_empty() {
                form.models.prime(models.clone());
            }
            let address = form.input(FormField::Address).clone();
            let model = form.input(FormField::Model).clone();
            crate::codex_ui::autofill_input(window, address, Some(host.clone()), cx);
            crate::codex_ui::autofill_input(window, model, models.first().cloned(), cx);
        }
    }
}
