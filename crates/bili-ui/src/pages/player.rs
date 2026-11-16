//! 播放页:视频面(SwapChainPanel)+ Fluent 传输控制 + 信息区(`docs/ui-design.md` §4)。
//!
//! 播放状态(playing/position/volume)由 AppShell 单源持有(经引擎状态快照
//! 校正);视频面是 `widgets::video_surface::VideoSurface`(SwapChainPanel)。

use std::sync::Arc;

use windows_reactor::*;
use bili_video::engine::VideoEngine;
use crate::data::VideoItem;
use crate::theme;
use crate::widgets::video_surface::{VideoSurface, VideoSurfaceInput};

#[derive(Clone)]
pub struct PlayerInput {
    pub item: VideoItem,
    pub playing: bool,
    pub position: u32,
    pub volume: u32,
    /// SwapChain 挂载尝试节拍（仅在未挂载时按 250ms 递增驱动 VideoSurface::try_mount）。
    pub mount_tick: u32,
    /// 展示总长(引擎 duration 优先,占位数据兜底)。
    pub duration_secs: u32,
    /// 引擎错误/媒体失败提示(None = 无)。
    pub notice: Option<String>,
    pub engine: Arc<VideoEngine>,
    pub on_toggle: Callback<()>,
    pub on_seek: Callback<f64>,
    pub on_volume: Callback<f64>,
}

impl PartialEq for PlayerInput {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.playing == other.playing
            && self.position == other.position
            && self.volume == other.volume
            && self.mount_tick == other.mount_tick
            && self.duration_secs == other.duration_secs
            && self.notice == other.notice
            && Arc::ptr_eq(&self.engine, &other.engine)
            && self.on_toggle == other.on_toggle
            && self.on_seek == other.on_seek
            && self.on_volume == other.on_volume
    }
}

pub struct PlayerPage;

/// 本组件自身无内部消息（状态全部来自 Input）。
#[derive(Clone, PartialEq)]
pub enum Message {}

impl Component for PlayerPage {
    type Input = PlayerInput;
    type Message = Message;

    fn create(_input: &PlayerInput, _context: &ComponentContext<Self>) -> Self {
        Self
    }

    fn update(&mut self, _message: Message, _context: &ComponentContext<Self>) {
        match _message {}
    }

    fn view(&self, input: &PlayerInput, _context: &mut ViewContext<Self>) -> View {
        let surface: View = if let Some(notice) = &input.notice {
            // 媒体失败/引擎错误:提示覆盖视频面(黑底可读)。
            Border::new()
                .corner_radius(8.0)
                .background(Brush::Solid(Color::rgb(16, 16, 16)))
                .content(
                    StackPanel::new()
                        .spacing(10.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text("视频通道异常")
                                .font_size(16.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            TextBlock::new()
                                .text(notice.clone())
                                .font_size(12.0)
                                .opacity(0.7)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                )
                .into()
        } else {
            // 视频面:mpv 视频帧 + 引擎内 D2D 弹幕 overlay(同帧同 panel)。
            // mount_tick:引擎未挂载期间每 250ms 递增触发一次 try_mount（涵盖懒创建与 Resize 期间）。
            View::component::<VideoSurface>(VideoSurfaceInput {
                engine: input.engine.clone(),
                tick: input.mount_tick,
            })
        };

        let transport = StackPanel::new()
            .margin(Thickness::new(8.0, 12.0, 8.0, 4.0))
            .spacing(6.0)
            .children((
                Slider::new()
                    .minimum(0.0)
                    .maximum(f64::from(input.duration_secs.max(1)))
                    .value(input.position as f64)
                    .on_value_changed(input.on_seek.clone()),
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        TextBlock::new()
                            .text(theme::fmt_duration(input.position))
                            .font_size(12.0)
                            .opacity(0.6)
                            .vertical_alignment(VerticalAlignment::Center),
                        AppBarButton::new()
                            .icon(Icon::symbol(if input.playing {
                                Symbol::Pause
                            } else {
                                Symbol::Play
                            }))
                            .on_click(input.on_toggle.clone())
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .grid_column(1),
                        TextBlock::new()
                            .text(theme::fmt_duration(input.duration_secs))
                            .font_size(12.0)
                            .opacity(0.6)
                            .horizontal_alignment(HorizontalAlignment::Right)
                            .vertical_alignment(VerticalAlignment::Center)
                            .grid_column(2),
                    )),
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Pixel(160.0), GridLength::Auto])
                    .children((
                        AppBarButton::new().icon(Icon::symbol(if input.volume == 0 {
                            Symbol::Mute
                        } else {
                            Symbol::Volume
                        })),
                        Slider::new()
                            .grid_column(1)
                            .minimum(0.0)
                            .maximum(100.0)
                            .value(input.volume as f64)
                            .on_value_changed(input.on_volume.clone())
                            .vertical_alignment(VerticalAlignment::Center),
                        TextBlock::new()
                            .grid_column(2)
                            .text(format!("{}%", input.volume))
                            .font_size(12.0)
                            .opacity(0.6)
                            .vertical_alignment(VerticalAlignment::Center),
                    )),
            ));

        let info = StackPanel::new().spacing(14.0).children((
            TextBlock::new()
                .text(input.item.title.clone())
                .font_size(20.0)
                .font_weight(FontWeight::BOLD)
                .text_wrapping(TextWrapping::Wrap),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .children((
                    PersonPicture::new()
                        .display_name(input.item.author.as_str())
                        .width(36.0)
                        .height(36.0),
                    StackPanel::new()
                        .spacing(2.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text(input.item.author.clone())
                                .font_size(14.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            TextBlock::new()
                                .text("粉丝与认证信息待接入")
                                .font_size(11.0)
                                .opacity(0.55),
                        )),
                )),
            TextBlock::new()
                .text(format!(
                    "播放 {} · 弹幕 {} · 时长 {}",
                    theme::fmt_count(input.item.play_count),
                    theme::fmt_count(input.item.danmaku_count),
                    theme::fmt_duration(input.item.duration_secs)
                ))
                .font_size(12.0)
                .opacity(0.6),
            Expander::new()
                .header("简介")
                .is_expanded(false)
                .content(TextBlock::new()
                    .text(format!(
                        "《{}》—— UP：{}（占位简介；分 P 与简介 API 待接入）",
                        input.item.title, input.item.author
                    ))
                    .text_wrapping(TextWrapping::Wrap)),
            InfoBar::new()
                .title("评论区")
                .message("老接口 /x/v2/reply/main 已验证可用，数据源待接入。")
                .severity(InfoBarSeverity::Informational)
                .is_open(true)
                .is_closable(false),
            InfoBar::new()
                .title("弹幕")
                .message("seg.so protobuf 分段拉取与 D2D 同帧叠画已接入（可在设置页开关/调参数）。")
                .severity(InfoBarSeverity::Informational)
                .is_open(true)
                .is_closable(false),
        ));

        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(380.0)])
            .children((
                Grid::new()
                    .rows([GridLength::Star(1.0), GridLength::Auto])
                    .margin(Thickness::new(16.0, 16.0, 4.0, 16.0))
                    .children((surface, transport.grid_row(1))),
                ScrollViewer::new()
                    .grid_column(1)
                    .margin(Thickness::new(12.0, 16.0, 16.0, 16.0))
                    .content(info),
            ))
            .into()
    }
}
