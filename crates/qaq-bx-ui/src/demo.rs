//! 内置自驱动演示导演器（`--demo` / `QAQ_BX_DEMO=1`）。
//!
//! - 不驱动鼠标/不调用 `SetCursorPos`，全部由 `AppShell` 内部定时节拍推进；
//! - 窗口缩放围绕初始舞台中心点居中计算，使 `ffmpeg gdigrab` 固定区域可无裁剪直录全流程。

use std::time::{Duration, Instant};

use windows::Win32::minwindef::LPARAM;
use windows::Win32::processthreadsapi::GetCurrentThreadId;
use windows::Win32::windef::{HWND, RECT};
use windows::Win32::winuser::{
    EnumThreadWindows, GW_OWNER, GetClientRect, GetDpiForWindow, GetMonitorInfoW, GetWindow,
    GetWindowRect, IsWindowVisible, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    PostMessageW, PostQuitMessage, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, WM_CLOSE,
};
use windows_core::BOOL;

use crate::layout::{
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH,
};
use crate::pages::player::PlayerTab;

/// 演示节拍刷新间隔（约 35Hz，兼顾 4c8T CPU 占用与丝滑视口缩放）。
pub const DEMO_TICK_INTERVAL: Duration = Duration::from_millis(28);
/// 完整演示总时长（ms）。
pub const DEMO_TOTAL_MS: u64 = 18_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DemoConfig {
    pub auto_exit: bool,
}

