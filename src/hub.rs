//! Where model files come from: Hugging Face, or domestic copies for mainland China.
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
//!
//! hf-mirror.com proxies the Hub API but redirects large files to Hugging
//! Face's overseas CDN; ModelScope serves the same repositories, byte for
//! byte, from a mainland CDN. Direct file downloads therefore try ModelScope
//! first there, while libraries that speak the Hub API get the mirror.

use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const ORIGIN: &str = "https://huggingface.co";
pub const MIRROR: &str = "https://hf-mirror.com";
const MODELSCOPE: &str = "https://modelscope.cn";

/// How long a download waits for the country before trying the origin first.
const LOOKUP_DEADLINE: Duration = Duration::from_secs(3);
/// After a non-mainland answer, how long the other services may still say CN.
/// Split-routing proxies send domestic services direct and foreign ones abroad.
const LOOKUP_GRACE: Duration = Duration::from_millis(800);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// A Hub-compatible base URL: the origin, the mirror or `HF_ENDPOINT`.
    Hub(String),
    ModelScope,
}

impl Host {
    pub fn file_url(&self, repo: &str, file: &str) -> String {
        match self {
            Self::Hub(base) => format!("{base}/{repo}/resolve/main/{file}"),
            Self::ModelScope => format!("{MODELSCOPE}/models/{repo}/resolve/master/{file}"),
        }
    }

