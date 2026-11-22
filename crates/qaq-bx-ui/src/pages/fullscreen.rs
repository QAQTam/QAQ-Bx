//! 全屏播放页：独立沉浸式视频画布 + 自隐悬浮播控 HUD + 快捷键与双击路由（低耦合独立模块）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use qaq_bx_video::engine::VideoEngine;
use qaq_bx_video::native::NativePlayer;
use qaq_bx_video::stage::DanmakuStage;
use windows_reactor::*;

use crate::data::VideoItem;
use crate::pages::player::{
    danmaku_toggle_button, fullscreen_toggle_button, hud_foreground, hud_scrim_color,
    mute_toggle_button, play_pause_button, quality_selector, seek_slider, surface_with_loading,
    time_label, volume_slider,
};
use crate::widgets::video_surface::{VideoSurface, VideoSurfaceInput};
use crate::widgets::danmaku_surface::{DanmakuSurface, DanmakuSurfaceInput};
use crate::widgets::native_surface::{NativeSurface, NativeSurfaceInput};

const HUD_AUTO_HIDE_DELAY: Duration = Duration::from_secs(3);
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(380);
const DOUBLE_CLICK_MAX_DELTA: f64 = 8.0;
const POINTER_WAKE_THRESHOLD: f64 = 2.0;
const VK_F11: VirtualKey = VirtualKey(0x7A);

/// 视频画面左键双击检测器（纯值状态机，供全屏页、播放页与半窗抽屉共用）。
#[derive(Clone, Debug, Default)]
pub struct DoubleClickTracker {
    last_press: Option<(Instant, f64, f64)>,
}

impl DoubleClickTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次左键按下；在时间窗与位移阈值内命中第二次按下时返回 `true` 并重置状态防三连触发。
    pub fn record_press(&mut self, at: Instant, x: f64, y: f64, left_pressed: bool) -> bool {
        if !left_pressed || !x.is_finite() || !y.is_finite() {
            self.last_press = None;
            return false;
        }
        if let Some((prev_at, prev_x, prev_y)) = self.last_press {
            let elapsed = at.saturating_duration_since(prev_at);
            let dist = ((x - prev_x).powi(2) + (y - prev_y).powi(2)).sqrt();
            if elapsed <= DOUBLE_CLICK_WINDOW && dist <= DOUBLE_CLICK_MAX_DELTA {
                self.last_press = None;
                return true;
            }
        }
        self.last_press = Some((at, x, y));
        false
    }
}

/// 播放器全局快捷键动作（由 `resolve_player_hotkey` 纯函数从 `KeyEventInfo` 映射）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerHotkey {
    ToggleFullscreen,
    ExitFullscreen,
    TogglePlay,
    SeekDelta(i32),
    VolumeDelta(i32),
    ToggleMute,
    ToggleDanmaku,
}

/// 依据按键事件、播放器可见性、全屏态与输入框焦点状态判定是否触发播放器快捷键。
pub fn resolve_player_hotkey(
    ev: &KeyEventInfo,
    player_visible: bool,
    is_fullscreen: bool,
    text_focused: bool,
) -> Option<PlayerHotkey> {
    if ev.status.is_released {
        return None;
    }
    let has_cmd_modifiers = ev.modifiers.contains(InputModifiers::CONTROL)
        || ev.modifiers.contains(InputModifiers::ALT)
        || ev.modifiers.contains(InputModifiers::WINDOWS);
    if has_cmd_modifiers {
        return None;
    }

    let key = ev.key;
    if key == VK_F11
        && player_visible
        && !ev.status.was_down
        && ev.modifiers == InputModifiers::NONE
    {
        return Some(PlayerHotkey::ToggleFullscreen);
    }
    if key == VirtualKey::ESCAPE && is_fullscreen && !ev.status.was_down {
        return Some(PlayerHotkey::ExitFullscreen);
    }
    if text_focused || !player_visible {
        return None;
    }

    if key == VirtualKey::F && !ev.status.was_down {
        return Some(PlayerHotkey::ToggleFullscreen);
    }
    if key == VirtualKey::SPACE && !ev.status.was_down {
        return Some(PlayerHotkey::TogglePlay);
    }

    if !is_fullscreen {
        return None;
    }
    if key == VirtualKey::M && !ev.status.was_down {
        return Some(PlayerHotkey::ToggleMute);
    }
    if key == VirtualKey::D && !ev.status.was_down {
        return Some(PlayerHotkey::ToggleDanmaku);
    }
    if key == VirtualKey::LEFT {
        return Some(PlayerHotkey::SeekDelta(-5));
    }
    if key == VirtualKey::RIGHT {
        return Some(PlayerHotkey::SeekDelta(5));
    }
    if key == VirtualKey::UP {
        return Some(PlayerHotkey::VolumeDelta(5));
    }
    if key == VirtualKey::DOWN {
        return Some(PlayerHotkey::VolumeDelta(-5));
    }
    None
}

