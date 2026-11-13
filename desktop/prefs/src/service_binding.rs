//! What publishing a service changes: its defaults, and the repair it continues.
use crate::preferences::{BindingScope, ServicePurpose};
use c2m_engine::execution::Component;

/// What a service publication continues with once its credential is checked.
pub struct PublishPlan {
    pub continue_repair: bool,
    pub repair_task: Option<(String, Vec<Component>)>,
    pub scope: BindingScope,
}

/// Where a published service applies. Every task runs with the default services
/// current when it starts, so a repair's service becomes the default: that is the
/// service the repair runs with. A service saved in settings is the default only
/// when chosen so.
pub fn publish_scope(repairing: bool, also_default: bool) -> BindingScope {
    if repairing || also_default {
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
    fn a_repair_saves_the_default_service_it_runs_with() {
        for also_default in [false, true] {
            assert_eq!(publish_scope(true, also_default), BindingScope::Defaults);
        }
    }

    #[test]
    fn a_service_saved_in_settings_changes_defaults_only_when_asked() {
        assert_eq!(publish_scope(false, false), BindingScope::Saved);
        assert_eq!(publish_scope(false, true), BindingScope::Defaults);
    }
}
