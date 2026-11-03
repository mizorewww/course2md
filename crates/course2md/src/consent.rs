//! Whether a video command may download a local speech model it lacks.
//!
//! A model is a gigabyte or more, so the command line never fetches one silently: at a
//! terminal it asks first; without one (a pipe, a script, `--json`, `--quiet`) it stops
//! and says how to prepare the model, unless `--download-models` or
//! `COURSE2MD_DOWNLOAD_MODELS=1` allows it. `models prepare` and `models download` are
//! the explicit commands and never ask. Desktop tasks (`run-task`) never download.

use crate::models::ModelOffer;
use crate::tr;
use std::io::{BufRead, Write};

/// The environment variable that allows model downloads, like `--download-models`.
pub const DOWNLOAD_MODELS_ENV: &str = "COURSE2MD_DOWNLOAD_MODELS";

/// How this run treats a missing speech model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Download it without asking.
    Allowed,
    /// Ask at the terminal.
    Ask,
    /// Stop, naming the command that prepares it.
    Refused,
}

/// Where this run stands.
#[derive(Debug, Clone, Copy, Default)]
pub struct Situation {
    /// `--download-models`, or [`DOWNLOAD_MODELS_ENV`] set to a yes.
    pub opted_in: bool,
    /// The person agreed to the download earlier in this run (a model choice).
    pub agreed: bool,
    /// stdin and stderr are both terminals.
    pub interactive: bool,
    /// `--json`: stdout carries events only, and nobody answers a question.
    pub json: bool,
    /// `--quiet`: errors only, so no question either.
    pub quiet: bool,
}

/// The decision for `situation`. An explicit opt-in wins in every output mode; a
/// question needs a person at a terminal who sees it.
pub fn decide(situation: Situation) -> Consent {
    if situation.opted_in || situation.agreed {
        Consent::Allowed
    } else if situation.interactive && !situation.json && !situation.quiet {
        Consent::Ask
    } else {
        Consent::Refused
    }
}

/// Whether [`DOWNLOAD_MODELS_ENV`] says yes.
pub fn env_opt_in(value: Option<&std::ffi::OsStr>) -> bool {
    value.and_then(|value| value.to_str()).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Whether an answer to the `[y/N]` question is a yes. Anything else, an empty line
/// included, is a no.
pub fn accepts(answer: &str) -> bool {
    matches!(
        answer.trim().to_lowercase().as_str(),
        "y" | "yes" | "是" | "はい"
    )
}

/// The question for `offer`.
pub fn question(offer: &ModelOffer) -> String {
    let size = match offer.size {
        Some(bytes) => tr!("约 {} / about {}", size_text(bytes), size_text(bytes)),
        None => tr!("大小未知 / size unknown").to_owned(),
    };
    let location = offer.location.display();
    tr!(
        "需要下载识别模型 {}（{}），保存到 {}。现在下载吗？[y/N] / The speech model {} ({}) needs to be downloaded to {}. Download it now? [y/N]",
        offer.model,
        size,
        location,
        offer.model,
        size,
        location
    )
}

fn size_text(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{} MB", bytes.div_ceil(MB))
    }
}

/// Asks the question on `output` and reads one line of `input`.
pub fn ask(offer: &ModelOffer, input: &mut impl BufRead, output: &mut impl Write) -> bool {
    let _ = write!(output, "{} ", question(offer));
    let _ = output.flush();
    let mut answer = String::new();
    input.read_line(&mut answer).is_ok() && accepts(&answer)
}

/// Asks at this terminal: the question on stderr, the answer from stdin.
pub fn ask_terminal(offer: &ModelOffer) -> anyhow::Result<bool> {
    Ok(ask(
        offer,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> ModelOffer {
        ModelOffer {
            provider: crate::config::AsrProvider::Cpu,
            model: "qwen3-1.7b".into(),
            size: Some(2_520_744_288),
            location: "/models/llama-qwen3-1.7b".into(),
            prepare_command: "course2md models prepare --provider cpu --model qwen3-1.7b".into(),
        }
    }

    #[test]
    fn only_a_person_at_a_terminal_is_asked() {
        let terminal = Situation {
            interactive: true,
            ..Default::default()
        };
        assert_eq!(decide(terminal), Consent::Ask);
        assert_eq!(decide(Situation::default()), Consent::Refused);
        for situation in [
            Situation {
                json: true,
                ..terminal
            },
            Situation {
                quiet: true,
                ..terminal
            },
        ] {
            assert_eq!(decide(situation), Consent::Refused);
        }
        for situation in [
            Situation {
                opted_in: true,
                ..Default::default()
            },
            Situation {
                opted_in: true,
                json: true,
                ..Default::default()
            },
            Situation {
                agreed: true,
                ..terminal
            },
        ] {
            assert_eq!(decide(situation), Consent::Allowed);
        }
    }

    #[test]
    fn the_environment_opts_in_only_with_a_yes() {
        for yes in ["1", "true", "YES", " on "] {
            assert!(env_opt_in(Some(yes.as_ref())), "{yes}");
        }
        for no in ["", "0", "false", "no"] {
            assert!(!env_opt_in(Some(no.as_ref())), "{no}");
        }
        assert!(!env_opt_in(None));
    }

    #[test]
    fn the_question_names_the_model_its_size_and_place_and_defaults_to_no() {
        // The words follow the interface language, which other tests set; the facts do not.
        let mut shown = Vec::new();
        assert!(ask(&offer(), &mut "y\n".as_bytes(), &mut shown));
        let shown = String::from_utf8(shown).unwrap();
        for fact in ["qwen3-1.7b", "2.3 GB", "/models/llama-qwen3-1.7b", "[y/N] "] {
            assert!(shown.contains(fact), "{fact}: {shown}");
        }
        for (answer, yes) in [
            ("yes\n", true),
            ("Y\n", true),
            ("\n", false),
            ("", false),
            ("n\n", false),
            ("maybe\n", false),
        ] {
            assert_eq!(
                ask(&offer(), &mut answer.as_bytes(), &mut Vec::new()),
                yes,
                "{answer:?}"
            );
        }
    }
}