pub fn pointer_moved_meaningfully(prev: Option<(f64, f64)>, next: (f64, f64)) -> bool {
    if !next.0.is_finite() || !next.1.is_finite() {
        return false;
    }
    match prev {
        None => true,
        Some((px, py)) => {
            ((next.0 - px).powi(2) + (next.1 - py).powi(2)).sqrt() >= POINTER_WAKE_THRESHOLD
        }
    }
}

pub fn hud_should_auto_hide(
    timer_seq: u64,
    current_seq: u64,
    pointer_over_hud: bool,
    playing: bool,
    loading: bool,
    has_notice: bool,
) -> bool {
    timer_seq == current_seq && !pointer_over_hud && playing && !loading && !has_notice
}

#[derive(Clone)]
pub struct FullscreenInput {
    pub item: VideoItem,
    pub playing: bool,
    pub position: u32,
    pub duration_secs: u32,
    pub volume: u32,
    pub danmaku_enabled: bool,
    /// 播放页画质菜单((qn, 标签) 降序;F2)。
    pub quality_menu: Vec<(i64, String)>,
    /// 当前生效清晰度代号(0 = 未加载)。
    pub current_qn: i64,
    pub mount_tick: u32,
    pub notice: Option<String>,
    pub loading: bool,
    pub window_width: f64,
    pub engine: Arc<VideoEngine>,
    /// v0.3:native 系统媒体引擎(QAQ_BX_NATIVE=1 时全屏页也走 MPE + 透明弹幕层;
    /// 面板重建由引擎的断点续播协议接管)。
    pub native: Option<Arc<NativePlayer>>,
    pub stage: Option<Arc<DanmakuStage>>,
    pub on_toggle: Callback<()>,
    pub on_seek: Callback<f64>,
    pub on_volume: Callback<f64>,
    /// 音量条滚轮(±5/格,复用 shell `VolumeDelta` 钳制)。
    pub on_volume_wheel: Callback<i32>,
    pub on_toggle_mute: Callback<()>,
    pub on_toggle_danmaku: Callback<()>,
    pub on_select_quality: Callback<i64>,
    pub on_exit_fullscreen: Callback<()>,
    /// HUD 覆盖模式:不创建渲染面,只画悬浮播控。全屏时播放面由 shell 中
    /// 稳定 keyed 的播放页继续持有,组件不被销毁(否则 native 必须换源重载)。
    pub hud_only: bool,
}

