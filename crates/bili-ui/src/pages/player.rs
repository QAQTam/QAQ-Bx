//! 播放页：双栏布局（左：视频面 + 传输控制 + 详情 + 底部评论区；右：相关推荐）
//! 与评论区下滚触发右下角小窗（YouTube 式页内画中画，保持 `VideoSurface` 挂载不重建）。

use std::collections::HashMap;
use std::sync::Arc;

use bili_core::reply::{ReplyItem, ReplySort};
use bili_video::engine::VideoEngine;
use windows_reactor::*;

use crate::auth::avatar_thumbnail_url;
use crate::data::VideoItem;
use crate::theme;
use crate::widgets::video_surface::{VideoSurface, VideoSurfaceInput};

const PLAYER_SLOT_HEIGHT: f64 = 420.0;
const RIGHT_RAIL_WIDTH: f64 = 360.0;
const PIP_SCROLL_THRESHOLD: f64 = 300.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommentSort {
    #[default]
    Hot,
    Latest,
}

impl From<CommentSort> for ReplySort {
    fn from(value: CommentSort) -> Self {
        match value {
            CommentSort::Hot => ReplySort::Hot,
            CommentSort::Latest => ReplySort::Latest,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubReplyState {
    pub expanded: bool,
    pub loading: bool,
    pub items: Vec<ReplyItem>,
    pub total_count: u64,
    pub next_pn: u32,
    pub has_more: bool,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct CommentsData {
    pub aid: u64,
    pub sort: CommentSort,
    pub upper_mid: u64,
    pub total_count: Option<u64>,
    pub loading: bool,
    pub loading_more: bool,
    pub loaded_once: bool,
    pub has_more: bool,
    pub error: Option<String>,
    pub top_replies: Vec<ReplyItem>,
    pub replies: Vec<ReplyItem>,
    pub sub_replies: HashMap<u64, SubReplyState>,
    /// 本次展示涉及的已解码头像（按 `avatar_thumbnail_url` 索引）。
    pub avatars: HashMap<String, EncodedImage>,
}

/// 评论区只读快照：用 `rev + Arc::ptr_eq` 做 O(1) 变更检测，避免 250ms 播放节拍重复比对整表。
#[derive(Clone, Default)]
pub struct CommentsSnapshot {
    pub rev: u64,
    pub data: Arc<CommentsData>,
}

impl PartialEq for CommentsSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.rev == other.rev && Arc::ptr_eq(&self.data, &other.data)
    }
}

#[derive(Clone, PartialEq)]
pub struct CommentCallbacks {
    pub on_sort: Callback<CommentSort>,
    pub on_load_more: Callback<()>,
    pub on_retry: Callback<()>,
    pub on_toggle_sub: Callback<u64>,
    pub on_load_more_sub: Callback<u64>,
}

/// 播放页右栏「相关推荐」状态（真实 `archive/related` 数据，shell 单源持有）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RelatedPane {
    /// 当前关联目标（与播放代次一起做双校验）。
    pub bvid: String,
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    pub items: Vec<VideoItem>,
}

#[derive(Clone)]
pub struct PlayerInput {
    pub item: VideoItem,
    pub author_avatar: Option<EncodedImage>,
    pub my_avatar: Option<EncodedImage>,
    pub desc: Option<String>,
    pub playing: bool,
    pub position: u32,
    pub volume: u32,
    pub compact: bool,
    /// SwapChain 挂载尝试节拍（仅在未挂载时按 250ms 递增驱动 VideoSurface::try_mount）。
    pub mount_tick: u32,
    /// 展示总长(引擎 duration 优先,卡片元数据兜底)。
    pub duration_secs: u32,
    /// 引擎错误/媒体失败提示(None = 无)。
    pub notice: Option<String>,
    pub engine: Arc<VideoEngine>,
    pub comments: CommentsSnapshot,
    pub comment_cb: CommentCallbacks,
    /// 相关推荐（真实 archive/related 数据）。
    pub related: RelatedPane,
    pub on_open_video: Callback<String>,
    pub on_related_retry: Callback<()>,
    pub on_toggle: Callback<()>,
    pub on_seek: Callback<f64>,
    pub on_volume: Callback<f64>,
}

impl PartialEq for PlayerInput {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.author_avatar == other.author_avatar
            && self.my_avatar == other.my_avatar
            && self.desc == other.desc
            && self.playing == other.playing
            && self.position == other.position
            && self.volume == other.volume
            && self.compact == other.compact
            && self.mount_tick == other.mount_tick
            && self.duration_secs == other.duration_secs
            && self.notice == other.notice
            && Arc::ptr_eq(&self.engine, &other.engine)
            && self.comments == other.comments
            && self.comment_cb == other.comment_cb
            && self.related == other.related
            && self.on_open_video == other.on_open_video
            && self.on_related_retry == other.on_related_retry
            && self.on_toggle == other.on_toggle
            && self.on_seek == other.on_seek
            && self.on_volume == other.on_volume
    }
}

pub struct PlayerPage {
    scroll_y: f64,
    scroll_reset_gen: u64,
    pip_pinned: bool,
}

#[derive(Clone, PartialEq)]
pub enum Message {
    Scrolled(f64),
    TogglePip,
    BackToTop,
}

impl Component for PlayerPage {
    type Input = PlayerInput;
    type Message = Message;