impl DemoConfig {
    /// 从 CLI 参数或环境变量解析演示开关；未启用返回 `None`（零运行时开销）。
    pub fn from_env_and_args() -> Option<Self> {
        let enabled = env_truthy("QAQ_BX_DEMO") || std::env::args().any(|a| a == "--demo");
        if !enabled {
            return None;
        }
        let auto_exit =
            env_truthy("QAQ_BX_DEMO_EXIT") || std::env::args().any(|a| a == "--demo-exit");
        Some(Self { auto_exit })
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoAction {
    OpenPopular,
    ToggleMoreMenu,
    CloseMoreMenu,
    OpenSearchOverlay,
    SetSearchDraft(&'static str),
    SubmitSearch,
    OpenDemoVideo,
    PlayerExpandDesc,
    PlayerShowComments,
    Complete,
}

const WINDOW_KEYFRAMES: &[(u64, f64, f64)] = &[
    (0, 1280.0, 800.0),
    (2_000, 1280.0, 800.0),
    (3_300, 640.0, 760.0),
    (4_000, 640.0, 760.0),
    (4_900, 390.0, 760.0),
    (8_400, 390.0, 760.0),
    (9_200, 640.0, 760.0),
    (11_400, 640.0, 760.0),
    (12_100, 390.0, 760.0),
    (15_800, 390.0, 760.0),
    (17_200, 1280.0, 800.0),
    (18_500, 1280.0, 800.0),
];

const DEMO_TIMELINE: &[(u64, DemoAction)] = &[
    (5_100, DemoAction::OpenPopular),
    (6_900, DemoAction::ToggleMoreMenu),
    (8_400, DemoAction::CloseMoreMenu),
    (9_300, DemoAction::OpenSearchOverlay),
    (9_700, DemoAction::SetSearchDraft("Rust WinUI")),
    (10_200, DemoAction::SubmitSearch),
    (12_200, DemoAction::OpenDemoVideo),
    (13_400, DemoAction::PlayerExpandDesc),
    (14_500, DemoAction::PlayerShowComments),
    (DEMO_TOTAL_MS, DemoAction::Complete),
];

#[cfg(test)]
pub fn demo_events() -> &'static [(u64, DemoAction)] {
    DEMO_TIMELINE
}

/// 三次缓入缓出曲线，[0, 1] -> [0, 1]。
pub fn ease_in_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// 返回演示时刻 `ms` 对应的目标客户区逻辑宽高（DIP）。
pub fn window_target_at_ms(ms: u64) -> (f64, f64) {
    let first = WINDOW_KEYFRAMES[0];
    if ms <= first.0 {
        return clamp_client_dips(first.1, first.2);
    }
    for pair in WINDOW_KEYFRAMES.windows(2) {
        let (t0, w0, h0) = pair[0];
        let (t1, w1, h1) = pair[1];
        if ms <= t1 {
            if t1 <= t0 || ((w1 - w0).abs() < f64::EPSILON && (h1 - h0).abs() < f64::EPSILON) {
                return clamp_client_dips(w1, h1);
            }
            let raw_t = (ms - t0) as f64 / (t1 - t0) as f64;
            let k = ease_in_out_cubic(raw_t);
            let w = w0 + (w1 - w0) * k;
            let h = h0 + (h1 - h0) * k;
            return clamp_client_dips(w, h);
        }
    }
    let last = WINDOW_KEYFRAMES[WINDOW_KEYFRAMES.len() - 1];
    clamp_client_dips(last.1, last.2)
}

fn clamp_client_dips(w: f64, h: f64) -> (f64, f64) {
    (
        w.clamp(MIN_WINDOW_WIDTH, DEFAULT_WINDOW_WIDTH),
        h.clamp(MIN_WINDOW_HEIGHT, DEFAULT_WINDOW_HEIGHT),
    )
}

/// 根据初始舞台中心点和边框厚度计算居中的外框物理坐标 `(x, y, outer_w, outer_h)`。
pub fn center_outer_rect(
    stage_cx: i32,
    stage_cy: i32,
    client_w_dip: f64,
    client_h_dip: f64,
    dpi: u32,
    chrome_w: i32,
    chrome_h: i32,
) -> (i32, i32, i32, i32) {
    let scale = f64::from(dpi.max(96)) / 96.0;
    let client_w_px = (client_w_dip * scale).round() as i32;
    let client_h_px = (client_h_dip * scale).round() as i32;
    let outer_w = (client_w_px + chrome_w.max(0)).max(1);
    let outer_h = (client_h_px + chrome_h.max(0)).max(1);
    let x = stage_cx - outer_w / 2;
    let y = (stage_cy - outer_h / 2).max(0);
    (x, y, outer_w, outer_h)
}

pub struct DemoState {
    config: DemoConfig,
    started_at: Instant,
    next_event_idx: usize,
    hwnd_raw: isize,
    chrome_pad: Option<(i32, i32)>,
    stage_center: Option<(i32, i32)>,
    dpi: u32,
    last_applied_px: Option<(i32, i32)>,
    finished: bool,
    /// 通过 `PlayerInput::demo_override` 在播放页自动展开简介 / 切到评论标签。
    pub player_override: Option<(u64, PlayerTab, bool)>,
}

impl DemoState {
    pub fn new(config: DemoConfig) -> Self {
        Self {
            config,
            started_at: Instant::now(),
            next_event_idx: 0,
            hwnd_raw: 0,
            chrome_pad: None,
            stage_center: None,
            dpi: 96,
            last_applied_px: None,
            finished: false,
            player_override: None,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 推进一步内部演示时间线并同步 Win32 窗口尺寸（绕开鼠标，不移动光标）。
    pub fn step(&mut self) -> Vec<DemoAction> {
        if self.finished {
            return Vec::new();
        }
        let elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        let (target_w, target_h) = window_target_at_ms(elapsed_ms);
        self.apply_window_size(target_w, target_h);

        let mut fired = Vec::new();
        while let Some(&(at_ms, action)) = DEMO_TIMELINE.get(self.next_event_idx) {
            if elapsed_ms < at_ms {
                break;
            }
            self.next_event_idx += 1;
            if matches!(action, DemoAction::Complete) {
                self.finished = true;
            }
            fired.push(action);
        }
        fired
    }

    pub fn request_exit_if_configured(&self) {
        if !self.config.auto_exit {
            return;
        }
        if let Some(hwnd) = self.current_hwnd() {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE as u32, 0, 0);
            }
        } else {
            unsafe {
                PostQuitMessage(0);
            }
        }
    }

    fn current_hwnd(&self) -> Option<HWND> {
        if self.hwnd_raw == 0 {
            None
        } else {
            Some(self.hwnd_raw as HWND)
        }
    }

    fn ensure_window_metrics(&mut self) -> Option<HWND> {
        if self.hwnd_raw == 0 {
            let hwnd = find_demo_hwnd()?;
            self.hwnd_raw = hwnd as isize;
        }
        let hwnd = self.current_hwnd()?;
        if self.chrome_pad.is_some() && self.stage_center.is_some() {
            return Some(hwnd);
        }

        let mut wr = RECT::default();
        let mut cr = RECT::default();
        let ok_w = unsafe { GetWindowRect(hwnd, &mut wr).as_bool() };
        let ok_c = unsafe { GetClientRect(hwnd, &mut cr).as_bool() };
        if !ok_w || !ok_c || (cr.right - cr.left) < 100 {
            return None;
        }

        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let chrome_w = ((wr.right - wr.left) - (cr.right - cr.left)).max(0);
        let chrome_h = ((wr.bottom - wr.top) - (cr.bottom - cr.top)).max(0);
        let (cx, cy) = monitor_work_center(hwnd)
            .unwrap_or(((wr.left + wr.right) / 2, (wr.top + wr.bottom) / 2));

        self.dpi = dpi;
        self.chrome_pad = Some((chrome_w, chrome_h));
        self.stage_center = Some((cx, cy));
        Some(hwnd)
    }

    fn apply_window_size(&mut self, client_w_dip: f64, client_h_dip: f64) {
        let Some(hwnd) = self.ensure_window_metrics() else {
            return;
        };
        let (Some((chrome_w, chrome_h)), Some((cx, cy))) = (self.chrome_pad, self.stage_center)
        else {
            return;
        };
        let (x, y, outer_w, outer_h) = center_outer_rect(
            cx,
            cy,
            client_w_dip,
            client_h_dip,
            self.dpi,
            chrome_w,
            chrome_h,
        );
        if self.last_applied_px == Some((outer_w, outer_h)) {
            return;
        }
        self.last_applied_px = Some((outer_w, outer_h));
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                outer_w,
                outer_h,
                (SWP_NOZORDER | SWP_NOACTIVATE) as u32,
            );
        }
    }
}