impl PartialEq for FullscreenInput {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.playing == other.playing
            && self.position == other.position
            && self.duration_secs == other.duration_secs
            && self.volume == other.volume
            && self.danmaku_enabled == other.danmaku_enabled
            && self.quality_menu == other.quality_menu
            && self.current_qn == other.current_qn
            && self.mount_tick == other.mount_tick
            && self.notice == other.notice
            && self.loading == other.loading
            && (self.window_width - other.window_width).abs() < 0.5
            && Arc::ptr_eq(&self.engine, &other.engine)
            && match (&self.native, &other.native) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
            && match (&self.stage, &other.stage) {
                (Some(a), Some(b)) => a == b,
                (None, None) => true,
                _ => false,
            }
            && self.on_toggle == other.on_toggle
            && self.on_seek == other.on_seek
            && self.on_volume == other.on_volume
            && self.on_volume_wheel == other.on_volume_wheel
            && self.on_toggle_mute == other.on_toggle_mute
            && self.on_toggle_danmaku == other.on_toggle_danmaku
            && self.on_select_quality == other.on_select_quality
            && self.on_exit_fullscreen == other.on_exit_fullscreen
            && self.hud_only == other.hud_only
    }
}

pub struct FullscreenPlayer {
    hud_visible: bool,
    hud_seq: u64,
    hud_timer: Option<ComponentTimer>,
    pointer_over_hud: bool,
    last_pointer_pos: Option<(f64, f64)>,
    last_playing: bool,
    last_loading: bool,
    last_has_notice: bool,
    double_click: DoubleClickTracker,
    root_ref: ElementRef<Border>,
}

#[derive(Clone, PartialEq)]
pub enum Message {
    PointerMoved(f64, f64),
    CanvasPressed(f64, f64, bool),
    HudHovered(bool),
    AutoHideHud(u64),
}

impl FullscreenPlayer {
    fn arm_auto_hide(
        &mut self,
        playing: bool,
        loading: bool,
        has_notice: bool,
        context: &ComponentContext<Self>,
    ) {
        self.hud_seq = self.hud_seq.wrapping_add(1);
        let seq = self.hud_seq;
        if playing && !loading && !has_notice && !self.pointer_over_hud {
            self.hud_timer = Some(
                context.set_local_timeout(HUD_AUTO_HIDE_DELAY, move || Message::AutoHideHud(seq)),
            );
        } else {
            self.hud_timer = None;
        }
    }

    fn keep_hud_pinned(&self) -> bool {
        !self.last_playing || self.last_loading || self.last_has_notice
    }
}

impl Component for FullscreenPlayer {
    type Input = FullscreenInput;
    type Message = Message;

    fn create(input: &FullscreenInput, context: &ComponentContext<Self>) -> Self {
        let mut this = Self {
            hud_visible: true,
            hud_seq: 1,
            hud_timer: None,
            pointer_over_hud: false,
            last_pointer_pos: None,
            last_playing: input.playing,
            last_loading: input.loading,
            last_has_notice: input.notice.is_some(),
            double_click: DoubleClickTracker::new(),
            root_ref: ElementRef::new(),
        };
        this.arm_auto_hide(
            input.playing,
            input.loading,
            input.notice.is_some(),
            context,
        );
        this
    }

