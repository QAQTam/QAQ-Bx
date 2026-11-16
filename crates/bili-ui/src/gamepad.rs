//! Xbox 手柄（XInput）轮询与 Fluent 四色键位指引胶囊。
//!
//! - 仅在当前进程窗口处于系统前台时响应输入，避免后台误触；
//! - 无手柄连接时自动退避为 500ms 轻轮询（近零 CPU 占用），连接后以 16ms（~60Hz）采样；
//! - 方向键与左摇杆支持“首次触发 + 长按连发（320ms 延迟 / 90ms 间隔）”。

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use windows::Win32::processthreadsapi::GetCurrentProcessId;
use windows::Win32::winuser::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::Win32::xinput::{
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_DPAD_DOWN, XINPUT_GAMEPAD_DPAD_LEFT,
    XINPUT_GAMEPAD_DPAD_RIGHT, XINPUT_GAMEPAD_DPAD_UP, XINPUT_GAMEPAD_LEFT_SHOULDER,
    XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE, XINPUT_GAMEPAD_RIGHT_SHOULDER,
    XINPUT_GAMEPAD_TRIGGER_THRESHOLD, XINPUT_GAMEPAD_X, XINPUT_GAMEPAD_Y, XINPUT_STATE,
    XInputGetState,
};
use windows_reactor::*;

use crate::router::Route;

const ACTIVE_POLL: Duration = Duration::from_millis(16);
const IDLE_POLL: Duration = Duration::from_millis(500);
const REPEAT_INITIAL_DELAY: Duration = Duration::from_millis(320);
const REPEAT_INTERVAL: Duration = Duration::from_millis(90);
const STICK_THRESHOLD: i16 = 16_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamepadAction {
    Confirm,
    Back,
    Refresh,
    TogglePlay,
    PrevTab,
    NextTab,
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamepadEvent {
    Connected(bool),
    Action(GamepadAction),
}

/// 启动 XInput 后台轮询线程并返回事件接收端。
pub fn spawn_gamepad_poller() -> Receiver<GamepadEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("biliplay-xinput".to_string())
        .spawn(move || poll_loop(tx))
        .expect("spawn xinput poller thread");
    rx
}

fn poll_loop(tx: Sender<GamepadEvent>) {
    let mut was_connected = false;
    let mut prev_buttons: u16 = 0;
    let mut repeat = RepeatTracker::default();

    loop {
        let state = read_first_connected_pad();
        let is_connected = state.is_some();
        if is_connected != was_connected {
            was_connected = is_connected;
            if tx.send(GamepadEvent::Connected(is_connected)).is_err() {
                return;
            }
            if !is_connected {
                prev_buttons = 0;
                repeat.reset();
            }
        }

        let Some(pad) = state else {
            std::thread::sleep(IDLE_POLL);
            continue;
        };

        if !is_app_foreground() {
            prev_buttons = 0;
            repeat.reset();
            std::thread::sleep(Duration::from_millis(120));
            continue;
        }

        let buttons = pad.Gamepad.wButtons;
        let rising = buttons & !prev_buttons;
        prev_buttons = buttons;

        let edge_map = [
            (XINPUT_GAMEPAD_A as u16, GamepadAction::Confirm),
            (XINPUT_GAMEPAD_B as u16, GamepadAction::Back),
            (XINPUT_GAMEPAD_X as u16, GamepadAction::Refresh),
            (XINPUT_GAMEPAD_Y as u16, GamepadAction::TogglePlay),
            (XINPUT_GAMEPAD_LEFT_SHOULDER as u16, GamepadAction::PrevTab),
            (XINPUT_GAMEPAD_RIGHT_SHOULDER as u16, GamepadAction::NextTab),
        ];
        for (mask, action) in edge_map {
            if (rising & mask) != 0 && tx.send(GamepadEvent::Action(action)).is_err() {
                return;
            }
        }

        let lx = apply_deadzone(pad.Gamepad.sThumbLX, XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE as i16);
        let ly = apply_deadzone(pad.Gamepad.sThumbLY, XINPUT_GAMEPAD_LEFT_THUMB_DEADZONE as i16);
        let lt = pad.Gamepad.bLeftTrigger > XINPUT_GAMEPAD_TRIGGER_THRESHOLD as u8;
        let rt = pad.Gamepad.bRightTrigger > XINPUT_GAMEPAD_TRIGGER_THRESHOLD as u8;

        let left = (buttons & XINPUT_GAMEPAD_DPAD_LEFT as u16) != 0 || lx < -STICK_THRESHOLD || lt;
        let right = (buttons & XINPUT_GAMEPAD_DPAD_RIGHT as u16) != 0 || lx > STICK_THRESHOLD || rt;
        let up = (buttons & XINPUT_GAMEPAD_DPAD_UP as u16) != 0 || ly > STICK_THRESHOLD;
        let down = (buttons & XINPUT_GAMEPAD_DPAD_DOWN as u16) != 0 || ly < -STICK_THRESHOLD;

        let dir = if left {
            Some(GamepadAction::Left)
        } else if right {
            Some(GamepadAction::Right)
        } else if up {
            Some(GamepadAction::Up)
        } else if down {
            Some(GamepadAction::Down)
        } else {
            None
        };

        if let Some(action) = repeat.update(dir, Instant::now())
            && tx.send(GamepadEvent::Action(action)).is_err()
        {
            return;
        }

        std::thread::sleep(ACTIVE_POLL);
    }
}

