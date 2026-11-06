//! What local Ollama discovery has found.
#[derive(Default)]
pub struct OllamaUi {
    pub working: bool,
    /// Ok((address, models)) when found; Err(explanation) when not; None before the first check.
    pub status: Option<Result<(String, Vec<String>), String>>,
}

impl OllamaUi {
    /// A found service is reused; a failed check is retried on the next form.
    pub fn found(&self) -> bool {
        matches!(self.status, Some(Ok(_)))
    }

    /// What the check established, for the Ollama entry of the type select.
    pub fn status_line(&self) -> Option<String> {
        if self.working {
            return Some(c2m_i18n::tr!("正在检查本机的 Ollama…").into());
        }
        match &self.status {
            Some(Ok((host, models))) if models.len() == 1 => {
                Some(c2m_i18n::tr!("已在 {} 检测到 1 个模型", host))
            }
            Some(Ok((host, models))) => Some(c2m_i18n::tr!(
                "已在 {} 检测到 {} 个模型",
                host,
                models.len()
            )),
            Some(Err(message)) => Some(message.clone()),
            None => None,
        }
    }
}
