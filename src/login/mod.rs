//! 登录方法注册表（插件式扩展点）。
//!
//! 新增一种登录方式 = 实现 [`LoginMethod`] + 在 [`METHODS`] 注册一行 +
//! 在 [`crate::cli::LoginPlatform`] 增加同名变体。CLI 经 `--login/--logout <id>`
//! 分发；凭据统一存 `{config_dir}/auth/`（0600 原子写，见各实现）。

mod bilibili;
pub mod codex;
pub mod ollama;

use anyhow::Result;

use crate::cli::LoginPlatform;

/// 一种登录方法。实现为无状态单位 struct，多次调用间不共享运行时状态。
pub trait LoginMethod: Sync {
    /// 稳定标识：与 `LoginPlatform` 的命令行值一致（kebab-case）。
    fn id(&self) -> &'static str;
    /// 双语展示名。
    fn label(&self) -> &'static str;
    /// CLI 交互登录；成功后自行打印确认信息。
    fn login(&self) -> Result<()>;
    /// 清除本地凭据；未登录时同样成功并提示。
    fn logout(&self) -> Result<()>;
    /// 当前状态。网络失败返回 Err，不得当作 Disconnected。
    fn status(&self) -> Result<LoginStatus>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginStatus {
    Disconnected,
    /// 已连接：账号名或服务描述
    Connected(String),
    /// 凭据存在但已失效
    Expired,
}

/// 已注册的登录方法。新增方法在此追加一行。
const METHODS: &[&dyn LoginMethod] = &[
    &bilibili::BilibiliLogin,
    &ollama::OllamaLogin,
    &codex::CodexLogin,
];

pub fn methods() -> &'static [&'static dyn LoginMethod] {
    METHODS
}

/// CLI 分发入口：`LoginPlatform` 变体必须在注册表中有同 id 的实现。
pub fn for_platform(platform: LoginPlatform) -> &'static dyn LoginMethod {
    let id = platform.id();
    methods()
        .iter()
        .copied()
        .find(|m| m.id() == id)
        .unwrap_or_else(|| unreachable!("登录方法未注册 / Login method not registered: {id}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_platform_has_a_registered_method() {
        for platform in [
            crate::cli::LoginPlatform::Bilibili,
            crate::cli::LoginPlatform::Ollama,
            crate::cli::LoginPlatform::Codex,
        ] {
            assert_eq!(super::for_platform(platform).id(), platform.id());
        }
        // 注册表无重复 id
        let mut ids: Vec<_> = super::methods().iter().map(|m| m.id()).collect();
        ids.sort();
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(ids, unique);
    }
}
