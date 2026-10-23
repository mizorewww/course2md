use crate::i18n::{Lang, t};
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
    about = crate::tr!("将课程视频转换为图文笔记 / Turn course videos into illustrated notes"),
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

#[derive(Args, Clone, Debug, Default)]
pub struct RunOpts {
    /// 输出根目录（按平台/标题/ID 建目录）/ Output root (platform/title/ID)
    #[arg(short, long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub out: Option<PathBuf>,

    /// 画面相似度阈值 (0,1]，越高截图越多 / Similarity threshold; higher captures more slides
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub similarity: Option<f64>,

    /// 画面采样间隔（秒）/ Frame sampling interval in seconds
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub sample_interval: Option<f64>,

    /// 截图最短间隔（秒）/ Minimum seconds between screenshots
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub cooldown: Option<f64>,

    /// 下载视频最大高度（推荐 1080）/ Maximum download height (1080 recommended)
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub max_height: Option<u32>,

    /// 截图时机：first 立即，stable 等画面稳定 / Capture immediately or wait for stable slides
    #[arg(long, value_enum)]
    #[arg(help_heading = "截图 / Slides")]
    pub slide_mode: Option<crate::config::SlideMode>,

    /// stable 模式的画面稳定时间（秒）/ Seconds to wait for a stable frame
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub stable_secs: Option<f64>,

    /// 只看画面的某块区域判断翻页，格式：左%,上%-右%,下% / Detect slide changes in a region: left%,top%-right%,bottom%
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub roi: Option<String>,

    /// 语音识别线程数（至少 1）/ Speech recognition threads (minimum 1)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub threads: Option<i32>,

    /// 识别后端：coreml（Apple 芯片）、gpu、cpu、npu（Intel）、api（云端）/ Speech backend: coreml (Apple Silicon), gpu, cpu, npu (Intel), api (cloud)
    #[arg(long, value_enum)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub provider: Option<crate::config::AsrProvider>,

    /// 识别模型（需与后端兼容）/ Speech model (must match the backend)
    /// gpu/cpu: qwen3-1.7b；coreml: qwen3-1.7b、qwen3-0.6b、whisper；npu: whisper 或 Hugging Face 仓库名 / or a Hugging Face repository
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_model: Option<String>,

    /// 云端识别地址 / Cloud speech API base URL (OpenAI-compatible)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_base_url: Option<String>,

    /// 云端识别密钥；建议改用 COURSE2MD_ASR_API_KEY 环境变量，避免留在命令历史 / Cloud API key; prefer COURSE2MD_ASR_API_KEY to keep it out of shell history
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_key: Option<String>,

    /// 云端识别模型名称 / Cloud speech model name
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_model: Option<String>,

    /// 云端请求方式 / Cloud request mode: transcriptions (default) or chat (audio-capable models)
    #[arg(long, value_enum)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_mode: Option<crate::settings::AsrApiMode>,

    /// 文字来源：auto 优先字幕，subtitle 仅字幕，asr 语音识别 / Transcript source: auto, subtitle, asr
    #[arg(long, value_enum)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub transcript_source: Option<crate::config::TranscriptSource>,