    fn input_changed(&mut self, input: &FullscreenInput, context: &ComponentContext<Self>) {
        let has_notice = input.notice.is_some();
        let was_pinned = self.keep_hud_pinned();
        let now_pinned = !input.playing || input.loading || has_notice;
        self.last_playing = input.playing;
        self.last_loading = input.loading;
        self.last_has_notice = has_notice;

        if now_pinned {
            self.hud_visible = true;
            self.hud_seq = self.hud_seq.wrapping_add(1);
            self.hud_timer = None;
        } else if was_pinned && !now_pinned && self.hud_visible {
            self.arm_auto_hide(input.playing, input.loading, has_notice, context);
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::PointerMoved(x, y) => {
                if pointer_moved_meaningfully(self.last_pointer_pos, (x, y)) {
                    self.last_pointer_pos = Some((x, y));
                    self.hud_visible = true;
                    self.arm_auto_hide(
                        self.last_playing,
                        self.last_loading,
                        self.last_has_notice,
                        context,
                    );
                }
            }
            Message::CanvasPressed(x, y, left_pressed) => {
                if self
                    .double_click
                    .record_press(Instant::now(), x, y, left_pressed)
                {
                    context.input().on_exit_fullscreen.call(());
                    return;
                }
                if !left_pressed {
                    return;
                }
                self.last_pointer_pos = Some((x, y));
                if self.keep_hud_pinned() {
                    self.hud_visible = true;
                    return;
                }
                if self.hud_visible {
                    self.hud_visible = false;
                    self.hud_seq = self.hud_seq.wrapping_add(1);
                    self.hud_timer = None;
                    let _ = self.root_ref.request_focus();
                } else {
                    self.hud_visible = true;
                    self.arm_auto_hide(
                        self.last_playing,
                        self.last_loading,
                        self.last_has_notice,
                        context,
                    );
                }
            }
            Message::HudHovered(hovered) => {
                self.pointer_over_hud = hovered;
                if hovered {
                    self.hud_seq = self.hud_seq.wrapping_add(1);
                    self.hud_timer = None;
                } else if self.hud_visible {
                    self.arm_auto_hide(
                        self.last_playing,
                        self.last_loading,
                        self.last_has_notice,
                        context,
                    );
                }
            }
            Message::AutoHideHud(seq) => {
                if hud_should_auto_hide(
                    seq,
                    self.hud_seq,
                    self.pointer_over_hud,
                    self.last_playing,
                    self.last_loading,
                    self.last_has_notice,
                ) {
                    self.hud_visible = false;
                    self.hud_timer = None;
                    let _ = self.root_ref.request_focus();
                }
            }
        }
    }

