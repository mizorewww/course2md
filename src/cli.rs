use crate::i18n::{Lang, t};
pub use crate::options::RunOpts;
use crate::tr;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

pub const QUICK_START: &str = "快速开始 / Quick start:
  course2md ./lecture.mp4
  course2md https://www.youtube.com/watch?v=VIDEO_ID
  course2md ./lecture.mp4 -o ./notes

转写方式 / Transcripts:
  默认优先使用平台字幕，无字幕时使用语音识别。
  Uses platform subtitles first, then speech recognition when needed.
  --transcript-source subtitle  仅使用字幕 / Subtitles only
  --transcript-source asr       强制语音识别 / Speech recognition only

下一步 / Next steps:
  course2md --login bilibili    扫码登录 / Log in with a QR code
  course2md --logout bilibili   清除登录 / Remove saved login
  course2md doctor              检查依赖与配置 / Check dependencies and settings
  course2md config init         生成配置模板 / Create a configuration template
  course2md llm setup           配置可选的 AI 校对 / Set up optional AI proofreading
  course2md summarize ./notes   为已有笔记生成摘要 / Summarize existing notes

交互终端首次转换会引导配置；脚本请显式传入所需参数。
First conversion in a terminal offers setup; scripts should pass options explicitly.
使用 <command> --help 查看子命令帮助 / Use <command> --help for details.
";

#[derive(Parser)]
#[command(
    name = "course2md",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("COURSE2MD_COMMIT_HASH"), ")"),
    about = tr!("将课程视频转换为图文笔记 / Turn course videos into illustrated notes"),
    args_conflicts_with_subcommands = true,
    after_help = QUICK_START
)]
pub struct Cli {
    /// 界面语言：auto、en、zh-Hans、zh-Hant、ja / Interface language: auto, en, zh-Hans, zh-Hant, ja
    #[arg(long, global = true, value_name = "LANG")]
    pub lang: Option<String>,

    /// 登录（bilibili 扫码；ollama 本地服务；codex 订阅），可接视频地址继续转换 / Log in (bilibili QR, ollama local, codex subscription), optionally then process a video
    #[arg(long, value_enum, conflicts_with_all = ["logout", "json"])]
    pub login: Option<LoginPlatform>,

    /// 清除保存的登录凭据 / Remove saved login credentials
    #[arg(long, value_enum, conflicts_with_all = ["login", "source", "json"])]
    pub logout: Option<LoginPlatform>,

    /// 视频链接或本地文件 / Video URL or local file
    pub source: Option<String>,

    #[command(flatten)]
    pub opts: RunOpts,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum LoginPlatform {
    Bilibili,
    Ollama,
    Codex,
}

/// How a helper process reports progress: as its parent does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ChildProgress {
    Json,
    Quiet,
    Human,
}

#[derive(Subcommand)]
pub enum Command {
    /// 执行已保存的任务；从标准输入读取完整 JSON 参数 / Execute a saved task from stdin JSON
    #[command(hide = true)]
    RunTask,
    /// 在独立进程中准备 Apple 模型缓存 / Prepare the Apple model cache in its own process
    #[command(hide = true)]
    AppleCache {
        model: String,
        #[arg(long, value_enum, default_value_t = ChildProgress::Human)]
        progress: ChildProgress,
    },
    /// 管理本地识别模型 / Manage local speech models
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// 配置可选的 AI 校对 / Configure optional AI proofreading
    Llm {
        #[command(subcommand)]
        cmd: LlmCmd,
    },
    /// 检查依赖、识别后端和配置 / Check tools, speech backends and settings
    Doctor,
    /// 管理配置文件 / Manage the configuration file
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// 为已有笔记生成 AI 摘要 / Add AI summaries to existing notes
    Summarize(SummarizeArgs),
    /// 清除 AI 校对的地址、密钥、模型和 Codex 登录 / Clear the AI proofreading endpoint, key, model and Codex login
    Remove(RemoveArgs),
}

