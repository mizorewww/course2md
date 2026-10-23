//! Scrollbar presentation shared by every scrolling page.
use std::process::{Command, Stdio};
use std::thread;

/// macOS「始终显示滚动条」系统偏好（states-and-motion 滚动条契约要求尊重该偏好）。
/// 首次调用在后台线程读取 `defaults`，渲染期只读缓存值；读到之前与非 macOS 均按
/// Automatic（滚动时显示）。
pub fn scrollbars_always_visible() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ALWAYS: AtomicBool = AtomicBool::new(false);
    static PROBE: std::sync::Once = std::sync::Once::new();
    if cfg!(target_os = "macos") {
        PROBE.call_once(|| {
            thread::spawn(|| {
                let always = Command::new("defaults")
                    .args(["read", "-g", "AppleShowScrollBars"])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .filter(|out| out.status.success())
                    .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "Always");
                ALWAYS.store(always, Ordering::Relaxed);
            });
        });
    }
    ALWAYS.load(Ordering::Relaxed)
}

/// 滚动条模式：常显偏好→Always；其余滚动时显示、闲置淡出。
pub(crate) fn vertical_scrollbar(handle: &gpui::ScrollHandle) -> gpui_component::scroll::Scrollbar {
    vertical_scrollbar_for(handle)
}

/// 任意 ScrollbarHandle（含 gpui::ListState 虚拟列表）都走同一模式决策：
/// 系统「始终显示滚动条」偏好开启时常驻，否则滚动时显示。
pub(crate) fn vertical_scrollbar_for<H: gpui_component::scroll::ScrollbarHandle + Clone>(
    handle: &H,
) -> gpui_component::scroll::Scrollbar {
    use gpui_component::scroll::{Scrollbar, ScrollbarMode};
    Scrollbar::vertical(handle).mode(if scrollbars_always_visible() {
        ScrollbarMode::Always
    } else {
        ScrollbarMode::Scrolling
    })
}