    fn view(&self, input: &FullscreenInput, context: &mut ViewContext<Self>) -> View {
        let root_ref = self.root_ref.clone();
        context.use_effect("fullscreen-focus", (), move || {
            let _ = root_ref.request_focus();
            None
        });

        let surface: View = if let Some(notice) = &input.notice {
            Border::new()
                .background(Brush::Solid(Color::rgb(12, 12, 12)))
                .content(
                    StackPanel::new()
                        .spacing(10.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text("视频通道异常")
                                .font_size(18.0)
                                .font_weight(FontWeight::SEMI_BOLD)
                                .foreground(Brush::Solid(Color::rgb(255, 255, 255))),
                            TextBlock::new()
                                .text(notice.clone())
                                .font_size(13.0)
                                .opacity(0.75)
                                .foreground(Brush::Solid(Color::rgb(255, 255, 255)))
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                )
                .into()
        } else if let Some(native) = &input.native {
            // v0.3:native 渲染面(MPE)+ 同 cell 弹幕透明层(结构固定,不随
            // 弹幕开关翻转,避免视图树重建把面板拆走)。
            let mut views: Vec<View> = Vec::with_capacity(2);
            views.push(
                View::component::<NativeSurface>(NativeSurfaceInput {
                    player: native.clone(),
                    tick: input.mount_tick,
                })
                .into(),
            );
            if let Some(stage) = &input.stage {
                views.push(
                    View::component::<DanmakuSurface>(DanmakuSurfaceInput {
                        stage: stage.clone(),
                        tick: input.mount_tick,
                    })
                    .into(),
                );
            }
            Grid::new().children(views).into()
        } else {
            View::component::<VideoSurface>(VideoSurfaceInput {
                engine: input.engine.clone(),
                tick: input.mount_tick,
            })
        };
        // WinUI 3 DirectComposition workaround: without a non-zero corner radius clip on the
        // enclosing Border, MediaPlayerPresenter's HWCompMediaNode can be occluded/dropped when
        // filling the entire fullscreen client rect over a solid backdrop.
        let surface = Border::new()
            .corner_radius(1.0)
            .content(surface_with_loading(surface, input.loading));

        let hit_overlay =
            Border::new()
                .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                .focus_on_pointer_release(true)
                .on_pointer_moved(context.callback(|ev: PointerEventInfo| {
                    Message::PointerMoved(ev.window_x, ev.window_y)
                }))
                .on_pointer_pressed(context.callback(|ev: PointerEventInfo| {
                    Message::CanvasPressed(ev.window_x, ev.window_y, ev.is_left_button_pressed)
                }));

        // HUD 覆盖模式下不挂载任何渲染面:播放面由 shell 中稳定 keyed 的播放页
        // 继续持有。普通模式下 VideoSurface 仍在 keyed("fs-canvas") 首层。
        // hud_only 补一块全窗命中层(fs-hit):根 Border 虽透明可命中但无处理器,
        // 没有它画布点击/移动全部落空 —— HUD 自动隐藏后无法用鼠标唤醒、双击
        // 无法退全屏(v0.6.0 换 hud_only 时丢失)。命中层在 HUD 条之下,
        // 条内控件命中不会被本层截走。
        let mut layers: Vec<KeyedView> = if input.hud_only {
            vec![keyed("fs-hit", View::from(hit_overlay))]
        } else {
            vec![keyed(
                "fs-canvas",
                Grid::new().children((surface, View::from(hit_overlay))),
            )]
        };

        let show_hud =
            self.hud_visible || !input.playing || input.loading || input.notice.is_some();
        if show_hud {
            let compact = input.window_width.is_finite() && input.window_width < 640.0;

            let top_bar = Border::new()
                .margin(Thickness::new(16.0, 12.0, 16.0, 0.0))
                .padding(Thickness::xy(12.0, 7.0))
                .corner_radius(10.0)
                .background(Brush::Solid(hud_scrim_color()))
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .vertical_alignment(VerticalAlignment::Top)
                .on_pointer_entered(context.callback(|_| Message::HudHovered(true)))
                .on_pointer_exited(context.callback(|_| Message::HudHovered(false)))
                .content(
                    Grid::new()
                        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                        .column_spacing(12.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Subtle)
                                .on_click(input.on_exit_fullscreen.clone())
                                .content(
                                    FontIcon::new()
                                        .glyph("\u{E72B}")
                                        .font_size(14.0)
                                        .foreground(hud_foreground(true)),
                                ),
                            StackPanel::new()
                                .grid_column(1)
                                .spacing(2.0)
                                .vertical_alignment(VerticalAlignment::Center)
                                .children((
                                    TextBlock::new()
                                        .text(input.item.title.clone())
                                        .font_size(if compact { 14.0 } else { 15.5 })
                                        .font_weight(FontWeight::SEMI_BOLD)
                                        .foreground(hud_foreground(true))
                                        .text_trimming(TextTrimming::CharacterEllipsis)
                                        .max_lines(1),
                                    TextBlock::new()
                                        .text(if input.item.author.is_empty() {
                                            input.item.bvid.clone()
                                        } else {
                                            input.item.author.clone()
                                        })
                                        .font_size(11.5)
                                        .foreground(hud_foreground(true))
                                        .opacity(0.65)
                                        .text_trimming(TextTrimming::CharacterEllipsis)
                                        .max_lines(1),
                                )),
                            Button::new()
                                .grid_column(2)
                                .style(ButtonStyle::Subtle)
                                .on_click(input.on_exit_fullscreen.clone())
                                .content(
                                    StackPanel::new()
                                        .orientation(Orientation::Horizontal)
                                        .spacing(6.0)
                                        .children((
                                            FontIcon::new().glyph("\u{E73F}").font_size(14.0),
                                            TextBlock::new()
                                                .text("退出全屏")
                                                .font_size(12.5)
                                                .foreground(hud_foreground(true))
                                                .vertical_alignment(VerticalAlignment::Center),
                                        )),
                                ),
                        )),
                );

            let play_btn = play_pause_button(input.playing, true, true, input.on_toggle.clone());
            let mute_btn =
                mute_toggle_button(input.volume == 0, true, true, input.on_toggle_mute.clone());
            let danmaku_btn = danmaku_toggle_button(
                input.danmaku_enabled,
                true,
                true,
                input.on_toggle_danmaku.clone(),
            );
            let exit_fs_btn =
                fullscreen_toggle_button(true, true, true, input.on_exit_fullscreen.clone());

            let mut right_items: Vec<View> = vec![mute_btn];
            if !compact {
                right_items.push(volume_slider(104.0, input.volume, &input.on_volume, &input.on_volume_wheel).into());
                right_items.push(
                    TextBlock::new()
                        .text(format!("{}%", input.volume))
                        .font_size(12.0)
                        .foreground(hud_foreground(true))
                        .opacity(0.7)
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                );
            }
            right_items.push(danmaku_btn);
            if let Some(q) = quality_selector(
                &input.quality_menu,
                input.current_qn,
                &input.on_select_quality,
                true,
            ) {
                right_items.push(q);
            }
            right_items.push(exit_fs_btn);

            let bottom_bar = Border::new()
                .margin(Thickness::new(16.0, 0.0, 16.0, 14.0))
                .padding(Thickness::xy(14.0, 8.0))
                .corner_radius(10.0)
                .background(Brush::Solid(hud_scrim_color()))
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .vertical_alignment(VerticalAlignment::Bottom)
                .on_pointer_entered(context.callback(|_| Message::HudHovered(true)))
                .on_pointer_exited(context.callback(|_| Message::HudHovered(false)))
                .content(
                    StackPanel::new().spacing(6.0).children((
                        seek_slider(input.position, input.duration_secs, input.on_seek.clone()),
                        Grid::new()
                            .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                            .children((
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(8.0)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .children((
                                        play_btn,
                                        time_label(
                                            input.position,
                                            input.duration_secs,
                                            if compact { 11.0 } else { 12.0 },
                                        )
                                        .foreground(hud_foreground(true)),
                                    )),
                                StackPanel::new()
                                    .grid_column(2)
                                    .orientation(Orientation::Horizontal)
                                    .spacing(if compact { 6.0 } else { 8.0 })
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .children(right_items),
                            )),
                    )),
                );

            layers.push(keyed("fs-top-hud", top_bar));
            layers.push(keyed("fs-bottom-hud", bottom_bar));
        }

