//! CLI 国际化：跟随系统 locale（LC_ALL/LC_MESSAGES/LANG 以 zh 开头 → 中文，否则英文）。
//! 用法：`i18n::tr("English", "中文")`；CLI 帮助文本在 main 里统一改写。

use std::sync::OnceLock;

static ZH: OnceLock<bool> = OnceLock::new();

pub fn init() -> bool {
    *ZH.get_or_init(detect)
}

fn detect() -> bool {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.to_ascii_lowercase();
            if v.starts_with("zh") {
                return true;
            }
            if v == "c" || v == "posix" {
                continue;
            }
            return false;
        }
    }
    false
}

pub fn is_zh() -> bool {
    *ZH.get_or_init(detect)
}

/// 按当前语言返回文案。
pub fn tr<'a>(en: &'a str, zh: &'a str) -> &'a str {
    if is_zh() { zh } else { en }
}

/// 把 clap Command 的帮助文案替换为当前语言。
pub fn apply_cli(cmd: &mut clap::Command) {
    apply_cli_inner(cmd, is_zh());
}

fn arg_help(cmd: &mut clap::Command, id: &str, help: &str) {
    let h = help.to_string();
    *cmd = std::mem::take(cmd).mut_arg(id, move |a| a.help(h));
}

fn sub_about(cmd: &mut clap::Command, path: &[&str], about: &str) {
    let about = about.to_string();
    let mut c = cmd;
    for p in path {
        c = match c.find_subcommand_mut(p) {
            Some(s) => s,
            None => return,
        };
    }
    *c = std::mem::take(c).about(about);
}

fn apply_cli_inner(cmd: &mut clap::Command, zh: bool) {
    if !zh {
        // 英文是 derive 内置文案，无需改写
        return;
    }
    *cmd = std::mem::take(cmd).about("把网课视频转成带截图的文字稿");
    let after = Some(
        "示例：\n  course2md https://www.bilibili.com/video/BV1pb8o6yE8f\n  course2md https://youtu.be/dQw4w9WgXcQ\n  course2md ./lecture.mp4\n  course2md models download\n  course2md config init   # 生成配置文件模板\n  course2md setup         # 体检并自动安装缺失的外部工具\n  course2md llm setup     # 配置 LLM 字幕润色\n  course2md summarize <输出目录|输出根>  # 为已有输出生成视频总结（支持批量）\n  course2md remove                    # 清除 LLM/STT 的 API 配置（提交代码前执行）",
    );
    *cmd = std::mem::take(cmd).after_help(after);

    for (id, help) in [
        ("source", "视频链接或本地文件"),
        ("out", "输出根目录（其下按 平台/标题/编号 归类）"),
        ("similarity", "画面相似度阈值，越高越敏感（截图更多）"),
        ("sample_interval", "每隔几秒检查一次画面"),
        ("cooldown", "新截图之后至少间隔多少秒"),
        ("roi", "只比较画面中的区域，如 40%,0%-100%,100%"),
        ("threads", "识别线程数"),
        (
            "provider",
            "识别后端：gpu（默认推荐，Metal/CUDA）/ npu（Intel NPU加速）/ coreml / cpu / api（云端）",
        ),
        (
            "asr_model",
            "识别模型：qwen3（强烈推荐 Qwen3-ASR 1.7B）| whisper（多语言）| tiny | base",
        ),
        (
            "asr_api_base_url",
            "云端 STT base URL（OpenAI 兼容，如 https://openrouter.ai/api/v1）",
        ),
        (
            "asr_api_key",
            "云端 STT API Key（也读 OPENROUTER_API_KEY 环境变量）",
        ),
        (
            "asr_api_model",
            "云端 STT 模型（如 qwen/qwen3-asr-flash-2026-02-10）",
        ),
        ("max_speech", "单段语音最长秒数（过长会切分）"),
        ("formats", "输出格式"),
        ("model_dir", "模型目录（默认 ~/.cache/course2md/models）"),
        ("keep_video", "保留下载的视频文件（media.mp4）"),
        ("no_download", "跳过下载（目录里已有视频）"),
        ("llm", "本次运行启用 LLM 字幕润色（覆盖配置文件）"),
        ("no_llm", "本次运行禁用 LLM 字幕润色"),
        ("llm_base_url", "覆盖 LLM base URL（OpenAI 兼容）"),
        ("llm_api_key", "覆盖 LLM API Key"),
        ("llm_model", "覆盖 LLM 模型名"),
        ("llm_prompt", "覆盖 LLM 校对提示词"),
        (
            "llm_vision",
            "视觉润色：请求附对应幻灯片截图，辅助纠正技术词汇（需多模态模型）",
        ),
        ("no_llm_vision", "本次运行关闭视觉润色"),
        ("no_llm_hint", "关闭结束时「可开启 LLM」提示（本次运行）"),
        ("verbose", "更详细日志"),
        ("quiet", "只显示错误"),
        ("no_install", "本次运行禁用外部工具自动安装"),
    ] {
        arg_help(cmd, id, help);
    }

    sub_about(cmd, &["models"], "模型管理");
    sub_about(cmd, &["models", "download"], "下载离线识别模型（约 2.4GB）");
    sub_about(cmd, &["models", "list"], "查看已下载的模型");
    sub_about(cmd, &["llm"], "LLM 字幕润色配置");
    sub_about(
        cmd,
        &["llm", "setup"],
        "交互式配置并开启（缺省项会提示输入；也可全部用参数传入）",
    );
    sub_about(cmd, &["llm", "status"], "查看当前配置（密钥打码）");
    sub_about(cmd, &["llm", "disable"], "关闭 LLM 润色（保留凭据）");
    sub_about(
        cmd,
        &["doctor"],
        "环境体检：依赖工具/后端可用性/配置/模型缓存",
    );
    sub_about(
        cmd,
        &["setup"],
        "体检并自动安装缺失的外部工具（ffmpeg/yt-dlp/llama-server/uv，下载到私有工具目录）",
    );
    sub_about(cmd, &["setup", "check"], "只报告，不做任何更改");
    sub_about(cmd, &["setup", "yes"], "跳过确认直接安装");
    sub_about(
        cmd,
        &["setup", "all"],
        "可选工具（llama-server/uv）也一并安装",
    );
    sub_about(cmd, &["config"], "配置文件管理");
    sub_about(
        cmd,
        &["config", "init"],
        "生成带注释的配置文件模板（已存在则拒绝，--force 覆盖）",
    );
    sub_about(cmd, &["config", "show"], "查看配置文件路径与文件中的设置");
    sub_about(
        cmd,
        &["summarize"],
        "用 LLM 为已有输出目录生成视频总结（写入 course.md/html）",
    );
    sub_about(
        cmd,
        &["remove"],
        "清除 LLM/STT 的 API 配置（凭据清除，提交代码前可执行）",
    );
}
