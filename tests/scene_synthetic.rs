//! 场景检测的合成视频回归测试。
//! 用 ffmpeg 现场生成“色块换页”视频（无需提交 fixture），验证状态机的关键行为：
//! - 换页被检出、时间戳为候选首次出现时间（而非 cooldown 到期时间）
//! - cooldown 期间检测不休眠：跳过的中间页、后续页真实起点仍正确
//!
//! 采样间隔 0.5s，采样时刻为 0、0.5、1.0…；所有换页都放在两次采样之间
//! （x.25 / x.75），源视频 10fps，离最近采样点各有 0.2s 余量。
//! 因此每个采样点看到的画面是确定的，可以断言完整的输出序列。
//!
//! 运行需要 ffmpeg；本地缺少时跳过，CI 中缺少则失败。

mod common;

use std::path::Path;
use std::process::Command;

const INTERVAL: f64 = 0.5;

/// 用 lavfi 生成白底 1280x720 10fps 视频，`pages` 依次整屏填充 (颜色, 起, 止)。
/// 黑/灰/白亮度差异大、SSIM 可区分；不用 drawtext 以兼容无 freetype 的 ffmpeg 构建。
fn make_video(path: &Path, secs: u32, pages: &[(&str, f64, f64)]) {
    let filter = pages
        .iter()
        .map(|(color, from, to)| {
            format!("drawbox=color={color}:t=fill:enable='between(t,{from},{to})'")
        })
        .collect::<Vec<_>>()
        .join(",");
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", &format!("color=c=white:s=1280x720:d={secs}:r=10")])
        .args(["-vf", &filter])
        .arg(path)
        .status()
        .expect("spawn ffmpeg");
    assert!(st.success());
}

/// 两个场景测试共享的 PipelineConfig 基座；差异字段在调用处用结构体更新语法覆盖。
fn test_cfg(video: &Path, dir: &Path) -> course2md::config::PipelineConfig {
    course2md::config::PipelineConfig {
        url: video.display().to_string(),
        out_dir: dir.to_path_buf(),
        out_root: dir.to_path_buf(),
        similarity: 0.9,
        sample_interval: INTERVAL,
        cooldown: 10.0,
        slide_mode: course2md::config::SlideMode::First,
        stable_secs: 0.0,
        max_height: 1080,
        roi: None,
        threads: 2,
        provider: course2md::config::AsrProvider::Cpu,
        max_speech: 20.0,
        formats: vec![course2md::config::OutputFormat::Md],
        model_dir: dir.to_path_buf(),
        keep_video: true,
        no_download: true,
        resume: false,
        llm: Default::default(),
        asr_api: Default::default(),
        asr_model: None,
        gpu_layers: course2md::config::DEFAULT_GPU_LAYERS,
        mmproj_offload: true,
        transcript_source: course2md::config::TranscriptSource::Asr,
    }
}

fn onsets(cfg: &course2md::config::PipelineConfig, video: &Path) -> Vec<f64> {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(course2md::scene::run(
            cfg,
            video,
            &course2md::dispatch::TaskControl::default(),
        ))
        .expect("scene run")
        .iter()
        .map(|frame| frame.t)
        .collect()
}

#[test]
fn scene_detects_slides_with_true_timestamps() {
    if !common::ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    // 0-6.25s 白 A、6.25-7.25s 黑 B（过渡页）、7.25-14.25s 灰 C、14.25-20s 白 D。
    make_video(
        &video,
        20,
        &[
            ("black", 6.25, 7.25),
            ("gray", 7.25, 14.25),
            ("white", 14.25, 20.),
        ],
    );
    let cfg = test_cfg(&video, dir.path());

    // cooldown=10s、first 模式：
    //   0s 发射 A；6.5s B 成为候选，7.5s 被 C 替换（候选持续更新，检测无盲区）；
    //   10s cooldown 到期时发射 C，时间戳是 C 首次被采到的 7.5s（而非 10s）；
    //   14.5s D 成为候选，距上次发射 < 10s 不发射，直到 EOF 冲刷补发尾页。
    assert_eq!(onsets(&cfg, &video), [0.0, 7.5, 14.5]);
}

/// stable 模式：未稳定满 stable_secs 的过渡页不发射，只发射最终稳定态，
/// 且时间线用 onset（候选首现）。
#[test]
fn stable_mode_waits_for_settled_state() {
    if !common::ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    // 0-2.25s 白、2.25-3.75s 黑（过渡页）、3.75-10s 灰（最终稳定态）。
    make_video(&video, 10, &[("black", 2.25, 3.75), ("gray", 3.75, 10.)]);
    let cfg = course2md::config::PipelineConfig {
        cooldown: 2.0,
        slide_mode: course2md::config::SlideMode::Stable,
        stable_secs: 1.2,
        ..test_cfg(&video, dir.path())
    };

    // 白页 0s 起，1.5s 时稳定满 1.2s 发射；黑页只在 2.5/3.0/3.5s 被采到，
    // 观察到的稳定时长 1.0s < 1.2s，不发射；灰页 4.0s 首现，5.5s 发射。
    assert_eq!(onsets(&cfg, &video), [0.0, 4.0]);
}