        Border::new()
            .element_ref(&self.root_ref)
            .automation_id("fs-hud")
            .is_tab_stop(true)
            .focus_on_pointer_release(true)
            .background(Brush::Solid(if input.hud_only {
                Color::argb(0, 0, 0, 0)
            } else {
                Color::rgb(0, 0, 0)
            }))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .vertical_alignment(VerticalAlignment::Stretch)
            .content(Grid::new().keyed_children(layers))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_ev(
        key: VirtualKey,
        released: bool,
        was_down: bool,
        modifiers: InputModifiers,
    ) -> KeyEventInfo {
        KeyEventInfo {
            key,
            original_key: key,
            status: PhysicalKeyStatus {
                repeat_count: 1,
                scan_code: 0,
                is_extended: false,
                is_menu_down: false,
                was_down,
                is_released: released,
            },
            modifiers,
        }
    }

    #[test]
    fn double_click_tracker_respects_window_distance_and_reset() {
        let mut tracker = DoubleClickTracker::new();
        let t0 = Instant::now();
        assert!(!tracker.record_press(t0, 100.0, 100.0, true));
        // 第二次点击在时间和距离阈值内 → 命中双击并重置
        assert!(tracker.record_press(t0 + Duration::from_millis(200), 103.0, 104.0, true));
        // 紧接着第三次点击不得连续触发双击
        assert!(!tracker.record_press(t0 + Duration::from_millis(260), 103.0, 104.0, true));

        // 位移过大不触发双击
        let mut moved = DoubleClickTracker::new();
        assert!(!moved.record_press(t0, 50.0, 50.0, true));
        assert!(!moved.record_press(t0 + Duration::from_millis(120), 90.0, 50.0, true));

        // 超时窗口不触发双击
        let mut slow = DoubleClickTracker::new();
        assert!(!slow.record_press(t0, 50.0, 50.0, true));
        assert!(!slow.record_press(t0 + Duration::from_millis(450), 50.0, 50.0, true));

        // 右键或非有限坐标忽略
        let mut invalid = DoubleClickTracker::new();
        assert!(!invalid.record_press(t0, 50.0, 50.0, false));
        assert!(!invalid.record_press(t0 + Duration::from_millis(50), 50.0, 50.0, false));
        assert!(!invalid.record_press(t0, f64::NAN, 50.0, true));
    }

