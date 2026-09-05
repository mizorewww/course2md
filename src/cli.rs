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
  course2md doctor          检查依赖与配置 / Check dependencies and settings
  course2md config init     生成配置模板 / Create a configuration template
  course2md llm setup       配置可选的 AI 润色 / Set up optional AI proofreading
  course2md summarize ./notes  为已有笔记生成总结 / Summarize existing notes

交互终端首次转换会引导配置；脚本请显式传入所需参数。
First conversion in a terminal offers setup; scripts should pass options explicitly.
使用 <command> --help 查看子命令帮助 / Use <command> --help for details.
";

#[derive(Parser)]
#[command(
    name = "course2md",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("COURSE2MD_COMMIT_HASH"), ")"),
    about = "将课程视频转换为图文笔记 / Turn course videos into illustrated notes",
    args_conflicts_with_subcommands = true,
    after_help = QUICK_START
)]
pub struct Cli {
    /// 视频链接或本地文件 / Video URL or local file
    pub source: Option<String>,

    #[command(flatten)]
    pub opts: RunOpts,

    #[command(subcommand)]
    pub command: Option<Command>,
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

    /// 下载视频最大高度 / Maximum download height (1080 recommended)
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

    /// 仅比较指定区域 / Compare a region, e.g. 40%,0%-100%,100%
    #[arg(long)]
    #[arg(help_heading = "截图 / Slides")]
    pub roi: Option<String>,

    /// 语音识别线程数（至少 1）/ Speech recognition threads (minimum 1)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub threads: Option<i32>,

    /// 识别后端 / Speech backend: coreml (Apple Silicon), gpu, cpu, npu (Intel), api (cloud)
    #[arg(long, value_enum)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub provider: Option<crate::config::AsrProvider>,

    /// 识别模型（需与后端兼容）/ Speech model (must match backend)
    /// gpu/cpu: qwen3; coreml: qwen3-1.7b, qwen3-0.6b, whisper; npu/api: 按后端配置 / backend-specific
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_model: Option<String>,

    /// 云端识别地址 / Cloud speech API base URL (OpenAI-compatible)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub asr_api_base_url: Option<String>,

    /// 云端识别密钥；建议用 COURSE2MD_ASR_API_KEY 环境变量 / Cloud API key; prefer the environment variable to avoid shell history
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

    /// 每段语音最长秒数 (1–600) / Maximum speech segment length in seconds
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub max_speech: Option<f32>,

    /// 输出格式，逗号分隔 / Output formats, comma-separated: md,html,json
    #[arg(long, value_delimiter = ',', value_enum)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub formats: Option<Vec<crate::config::OutputFormat>>,

    /// 本地模型目录 / Local model directory (default: platform cache)
    #[arg(long)]
    #[arg(help_heading = "语音识别 / Speech recognition")]
    pub model_dir: Option<PathBuf>,

    /// 保留下载的视频 / Keep downloaded media.mp4
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub keep_video: bool,

    /// 成功后删除本次下载的视频 / Delete newly downloaded video after success
    #[arg(long, conflicts_with = "keep_video")]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_keep_video: bool,

    /// 复用输出目录中已有的 media.mp4 / Reuse media.mp4 already in the output directory
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub no_download: bool,

    /// 本次启用 AI 润色 / Enable AI proofreading for this run
    #[arg(long)]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub llm: bool,

    /// 本次禁用 AI 润色 / Disable AI proofreading for this run
    #[arg(long, conflicts_with = "llm")]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub no_llm: bool,

    /// AI 润色服务地址 / AI proofreading base URL (OpenAI-compatible)
    #[arg(long)]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub llm_base_url: Option<String>,

    /// AI 润色密钥（建议通过 llm setup 设置，避免命令历史记录）/ AI API key (prefer llm setup)
    #[arg(long)]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub llm_api_key: Option<String>,

    /// AI 润色模型名称 / AI proofreading model name
    #[arg(long)]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub llm_model: Option<String>,

    /// AI 润色提示词 / AI proofreading instructions
    #[arg(long)]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub llm_prompt: Option<String>,

    /// 附截图辅助润色（需要视觉模型）/ Attach slides for proofreading (requires image support)
    #[arg(long, conflicts_with = "no_llm_vision")]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub llm_vision: bool,

    /// 本次禁用截图辅助润色 / Disable image-assisted proofreading
    #[arg(long, conflicts_with = "llm_vision")]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub no_llm_vision: bool,

    /// 隐藏完成后的 AI 润色提示 / Hide the optional AI proofreading tip
    #[arg(long)]
    #[arg(help_heading = "AI 润色 / AI proofreading")]
    pub no_llm_hint: bool,

    /// 显示诊断日志；-vv 显示调试细节 / Diagnostic logs; -vv for debug details
    #[arg(short, long, action = clap::ArgAction::Count)]
    #[arg(help_heading = "终端输出 / Terminal output")]
    pub verbose: u8,

    /// 从检查点恢复，跳过已识别片段 / Resume from saved speech checkpoints
    #[arg(long)]
    #[arg(help_heading = "输入与输出 / Input and output")]
    pub resume: bool,

    /// 忽略语音识别检查点，重新识别 / Ignore speech checkpoints and transcribe again
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
    /// 管理 gpu/cpu 本地识别模型 / Manage gpu/cpu speech models
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// 配置可选的 AI 润色 / Configure optional AI proofreading
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
    /// 为已有笔记生成 AI 总结 / Add AI summaries to existing course.md/html
    Summarize(SummarizeArgs),
    /// 清除已保存的 AI 服务配置 / Clear saved AI service settings
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
    /// 笔记目录或其父目录（包含 timeline.jsonl）/ Note directories or a parent directory
    #[arg(required = true)]
    pub dirs: Vec<PathBuf>,
    /// 替换已有总结 / Replace existing summaries
    #[arg(long)]
    pub force: bool,
    /// 另存独立总结到此目录 / Also export standalone summaries to this directory
    #[arg(short = 'o', long)]
    pub out: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// 下载 gpu/cpu 模型（约 2.4GB）/ Download gpu/cpu models (~2.4GB)
    Download {
        #[arg(long)]
        dir: Option<PathBuf>,
        /// 以 NDJSON 输出进度，禁用交互 / Stream NDJSON progress without prompts
        #[arg(long)]
        json: bool,
    },
    /// 检查 gpu/cpu 模型文件 / Check downloaded gpu/cpu model files
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum LlmCmd {
    /// 配置并启用 AI 润色（终端内询问缺失项）/ Set up AI proofreading; prompts in a terminal
    Setup {
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// 关闭完成后的润色提示 / Hide the completion tip
        #[arg(long)]
        disable_hint: bool,
    },
    /// 显示当前配置（隐藏密钥）/ Show settings with keys hidden
    Status,
    /// 关闭 AI 润色并保留凭据 / Disable AI proofreading and keep credentials
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
