//! Service-binding labels and default-scope helpers for settings and setup.
use crate::preferences::{BindingScope, ServicePurpose};

/// Where a published service applies: a service saved in settings becomes the
/// default only when chosen so.
pub fn publish_scope(also_default: bool) -> BindingScope {
    if also_default {
        BindingScope::Defaults
    } else {
        BindingScope::Saved
    }
}

pub fn purpose_label(purpose: ServicePurpose) -> &'static str {
    match purpose {
        ServicePurpose::Speech => c2m_i18n::tr!("语音服务"),
        ServicePurpose::Ai => c2m_i18n::tr!("AI 服务"),
    }
}

pub fn add_service_label(purpose: ServicePurpose) -> &'static str {
    match purpose {
        ServicePurpose::Speech => c2m_i18n::tr!("添加语音服务"),
        ServicePurpose::Ai => c2m_i18n::tr!("添加 AI 服务"),
    }
}

#[cfg(test)]
mod tests {
    use super::{BindingScope, publish_scope};

    #[test]
    fn a_service_saved_in_settings_changes_defaults_only_when_asked() {
        assert_eq!(publish_scope(false), BindingScope::Saved);
        assert_eq!(publish_scope(true), BindingScope::Defaults);
    }
}
