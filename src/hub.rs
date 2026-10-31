//! Where model files come from, for this application. `model-fetch` holds the hosts and
//! their order; this is the policy.
//!
//! `HF_ENDPOINT` is authoritative when set. Otherwise this machine's own locale and
//! time zone order the hosts, and the rest remain fallbacks, so a wrong guess (a
//! traveller, an English system in Beijing) costs one failed connection, not the
//! download. Nothing is sent anywhere to decide this.
//!
//! Asking public services for this machine's country is more accurate behind a VPN
//! or on an unusual system, but it tells them the address of everyone who downloads
//! a model. It happens only when chosen: [`allow_ip_lookup`] for an application,
//! `COURSE2MD_IP_LOOKUP=1` for a person. Then it runs only when files are missing.

use model_fetch::{Reason, local_country, lookup_country, normalize_endpoint, system_time_zone};
use std::sync::OnceLock;

pub use model_fetch::{Endpoints, MIRROR, ORIGIN};

static IP_LOOKUP: OnceLock<bool> = OnceLock::new();

/// An application's decision about public IP lookups, made before the first
/// [`endpoints`] call. Returns false if the decision was already taken.
pub fn allow_ip_lookup(allowed: bool) -> bool {
    IP_LOOKUP.set(allowed).is_ok()
}

fn ip_lookup_allowed() -> bool {
    *IP_LOOKUP.get_or_init(|| {
        std::env::var("COURSE2MD_IP_LOOKUP")
            .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
    })
}

/// The endpoints for this process, resolved once on first use.
pub fn endpoints() -> &'static Endpoints {
    static RESOLVED: OnceLock<Endpoints> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let configured = std::env::var("HF_ENDPOINT").ok();
        let country = if normalize_endpoint(configured.clone()).is_some() {
            None
        } else {
            let looked_up = ip_lookup_allowed().then(lookup_country).flatten();
            looked_up.or_else(|| {
                local_country(
                    sys_locale::get_locale().as_deref(),
                    system_time_zone().as_deref(),
                )
            })
        };
        let endpoints = Endpoints::choose(configured, country.as_deref());
        tracing::info!(
            hosts = ?endpoints.hosts,
            reason = ?endpoints.reason,
            country = country.as_deref().unwrap_or("?"),
            "model endpoints"
        );
        if endpoints.reason == Reason::MainlandChina
            && !crate::progress::is_json()
            && !crate::progress::is_quiet()
        {
            crate::eprintln_tr!(
                "位于中国大陆，优先从国内镜像下载模型（可用 HF_ENDPOINT 指定地址） / In mainland China: trying domestic mirrors first for models (HF_ENDPOINT sets a host)"
            );
        }
        endpoints
    })
}
