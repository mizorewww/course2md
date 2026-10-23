pub mod apple;
pub mod artifact;
pub mod asr;
pub mod auth;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod dispatch;
pub mod doctor;
pub mod download;
pub mod error;
pub mod execution;
pub mod fetch;
pub mod hub;
pub mod i18n;
pub mod language;
pub mod llm;
pub mod login;
pub mod math;
pub mod media;
pub mod models;
pub mod npu;
pub mod options;
pub mod pipeline;
pub mod portable;
pub mod progress;
pub mod provider;
pub mod render;
pub mod runtime;
pub mod scene;
pub mod settings;
pub mod subtitle;
pub mod summarize;
#[cfg(test)]
mod test_http;
pub mod timeline;
pub mod wizard;
pub mod ytdlp;

/// 操作系统随机源生成的 URL-safe base64 串（会话令牌、PKCE 参数）。
pub(crate) fn random_b64url(bytes: usize) -> anyhow::Result<String> {
    use base64::Engine as _;
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|_| {
        anyhow::anyhow!(crate::tr!(
            "无法生成安全随机数 / Cannot generate secure random bytes"
        ))
    })?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}