    /// 放到 GPU 上运行的层数（0–99，默认全部）；核显不稳定时调低 / Layers run on the GPU (0–99, all by default); lower on unstable integrated GPUs
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=99))]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub gpu_layers: Option<u32>,

    /// 音频编码部分也用 GPU（默认）/ Also run the audio encoder on the GPU (default)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub mmproj_offload: bool,

    /// 音频编码部分改用 CPU（显存不足时）/ Run the audio encoder on the CPU (when GPU memory is low)
    #[arg(long, conflicts_with = "mmproj_offload")]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub no_mmproj_offload: bool,

    /// 每段语音最长秒数 (1–600) / Maximum speech segment length in seconds
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub max_speech: Option<f32>,

    /// 输出格式，逗号分隔 / Output formats, comma-separated: md,html,json
    #[arg(long, value_delimiter = ',', value_enum)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub formats: Option<Vec<crate::config::OutputFormat>>,

    /// 本地模型目录（默认使用系统缓存）/ Local model directory (default: platform cache)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub model_dir: Option<PathBuf>,

    /// 保留下载的视频 / Keep the downloaded video
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub keep_video: bool,

    /// 成功后删除本次下载的视频 / Delete newly downloaded video after success
    #[arg(long, conflicts_with = "keep_video")]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_keep_video: bool,

    /// 不重新下载，使用上次下载的视频 / Reuse the previously downloaded video
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_download: bool,

    /// 本次启用 AI 校对 / Enable AI proofreading for this run
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm: bool,

    /// 本次禁用 AI 校对 / Disable AI proofreading for this run
    #[arg(long, conflicts_with = "llm")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_llm: bool,

    /// AI 校对服务地址 / AI proofreading base URL (OpenAI-compatible)
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_base_url: Option<String>,

    /// AI 校对密钥（建议用 llm setup 设置，避免留在命令历史）/ AI proofreading key (prefer llm setup to keep it out of shell history)
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_api_key: Option<String>,

    /// AI 校对模型名称 / AI proofreading model name
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_model: Option<String>,

    /// AI 校对提示词 / AI proofreading instructions
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_prompt: Option<String>,

    /// 附截图辅助校对（需要视觉模型）/ Attach slides for proofreading (requires image support)
    #[arg(long, conflicts_with = "no_llm_vision")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub llm_vision: bool,

    /// 本次禁用截图辅助校对 / Disable image-assisted proofreading
    #[arg(long, conflicts_with = "llm_vision")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_llm_vision: bool,

    /// 隐藏完成后的 AI 校对提示 / Hide the optional AI proofreading tip
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_llm_hint: bool,

    /// 本次生成视频摘要并写入笔记 / Generate a video summary and add it to the notes
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub summarize: bool,

    /// 本次禁用视频摘要 / Disable video summary for this run
    #[arg(long, conflicts_with = "summarize")]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_summarize: bool,

    /// 双语笔记：把文字翻译为该语言（zh-Hans、zh-Hant、en、ja、ko、fr、de、es） / Bilingual notes: translate the text into this language (zh-Hans, zh-Hant, en, ja, ko, fr, de, es)
    #[arg(long, value_name = "LANG", conflicts_with = "no_translate", value_parser = parse_translation_target)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub translate: Option<String>,

    /// 本次不翻译 / Do not translate this run
    #[arg(long)]
    #[arg(help_heading = "AI 校对 / AI proofreading")]
    pub no_translate: bool,

    /// 显示诊断日志；-vv 显示调试细节 / Diagnostic logs; -vv for debug details
    #[arg(short, long, action = clap::ArgAction::Count)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub verbose: u8,

    /// 继续上次中断的识别（默认开启）/ Continue interrupted transcription (on by default)
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub resume: bool,

    /// 从头重新识别 / Transcribe again from the start
    #[arg(long, conflicts_with = "resume")]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_resume: bool,

    /// 静默模式，仅报告错误 / Quiet mode: report errors only
    #[arg(short, long)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub quiet: bool,

    /// 以 NDJSON 输出进度，禁用交互 / Stream NDJSON progress without prompts
    #[arg(long)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// 执行已保存的任务；从标准输入读取完整 JSON 参数 / Execute a saved task from stdin JSON
    #[command(hide = true)]
    RunTask,
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
    #[arg(long, default_value = crate::config::DEFAULT_ASR_MODEL)]
    pub model: String,
    /// 模型目录 / Model directory
    #[arg(long)]
    pub dir: Option<PathBuf>,
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
    let mut literal = false;
    while index < args.len() {
        let arg = args[index].to_string_lossy().into_owned();
        if literal {
            index += 1;
            continue;
        }
        if arg == "--" {
            literal = true;
            index += 1;
            continue;
        }
        if arg == "--lang" {
            if index + 1 < args.len() {
                value = Some(args[index + 1].to_string_lossy().into_owned());
                args.drain(index..index + 2);
                continue;
            }
            // Leave a value-less flag for clap to report the missing value.
            index += 1;
            continue;
        }
        if let Some(rest) = arg.strip_prefix("--lang=") {
            value = Some(rest.to_owned());
            args.remove(index);
            continue;
        }
        index += 1;
    }
    value
}