    /// `huggingface.co`, `hf-mirror.com`, `modelscope.cn`: shown with progress.
    pub fn name(&self) -> String {
        match self {
            Self::Hub(base) => host_name(base),
            Self::ModelScope => host_name(MODELSCOPE),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// `HF_ENDPOINT` was set; nothing else is tried.
    Configured,
    MainlandChina,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Hosts in the order they are tried.
    pub hosts: Vec<Host>,
    pub reason: Reason,
}

impl Endpoints {
    /// The Hub endpoint for libraries that take `HF_ENDPOINT` (Apple and NPU
    /// backends), when it should differ from what the environment already says.
    pub fn hub_override(&self) -> Option<&'static str> {
        (self.reason == Reason::MainlandChina).then_some(MIRROR)
    }
}

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
        let country = if normalize(configured.clone()).is_some() {
            None
        } else {
            let looked_up = ip_lookup_allowed().then(lookup_country).flatten();
            looked_up.or_else(|| local_country(sys_locale::get_locale().as_deref(), time_zone().as_deref()))
        };
        let endpoints = choose(configured, country.as_deref());
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

/// `CN` when this machine says it is set up for mainland China: a locale in that
/// region (`zh-CN`, `en-CN`, `zh_CN.UTF-8`), or a mainland time zone. Taiwan, Hong
/// Kong and Macau have their own regions and zones and keep the default order.
fn local_country(locale: Option<&str>, time_zone: Option<&str>) -> Option<String> {
    let region = locale.is_some_and(|locale| {
        locale
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .split(['-', '_'])
            .skip(1)
            .any(|subtag| subtag.eq_ignore_ascii_case("CN"))
    });
    let zone = time_zone.is_some_and(|zone| {
        // `/usr/share/zoneinfo/Asia/Shanghai`, `:Asia/Shanghai` or a bare name.
        let name = zone.rsplit("zoneinfo/").next().unwrap_or(zone);
        matches!(
            name.trim_start_matches(':').trim(),
            "Asia/Shanghai"
                | "Asia/Urumqi"
                | "Asia/Chongqing"
                | "Asia/Chungking"
                | "Asia/Harbin"
                | "Asia/Kashgar"
                | "PRC"
        )
    });
    (region || zone).then(|| "CN".to_owned())
}

/// The system's IANA time zone, where it can be read without another dependency:
/// `TZ`, then `/etc/localtime`'s link. Windows has neither; its locale decides.
fn time_zone() -> Option<String> {
    std::env::var("TZ")
        .ok()
        .filter(|zone| !zone.trim().is_empty())
        .or_else(|| {
            std::fs::read_link("/etc/localtime")
                .ok()
                .map(|target| target.to_string_lossy().into_owned())
        })
}

fn normalize(endpoint: Option<String>) -> Option<String> {
    endpoint
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

fn choose(configured: Option<String>, country: Option<&str>) -> Endpoints {
    if let Some(endpoint) = normalize(configured) {
        return Endpoints {
            hosts: vec![Host::Hub(endpoint)],
            reason: Reason::Configured,
        };
    }
    let origin = Host::Hub(ORIGIN.into());
    let mirror = Host::Hub(MIRROR.into());
    if country == Some("CN") {
        Endpoints {
            hosts: vec![Host::ModelScope, mirror, origin],
            reason: Reason::MainlandChina,
        }
    } else {
        Endpoints {
            hosts: vec![origin, Host::ModelScope, mirror],
            reason: Reason::Default,
        }
    }
}

/// `https://hf-mirror.com/a/b` → `hf-mirror.com`, for progress and messages.
pub fn host_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

type Parser = fn(&str) -> Option<String>;

/// Services answer from different networks: Cloudflare and ipinfo abroad,
/// ipip.net reliably from inside mainland China. The first answer wins.
const PROBES: [(&str, Parser); 3] = [
    ("https://www.cloudflare.com/cdn-cgi/trace", parse_trace),
    ("https://myip.ipip.net/json", parse_ipip),
    ("https://ipinfo.io/country", parse_ipinfo),
];

/// ISO country code of this machine's public IP, or `None` when no service
/// answered in time. Any mainland answer wins (see [`LOOKUP_GRACE`]).
fn lookup_country() -> Option<String> {
    let (sender, receiver) = mpsc::channel();
    for (url, parse) in PROBES {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let agent = ureq::Agent::config_builder()
                .proxy(None)
                .timeout_global(Some(LOOKUP_DEADLINE))
                .build()
                .new_agent();
            let country = agent
                .get(url)
                .call()
                .ok()
                .and_then(|mut response| response.body_mut().read_to_string().ok())
                .and_then(|body| parse(&body));
            let _ = sender.send(country);
        });
    }
    drop(sender);
    let mut deadline = Instant::now() + LOOKUP_DEADLINE;
    let mut answer = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match receiver.recv_timeout(left) {
            Ok(Some(country)) if country == "CN" => return Some(country),
            Ok(Some(country)) => {
                if answer.is_none() {
                    deadline = deadline.min(Instant::now() + LOOKUP_GRACE);
                    answer = Some(country);
                }
            }
            Ok(None) => continue,
            Err(_) => break,
        }
    }
    answer
}

fn country_code(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 2 && value.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| value.to_ascii_uppercase())
}

fn parse_trace(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix("loc="))
        .and_then(country_code)
}

fn parse_ipinfo(body: &str) -> Option<String> {
    country_code(body)
}