fn read_first_connected_pad() -> Option<XINPUT_STATE> {
    let mut state = XINPUT_STATE::default();
    for idx in 0..4u32 {
        let rc = unsafe { XInputGetState(idx, &mut state) };
        if rc == 0 {
            return Some(state);
        }
    }
    None
}

fn is_app_foreground() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return false;
    }
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid != 0 && pid == GetCurrentProcessId()
    }
}

fn apply_deadzone(value: i16, deadzone: i16) -> i16 {
    if value.saturating_abs() <= deadzone {
        0
    } else {
        value
    }
}

#[derive(Default)]
struct RepeatTracker {
    held: Option<GamepadAction>,
    next_fire: Option<Instant>,
}

impl RepeatTracker {
    fn reset(&mut self) {
        self.held = None;
        self.next_fire = None;
    }

    fn update(&mut self, dir: Option<GamepadAction>, now: Instant) -> Option<GamepadAction> {
        let Some(action) = dir else {
            self.reset();
            return None;
        };
        if self.held != Some(action) {
            self.held = Some(action);
            self.next_fire = Some(now + REPEAT_INITIAL_DELAY);
            return Some(action);
        }
        if let Some(deadline) = self.next_fire
            && now >= deadline
        {
            self.next_fire = Some(now + REPEAT_INTERVAL);
            return Some(action);
        }
        None
    }
}