    fn create(_input: &PlayerInput, _context: &ComponentContext<Self>) -> Self {
        Self {
            scroll_y: 0.0,
            scroll_reset_gen: 1,
            pip_pinned: false,
        }
    }

    fn update(&mut self, message: Message, _context: &ComponentContext<Self>) {
        match message {
            Message::Scrolled(y) => self.scroll_y = y.max(0.0),
            Message::TogglePip => self.pip_pinned = !self.pip_pinned,
            Message::BackToTop => {
                self.pip_pinned = false;
                self.scroll_y = 0.0;
                self.scroll_reset_gen = self.scroll_reset_gen.wrapping_add(1);
            }
        }
    }

    fn view(&self, input: &PlayerInput, context: &mut ViewContext<Self>) -> View {
        let is_pip = self.pip_pinned || self.scroll_y >= PIP_SCROLL_THRESHOLD;

        let surface: View = if let Some(notice) = &input.notice {
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
            View::component::<VideoSurface>(VideoSurfaceInput {
                engine: input.engine.clone(),
                tick: input.mount_tick,
            })
        };

        // 顶部主播放占位槽：未小窗时由上层稳定悬浮的 VideoSurface 覆盖；进入小窗后展示回到顶部提示。
        let top_player_slot: View = if is_pip {
            Border::new()
                .height(PLAYER_SLOT_HEIGHT)
                .corner_radius(8.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .content(
                    StackPanel::new()
                        .spacing(10.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text("正在右下角小窗播放")
                                .font_size(15.0)
                                .font_weight(FontWeight::SEMI_BOLD),
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .content("回到顶部主画面")
                                .on_click(context.message(Message::BackToTop)),
                        )),
                )
                .into()
        } else {
            Border::new().height(PLAYER_SLOT_HEIGHT).into()
        };

        let transport = StackPanel::new()
            .margin(Thickness::new(0.0, 10.0, 0.0, 4.0))
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
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((
                                AppBarButton::new()
                                    .icon(Icon::symbol(if input.playing {
                                        Symbol::Pause
                                    } else {
                                        Symbol::Play
                                    }))
                                    .on_click(input.on_toggle.clone()),
                                TextBlock::new()
                                    .text(format!(
                                        "{} / {}",
                                        theme::fmt_duration(input.position),
                                        theme::fmt_duration(input.duration_secs)
                                    ))
                                    .font_size(12.0)
                                    .opacity(0.7)
                                    .vertical_alignment(VerticalAlignment::Center),
                            )),
                        StackPanel::new()
                            .grid_column(2)
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((
                                AppBarButton::new().icon(Icon::symbol(if input.volume == 0 {
                                    Symbol::Mute
                                } else {
                                    Symbol::Volume
                                })),
                                Slider::new()
                                    .width(110.0)
                                    .minimum(0.0)
                                    .maximum(100.0)
                                    .value(input.volume as f64)
                                    .on_value_changed(input.on_volume.clone())
                                    .vertical_alignment(VerticalAlignment::Center),
                                TextBlock::new()
                                    .text(format!("{}%", input.volume))
                                    .font_size(12.0)
                                    .opacity(0.6)
                                    .vertical_alignment(VerticalAlignment::Center),
                                Button::new()
                                    .style(if self.pip_pinned {
                                        ButtonStyle::Accent
                                    } else {
                                        ButtonStyle::Subtle
                                    })
                                    .content(if is_pip { "还原主窗" } else { "右下角小窗" })
                                    .on_click(context.message(Message::TogglePip)),
                            )),
                    )),
            ));

        let info_and_actions = StackPanel::new().spacing(12.0).children((
            TextBlock::new()
                .text(input.item.title.clone())
                .font_size(20.0)
                .font_weight(FontWeight::BOLD)
                .text_wrapping(TextWrapping::Wrap),
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(10.0)
                        .children((
                            theme::avatar_view(
                                input.author_avatar.as_ref(),
                                if input.item.author.is_empty() {
                                    "UP"
                                } else {
                                    input.item.author.as_str()
                                },
                                36.0,
                                18.0,
                            ),
                            StackPanel::new()
                                .spacing(2.0)
                                .vertical_alignment(VerticalAlignment::Center)
                                .children((
                                    TextBlock::new()
                                        .text(if input.item.author.is_empty() {
                                            "UP主".to_string()
                                        } else {
                                            input.item.author.clone()
                                        })
                                        .font_size(14.0)
                                        .font_weight(FontWeight::SEMI_BOLD),
                                    TextBlock::new()
                                        .text(format!(
                                            "播放 {} · 弹幕 {} · 时长 {}",
                                            theme::fmt_count(input.item.play_count),
                                            theme::fmt_count(input.item.danmaku_count),
                                            theme::fmt_duration(input.item.duration_secs)
                                        ))
                                        .font_size(12.0)
                                        .opacity(0.6),
                                )),
                        )),
                    StackPanel::new()
                        .grid_column(1)
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            action_pill("点赞", input.item.like_count),
                            action_pill("投币", input.item.coin_count),
                            action_pill("收藏", input.item.favorite_count),
                            action_pill("分享", input.item.share_count),
                        )),
                )),
            Expander::new()
                .header("简介")
                .is_expanded(false)
                .content(
                    TextBlock::new()
                        .text(format_desc_text(
                            &input.item.title,
                            &input.item.bvid,
                            input.desc.as_deref(),
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ),
        ));

        let comments_section = comments_panel_with(
            &input.comments,
            input.my_avatar.as_ref(),
            &input.comment_cb,
            input.compact,
        );

        if input.compact {
            let compact_body = StackPanel::new()
                .spacing(12.0)
                .margin(Thickness::new(12.0, 10.0, 12.0, 20.0))
                .children((
                    Border::new()
                        .height(220.0)
                        .corner_radius(8.0)
                        .content(surface),
                    View::from(transport),
                    View::from(info_and_actions),
                    related_panel(input),
                    comments_section,
                ));
            return ScrollViewer::new().content(compact_body).into();
        }

        let left_scroll_content = StackPanel::new()
            .spacing(14.0)
            .margin(Thickness::new(16.0, 16.0, 12.0, 24.0))
            .children((
                top_player_slot,
                View::from(transport),
                View::from(info_and_actions),
                comments_section,
            ));

        let left_scroll = Grid::new()
            .grid_column(0)
            .keyed_children([keyed(
                format!("player-left-scroll-{}", self.scroll_reset_gen),
                ScrollViewer::new()
                    .on_view_changed(context.callback(|ev: ScrollEventInfo| {
                        Message::Scrolled(ev.offset_y)
                    }))
                    .content(left_scroll_content),
            )]);

        let right_rail = ScrollViewer::new()
            .grid_column(1)
            .margin(Thickness::new(4.0, 16.0, 16.0, 16.0))
            .content(related_panel(input));

        let two_col_body = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(RIGHT_RAIL_WIDTH)])
            .children((View::from(left_scroll), View::from(right_rail)));

        // 稳定挂载 VideoSurface：通过对齐与尺寸切换在“顶部主位”和“右下角小窗”间过渡，不销毁 SwapChainPanel。
        let floating_surface = if is_pip {
            Border::new()
                .width(360.0)
                .height(228.0)
                .margin(Thickness::new(0.0, 0.0, 24.0, 24.0))
                .corner_radius(10.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Bottom)
                .content(
                    Grid::new()
                        .rows([GridLength::Star(1.0), GridLength::Auto])
                        .children((
                            View::from(Border::new().corner_radius(8.0).content(surface)),
                            Grid::new()
                                .grid_row(1)
                                .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                                .margin(Thickness::xy(8.0, 4.0))
                                .children((
                                    Button::new()
                                        .style(ButtonStyle::Subtle)
                                        .content(if input.playing { "暂停" } else { "播放" })
                                        .on_click(input.on_toggle.clone()),
                                    TextBlock::new()
                                        .grid_column(1)
                                        .text(input.item.title.clone())
                                        .font_size(11.0)
                                        .max_lines(1)
                                        .opacity(0.75)
                                        .margin(Thickness::xy(6.0, 0.0))
                                        .vertical_alignment(VerticalAlignment::Center),
                                    Button::new()
                                        .grid_column(2)
                                        .style(ButtonStyle::Subtle)
                                        .content("回主窗")
                                        .on_click(context.message(Message::BackToTop)),
                                )),
                        )),
                )
        } else {
            Border::new()
                .height(PLAYER_SLOT_HEIGHT)
                .margin(Thickness::new(16.0, 16.0, RIGHT_RAIL_WIDTH + 12.0, 0.0))
                .corner_radius(8.0)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .vertical_alignment(VerticalAlignment::Top)
                .content(surface)
        };

        Grid::new()
            .keyed_children([
                keyed("player-two-col", two_col_body),
                keyed("player-surface-host", floating_surface),
            ])
            .into()
    }
}