/// `{"ret":"ok","data":{"location":["中国","广东","深圳",...]}}`. ipip.net
/// lists Hong Kong, Macau and Taiwan under 中国 with the region second.
fn parse_ipip(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let location = value.pointer("/data/location")?.as_array()?;
    let country = location.first()?.as_str()?;
    let region = location.get(1).and_then(|v| v.as_str()).unwrap_or_default();
    Some(match (country, region) {
        ("中国", "香港") => "HK".into(),
        ("中国", "澳门") => "MO".into(),
        ("中国", "台湾") => "TW".into(),
        ("中国", _) => "CN".into(),
        _ => "ZZ".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_endpoint_is_the_only_host() {
        let chosen = choose(Some(" https://example.org/ ".into()), Some("CN"));
        assert_eq!(chosen.hosts, [Host::Hub("https://example.org".into())]);
        assert_eq!(chosen.hub_override(), None);
        assert_eq!(chosen.reason, Reason::Configured);
        assert_eq!(choose(Some("  ".into()), None).reason, Reason::Default);
    }

    #[test]
    fn mainland_china_prefers_domestic_hosts_and_keeps_the_origin() {
        let origin = Host::Hub(ORIGIN.into());
        let mirror = Host::Hub(MIRROR.into());
        let chosen = choose(None, Some("CN"));
        assert_eq!(
            chosen.hosts,
            [Host::ModelScope, mirror.clone(), origin.clone()]
        );
        assert_eq!(chosen.hub_override(), Some(MIRROR));
        for country in [None, Some("HK"), Some("US")] {
            let chosen = choose(None, country);
            assert_eq!(chosen.hosts[0], origin);
            assert_eq!(chosen.hub_override(), None);
        }
    }

    #[test]
    fn local_settings_alone_find_mainland_china() {
        for (locale, zone) in [
            (Some("zh-CN"), None),
            (Some("zh-Hans-CN"), None),
            (Some("zh_CN.UTF-8"), None),
            (Some("en-CN"), None),
            (Some("en-US"), Some("Asia/Shanghai")),
            (None, Some("/var/db/timezone/zoneinfo/Asia/Shanghai")),
            (None, Some("/usr/share/zoneinfo/Asia/Urumqi")),
            (None, Some(":PRC")),
        ] {
            assert_eq!(
                local_country(locale, zone).as_deref(),
                Some("CN"),
                "{locale:?} {zone:?}"
            );
        }
        for (locale, zone) in [
            (Some("zh-TW"), Some("Asia/Taipei")),
            (Some("zh-HK"), Some("Asia/Hong_Kong")),
            (Some("zh-Hant-MO"), Some("Asia/Macau")),
            (Some("zh"), Some("America/Vancouver")),
            (Some("en-US"), Some("UTC")),
            // Neither a language nor a word that merely contains the region.
            (Some("cn"), None),
            (Some("en_US.CN"), None),
            (None, Some("Not/Asia/Shanghai")),
            (None, None),
        ] {
            assert_eq!(local_country(locale, zone), None, "{locale:?} {zone:?}");
        }
    }

    #[test]
    fn file_urls_follow_each_host() {
        let repo = "ggml-org/Qwen3-ASR-1.7B-GGUF";
        assert_eq!(
            Host::Hub(MIRROR.into()).file_url(repo, "a.gguf"),
            "https://hf-mirror.com/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main/a.gguf"
        );
        assert_eq!(
            Host::ModelScope.file_url(repo, "a.gguf"),
            "https://modelscope.cn/models/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/master/a.gguf"
        );
        assert_eq!(Host::ModelScope.name(), "modelscope.cn");
    }

    #[test]
    fn lookup_answers_parse() {
        assert_eq!(
            parse_trace("fl=1\nip=1.2.3.4\nloc=CN\ntls=TLSv1.3\n").as_deref(),
            Some("CN")
        );
        assert_eq!(parse_trace("ip=1.2.3.4\n"), None);
        assert_eq!(parse_ipinfo("us\n").as_deref(), Some("US"));
        assert_eq!(parse_ipinfo("<html>"), None);
        let ipip = |place: &str| {
            parse_ipip(&format!(
                r#"{{"ret":"ok","data":{{"ip":"1.2.3.4","location":[{place}]}}}}"#
            ))
        };
        assert_eq!(
            ipip(r#""中国","广东","深圳","","电信""#).as_deref(),
            Some("CN")
        );
        assert_eq!(ipip(r#""中国","香港","""#).as_deref(), Some("HK"));
        assert_eq!(ipip(r#""美国","加利福尼亚""#).as_deref(), Some("ZZ"));
    }

    #[test]
    fn host_names_drop_scheme_and_path() {
        assert_eq!(host_name("https://hf-mirror.com/a/b.gguf"), "hf-mirror.com");
        assert_eq!(host_name(ORIGIN), "huggingface.co");
    }
}
