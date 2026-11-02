//! 共享的有界 JSON HTTP 原语：服务测试与模型发现共用。
//!
//! 纪律（与引擎 dispatch 一致）：禁止跟随重定向；Authorization 头只在有密钥时设置；
//! 响应体按 max+1 读取并超限即拒，不把未截断的读取当成完整响应。

use std::time::Duration;

use crate::credentials::Secret;

/// 无重定向的 JSON agent，与引擎同一起点（[`course2md::runtime::agent_builder`]：
/// 不跟随重定向、不读环境代理）。各超时独立传入——探测与测试的预算不同：
/// `connect` 每次连接，`read`/`write` 每次传输读写，`total` 整个请求。
/// 错误状态码照常作为响应返回，调用方读状态与响应体。
pub fn json_agent(
    connect: Duration,
    read: Duration,
    write: Option<Duration>,
    total: Duration,
) -> ureq::Agent {
    course2md::runtime::agent_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(connect))
        .timeout_per_read(Some(read))
        .timeout_per_write(write)
        .timeout_global(Some(total))
        .build()
        .new_agent()
}

/// Accept: application/json；有密钥才加 Bearer 头。
pub fn json_call<B>(
    call: ureq::RequestBuilder<B>,
    authorization: Option<&Secret>,
) -> ureq::RequestBuilder<B> {
    let call = call.header("Accept", "application/json");
    match authorization {
        Some(secret) => {
            let header = zeroize::Zeroizing::new(format!("Bearer {}", secret.expose()));
            call.header("Authorization", header.as_str())
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
pub fn read_bounded(
    response: ureq::http::Response<ureq::Body>,
    max: u64,
) -> Result<Vec<u8>, BoundedReadError> {
    use std::io::Read;
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(max + 1)
        .read_to_end(&mut body)
        .map_err(|_| BoundedReadError::Network)?;
    if body.len() as u64 > max {
        return Err(BoundedReadError::TooLarge);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> ureq::Agent {
        let second = Duration::from_secs(5);
        json_agent(second, second, Some(second), second)
    }

    #[test]
    fn an_error_status_arrives_with_its_body_and_the_key_is_sent() {
        let body = r#"{"error":"bad key"}"#;
        let (base, server) = c2m_test_http::serve(vec![format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )]);
        let secret = Secret::new("sk-test");
        let call = json_call(agent().post(format!("{base}/v1/test")), Some(&secret));
        let response = call.send(&b"{}"[..]).unwrap();
        assert_eq!(response.status().as_u16(), 401);
        assert_eq!(read_bounded(response, 1024).ok().unwrap(), body.as_bytes());
        let request = &server.join().unwrap()[0];
        let head = request.head.to_ascii_lowercase();
        assert!(head.contains("authorization: bearer sk-test"), "{head}");
        assert!(head.contains("accept: application/json"), "{head}");
        assert_eq!(request.body, b"{}");
    }

    #[test]
    fn a_redirect_is_not_followed_and_a_large_body_is_refused() {
        let (base, server) = c2m_test_http::serve(vec![
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            c2m_test_http::json_response(&"x".repeat(64)),
        ]);
        let response = json_call(agent().get(&base), None).call().unwrap();
        assert_eq!(response.status().as_u16(), 307);
        let response = json_call(agent().get(&base), None).call().unwrap();
        assert!(matches!(
            read_bounded(response, 63),
            Err(BoundedReadError::TooLarge)
        ));
        assert_eq!(server.join().unwrap().len(), 2);
    }
}