#[derive(Args)]
pub struct RemoveArgs {
    /// 同时清除云端语音识别密钥 / Also clear the cloud speech API key
    #[arg(long)]
    pub asr: bool,
}

#[derive(Args)]
pub struct SummarizeArgs {
    /// 笔记目录，或存放多份笔记的上级目录 / A note directory or a folder of notes
    #[arg(required = true)]
    pub dirs: Vec<PathBuf>,
    /// 重新生成已有摘要 / Regenerate existing summaries
    #[arg(long)]
    pub force: bool,
    /// 另存一份摘要（Markdown）到此目录 / Also save each summary as Markdown in this directory
    #[arg(short = 'o', long)]
    pub out: Option<PathBuf>,
    /// 静默模式，仅报告错误 / Quiet mode: report errors only
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// 检查模型是否已下载 / Check whether a model is downloaded
    Inspect {
        #[command(flatten)]
        target: ModelTarget,
        /// 以 JSON 输出 / Print JSON
        #[arg(long)]
        json: bool,
    },
    /// 下载并验证模型 / Download and verify a model
    Prepare {
        #[command(flatten)]
        target: ModelTarget,
        /// 以 NDJSON 输出进度，禁用交互 / Stream NDJSON progress without prompts
        #[arg(long)]
        json: bool,
    },
    /// 下载 gpu/cpu 模型（约 2.4GB）/ Download gpu/cpu models (~2.4GB)
    Download {
        /// 模型目录 / Model directory
        #[arg(long)]
        dir: Option<PathBuf>,
        /// 以 NDJSON 输出进度，禁用交互 / Stream NDJSON progress without prompts
        #[arg(long)]
        json: bool,
    },
    /// 检查 gpu/cpu 模型文件 / Check downloaded gpu/cpu model files
    List {
        /// 模型目录 / Model directory
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct ModelTarget {
    /// 识别方式 / Speech backend
    #[arg(long, value_enum)]
    pub provider: crate::config::AsrProvider,
    /// 模型（默认为该识别方式的默认模型）/ Model (defaults to the backend's default)
    #[arg(long)]
    pub model: Option<String>,
    /// 模型目录 / Model directory
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

impl ModelTarget {
    /// The requested model; empty means the backend's default.
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or_default()
    }
}

#[derive(Subcommand)]
pub enum LlmCmd {
    /// 配置并启用 AI 校对（终端内询问缺失项）/ Set up AI proofreading; prompts in a terminal
    Setup {
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// 关闭完成后的校对提示 / Hide the completion tip
        #[arg(long)]
        disable_hint: bool,
    },
    /// 显示当前配置（隐藏密钥）/ Show settings with keys hidden
    Status,
    /// 关闭 AI 校对并保留凭据 / Disable AI proofreading and keep credentials
    Disable,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// 生成配置模板（--force 覆盖已有配置）/ Create a template; --force replaces existing settings
    Init {
        #[arg(long)]
        force: bool,
    },
    /// 显示配置路径和文件设置 / Show the config path and file settings
    Show,
}

/// Removes `--lang` from the argument list before clap parses it.
///
/// `--lang` is a global option, but clap's `args_conflicts_with_subcommands`
/// would otherwise reject `course2md --lang en doctor`. Keeping the flag in the
/// command definition documents it in `--help`; taking it out here lets it
/// appear before or after a subcommand.
pub fn take_lang_arg(args: &mut Vec<std::ffi::OsString>) -> Option<String> {
    let mut value = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_string_lossy().into_owned();
        if arg == "--" {
            break;
        }
        // A value-less flag stays for clap to report the missing value.
        if arg == "--lang" && index + 1 < args.len() {
            value = Some(args[index + 1].to_string_lossy().into_owned());
            args.drain(index..index + 2);
        } else if let Some(rest) = arg.strip_prefix("--lang=") {
            value = Some(rest.to_owned());
            args.remove(index);
        } else {
            index += 1;
        }
    }
    value
}

/// Replaces clap's compile-time help and about texts with catalog translations.
///
/// The catalog is keyed by the original text, so the compiled doc comments stay
/// the single source of truth and a missing entry falls back to the source.
pub fn localize_command(command: clap::Command) -> clap::Command {
    use clap::builder::{Resettable, StyledStr};
    fn localized(value: Option<&StyledStr>) -> Resettable<StyledStr> {
        value.map_or(Resettable::Reset, |value| {
            Resettable::Value(t(&value.to_string()).to_owned().into())
        })
    }
    let about = localized(command.get_about());
    let long_about = localized(command.get_long_about());
    let before = localized(command.get_before_help());
    let after = localized(command.get_after_help());
    command
        .about(about)
        .long_about(long_about)
        .before_help(before)
        .after_help(after)
        .mut_args(|arg| {
            let help = localized(arg.get_help());
            let long_help = localized(arg.get_long_help());
            // clap's heading type only accepts static strings; this runs once per process.
            let heading = arg
                .get_help_heading()
                .map(|heading| &*Box::leak(t(heading).to_owned().into_boxed_str()));
            arg.help(help).long_help(long_help).help_heading(heading)
        })
        .mut_subcommands(localize_command)
}

/// Localizes clap's own section headings in rendered help and error text.
///
/// Clap has no runtime translations for these strings; replacing them keeps the
/// argument help itself (already localized) untouched.
pub fn localize_rendered(text: &str) -> String {
    if crate::i18n::lang() == Lang::En {
        return text.to_owned();
    }
    let headings = [
        ("Usage:", tr!("用法：")),
        ("Options:", tr!("选项：")),
        ("Commands:", tr!("命令：")),
        ("Arguments:", tr!("参数：")),
        (
            "For more information, try '--help'.",
            tr!("更多信息请运行 --help。"),
        ),
    ];
    let phrases = [
        ("Print help", tr!("显示帮助")),
        ("Print version", tr!("显示版本")),
        (
            "Print this message or the help of the given subcommand(s)",
            tr!("显示此消息或子命令的帮助"),
        ),
    ];
    text.split_inclusive('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            headings
                .iter()
                .find(|(english, _)| trimmed.starts_with(english))
                .or_else(|| phrases.iter().find(|(english, _)| line.contains(english)))
                .map_or_else(
                    || line.to_owned(),
                    |(english, local)| line.replacen(english, local, 1),
                )
        })
        .collect()
}