/// 互动计数胶囊：数字来自真实卡片/`video_meta` 统计；未知时只显示标签不编造数字。
fn action_pill(label: &str, count: Option<u64>) -> View {
    let count_text: Option<View> = count.map(|value| {
        View::from(
            TextBlock::new()
                .text(theme::fmt_count(value))
                .font_size(11.0)
                .opacity(0.65),
        )
    });
    let children: Vec<View> = std::iter::once(
        TextBlock::new()
            .text(label)
            .font_size(12.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .into(),
    )
    .chain(count_text)
    .collect();
    Button::new()
        .style(ButtonStyle::Subtle)
        .content(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children(children),
        )
        .into()
}

fn format_desc_text(title: &str, bvid: &str, desc: Option<&str>) -> String {
    if let Some(d) = desc.map(str::trim).filter(|s| !s.is_empty() && *s != "-") {
        format!("{title}\n\n{d}\n\nBV 号：{bvid}")
    } else {
        format!("{title}\n\nBV 号：{bvid}")
    }
}

fn comments_panel_with(
    snapshot: &CommentsSnapshot,
    my_avatar: Option<&EncodedImage>,
    cb: &CommentCallbacks,
    compact_pane: bool,
) -> View {
    let data = &snapshot.data;
    let on_hot = {
        let on_sort = cb.on_sort.clone();
        move || on_sort.call(CommentSort::Hot)
    };
    let on_latest = {
        let on_sort = cb.on_sort.clone();
        move || on_sort.call(CommentSort::Latest)
    };

    let mut title_parts: Vec<View> = vec![
        TextBlock::new()
            .text("评论")
            .font_size(17.0)
            .font_weight(FontWeight::BOLD)
            .into(),
    ];
    if let Some(total) = data.total_count {
        title_parts.push(
            TextBlock::new()
                .text(theme::fmt_count(total))
                .font_size(13.0)
                .opacity(0.6)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }

    let header = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .children((
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children(title_parts),
            StackPanel::new()
                .grid_column(1)
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    Button::new()
                        .style(if data.sort == CommentSort::Hot {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Subtle
                        })
                        .content("按热度")
                        .on_click(on_hot),
                    Button::new()
                        .style(if data.sort == CommentSort::Latest {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Subtle
                        })
                        .content("按时间")
                        .on_click(on_latest),
                )),
        ));

    let composer = Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .children((
            theme::avatar_view(my_avatar, "我", 34.0, 17.0)
                .vertical_alignment(VerticalAlignment::Center),
            TextBox::new()
                .grid_column(1)
                .margin(Thickness::xy(10.0, 0.0))
                .placeholder_text("发一条友善的评论…"),
            Button::new()
                .grid_column(2)
                .style(ButtonStyle::Accent)
                .content("发布"),
        ));

    let mut rows: Vec<KeyedView> = vec![
        KeyedView::new("c-header", header),
        KeyedView::new("c-composer", composer),
    ];

    if data.top_replies.is_empty() && data.replies.is_empty() {
        if data.loading || data.aid == 0 {
            rows.push(KeyedView::new(
                "c-loading",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(10.0)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 18.0))
                    .children((
                        ProgressRing::new()
                            .is_indeterminate(true)
                            .is_active(true)
                            .width(20.0)
                            .height(20.0),
                        TextBlock::new()
                            .text("评论加载中…")
                            .font_size(12.0)
                            .opacity(0.65)
                            .vertical_alignment(VerticalAlignment::Center),
                    )),
            ));
        } else if let Some(err) = &data.error {
            let on_retry = cb.on_retry.clone();
            rows.push(KeyedView::new(
                "c-error",
                StackPanel::new()
                    .spacing(8.0)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 16.0))
                    .children((
                        TextBlock::new()
                            .text(err.clone())
                            .font_size(12.0)
                            .opacity(0.65)
                            .text_wrapping(TextWrapping::Wrap)
                            .horizontal_alignment(HorizontalAlignment::Center),
                        Button::new()
                            .content("重试")
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .on_click(move || on_retry.call(())),
                    )),
            ));
        } else {
            rows.push(KeyedView::new(
                "c-empty",
                TextBlock::new()
                    .text("暂无评论")
                    .font_size(13.0)
                    .opacity(0.55)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 18.0)),
            ));
        }
    } else {
        for item in &data.top_replies {
            rows.push(KeyedView::new(
                format!("c-top-{}", item.rpid),
                comment_card(item, data, cb, compact_pane),
            ));
        }
        for item in &data.replies {
            rows.push(KeyedView::new(
                format!("c-{}", item.rpid),
                comment_card(item, data, cb, compact_pane),
            ));
        }

        if let Some(err) = &data.error {
            let on_retry = cb.on_retry.clone();
            rows.push(KeyedView::new(
                "c-footer-err",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 6.0))
                    .children((
                        TextBlock::new()
                            .text(err.clone())
                            .font_size(12.0)
                            .opacity(0.65)
                            .vertical_alignment(VerticalAlignment::Center),
                        Button::new()
                            .style(ButtonStyle::Subtle)
                            .content("重试")
                            .on_click(move || on_retry.call(())),
                    )),
            ));
        } else if data.loading_more {
            rows.push(KeyedView::new(
                "c-footer-loading",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 6.0))
                    .children((
                        ProgressRing::new()
                            .is_indeterminate(true)
                            .is_active(true)
                            .width(16.0)
                            .height(16.0),
                        TextBlock::new()
                            .text("加载更多评论…")
                            .font_size(12.0)
                            .opacity(0.65)
                            .vertical_alignment(VerticalAlignment::Center),
                    )),
            ));
        } else if data.has_more {
            let on_more = cb.on_load_more.clone();
            rows.push(KeyedView::new(
                "c-footer-more",
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content("加载更多评论")
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 4.0))
                    .on_click(move || on_more.call(())),
            ));
        } else if data.loaded_once {
            rows.push(KeyedView::new(
                "c-footer-end",
                TextBlock::new()
                    .text("没有更多评论了")
                    .font_size(12.0)
                    .opacity(0.5)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 4.0)),
            ));
        }
    }

    Border::new()
        .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
        .padding(Thickness::xy(16.0, 16.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(StackPanel::new().spacing(14.0).keyed_children(rows))
        .into()
}

fn comment_badge(label: &str, accent: bool) -> View {
    let mut border = Border::new()
        .corner_radius(3.0)
        .padding(Thickness::xy(4.0, 1.0))
        .background(ThemeBrush::SolidBackground)
        .border_thickness(1.0)
        .vertical_alignment(VerticalAlignment::Center);
    let mut text = TextBlock::new()
        .text(label)
        .font_size(10.0);
    if accent {
        border = border.border_brush(ThemeBrush::Accent);
        text = text.foreground(ThemeBrush::AccentText);
    } else {
        border = border.border_brush(ThemeBrush::CardStroke);
        text = text.opacity(0.8);
    }
    border.content(text).into()
}

fn comment_meta_text(item: &ReplyItem) -> String {
    let time = if !item.time_desc.is_empty() {
        item.time_desc.clone()
    } else {
        theme::fmt_time(item.ctime)
    };
    match (time.is_empty(), item.location.is_empty()) {
        (false, false) => format!("{time} · {}", item.location),
        (false, true) => time,
        (true, false) => item.location.clone(),
        (true, true) => String::new(),
    }
}

fn comment_card(
    item: &ReplyItem,
    data: &CommentsData,
    cb: &CommentCallbacks,
    compact_pane: bool,
) -> View {
    let is_up = data.upper_mid > 0 && item.mid == data.upper_mid;
    let mut author_row: Vec<View> = vec![
        TextBlock::new()
            .text(item.uname.clone())
            .font_size(13.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .max_width(if compact_pane { 150.0 } else { 260.0 })
            .max_lines(1)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if item.is_up_top {
        author_row.push(comment_badge("置顶", true));
    }
    if is_up {
        author_row.push(comment_badge("UP主", true));
    }
    author_row.push(comment_badge(&format!("LV{}", item.level), false));

    let mut content_col: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(author_row)
            .into(),
    ];

    let meta = comment_meta_text(item);
    if !meta.is_empty() {
        content_col.push(
            TextBlock::new()
                .text(meta)
                .font_size(11.0)
                .opacity(0.5)
                .into(),
        );
    }

    if !item.message.trim().is_empty() {
        content_col.push(
            TextBlock::new()
                .text(item.message.clone())
                .font_size(13.0)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }

    let sub_state = data.sub_replies.get(&item.rpid);
    let sub_expanded = sub_state.is_some_and(|s| s.expanded);

    let mut actions: Vec<View> = vec![
        TextBlock::new()
            .text(format!("👍 {}", theme::fmt_count(item.like)))
            .font_size(11.0)
            .opacity(0.65)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if item.up_liked {
        actions.push(comment_badge("UP主觉得很赞", false));
    }
    if item.rcount > 0 {
        let rpid = item.rpid;
        let on_toggle = cb.on_toggle_sub.clone();
        let label = if sub_expanded {
            "收起回复".to_string()
        } else {
            format!("{}条回复", theme::fmt_count(item.rcount))
        };
        actions.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .content(
                    TextBlock::new()
                        .text(label)
                        .font_size(11.0)
                        .opacity(0.75),
                )
                .on_click(move || on_toggle.call(rpid))
                .into(),
        );
    }
    content_col.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .children(actions)
            .into(),
    );

    if let Some(sub_box) = sub_replies_box(item, data, sub_state, cb) {
        content_col.push(sub_box);
    }

    let avatar_img = (!item.avatar.is_empty())
        .then(|| data.avatars.get(&avatar_thumbnail_url(&item.avatar)))
        .flatten();
    let avatar_size = if compact_pane { 30.0 } else { 34.0 };
    let avatar = theme::avatar_view(avatar_img, &item.uname, avatar_size, avatar_size / 2.0)
        .vertical_alignment(VerticalAlignment::Top);

    Border::new()
        .padding(Thickness::new(0.0, 10.0, 0.0, 6.0))
        .content(
            Grid::new()
                .columns([GridLength::Auto, GridLength::Star(1.0)])
                .children((
                    avatar,
                    StackPanel::new()
                        .grid_column(1)
                        .margin(Thickness::new(12.0, 0.0, 0.0, 0.0))
                        .spacing(6.0)
                        .children(content_col),
                )),
        )
        .into()
}

fn sub_replies_box(
    parent: &ReplyItem,
    data: &CommentsData,
    sub_state: Option<&SubReplyState>,
    cb: &CommentCallbacks,
) -> Option<View> {
    let expanded = sub_state.is_some_and(|s| s.expanded);
    if !expanded && parent.replies.is_empty() && parent.rcount == 0 {
        return None;
    }

    let mut sub_rows: Vec<KeyedView> = Vec::new();
    let rpid = parent.rpid;

    if let Some(state) = sub_state.filter(|s| s.expanded) {
        if state.items.is_empty() {
            if state.loading {
                sub_rows.push(KeyedView::new(
                    "sub-loading",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            ProgressRing::new()
                                .is_indeterminate(true)
                                .is_active(true)
                                .width(14.0)
                                .height(14.0),
                            TextBlock::new()
                                .text("加载回复中…")
                                .font_size(11.0)
                                .opacity(0.65)
                                .vertical_alignment(VerticalAlignment::Center),
                        )),
                ));
            } else if let Some(err) = &state.error {
                let on_retry = cb.on_load_more_sub.clone();
                sub_rows.push(KeyedView::new(
                    "sub-err",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            TextBlock::new()
                                .text(err.clone())
                                .font_size(11.0)
                                .opacity(0.65)
                                .vertical_alignment(VerticalAlignment::Center),
                            Button::new()
                                .style(ButtonStyle::Subtle)
                                .content("重试")
                                .on_click(move || on_retry.call(rpid)),
                        )),
                ));
            } else {
                sub_rows.push(KeyedView::new(
                    "sub-empty",
                    TextBlock::new()
                        .text("暂无回复")
                        .font_size(11.0)
                        .opacity(0.55),
                ));
            }
        } else {
            for sub in &state.items {
                sub_rows.push(KeyedView::new(
                    format!("sub-{}", sub.rpid),
                    expanded_sub_reply_row(sub, data),
                ));
            }
            if let Some(err) = &state.error {
                let on_retry = cb.on_load_more_sub.clone();
                sub_rows.push(KeyedView::new(
                    "sub-err",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            TextBlock::new()
                                .text(err.clone())
                                .font_size(11.0)
                                .opacity(0.65)
                                .vertical_alignment(VerticalAlignment::Center),
                            Button::new()
                                .style(ButtonStyle::Subtle)
                                .content("重试")
                                .on_click(move || on_retry.call(rpid)),
                        )),
                ));
            }
        }

        let mut footer_btns: Vec<View> = Vec::new();
        if state.has_more {
            let on_more = cb.on_load_more_sub.clone();
            footer_btns.push(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .is_enabled(!state.loading)
                    .content(
                        TextBlock::new()
                            .text(if state.loading { "加载中…" } else { "加载更多回复" })
                            .font_size(11.0),
                    )
                    .on_click(move || on_more.call(rpid))
                    .into(),
            );
        }
        let on_collapse = cb.on_toggle_sub.clone();
        footer_btns.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .content(
                    TextBlock::new()
                        .text("收起回复")
                        .font_size(11.0)
                        .opacity(0.75),
                )
                .on_click(move || on_collapse.call(rpid))
                .into(),
        );
        sub_rows.push(KeyedView::new(
            "sub-footer",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(footer_btns),
        ));
    } else {
        for sub in &parent.replies {
            let prefix = if data.upper_mid > 0 && sub.mid == data.upper_mid {
                "UP主 · "
            } else {
                ""
            };
            sub_rows.push(KeyedView::new(
                format!("subp-{}", sub.rpid),
                TextBlock::new()
                    .text(format!("{prefix}{}：{}", sub.uname, sub.message))
                    .font_size(12.0)
                    .opacity(0.85)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if parent.rcount > parent.replies.len() as u64 {
            let on_expand = cb.on_toggle_sub.clone();
            sub_rows.push(KeyedView::new(
                "sub-expand",
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content(
                        TextBlock::new()
                            .text(format!(
                                "共 {} 条回复，点击展开 ›",
                                theme::fmt_count(parent.rcount)
                            ))
                            .font_size(11.0)
                            .foreground(ThemeBrush::AccentText),
                    )
                    .on_click(move || on_expand.call(rpid)),
            ));
        }
    }

    if sub_rows.is_empty() {
        return None;
    }

    Some(
        Border::new()
            .margin(Thickness::new(0.0, 4.0, 0.0, 0.0))
            .padding(Thickness::xy(10.0, 8.0))
            .corner_radius(6.0)
            .background(ThemeBrush::SolidBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .content(StackPanel::new().spacing(6.0).keyed_children(sub_rows))
            .into(),
    )
}

fn expanded_sub_reply_row(sub: &ReplyItem, data: &CommentsData) -> View {
    let avatar_img = (!sub.avatar.is_empty())
        .then(|| data.avatars.get(&avatar_thumbnail_url(&sub.avatar)))
        .flatten();
    let avatar = theme::avatar_view(avatar_img, &sub.uname, 22.0, 11.0)
        .vertical_alignment(VerticalAlignment::Top);

    let is_up = data.upper_mid > 0 && sub.mid == data.upper_mid;
    let mut name_parts: Vec<View> = vec![
        TextBlock::new()
            .text(sub.uname.clone())
            .font_size(12.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .max_width(180.0)
            .max_lines(1)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if is_up {
        name_parts.push(comment_badge("UP主", true));
    }
    name_parts.push(comment_badge(&format!("LV{}", sub.level), false));

    let meta = comment_meta_text(sub);
    let mut info_col: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(name_parts)
            .into(),
    ];
    if !meta.is_empty() {
        info_col.push(
            TextBlock::new()
                .text(meta)
                .font_size(10.5)
                .opacity(0.5)
                .into(),
        );
    }
    info_col.push(
        TextBlock::new()
            .text(sub.message.clone())
            .font_size(12.0)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    );
    info_col.push(
        TextBlock::new()
            .text(format!("👍 {}", theme::fmt_count(sub.like)))
            .font_size(10.5)
            .opacity(0.6)
            .into(),
    );

    Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            avatar,
            StackPanel::new()
                .grid_column(1)
                .margin(Thickness::new(8.0, 0.0, 0.0, 0.0))
                .spacing(3.0)
                .children(info_col),
        ))
        .into()
}

/// 相关推荐状态行（加载中/错误重试/空态共用骨架样式）。
fn related_status_view(text: &str, on_retry: Option<&Callback<()>>) -> View {
    let mut children: Vec<View> = vec![
        TextBlock::new()
            .text(text)
            .font_size(12.0)
            .opacity(0.6)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    ];
    if let Some(on_retry) = on_retry {
        let on_retry = on_retry.clone();
        children.push(
            Button::new()
                .content("重试")
                .horizontal_alignment(HorizontalAlignment::Left)
                .on_click(move || on_retry.call(()))
                .into(),
        );
    }
    Border::new()
        .padding(Thickness::xy(10.0, 10.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(StackPanel::new().spacing(8.0).children(children))
        .into()
}

fn related_panel(input: &PlayerInput) -> View {
    let related = &input.related;
    let mut cards: Vec<View> = vec![
        TextBlock::new()
            .text("接下来播放 · 相关推荐")
            .font_size(15.0)
            .font_weight(FontWeight::BOLD)
            .into(),
    ];

    if !related.loaded_once {
        if let Some(err) = &related.error {
            cards.push(related_status_view(
                &format!("相关推荐加载失败：{err}"),
                Some(&input.on_related_retry),
            ));
        } else {
            cards.push(related_status_view("相关推荐加载中…", None));
        }
    } else if related.items.is_empty() {
        cards.push(related_status_view("暂无相关视频", None));
    } else {
        for item in &related.items {
            cards.push(related_card(item, &input.on_open_video));
        }
    }

    StackPanel::new().spacing(10.0).children(cards).into()
}

fn related_card(item: &VideoItem, on_open: &Callback<String>) -> View {
    let bvid = item.bvid.clone();
    let on_click = on_open.clone();
    let cover_inner: View = match &item.cover {
        Some(img) => Image::new()
            .source_data(img.clone())
            .stretch(Stretch::UniformToFill)
            .into(),
        None => TextBlock::new()
            .text("封面加载中")
            .font_size(11.0)
            .opacity(0.35)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    };
    Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .on_click(move || on_click.call(bvid.clone()))
        .content(
            Border::new()
                .padding(Thickness::xy(8.0, 8.0))
                .corner_radius(8.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .content(
                    Grid::new()
                        .columns([GridLength::Pixel(132.0), GridLength::Star(1.0)])
                        .children((
                            Grid::new().height(74.0).children((
                                Border::new()
                                    .corner_radius(6.0)
                                    .background(ThemeBrush::SolidBackground)
                                    .content(cover_inner),
                                Border::new()
                                    .background(ThemeBrush::SolidBackground)
                                    .border_brush(ThemeBrush::CardStroke)
                                    .border_thickness(1.0)
                                    .corner_radius(4.0)
                                    .padding(Thickness::xy(5.0, 1.0))
                                    .margin(Thickness::new(0.0, 0.0, 5.0, 5.0))
                                    .horizontal_alignment(HorizontalAlignment::Right)
                                    .vertical_alignment(VerticalAlignment::Bottom)
                                    .content(
                                        TextBlock::new()
                                            .text(theme::fmt_duration(item.duration_secs))
                                            .foreground(ThemeBrush::PrimaryText)
                                            .font_size(10.0),
                                    ),
                            )),
                            StackPanel::new()
                                .grid_column(1)
                                .margin(Thickness::new(10.0, 0.0, 0.0, 0.0))
                                .spacing(4.0)
                                .children((
                                    TextBlock::new()
                                        .text(item.title.clone())
                                        .font_size(13.0)
                                        .font_weight(FontWeight::SEMI_BOLD)
                                        .text_wrapping(TextWrapping::Wrap)
                                        .max_lines(2),
                                    TextBlock::new()
                                        .text(item.author.clone())
                                        .font_size(11.0)
                                        .opacity(0.6),
                                    TextBlock::new()
                                        .text(format!("▶ {}", theme::fmt_count(item.play_count)))
                                        .font_size(11.0)
                                        .opacity(0.55),
                                )),
                        )),
                ),
        )
        .into()
}

/// 首页右侧半窗预览输入（复用同一 `VideoEngine`，无播放控制条，最左侧带拖动柄与展开图标）。
#[derive(Clone)]
pub struct HalfPaneInput {
    pub item: VideoItem,
    pub author_avatar: Option<EncodedImage>,
    pub my_avatar: Option<EncodedImage>,
    pub desc: Option<String>,
    pub mount_tick: u32,
    pub width: f64,
    pub max_width: f64,
    pub notice: Option<String>,
    pub engine: Arc<VideoEngine>,
    pub comments: CommentsSnapshot,
    pub comment_cb: CommentCallbacks,
    pub on_expand: Callback<()>,
    pub on_close: Callback<()>,
    pub on_resize: Callback<f64>,
}

impl PartialEq for HalfPaneInput {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
            && self.author_avatar == other.author_avatar
            && self.my_avatar == other.my_avatar
            && self.desc == other.desc
            && self.mount_tick == other.mount_tick
            && (self.width - other.width).abs() < 0.5
            && (self.max_width - other.max_width).abs() < 0.5
            && self.notice == other.notice
            && Arc::ptr_eq(&self.engine, &other.engine)
            && self.comments == other.comments
            && self.comment_cb == other.comment_cb
            && self.on_expand == other.on_expand
            && self.on_close == other.on_close
            && self.on_resize == other.on_resize
    }
}

pub struct HalfPanePreview {
    drag_origin: Option<(f64, f64)>,
    live_width: Option<f64>,
}

#[derive(Clone, PartialEq)]
pub enum HalfPaneMessage {
    DragStart(f64, f64),
    DragMove(f64),
    DragEnd,
}

impl Component for HalfPanePreview {
    type Input = HalfPaneInput;
    type Message = HalfPaneMessage;

    fn create(_input: &HalfPaneInput, _context: &ComponentContext<Self>) -> Self {
        Self {
            drag_origin: None,
            live_width: None,
        }
    }

    fn update(&mut self, message: HalfPaneMessage, _context: &ComponentContext<Self>) {
        match message {
            HalfPaneMessage::DragStart(window_x, current_w) => {
                self.drag_origin = Some((window_x, current_w));
            }
            HalfPaneMessage::DragMove(window_x) => {
                if let Some((start_x, start_w)) = self.drag_origin {
                    let next_w = (start_w + (start_x - window_x)).clamp(340.0, 820.0);
                    self.live_width = Some(next_w);
                }
            }
            HalfPaneMessage::DragEnd => {
                self.drag_origin = None;
            }
        }
    }

    fn view(&self, input: &HalfPaneInput, context: &mut ViewContext<Self>) -> View {
        let max_w = input.max_width.clamp(340.0, 820.0);
        let pane_width = self.live_width.unwrap_or(input.width).clamp(320.0, max_w);
        let video_height = ((pane_width - 40.0 - 24.0) * 9.0 / 16.0).clamp(160.0, 440.0);

        let surface: View = if let Some(notice) = &input.notice {
            Border::new()
                .corner_radius(8.0)
                .background(Brush::Solid(Color::rgb(16, 16, 16)))
                .content(
                    TextBlock::new()
                        .text(notice.clone())
                        .font_size(12.0)
                        .opacity(0.75)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .text_wrapping(TextWrapping::Wrap),
                )
                .into()
        } else {
            View::component::<VideoSurface>(VideoSurfaceInput {
                engine: input.engine.clone(),
                tick: input.mount_tick,
            })
        };

        let cur_w = pane_width;
        let drag_handle = Border::new()
            .grid_row(1)
            .width(40.0)
            .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
            .on_pointer_pressed(context.callback(move |ev: PointerEventInfo| {
                HalfPaneMessage::DragStart(ev.window_x, cur_w)
            }))
            .content(
                StackPanel::new()
                    .spacing(6.0)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
                        FontIcon::new()
                            .glyph("\u{E76F}")
                            .font_size(14.0)
                            .opacity(if self.drag_origin.is_some() { 1.0 } else { 0.55 }),
                        Border::new()
                            .width(3.0)
                            .height(28.0)
                            .corner_radius(1.5)
                            .background(if self.drag_origin.is_some() {
                                ThemeBrush::Accent
                            } else {
                                ThemeBrush::CardStroke
                            }),
                    )),
            );

        // 最左侧纯图标条：顶部关闭 + 中间拖动柄 + 底部展开图标（无多余文字）。
        let left_strip = Border::new()
            .grid_column(0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::new(0.0, 0.0, 1.0, 0.0))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((
                        Button::new()
                            .style(ButtonStyle::Subtle)
                            .width(40.0)
                            .height(40.0)
                            .content(FontIcon::new().glyph("\u{E711}").font_size(12.0))
                            .on_click(input.on_close.clone()),
                        View::from(drag_handle),
                        Button::new()
                            .grid_row(2)
                            .style(ButtonStyle::Subtle)
                            .width(40.0)
                            .height(44.0)
                            .content(FontIcon::new().glyph("\u{E740}").font_size(14.0))
                            .on_click(input.on_expand.clone()),
                    )),
            );

        let info_and_comments = StackPanel::new()
            .spacing(12.0)
            .margin(Thickness::new(14.0, 12.0, 14.0, 20.0))
            .children((
                TextBlock::new()
                    .text(input.item.title.clone())
                    .font_size(16.0)
                    .font_weight(FontWeight::BOLD)
                    .text_wrapping(TextWrapping::Wrap),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(10.0)
                    .children((
                        theme::avatar_view(
                            input.author_avatar.as_ref(),
                            if input.item.author.is_empty() {
                                "UP"
                            } else {
                                input.item.author.as_str()
                            },
                            30.0,
                            15.0,
                        ),
                        StackPanel::new()
                            .spacing(2.0)
                            .vertical_alignment(VerticalAlignment::Center)
                            .children((
                                TextBlock::new()
                                    .text(if input.item.author.is_empty() {
                                        "UP主".to_string()
                                    } else {
                                        input.item.author.clone()
                                    })
                                    .font_size(13.0)
                                    .font_weight(FontWeight::SEMI_BOLD),
                                TextBlock::new()
                                    .text(format!(
                                        "播放 {} · 弹幕 {} · 时长 {}",
                                        theme::fmt_count(input.item.play_count),
                                        theme::fmt_count(input.item.danmaku_count),
                                        theme::fmt_duration(input.item.duration_secs)
                                    ))
                                    .font_size(11.0)
                                    .opacity(0.6),
                            )),
                    )),
                Expander::new()
                    .header("简介")
                    .is_expanded(false)
                    .content(
                        TextBlock::new()
                            .text(format_desc_text(
                                &input.item.title,
                                &input.item.bvid,
                                input.desc.as_deref(),
                            ))
                            .text_wrapping(TextWrapping::Wrap),
                    ),
                comments_panel_with(
                    &input.comments,
                    input.my_avatar.as_ref(),
                    &input.comment_cb,
                    true,
                ),
            ));

        let main_pane = Grid::new()
            .grid_column(1)
            .rows([GridLength::Pixel(video_height), GridLength::Star(1.0)])
            .children((
                Border::new()
                    .margin(Thickness::new(12.0, 12.0, 12.0, 0.0))
                    .corner_radius(8.0)
                    .content(surface),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(info_and_comments),
            ));

        let drawer = Border::new()
            .width(pane_width)
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Stretch)
            .background(ThemeBrush::SolidBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::new(1.0, 0.0, 0.0, 0.0))
            .content(
                Grid::new()
                    .columns([GridLength::Pixel(40.0), GridLength::Star(1.0)])
                    .children((View::from(left_strip), View::from(main_pane))),
            );

        if self.drag_origin.is_some() {
            let on_resize = input.on_resize.clone();
            let final_w = pane_width;
            Border::new()
                .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                .on_pointer_moved(context.callback(|ev: PointerEventInfo| {
                    HalfPaneMessage::DragMove(ev.window_x)
                }))
                .on_pointer_released(context.callback(move |_: PointerEventInfo| {
                    on_resize.call(final_w);
                    HalfPaneMessage::DragEnd
                }))
                .content(drawer)
                .into()
        } else {
            drawer.into()
        }
    }
}
