//! Ollama 本地服务登录：探测服务、选择模型、写入 LLM 配置。
//! Ollama 的 OpenAI 兼容端点无需凭据（空 api_key 不发 Authorization 头，
//! 见 llm.rs request_chat_once），"登录"实为服务发现 + 配置。

use anyhow::{Context, Result, bail, ensure};
use std::io::IsTerminal;

use super::{LoginMethod, LoginStatus};
use crate::llm::LlmProvider;

const DEFAULT_HOST: &str = "http://localhost:11434";

/// Ollama 服务地址：环境变量 OLLAMA_HOST > 默认 localhost:11434。
fn host() -> String {
    let raw = std::env::var("OLLAMA_HOST")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.into());
    let raw = raw.trim().trim_end_matches('/');
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .redirects(0)
        .build()
}

/// 已下载的模型名列表（GET {host}/api/tags）。
pub(super) fn models(host: &str) -> Result<Vec<String>> {
    let response = agent()
        .get(&format!("{host}/api/tags"))
        .call()
        .map_err(|_| {
            anyhow::anyhow!(
                "无法连接 Ollama 服务 {host}；请确认已安装并正在运行（ollama serve） / Cannot reach the Ollama service at {host}; make sure it is installed and running (ollama serve)"
            )
        })?;
    let value: serde_json::Value = response
        .into_json()
        .context("Ollama 响应无法解析 / Cannot parse the Ollama response")?;
    let Some(list) = value["models"].as_array() else {
        bail!("Ollama 响应缺少模型列表 / Ollama response is missing the model list");
    };
    Ok(list
        .iter()
        .filter_map(|m| m["name"].as_str().map(str::to_string))
        .collect())
}

/// 桌面端服务发现：探测本机 Ollama 并返回（服务地址, 模型列表）。
/// 只读探测，不读取也不写入任何配置；供 GUI 账号卡片与一键添加使用。
pub fn discover_for_desktop() -> Result<(String, Vec<String>)> {
    let host = host();
    let names = models(&host)?;
    Ok((host, names))
}

pub(super) struct OllamaLogin;
impl LoginMethod for OllamaLogin {
    fn id(&self) -> &'static str {
        "ollama"
    }
    fn label(&self) -> &'static str {
        "Ollama 本地服务 / Local Ollama"
    }

    fn login(&self) -> Result<()> {
        let host = host();
        let names = models(&host)?;
        ensure!(
            !names.is_empty(),
            "Ollama 中没有已下载的模型，请先运行 ollama pull <模型名> / No downloaded models found; run ollama pull <model> first"
        );
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        let model = if names.len() == 1 {
            names.into_iter().next().unwrap_or_default()
        } else if interactive {
            let pick = dialoguer::Select::new()
                .with_prompt("选择用于润色的模型 / Choose a model for proofreading")
                .items(&names)
                .default(0)
                .interact_opt()?
                .ok_or_else(|| anyhow::anyhow!("已取消登录，未保存配置。 / Login cancelled; no configuration saved."))?;
            names.into_iter().nth(pick).unwrap_or_default()
        } else {
            bail!(
                "Ollama 有多个模型可用，请在终端中运行 course2md --login ollama 以选择 / Multiple Ollama models available; run course2md --login ollama in a terminal to choose"
            );
        };

        let mut cfg = crate::settings::load()?;
        cfg.llm.provider = LlmProvider::Ollama;
        cfg.llm.base_url = format!("{host}/v1");
        cfg.llm.api_key.clear();
        cfg.llm.model = model.clone();
        if cfg.llm.vision {
            // 本地模型多数不支持图片输入；登录时回落纯文本，需要时可在 llm setup 重开
            cfg.llm.vision = false;
            println!("已关闭截图辅助润色（本地模型通常不支持图片输入）。/ Slide-assisted polish disabled (local models usually do not accept images).");
        }
        cfg.llm.enabled = true;
        crate::llm::validate(&cfg.llm)?;
        let path = crate::settings::save(&cfg)?;
        let saved = path.display().to_string();
        println!(
            "已登录 Ollama（{host}），模型 {model}，配置已保存并启用：{saved} / Logged in to Ollama; settings saved and enabled: {saved}"
        );
        match crate::llm::test_connection(&cfg.llm) {
            Ok(()) => println!("连接测试通过 / Connection test passed."),
            Err(e) => bail!(
                "配置已保存，但连接测试失败；请确认模型 {model} 可正常对话 / Settings saved, but the connection test failed; make sure {model} can chat: {e:#}"
            ),
        }
        Ok(())
    }

    fn logout(&self) -> Result<()> {
        let mut cfg = crate::settings::load()?;
        if cfg.llm.provider != LlmProvider::Ollama {
            println!("当前 LLM 配置不是 Ollama，未做改动。/ The current LLM configuration is not Ollama; nothing changed.");
            return Ok(());
        }
        cfg.llm = crate::llm::LlmSettings::default();
        crate::settings::save(&cfg)?;
        println!("已清除 Ollama 登录配置并关闭 AI 润色。/ Ollama login removed and AI proofreading disabled.");
        Ok(())
    }

    fn status(&self) -> Result<LoginStatus> {
        let cfg = crate::settings::load()?;
        if cfg.llm.provider != LlmProvider::Ollama {
            return Ok(LoginStatus::Disconnected);
        }
        let host = cfg
            .llm
            .base_url
            .trim()
            .trim_end_matches('/')
            .strip_suffix("/v1")
            .unwrap_or(cfg.llm.base_url.trim())
            .to_string();
        let names = models(&host)?;
        Ok(LoginStatus::Connected(format!(
            "{}（{} 个模型 / {} models）",
            cfg.llm.base_url,
            names.len(),
            names.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_normalization() {
        assert_eq!(super::DEFAULT_HOST, "http://localhost:11434");
    }
}