fn find_demo_hwnd() -> Option<HWND> {
    let mut found: HWND = std::ptr::null_mut();
    let tid = unsafe { GetCurrentThreadId() };
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let visible = IsWindowVisible(hwnd).as_bool();
            let owner = GetWindow(hwnd, GW_OWNER as u32);
            if visible && owner.is_null() {
                let mut cr = RECT::default();
                if GetClientRect(hwnd, &mut cr).as_bool() && (cr.right - cr.left) >= 200 {
                    let out = &mut *(lparam as *mut HWND);
                    *out = hwnd;
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
    }
    unsafe {
        let _ = EnumThreadWindows(tid, Some(enum_proc), &mut found as *mut HWND as LPARAM);
    }
    if found.is_null() { None } else { Some(found) }
}

fn monitor_work_center(hwnd: HWND) -> Option<(i32, i32)> {
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST as u32) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        let cx = (info.rcWork.left + info.rcWork.right) / 2;
        let cy = (info.rcWork.top + info.rcWork.bottom) / 2;
        Some((cx, cy))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_in_out_cubic_bounds_and_symmetry() {
        assert_eq!(ease_in_out_cubic(-0.5), 0.0);
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-9);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert_eq!(ease_in_out_cubic(1.5), 1.0);
        let a = ease_in_out_cubic(0.25);
        let b = ease_in_out_cubic(0.75);
        assert!((a + b - 1.0).abs() < 1e-9);
    }

    #[test]
    fn window_timeline_covers_desktop_medium_mobile_and_expands() {
        assert_eq!(window_target_at_ms(0), (1280.0, 800.0));
        assert_eq!(window_target_at_ms(2_000), (1280.0, 800.0));
        let (mid_w, _) = window_target_at_ms(2_650);
        assert!(mid_w > 640.0 && mid_w < 1280.0);
        assert_eq!(window_target_at_ms(3_300), (640.0, 760.0));
        assert_eq!(window_target_at_ms(4_900), (390.0, 760.0));
        assert_eq!(window_target_at_ms(9_200), (640.0, 760.0));
        assert_eq!(window_target_at_ms(12_100), (390.0, 760.0));
        assert_eq!(window_target_at_ms(17_200), (1280.0, 800.0));
        assert_eq!(window_target_at_ms(DEMO_TOTAL_MS + 1000), (1280.0, 800.0));
    }

    #[test]
    fn demo_events_monotonic_and_ordered() {
        let events = demo_events();
        assert!(!events.is_empty());
        for pair in events.windows(2) {
            assert!(pair[0].0 < pair[1].0);
        }
        assert_eq!(events.last().unwrap().1, DemoAction::Complete);
        assert_eq!(events.last().unwrap().0, DEMO_TOTAL_MS);
    }

    #[test]
    fn center_outer_rect_keeps_midpoint_and_clamps_top() {
        let (x, y, w, h) = center_outer_rect(960, 540, 390.0, 760.0, 96, 16, 39);
        assert_eq!((w, h), (406, 799));
        assert_eq!(x + w / 2, 960);
        assert_eq!(y + h / 2, 540);

        let (_x2, y2, _w2, h2) = center_outer_rect(960, 100, 1280.0, 800.0, 144, 16, 39);
        assert!(y2 >= 0);
        assert!(h2 > 1200);
    }
}