/// Replaces clap's compile-time help and about texts with catalog translations.
///
/// The catalog is keyed by the original text, so the compiled doc comments stay
/// the single source of truth and a missing entry falls back to the source.
pub fn localize_command(command: clap::Command) -> clap::Command {
    use clap::Command;

    fn localized(value: Option<&clap::builder::StyledStr>) -> Option<String> {
        value.map(|value| t(&value.to_string()).to_owned())
    }

    fn about(command: Command) -> Command {
        let about = localized(command.get_about());
        let long_about = localized(command.get_long_about());
        let before = localized(command.get_before_help());
        let after = localized(command.get_after_help());
        let mut command = command;
        if let Some(value) = about {
            command = command.about(value);
        }
        if let Some(value) = long_about {
            command = command.long_about(value);
        }
        if let Some(value) = before {
            command = command.before_help(value);
        }
        if let Some(value) = after {
            command = command.after_help(value);
        }
        command
    }

    fn args(command: Command) -> Command {
        command.mut_args(|arg| {
            let help = localized(arg.get_help());
            let long_help = localized(arg.get_long_help());
            let heading = arg.get_help_heading().map(|heading| {
                // clap's heading type only accepts static strings; this runs once per process.
                Box::leak(t(heading).to_owned().into_boxed_str()) as &'static str
            });
            let mut arg = arg;
            if let Some(value) = help {
                arg = arg.help(value);
            }
            if let Some(value) = long_help {
                arg = arg.long_help(value);
            }
            if let Some(value) = heading {
                arg = arg.help_heading(value);
            }
            arg
        })
    }

    let command = about(command);
    let command = args(command);
    command.mut_subcommands(localize_command)
}

/// Localizes clap's own section headings in rendered help and error text.
///
/// Clap has no runtime translations for these strings; replacing whole lines
/// keeps the argument help itself (already localized) untouched.
pub fn localize_rendered(text: &str) -> String {
    let lang = crate::i18n::lang();
    if lang == Lang::En {
        return text.to_owned();
    }
    let (usage, options, commands, arguments, more) = match lang {
        Lang::ZhHans => (
            crate::tr!("用法："),
            crate::tr!("选项："),
            crate::tr!("命令："),
            crate::tr!("参数："),
            crate::tr!("更多信息请运行 --help。"),
        ),
        Lang::ZhHant => (
            crate::tr!("用法："),
            crate::tr!("選項："),
            crate::tr!("命令："),
            crate::tr!("參數："),
            crate::tr!("更多資訊請執行 --help。"),
        ),
        Lang::Ja => (
            crate::tr!("使用法："),
            crate::tr!("オプション："),
            crate::tr!("コマンド："),
            crate::tr!("引数："),
            crate::tr!("詳細は --help を参照してください。"),
        ),
        Lang::En => unreachable!(),
    };
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start();
        let localized = if trimmed.starts_with("Usage:") {
            Some(line.replacen("Usage:", usage, 1))
        } else if trimmed.starts_with("Options:") {
            Some(line.replacen("Options:", options, 1))
        } else if trimmed.starts_with("Commands:") {
            Some(line.replacen("Commands:", commands, 1))
        } else if trimmed.starts_with("Arguments:") {
            Some(line.replacen("Arguments:", arguments, 1))
        } else if trimmed.starts_with("For more information, try '--help'.") {
            Some(line.replacen("For more information, try '--help'.", more, 1))
        } else if line.contains("Print help") {
            Some(line.replacen("Print help", more_help(lang), 1))
        } else if line.contains("Print version") {
            Some(line.replacen("Print version", more_version(lang), 1))
        } else if line.contains("Print this message or the help of the given subcommand(s)") {
            Some(line.replacen(
                "Print this message or the help of the given subcommand(s)",
                more_help_command(lang),
                1,
            ))
        } else {
            None
        };
        out.push_str(localized.as_deref().unwrap_or(line));
        out.push('\n');
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

fn more_help(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhHans => crate::tr!("显示帮助"),
        Lang::ZhHant => crate::tr!("顯示說明"),
        Lang::Ja => crate::tr!("ヘルプを表示"),
        Lang::En => "Print help",
    }
}

fn more_help_command(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhHans => crate::tr!("显示此消息或子命令的帮助"),
        Lang::ZhHant => crate::tr!("顯示此訊息或子命令的說明"),
        Lang::Ja => crate::tr!("このメッセージまたはサブコマンドのヘルプを表示"),
        Lang::En => "Print this message or the help of the given subcommand(s)",
    }
}

fn more_version(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhHans => crate::tr!("显示版本"),
        Lang::ZhHant => crate::tr!("顯示版本"),
        Lang::Ja => crate::tr!("バージョンを表示"),
        Lang::En => "Print version",
    }
}

fn parse_translation_target(value: &str) -> Result<String, String> {
    crate::language::target(value)
        .map(str::to_owned)
        .ok_or_else(|| {
            let codes: Vec<_> = crate::language::TARGETS
                .iter()
                .map(|(code, _)| *code)
                .collect();
            crate::tr!("支持的语言：{}", codes.join(", "))
        })
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
