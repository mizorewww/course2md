//! What publishing a service changes: its defaults, and which input or task it binds.
use crate::preferences::{BindingScope, ServicePurpose};
use c2m_engine::execution::Component;

/// What a service publication continues with once its credential is checked.
pub struct PublishPlan {
    pub draft_id: String,
    pub continue_repair: bool,
    pub target: Option<String>,
    pub repair_task: Option<(String, Vec<Component>)>,
    pub scope: BindingScope,
}

/// Where a published service applies. A repair, or a service chosen while
/// editing one input, binds to that task alone unless the person also made it
/// the default; a service saved in settings is the default only when chosen so.
pub fn publish_scope(repairing: bool, from_input: bool, also_default: bool) -> BindingScope {
    if repairing || (from_input && !also_default) {
        BindingScope::CurrentTask
    } else if also_default {
        BindingScope::Defaults
    } else {
        BindingScope::Saved
    }
}

/// The service the input records after a publish: its own binding, or none so
/// that it follows the defaults the publish just changed.
pub fn draft_service(scope: BindingScope, id: &str) -> Option<String> {
    (scope == BindingScope::CurrentTask).then(|| id.to_owned())
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
    use super::{BindingScope, draft_service, publish_scope};

    #[test]
    fn a_repair_binds_only_its_task_whatever_the_default_choice() {
        for (from_input, also_default) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            assert_eq!(
                publish_scope(true, from_input, also_default),
                BindingScope::CurrentTask
            );
        }
    }

    #[test]
    fn a_service_chosen_for_an_input_binds_it_unless_it_becomes_the_default() {
        let scope = publish_scope(false, true, false);
        assert_eq!(scope, BindingScope::CurrentTask);
        assert_eq!(draft_service(scope, "svc").as_deref(), Some("svc"));

        // Made the default as well: the input follows the new default instead
        // of pinning a copy of it.
        let scope = publish_scope(false, true, true);
        assert_eq!(scope, BindingScope::Defaults);
        assert_eq!(draft_service(scope, "svc"), None);
    }

    #[test]
    fn a_service_saved_in_settings_changes_defaults_only_when_asked() {
        assert_eq!(publish_scope(false, false, false), BindingScope::Saved);
        assert_eq!(publish_scope(false, false, true), BindingScope::Defaults);
    }
}