#[cfg(test)]
mod i18n_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn rendered_headings_follow_the_language() {
        crate::i18n::set_lang(Lang::ZhHans);
        let rendered = "Usage: course2md [OPTIONS]\n\nOptions:\n  -h, --help  Print help\n";
        let localized = localize_rendered(rendered);
        assert!(localized.contains("用法："));
        assert!(localized.contains("选项："));
        assert!(localized.contains("显示帮助"));
    }

    #[test]
    fn command_help_uses_the_catalog() {
        crate::i18n::set_lang(Lang::ZhHans);
        let mut command = localize_command(Cli::command());
        let rendered = command.render_help().to_string();
        assert!(rendered.contains("快速开始"), "{rendered}");
    }
}

pub fn login(platform: LoginPlatform) -> anyhow::Result<()> {
    match platform {
        LoginPlatform::Bilibili => crate::auth::login_bilibili(),
        LoginPlatform::Ollama => crate::login::ollama::login(),
        LoginPlatform::Codex => crate::login::codex::login(),
    }
}

pub fn logout(platform: LoginPlatform) -> anyhow::Result<()> {
    match platform {
        LoginPlatform::Bilibili => crate::auth::logout_bilibili(),
        LoginPlatform::Ollama => crate::login::ollama::logout(),
        LoginPlatform::Codex => crate::login::codex::logout(),
    }
}