    #[test]
    fn resolve_player_hotkey_guards_typing_repeats_and_modifiers() {
        let none = InputModifiers::NONE;
        // 播放器不可见时不拦截单键
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::F, false, false, none),
                false,
                false,
                false
            ),
            None
        );
        // 播放器可见：F / F11 / Space 生效
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::F, false, false, none),
                true,
                false,
                false
            ),
            Some(PlayerHotkey::ToggleFullscreen)
        );
        assert_eq!(
            resolve_player_hotkey(&key_ev(VK_F11, false, false, none), true, false, false),
            Some(PlayerHotkey::ToggleFullscreen)
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::SPACE, false, false, none),
                true,
                false,
                false
            ),
            Some(PlayerHotkey::TogglePlay)
        );
        // 输入框聚焦时不拦截 F / Space，避免劫持搜索与评论输入；Esc 在全屏态仍有效
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::F, false, false, none),
                true,
                false,
                true
            ),
            None
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::SPACE, false, false, none),
                true,
                false,
                true
            ),
            None
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::ESCAPE, false, false, none),
                true,
                true,
                true
            ),
            Some(PlayerHotkey::ExitFullscreen)
        );
        // 长按连发不反复翻转全屏/播停，但允许方向键快进与调音
        assert_eq!(
            resolve_player_hotkey(&key_ev(VirtualKey::F, false, true, none), true, true, false),
            None
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::RIGHT, false, true, none),
                true,
                true,
                false
            ),
            Some(PlayerHotkey::SeekDelta(5))
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::LEFT, false, false, none),
                true,
                true,
                false
            ),
            Some(PlayerHotkey::SeekDelta(-5))
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::UP, false, false, none),
                true,
                true,
                false
            ),
            Some(PlayerHotkey::VolumeDelta(5))
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::DOWN, false, false, none),
                true,
                true,
                false
            ),
            Some(PlayerHotkey::VolumeDelta(-5))
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::M, false, false, none),
                true,
                true,
                false
            ),
            Some(PlayerHotkey::ToggleMute)
        );
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::D, false, false, none),
                true,
                true,
                false
            ),
            Some(PlayerHotkey::ToggleDanmaku)
        );
        // Ctrl+F 等组合键不误触
        assert_eq!(
            resolve_player_hotkey(
                &key_ev(VirtualKey::F, false, false, InputModifiers::CONTROL),
                true,
                false,
                false
            ),
            None
        );
        // 松开事件忽略
        assert_eq!(
            resolve_player_hotkey(&key_ev(VirtualKey::F, true, true, none), true, false, false),
            None
        );
    }

    #[test]
    fn hud_auto_hide_and_pointer_jitter_predicates() {
        assert!(hud_should_auto_hide(5, 5, false, true, false, false));
        assert!(!hud_should_auto_hide(4, 5, false, true, false, false));
        assert!(!hud_should_auto_hide(5, 5, true, true, false, false));
        assert!(!hud_should_auto_hide(5, 5, false, false, false, false));
        assert!(!hud_should_auto_hide(5, 5, false, true, true, false));
        assert!(!hud_should_auto_hide(5, 5, false, true, false, true));

        assert!(pointer_moved_meaningfully(None, (10.0, 10.0)));
        assert!(!pointer_moved_meaningfully(
            Some((10.0, 10.0)),
            (10.5, 10.5)
        ));
        assert!(pointer_moved_meaningfully(Some((10.0, 10.0)), (13.0, 10.0)));
        assert!(!pointer_moved_meaningfully(
            Some((10.0, 10.0)),
            (f64::NAN, 10.0)
        ));
    }
}
