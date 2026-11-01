//! 共享的有界 JSON HTTP 原语：服务测试与模型发现共用。
//!
//! 纪律（与引擎 dispatch 一致）：禁止跟随重定向；Authorization 头只在有密钥时设置；
//! 响应体按 max+1 读取并超限即拒，不把未截断的读取当成完整响应。

use std::time::Duration;

use crate::credentials::Secret;

/// 无重定向的 JSON agent。各超时独立传入——探测与测试的预算不同。
pub fn json_agent(
    connect: Duration,
    read: Duration,
    write: Option<Duration>,
    total: Duration,
) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(connect)
        .timeout_read(read)
        .timeout(total);
    if let Some(write) = write {
        builder = builder.timeout_write(write);
    }
    builder.build()
}

/// Accept: application/json；有密钥才加 Bearer 头。
pub fn json_call(call: ureq::Request, authorization: Option<&Secret>) -> ureq::Request {
    let call = call.set("Accept", "application/json");
    match authorization {
        Some(secret) => {
            let header = zeroize::Zeroizing::new(format!("Bearer {}", secret.expose()));
            call.set("Authorization", &header)
        }
        None => call,
    }
}

/// 有界读取的失败分类：网络/IO 错误与响应体超限分开。
/// 调用方把 Network 统一映射为自己的网络错误。
pub enum BoundedReadError {
    Network,
    TooLarge,
}

/// 按 max+1 读取；超过 max 报 TooLarge，IO 失败报 Network。
pub fn read_bounded(response: ureq::Response, max: u64) -> Result<Vec<u8>, BoundedReadError> {
    use std::io::Read;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut body)
        .map_err(|_| BoundedReadError::Network)?;
    if body.len() as u64 > max {
        return Err(BoundedReadError::TooLarge);
    }
    Ok(body)
}