/// 渲染底部右侧 Xbox 四色键位指引胶囊（仅在手柄连接并激活时显示）。
pub fn gamepad_hint_bar(route: Route, focus_title: Option<&str>) -> View {
    let mut items: Vec<View> = match route {
        Route::Player => vec![
            abxy_chip("A", Color::rgb(16, 124, 16), Color::rgb(255, 255, 255), "播放/暂停"),
            abxy_chip("B", Color::rgb(232, 17, 35), Color::rgb(255, 255, 255), "返回"),
            key_chip("◀▶ / LT·RT", "快退/快进 10s"),
            key_chip("▲▼", "音量"),
            key_chip("LB/RB", "切页"),
        ],
        Route::Home => vec![
            abxy_chip("A", Color::rgb(16, 124, 16), Color::rgb(255, 255, 255), "打开选中"),
            abxy_chip("X", Color::rgb(0, 120, 212), Color::rgb(255, 255, 255), "换一换"),
            abxy_chip("Y", Color::rgb(255, 185, 0), Color::rgb(20, 20, 20), "暂停/续播"),
            abxy_chip("B", Color::rgb(232, 17, 35), Color::rgb(255, 255, 255), "返回"),
            key_chip("LB/RB", "切页"),
        ],
        _ => vec![
            abxy_chip("B", Color::rgb(232, 17, 35), Color::rgb(255, 255, 255), "返回"),
            abxy_chip("Y", Color::rgb(255, 185, 0), Color::rgb(20, 20, 20), "暂停/续播"),
            key_chip("LB/RB", "切页"),
        ],
    };

    if route == Route::Home
        && let Some(title) = focus_title
    {
        let short = truncate_chars(title, 18);
        items.insert(
            0,
            TextBlock::new()
                .text(format!("选中: {short}"))
                .font_size(11.0)
                .opacity(0.75)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }

    items.insert(
        0,
        FontIcon::new()
            .glyph("\u{E7FC}")
            .font_size(14.0)
            .foreground(ThemeBrush::AccentText)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    );

    Border::new()
        .corner_radius(16.0)
        .padding(Thickness::xy(12.0, 6.0))
        .margin(Thickness::new(16.0, 0.0, 24.0, 16.0))
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .horizontal_alignment(HorizontalAlignment::Right)
        .vertical_alignment(VerticalAlignment::Bottom)
        .content(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .children(items),
        )
        .into()
}

fn abxy_chip(letter: &str, bg: Color, fg: Color, label: &str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(5.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            Border::new()
                .width(18.0)
                .height(18.0)
                .corner_radius(9.0)
                .background(Brush::Solid(bg))
                .content(
                    TextBlock::new()
                        .text(letter)
                        .font_size(10.0)
                        .font_weight(FontWeight::BOLD)
                        .foreground(Brush::Solid(fg))
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center),
                ),
            TextBlock::new()
                .text(label)
                .font_size(11.0)
                .foreground(ThemeBrush::PrimaryText)
                .vertical_alignment(VerticalAlignment::Center),
        ))
        .into()
}

fn key_chip(key_text: &str, label: &str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(5.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            Border::new()
                .corner_radius(4.0)
                .padding(Thickness::xy(5.0, 1.0))
                .background(ThemeBrush::SolidBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .content(
                    TextBlock::new()
                        .text(key_text)
                        .font_size(10.0)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .foreground(ThemeBrush::PrimaryText),
                ),
            TextBlock::new()
                .text(label)
                .font_size(11.0)
                .foreground(ThemeBrush::PrimaryText)
                .vertical_alignment(VerticalAlignment::Center),
        ))
        .into()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_zeros_small_values() {
        assert_eq!(apply_deadzone(0, 7849), 0);
        assert_eq!(apply_deadzone(7849, 7849), 0);
        assert_eq!(apply_deadzone(-7849, 7849), 0);
        assert_eq!(apply_deadzone(20_000, 7849), 20_000);
    }

    #[test]
    fn repeat_tracker_fires_initial_then_waits_for_delay() {
        let mut tracker = RepeatTracker::default();
        let t0 = Instant::now();
        assert_eq!(tracker.update(Some(GamepadAction::Right), t0), Some(GamepadAction::Right));
        assert_eq!(
            tracker.update(Some(GamepadAction::Right), t0 + Duration::from_millis(100)),
            None
        );
        assert_eq!(
            tracker.update(Some(GamepadAction::Right), t0 + REPEAT_INITIAL_DELAY),
            Some(GamepadAction::Right)
        );
        assert_eq!(
            tracker.update(
                Some(GamepadAction::Right),
                t0 + REPEAT_INITIAL_DELAY + Duration::from_millis(30)
            ),
            None
        );
        assert_eq!(
            tracker.update(
                Some(GamepadAction::Right),
                t0 + REPEAT_INITIAL_DELAY + REPEAT_INTERVAL
            ),
            Some(GamepadAction::Right)
        );
    }
}
