//! 播放页：双栏布局（左：视频面 + 传输控制 + 详情 + 底部评论区；右：相关推荐）
//! 与评论区下滚触发右下角小窗（YouTube 式页内画中画，保持 `VideoSurface` 挂载不重建）。

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use qaq_bx_core::reply::{ReplyItem, ReplySort};
use qaq_bx_video::engine::VideoEngine;
use qaq_bx_video::native::NativePlayer;
use qaq_bx_video::stage::DanmakuStage;
use windows_reactor::*;

fn opt_arc_ptr_eq<T>(a: &Option<Arc<T>>, b: &Option<Arc<T>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn player_surface_view(
    engine: &Arc<VideoEngine>,
    native: Option<&Arc<NativePlayer>>,
    stage: Option<&Arc<DanmakuStage>>,
    tick: u32,
) -> View {
    if let Some(native) = native {
        // 两层结构固定:系统媒体引擎渲染面 + 同 cell 弹幕透明层,避免组件树重建产生合成空窗。
        let mut layers: Vec<View> = Vec::with_capacity(2);
        layers.push(
            View::component::<NativeSurface>(NativeSurfaceInput {
                player: native.clone(),
                tick,
            })
            .into(),
        );
        if let Some(stage) = stage {
            layers.push(
                View::component::<DanmakuSurface>(DanmakuSurfaceInput {
                    stage: stage.clone(),
                    tick,
                })
                .into(),
            );
        }
        Grid::new().children(layers).into()
    } else {
        View::component::<VideoSurface>(VideoSurfaceInput {
            engine: engine.clone(),
            tick,
        })
    }
}

use crate::auth::avatar_thumbnail_url;
use crate::data::VideoItem;
use crate::layout::ViewportLayout;
use crate::pages::fullscreen::DoubleClickTracker;
use crate::theme;
use crate::widgets::native_surface::{NativeSurface, NativeSurfaceInput};
use crate::widgets::danmaku_surface::{DanmakuSurface, DanmakuSurfaceInput};
use crate::widgets::video_surface::{VideoSurface, VideoSurfaceInput};

const PLAYER_SLOT_HEIGHT: f64 = 420.0;
const RIGHT_RAIL_WIDTH: f64 = 360.0;
const PIP_SCROLL_THRESHOLD: f64 = 300.0;
pub const HALF_PANE_MIN_WIDTH: f64 = 340.0;
pub const HALF_PANE_MAX_WIDTH: f64 = 820.0;
const HALF_PANE_MIN_VIDEO_HEIGHT: f64 = 160.0;
const HALF_PANE_MAX_VIDEO_HEIGHT: f64 = 440.0;
const HALF_PANE_CHROME_RESERVE_HEIGHT: f64 = 220.0;

pub fn clamp_half_pane_width(width: f64, max_width: f64) -> f64 {
    let upper = if max_width.is_finite() {
        max_width.clamp(HALF_PANE_MIN_WIDTH, HALF_PANE_MAX_WIDTH)
    } else {
        HALF_PANE_MAX_WIDTH
    };
    if width.is_finite() {
        width.clamp(HALF_PANE_MIN_WIDTH, upper)
    } else {
        HALF_PANE_MIN_WIDTH
    }
}

pub fn compute_half_pane_video_height(pane_width: f64, max_height: f64) -> f64 {
    let ideal_16_9 = (pane_width - 40.0 - 24.0) * 9.0 / 16.0;
    let max_by_viewport = if max_height.is_finite() && max_height > 0.0 {
        (max_height - HALF_PANE_CHROME_RESERVE_HEIGHT)
            .clamp(HALF_PANE_MIN_VIDEO_HEIGHT, HALF_PANE_MAX_VIDEO_HEIGHT)
    } else {
        HALF_PANE_MAX_VIDEO_HEIGHT
    };
    ideal_16_9
        .round()
        .clamp(HALF_PANE_MIN_VIDEO_HEIGHT, max_by_viewport)
}

/// 半窗预览音量滑条宽度:随面板宽度自适应,任何宽度都出滑条(竖屏下
/// 滑条是除静音外唯一的音量控制,不再按 440 DIP 阈值整体隐藏)。
pub fn half_pane_volume_slider_width(pane_width: f64) -> f64 {
    if pane_width >= 440.0 { 68.0 } else { 44.0 }
}

pub fn desktop_transport_compact(left_inner_width: f64) -> bool {
    left_inner_width < 540.0
}

/// 相关推荐卡封面高（132px 宽对应的 16:9 区域）。
const RELATED_COVER_HEIGHT: f64 = 74.0;
/// 相关推荐卡定宽：右栏 360px 栅格列 − ScrollViewer 左右缘距(4+16)。
/// 与首页卡同款「显式定宽」配方——ScrollViewer 横向量测不可依赖,
/// 卡片必须自带宽度才能严格等宽满栏。
const RELATED_CARD_WIDTH: f64 = RIGHT_RAIL_WIDTH - 4.0 - 16.0;
/// 相关推荐卡标题定高槽（恒定预留两行，标题换行与否不改变卡片高度）。
const RELATED_TITLE_HEIGHT: f64 = 40.0;
/// 相关推荐卡元信息定高槽（UP 主 / 播放量各一行）。
const RELATED_META_HEIGHT: f64 = 18.0;
/// 相关推荐卡文本列行距（与 StackPanel::spacing 同源，供高度推导）。
const RELATED_TEXT_SPACING: f64 = 4.0;

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
    /// 评论草稿变更(U1;shell 单源持有草稿)。
    pub on_draft: Callback<Rc<str>>,
    /// 提交发布(U1;未登录由 shell 侧拦截提示)。
    pub on_submit: Callback<()>,
    /// 发布错误/登录提示(内联展示;None = 无)。
    pub composer_notice: Option<String>,
    /// 是否已登录(未登录发布按钮置灰并提示)。
    pub can_post: bool,
    /// 评论草稿(shell 单源;TextBox 受控回显)。
    pub composer_draft: String,
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
    /// F3:渲染换代号——shell 每次写入(items 原位 push/封面回填/状态翻转)必须 +1,
    /// PlayerPage 据此重建相关推荐虚拟源(与 `CommentsSnapshot.rev` 同契约)。
    pub rev: u64,
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
    pub danmaku_enabled: bool,
    /// 播放页画质菜单((qn, 标签) 降序;空 = 未解析或单档;F2)。
    pub quality_menu: Vec<(i64, String)>,
    /// 当前生效清晰度代号(0 = 未加载)。
    pub current_qn: i64,
    /// U3 分P菜单((cid, 标签);空 = 单P 视频)。
    pub pages_menu: Vec<(u64, String)>,
    /// U3 当前生效分P cid。
    pub current_cid: u64,
    pub layout: ViewportLayout,
    /// SwapChain 挂载尝试节拍（仅在未挂载时按 250ms 递增驱动 VideoSurface::try_mount）。
    pub mount_tick: u32,
    /// 展示总长(引擎 duration 优先,卡片元数据兜底)。
    pub duration_secs: u32,
    /// 引擎错误/媒体失败提示(None = 无)。
    pub notice: Option<String>,
    /// 解析/缓冲中:视频面上叠加 Fluent 环形加载指示(shell 单源,首个 has_media 熄灭)。
    pub loading: bool,
    pub engine: Arc<VideoEngine>,
    /// v0.3 spike:系统媒体引擎(QAQ_BX_NATIVE=1 时与 engine 互斥生效)。
    pub native: Option<Arc<NativePlayer>>,
    /// v0.3:弹幕透明层(native 模式下与 `native` 同时就位——mpv 模式的弹幕
    /// 仍由视频引擎同帧叠画,这一层只在系统媒体引擎路径出现)。
    pub stage: Option<Arc<qaq_bx_video::stage::DanmakuStage>>,
    pub comments: CommentsSnapshot,
    pub comment_cb: CommentCallbacks,
    /// 相关推荐（真实 archive/related 数据）。
    pub related: RelatedPane,
    pub on_open_video: Callback<String>,
    pub on_related_retry: Callback<()>,
    pub on_toggle: Callback<()>,
    pub on_seek: Callback<f64>,
    pub on_volume: Callback<f64>,
    /// 音量条滚轮(±5/格,复用 shell `VolumeDelta` 钳制)。
    pub on_volume_wheel: Callback<i32>,
    pub on_toggle_mute: Callback<()>,
    pub on_toggle_danmaku: Callback<()>,
    /// 画质切换(F2;携带目标 qn)。
    pub on_select_quality: Callback<i64>,
    /// U3 分P切换(携带目标 cid)。
    pub on_select_page: Callback<u64>,
    pub on_enter_fullscreen: Callback<()>,
    /// `--demo` 导演器按时序覆写手机端标签页 / 简介展开态（生产路径固定为 None）。
    pub demo_override: Option<(u64, PlayerTab, bool)>,
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
            && self.danmaku_enabled == other.danmaku_enabled
            && self.quality_menu == other.quality_menu
            && self.current_qn == other.current_qn
            && self.pages_menu == other.pages_menu
            && self.current_cid == other.current_cid
            && self.layout == other.layout
            && self.mount_tick == other.mount_tick
            && self.duration_secs == other.duration_secs
            && self.notice == other.notice
            && self.loading == other.loading
            && Arc::ptr_eq(&self.engine, &other.engine)
            && opt_arc_ptr_eq(&self.native, &other.native)
            && opt_arc_ptr_eq(&self.stage, &other.stage)
            && self.comments == other.comments
            && self.comment_cb == other.comment_cb
            && self.related == other.related
            && self.on_open_video == other.on_open_video
            && self.on_related_retry == other.on_related_retry
            && self.on_toggle == other.on_toggle
            && self.on_seek == other.on_seek
            && self.on_volume == other.on_volume
            && self.on_volume_wheel == other.on_volume_wheel
            && self.on_toggle_mute == other.on_toggle_mute
            && self.on_toggle_danmaku == other.on_toggle_danmaku
            && self.on_select_quality == other.on_select_quality
            && self.on_select_page == other.on_select_page
            && self.on_enter_fullscreen == other.on_enter_fullscreen
            && self.demo_override == other.demo_override
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayerTab {
    #[default]
    Intro,
    Comments,
}

pub struct PlayerPage {
    scroll_y: f64,
    scroll_reset_gen: u64,
    pip_pinned: bool,
    active_tab: PlayerTab,
    desc_expanded: bool,
    last_bvid: String,
    last_demo_override: Option<(u64, PlayerTab, bool)>,
    double_click: DoubleClickTracker,
    /// F3:评论/相关列表虚拟源缓存(桌面/紧凑两档;同 (档位, rev) 复用同一实例)。
    comments_cache: VirtualSourceCache,
    related_cache: VirtualSourceCache,
}

#[derive(Clone, PartialEq)]
pub enum Message {
    Scrolled(f64),
    TogglePip,
    BackToTop,
    SelectTab(PlayerTab),
    ToggleDesc,
    VideoPressed(f64, f64, bool),
}

impl PlayerPage {
    /// F3:按 (档位, rev) 刷新评论/相关虚拟源缓存;rev 未变则零成本复用。
    /// 只在 create/input_changed 调用(view 持 &self 不可写);view 侧 peek。
    fn refresh_virtual_sources(&mut self, input: &PlayerInput) {
        let snapshot = &input.comments;
        let rev = snapshot.rev;
        let data = Arc::clone(&snapshot.data);
        let cb = input.comment_cb.clone();
        self.comments_cache.get(VARIANT_DESKTOP, rev, || {
            comments_virtual_source(&data, &cb, false, rev)
        });
        self.comments_cache.get(VARIANT_COMPACT, rev, || {
            comments_virtual_source(&data, &cb, true, rev)
        });
        let related = &input.related;
        let rel_rev = related.rev;
        let on_open = input.on_open_video.clone();
        self.related_cache.get(VARIANT_DESKTOP, rel_rev, || {
            related_virtual_source(related, &on_open, false, rel_rev)
        });
        self.related_cache.get(VARIANT_COMPACT, rel_rev, || {
            related_virtual_source(related, &on_open, true, rel_rev)
        });
    }

    /// view 侧取评论虚拟源(正常路径命中缓存;未命中现建兜底,正确性优先)。
    fn comments_source(&self, input: &PlayerInput, variant: usize) -> VirtualSource {
        let snapshot = &input.comments;
        self.comments_cache
            .peek(variant, snapshot.rev)
            .unwrap_or_else(|| {
                comments_virtual_source(
                    &snapshot.data,
                    &input.comment_cb,
                    variant == VARIANT_COMPACT,
                    snapshot.rev,
                )
            })
    }

    /// view 侧取相关推荐虚拟源(同上)。
    fn related_source(&self, input: &PlayerInput, variant: usize) -> VirtualSource {
        let related = &input.related;
        self.related_cache
            .peek(variant, related.rev)
            .unwrap_or_else(|| {
                related_virtual_source(
                    related,
                    &input.on_open_video,
                    variant == VARIANT_COMPACT,
                    related.rev,
                )
            })
    }
}

impl Component for PlayerPage {
    type Input = PlayerInput;
    type Message = Message;

    fn create(input: &PlayerInput, _context: &ComponentContext<Self>) -> Self {
        let (active_tab, desc_expanded) = input
            .demo_override
            .map(|(_, tab, expanded)| (tab, expanded))
            .unwrap_or((PlayerTab::Intro, false));
        let mut page = Self {
            scroll_y: 0.0,
            scroll_reset_gen: 1,
            pip_pinned: false,
            active_tab,
            desc_expanded,
            last_bvid: input.item.bvid.clone(),
            last_demo_override: input.demo_override,
            double_click: DoubleClickTracker::new(),
            comments_cache: VirtualSourceCache::default(),
            related_cache: VirtualSourceCache::default(),
        };
        page.refresh_virtual_sources(input);
        page
    }

    fn input_changed(&mut self, input: &PlayerInput, _context: &ComponentContext<Self>) {
        self.refresh_virtual_sources(input);
        if self.last_bvid != input.item.bvid {
            self.last_bvid = input.item.bvid.clone();
            self.active_tab = PlayerTab::Intro;
            self.desc_expanded = false;
            self.scroll_y = 0.0;
            self.scroll_reset_gen = self.scroll_reset_gen.wrapping_add(1);
        }
        if self.last_demo_override != input.demo_override {
            self.last_demo_override = input.demo_override;
            if let Some((_, tab, expanded)) = input.demo_override {
                self.active_tab = tab;
                self.desc_expanded = expanded;
            }
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::Scrolled(y) => self.scroll_y = y.max(0.0),
            Message::TogglePip => self.pip_pinned = !self.pip_pinned,
            Message::BackToTop => {
                self.pip_pinned = false;
                self.scroll_y = 0.0;
                self.scroll_reset_gen = self.scroll_reset_gen.wrapping_add(1);
            }
            Message::SelectTab(tab) => {
                self.active_tab = tab;
            }
            Message::ToggleDesc => {
                self.desc_expanded = !self.desc_expanded;
            }
            Message::VideoPressed(x, y, left_pressed) => {
                if self
                    .double_click
                    .record_press(Instant::now(), x, y, left_pressed)
                {
                    context.input().on_enter_fullscreen.call(());
                }
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
            player_surface_view(
                &input.engine,
                input.native.as_ref(),
                input.stage.as_ref(),
                input.mount_tick,
            )
        };
        let surface = surface_with_loading(surface, input.loading);
        let surface = surface_with_double_click(
            surface,
            context
                .callback(|ev: PointerEventInfo| {
                    Message::VideoPressed(ev.window_x, ev.window_y, ev.is_left_button_pressed)
                })
                .into(),
        );

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

        let is_single_col = input.layout.is_single_column();
        let is_mobile = input.layout.is_mobile();
        let margin_x = input.layout.page_margin_x();

        let transport: View = if is_single_col {
            single_column_transport(input, margin_x)
        } else {
            let left_inner_w = (input.layout.content_width() - RIGHT_RAIL_WIDTH - 28.0).max(320.0);
            let compact_desktop = desktop_transport_compact(left_inner_w);
            let play_btn = play_pause_button(
                input.playing,
                compact_desktop,
                false,
                input.on_toggle.clone(),
            );
            let mute_btn = mute_toggle_button(
                input.volume == 0,
                compact_desktop,
                false,
                input.on_toggle_mute.clone(),
            );
            let danmaku_btn = danmaku_toggle_button(
                input.danmaku_enabled,
                compact_desktop,
                false,
                input.on_toggle_danmaku.clone(),
            );
            let fullscreen_btn = fullscreen_toggle_button(
                false,
                compact_desktop,
                false,
                input.on_enter_fullscreen.clone(),
            );
            let mut right_items: Vec<View> = vec![
                mute_btn,
                volume_slider(
                    if compact_desktop { 64.0 } else { 110.0 },
                    input.volume,
                    &input.on_volume,
                    &input.on_volume_wheel,
                )
                .into(),
            ];
            if !compact_desktop {
                right_items.push(
                    TextBlock::new()
                        .text(format!("{}%", input.volume))
                        .font_size(12.0)
                        .opacity(0.6)
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                );
            }
            right_items.push(danmaku_btn);
            if let Some(q) = quality_selector(
                &input.quality_menu,
                input.current_qn,
                &input.on_select_quality,
                compact_desktop,
            ) {
                right_items.push(q);
            }
            if let Some(p) = pages_selector(
                &input.pages_menu,
                input.current_cid,
                &input.on_select_page,
                compact_desktop,
            ) {
                right_items.push(p);
            }
            let pip_label = match (compact_desktop, is_pip) {
                (true, true) => "主窗",
                (true, false) => "小窗",
                (false, true) => "还原主窗",
                (false, false) => "右下角小窗",
            };
            let pip_btn = Button::new()
                .style(if self.pip_pinned {
                    ButtonStyle::Accent
                } else {
                    ButtonStyle::Subtle
                })
                .content(
                    TextBlock::new()
                        .text(pip_label)
                        .font_size(if compact_desktop { 11.5 } else { 13.0 }),
                )
                .on_click(context.message(Message::TogglePip));
            right_items.push(pip_btn.into());
            right_items.push(fullscreen_btn);

            let transport_right = StackPanel::new()
                .grid_column(2)
                .orientation(Orientation::Horizontal)
                .spacing(if compact_desktop { 6.0 } else { 8.0 })
                .vertical_alignment(VerticalAlignment::Center)
                .children(right_items);

            StackPanel::new()
                .margin(Thickness::new(0.0, 10.0, 0.0, 4.0))
                .spacing(6.0)
                .children((
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
                                    time_label(input.position, input.duration_secs, 12.0),
                                )),
                            transport_right,
                        )),
                ))
                .into()
        };

        let up_row = StackPanel::new()
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
                    if is_mobile { 32.0 } else { 36.0 },
                    if is_mobile { 16.0 } else { 18.0 },
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
                            .font_size(if is_mobile { 13.0 } else { 14.0 })
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(1),
                        TextBlock::new()
                            .text(format!(
                                "播放 {} · 弹幕 {} · 时长 {}",
                                theme::fmt_count(input.item.play_count),
                                theme::fmt_count(input.item.danmaku_count),
                                theme::fmt_duration(input.item.duration_secs)
                            ))
                            .font_size(11.5)
                            .opacity(0.6)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(1),
                    )),
            ));

        // Mobile / Medium 单列：参照 Bilibili 手机端结构，顶部吸顶「16:9 视频 + 单行紧凑播控 + 简介/评论双页签」，
        // 下方切换独立滚动：「简介」含 UP/标题/折叠简介/4 等分互动 + 相关推荐；「评论」直达评论区。
        if is_single_col {
            let video_h = input.layout.player_video_height();
            let sticky_video = Border::new()
                .height(video_h)
                .margin(Thickness::new(margin_x, 4.0, margin_x, 0.0))
                .corner_radius(8.0)
                .content(surface);

            let comments_label = comments_tab_label(input.comments.data.total_count);
            let tab_item = |tab: PlayerTab, label: String| {
                let selected = self.active_tab == tab;
                Border::new()
                    .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                    .padding(Thickness::new(4.0, 6.0, 4.0, 0.0))
                    .on_pointer_released({
                        let msg = context.message(Message::SelectTab(tab));
                        move |_| msg.call(())
                    })
                    .content(
                        StackPanel::new().spacing(5.0).children((
                            TextBlock::new()
                                .text(label)
                                .font_size(14.0)
                                .font_weight(if selected {
                                    FontWeight::SEMI_BOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .opacity(if selected { 1.0 } else { 0.65 })
                                .horizontal_alignment(HorizontalAlignment::Center),
                            Border::new()
                                .width(24.0)
                                .height(2.5)
                                .corner_radius(2.0)
                                .horizontal_alignment(HorizontalAlignment::Center)
                                .background(if selected {
                                    Brush::Theme(ThemeBrush::Accent)
                                } else {
                                    Brush::Solid(Color::argb(0, 0, 0, 0))
                                }),
                        )),
                    )
            };

            let tab_bar = StackPanel::new().children((
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(22.0)
                    .margin(Thickness::new(margin_x, 2.0, margin_x, 0.0))
                    .children((
                        tab_item(PlayerTab::Intro, "简介".to_string()),
                        tab_item(PlayerTab::Comments, comments_label),
                    )),
                Border::new().height(1.0).background(ThemeBrush::CardStroke),
            ));

            let sticky_top = StackPanel::new().grid_row(0).children((
                View::from(sticky_video),
                transport,
                View::from(tab_bar),
            ));

            let tab_body = match self.active_tab {
                PlayerTab::Intro => {
                    let title_header = Border::new()
                        .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                        .on_pointer_released({
                            let msg = context.message(Message::ToggleDesc);
                            move |_| msg.call(())
                        })
                        .content(
                            Grid::new()
                                .columns([GridLength::Star(1.0), GridLength::Auto])
                                .column_spacing(8.0)
                                .children((
                                    TextBlock::new()
                                        .text(input.item.title.clone())
                                        .font_size(if is_mobile { 15.5 } else { 17.5 })
                                        .font_weight(FontWeight::BOLD)
                                        .text_wrapping(TextWrapping::Wrap)
                                        .text_trimming(TextTrimming::CharacterEllipsis)
                                        .max_lines(if self.desc_expanded { 10 } else { 2 }),
                                    FontIcon::new()
                                        .grid_column(1)
                                        .glyph(if self.desc_expanded {
                                            "\u{E70E}"
                                        } else {
                                            "\u{E70D}"
                                        })
                                        .font_size(12.0)
                                        .opacity(0.6)
                                        .vertical_alignment(VerticalAlignment::Top)
                                        .margin(Thickness::new(0.0, 4.0, 0.0, 0.0)),
                                )),
                        );

                    let mut intro_children: Vec<View> =
                        vec![View::from(up_row), View::from(title_header)];
                    if self.desc_expanded {
                        intro_children.push(
                            TextBlock::new()
                                .text(format_desc_body(&input.item.bvid, input.desc.as_deref()))
                                .font_size(12.5)
                                .opacity(0.78)
                                .text_wrapping(TextWrapping::Wrap)
                                .into(),
                        );
                    }
                    intro_children.push(
                        Grid::new()
                            .columns([
                                GridLength::Star(1.0),
                                GridLength::Star(1.0),
                                GridLength::Star(1.0),
                                GridLength::Star(1.0),
                            ])
                            .column_spacing(6.0)
                            .children((
                                action_pill("点赞", input.item.like_count, true, 0),
                                action_pill("投币", input.item.coin_count, true, 1),
                                action_pill("收藏", input.item.favorite_count, true, 2),
                                action_pill("分享", input.item.share_count, true, 3),
                            ))
                            .into(),
                    );
                    intro_children.push(
                        Border::new()
                            .height(1.0)
                            .background(ThemeBrush::CardStroke)
                            .into(),
                    );
                    intro_children.push(related_panel(
                        input,
                        is_mobile,
                        &self.related_source(
                            input,
                            if is_mobile {
                                VARIANT_COMPACT
                            } else {
                                VARIANT_DESKTOP
                            },
                        ),
                    ));

                    View::from(
                        StackPanel::new()
                            .spacing(12.0)
                            .margin(Thickness::new(margin_x, 12.0, margin_x, 24.0))
                            .children(intro_children),
                    )
                }
                PlayerTab::Comments => {
                    let comments_section = comments_panel_with(
                        &input.comments,
                        input.my_avatar.as_ref(),
                        &input.comment_cb,
                        &input.comment_cb.composer_draft,
                        true,
                        &self.comments_source(input, VARIANT_COMPACT),
                    );
                    View::from(
                        StackPanel::new()
                            .spacing(10.0)
                            .margin(Thickness::new(margin_x, 10.0, margin_x, 24.0))
                            .children([comments_section]),
                    )
                }
            };

            let tab_key = match self.active_tab {
                PlayerTab::Intro => "intro",
                PlayerTab::Comments => "comments",
            };
            let lower_scroll = Grid::new().grid_row(1).keyed_children([keyed(
                format!("player-mobile-{}-{}", input.item.bvid, tab_key),
                ScrollViewer::new()
                    .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                    .content(tab_body),
            )]);

            return Grid::new()
                .rows([GridLength::Auto, GridLength::Star(1.0)])
                .children((View::from(sticky_top), View::from(lower_scroll)))
                .into();
        }

        let up_and_actions = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .children((
                up_row,
                StackPanel::new()
                    .grid_column(1)
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
                        action_pill("点赞", input.item.like_count, false, 0),
                        action_pill("投币", input.item.coin_count, false, 0),
                        action_pill("收藏", input.item.favorite_count, false, 0),
                        action_pill("分享", input.item.share_count, false, 0),
                    )),
            ));

        let info_and_actions = StackPanel::new().spacing(12.0).children((
            TextBlock::new()
                .text(input.item.title.clone())
                .font_size(20.0)
                .font_weight(FontWeight::BOLD)
                .text_wrapping(TextWrapping::Wrap),
            up_and_actions,
            // 简介：内联展示（无展开/收起态，长文本随页面滚动），替代 Expander。
            StackPanel::new().spacing(4.0).children((
                TextBlock::new()
                    .text("简介")
                    .font_size(13.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .opacity(0.75),
                TextBlock::new()
                    .text(format_desc_body(&input.item.bvid, input.desc.as_deref()))
                    .font_size(13.0)
                    .opacity(0.85)
                    .text_wrapping(TextWrapping::Wrap),
            )),
        ));

        let comments_section = comments_panel_with(
            &input.comments,
            input.my_avatar.as_ref(),
            &input.comment_cb,
            &input.comment_cb.composer_draft,
            false,
            &self.comments_source(input, VARIANT_DESKTOP),
        );

        let left_scroll_content = StackPanel::new()
            .spacing(14.0)
            .margin(Thickness::new(16.0, 16.0, 12.0, 24.0))
            .children((
                top_player_slot,
                View::from(transport),
                View::from(info_and_actions),
                comments_section,
            ));

        let left_scroll = Grid::new().grid_column(0).keyed_children([keyed(
            format!("player-left-scroll-{}", self.scroll_reset_gen),
            ScrollViewer::new()
                .on_view_changed(
                    context.callback(|ev: ScrollEventInfo| Message::Scrolled(ev.offset_y)),
                )
                .content(left_scroll_content),
        )]);

        let right_rail = ScrollViewer::new()
            .grid_column(1)
            .margin(Thickness::new(4.0, 16.0, 16.0, 16.0))
            // 横向 Disabled:默认 Auto 会以无限宽量测内容,推荐卡 Star 列退化为
            // 自然宽,卡片随标题长短参差且不满栏;禁用后按视口宽满栏等宽。
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(related_panel(
                input,
                false,
                &self.related_source(input, VARIANT_DESKTOP),
            ));

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
                                .columns([
                                    GridLength::Auto,
                                    GridLength::Star(1.0),
                                    GridLength::Auto,
                                ])
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
fn action_pill(label: &str, count: Option<u64>, stretch: bool, col: i32) -> View {
    if stretch {
        let icon_glyph = match label {
            "点赞" => "\u{E8E1}",
            "投币" => "\u{E8EC}",
            "收藏" => "\u{E734}",
            "分享" => "\u{E72D}",
            _ => "\u{E8E1}",
        };
        let caption = match count {
            Some(value) if value > 0 => format!("{label} {}", theme::fmt_count(value)),
            _ => label.to_string(),
        };
        return Button::new()
            .grid_column(col)
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Center)
            .content(
                StackPanel::new().spacing(3.0).children((
                    FontIcon::new()
                        .glyph(icon_glyph)
                        .font_size(15.0)
                        .horizontal_alignment(HorizontalAlignment::Center),
                    TextBlock::new()
                        .text(caption)
                        .font_size(10.5)
                        .opacity(0.75)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .max_lines(1)
                        .horizontal_alignment(HorizontalAlignment::Center),
                )),
            )
            .into();
    }

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
                .spacing(4.0)
                .children(children),
        )
        .into()
}

fn comments_tab_label(total_count: Option<u64>) -> String {
    match total_count {
        Some(n) if n > 0 => format!("评论 {}", theme::fmt_count(n)),
        _ => "评论".to_string(),
    }
}

/// 加载态在视频面上叠加 Fluent 环形指示器(ProgressRing)。状态单源在 shell:
/// start_playback 点亮,首个 `has_media` 或失败熄灭;视图纯映射,无常驻组件状态,
/// 因此任何重建都不会让指示器卡死。
///
/// 结构稳定约定(黑屏排查 2026-11):loading 与否**不得改变树形**——早年
/// `if loading { Grid(surface, overlay) } else { surface }` 会让 VideoSurface
/// 组件路径随 loading 翻转变化,SwapChainPanel 反复销毁重建(卸载清理 + 重挂
/// 空窗),是快速伸缩窗口/切视频时视频区闪黑的主因之一。现两种状态共用同一
/// Grid 结构,仅切换覆盖层 opacity 与 Ring 动画开关。
pub(crate) fn surface_with_loading(surface: View, loading: bool) -> View {
    Grid::new()
        .children((
            surface,
            StackPanel::new()
                .spacing(10.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .opacity(if loading { 1.0 } else { 0.0 })
                .children((
                    ProgressRing::new()
                        .is_indeterminate(true)
                        .is_active(loading)
                        .width(44.0)
                        .height(44.0),
                    TextBlock::new()
                        .text("视频加载中…")
                        .font_size(13.0)
                        .foreground(Brush::Solid(Color::rgb(255, 255, 255)))
                        .opacity(0.85),
                )),
        ))
        .into()
}

fn surface_with_double_click(surface: View, on_pressed: PointerCallback) -> View {
    Grid::new()
        .children((
            surface,
            View::from(
                Border::new()
                    .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                    .on_pointer_pressed(on_pressed),
            ),
        ))
        .into()
}

/// 简介正文：不再内嵌标题（顶部已大字展示），空简介走占位，末行保留 BV 号。
/// 纯内联展示、无展开状态——任何视图重建都不会把用户操作"顶"回去。
fn format_desc_body(bvid: &str, desc: Option<&str>) -> String {
    let d = desc
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "-")
        .unwrap_or("暂无简介");
    format!("{d}\n\nBV 号：{bvid}")
}

// ---------- F3:评论/相关列表行虚拟化 ----------

/// 虚拟源缓存档位(桌面全量 / 移动紧凑各一枚)。
const VARIANT_DESKTOP: usize = 0;
const VARIANT_COMPACT: usize = 1;

/// 虚拟源按档位缓存。
/// `VirtualSource` 判等是 Rc 指针等价——同一 (档位, rev) 必须复用同一实例才能跨
/// pass 判等短路;每 pass 现建会得到新 Rc 闭包,判等必 false → 行整体重 realize。
#[derive(Default)]
struct VirtualSourceCache {
    entries: [Option<(u64, VirtualSource)>; 2],
}

impl VirtualSourceCache {
    /// 取/建:同 (档位, rev) 复用;否则 build 重建并占位。
    fn get(
        &mut self,
        variant: usize,
        rev: u64,
        build: impl FnOnce() -> VirtualSource,
    ) -> VirtualSource {
        if let Some((cached_rev, source)) = &self.entries[variant] {
            if *cached_rev == rev {
                return source.clone();
            }
        }
        let source = build();
        self.entries[variant] = Some((rev, source.clone()));
        source
    }

    /// 只读窥视(view 持 &self 不可写;未命中由调用方现建兜底)。
    fn peek(&self, variant: usize, rev: u64) -> Option<VirtualSource> {
        let (cached_rev, source) = self.entries[variant].as_ref()?;
        (*cached_rev == rev).then(|| source.clone())
    }
}

/// 评论行键:段位前缀(置顶 `c-top-` / 普通 `c-`,沿用旧 KeyedView 语义防 rpid
/// 跨段复用)+ 行序(reactor 对重复键在 mount/revision 变更时 panic;行序入键保证
/// 构造唯一)+ rpid(append-only 加载时已 realize 行按稿件身份复用)。
fn comment_row_key(row_idx: usize, top_len: usize, rpid: u64) -> String {
    if row_idx < top_len {
        format!("c-top-{row_idx}-{rpid}")
    } else {
        format!("c-{row_idx}-{rpid}")
    }
}

/// 相关推荐行键(行序防重,bvid 保身份)。
fn related_row_key(row_idx: usize, bvid: &str) -> String {
    format!("r-{row_idx}-{bvid}")
}

/// 评论区行虚拟源——一行一张评论卡(变高行,StackLayout 支持)。
/// 闭包捕获整份 `Arc<CommentsData>`(UP 标记/头像表/楼中楼展开态随快照 rev 整体
/// 换代)与 `CommentCallbacks`(行内只消费 on_toggle_sub / on_load_more_sub 两个
/// 路由回调)。
fn comments_virtual_source(
    data: &Arc<CommentsData>,
    cb: &CommentCallbacks,
    compact_pane: bool,
    rev: u64,
) -> VirtualSource {
    let top_len = data.top_replies.len();
    let mut rows: Vec<Rc<ReplyItem>> = Vec::with_capacity(top_len + data.replies.len());
    rows.extend(data.top_replies.iter().cloned().map(Rc::new));
    rows.extend(data.replies.iter().cloned().map(Rc::new));
    let rows = Rc::new(rows);
    let key_rows = Rc::clone(&rows);
    let data = Arc::clone(data);
    let cb = cb.clone();
    VirtualSource::new(
        rev,
        rows.len(),
        move |row_idx| Key::from(comment_row_key(row_idx, top_len, key_rows[row_idx].rpid)),
        move |row_idx| comment_card(&rows[row_idx], &data, &cb, compact_pane),
    )
}

/// 相关推荐行虚拟源——一行一张相关卡。
fn related_virtual_source(
    related: &RelatedPane,
    on_open: &Callback<String>,
    compact_card: bool,
    rev: u64,
) -> VirtualSource {
    let rows: Rc<Vec<VideoItem>> = Rc::new(related.items.clone());
    let key_rows = Rc::clone(&rows);
    let on_open = on_open.clone();
    VirtualSource::new(
        rev,
        rows.len(),
        move |row_idx| Key::from(related_row_key(row_idx, &key_rows[row_idx].bvid)),
        move |row_idx| related_card(&rows[row_idx], compact_card, &on_open),
    )
}

/// 评论发布器:单行三列 Grid `[头像 | 输入框 | 发布]`,未登录按钮置灰 +
/// 行内提示(notice 占第 2 行输入框列)。草稿/提交走 shell 单源。
/// 容器必须是 Grid——子元素的 Grid.Column 附加属性只有 Grid 布局会读,
/// 挂 StackPanel 会静默竖堆(v0.2.0 回归,见 composer 布局回归测试)。
fn comment_composer(
    my_avatar: Option<&EncodedImage>,
    cb: &CommentCallbacks,
    draft: &str,
) -> View {
    let submit_btn = if cb.can_post {
        Button::new()
            .grid_column(2)
            .style(ButtonStyle::Accent)
            .content("发布")
            .on_click(cb.on_submit.clone())
    } else {
        Button::new()
            .grid_column(2)
            .style(ButtonStyle::Subtle)
            .is_enabled(false)
            .content("发布")
    };
    let mut composer_rows: Vec<View> = vec![
        theme::avatar_view(my_avatar, "我", 34.0, 17.0)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        TextBox::new()
            .grid_column(1)
            .margin(Thickness::xy(10.0, 0.0))
            .placeholder_text(if cb.can_post {
                "发一条友善的评论…"
            } else {
                "登录后即可发布评论"
            })
            .text(draft.to_string())
            .on_text_changed({
                let on_draft = cb.on_draft.clone();
                move |text: Rc<str>| on_draft.call(text)
            })
            .into(),
        submit_btn.into(),
    ];
    if let Some(notice) = &cb.composer_notice {
        composer_rows.push(
            TextBlock::new()
                .grid_column(1)
                .grid_row(1)
                .text(notice.clone())
                .font_size(11.5)
                .foreground(Brush::Solid(Color::rgb(196, 43, 28)))
                .margin(Thickness::new(10.0, 6.0, 0.0, 0.0))
                .into(),
        );
    }
    // 无 notice 时第 2 行 Auto 收零高,不影响单行形态。
    Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .rows([GridLength::Auto, GridLength::Auto])
        .children(composer_rows)
        .into()
}

fn comments_panel_with(
    snapshot: &CommentsSnapshot,
    my_avatar: Option<&EncodedImage>,
    cb: &CommentCallbacks,
    draft: &str,
    compact_pane: bool,
    source: &VirtualSource,
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

    let composer = comment_composer(my_avatar, cb, draft);

    let mut rows: Vec<KeyedView> = vec![
        KeyedView::new("c-header", header),
        KeyedView::new("c-composer", composer),
    ];

    if data.top_replies.is_empty() && data.replies.is_empty() {
        if data.loading {
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
        } else if data.aid == 0 {
            // U4:直播间/元数据未就绪 → 不再永久转圈。
            rows.push(KeyedView::new(
                "c-empty",
                TextBlock::new()
                    .text("该内容暂无评论区")
                    .font_size(13.0)
                    .opacity(0.55)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(Thickness::xy(0.0, 18.0)),
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
        // F3:整表卡片换成虚拟源行。同 rev 复用缓存实例时,本面板每 pass 只构建
        // header/composer/footer 与一个 ItemsRepeater 节点,行声明零重跑;
        // 卡间距(原 StackPanel spacing 14)改由评论卡自带下边距补齐。
        rows.push(KeyedView::new(
            "c-list",
            View::from(ItemsRepeater::new().virtual_source(source.clone())),
        ));

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
        .padding(Thickness::xy(
            if compact_pane { 12.0 } else { 16.0 },
            if compact_pane { 12.0 } else { 16.0 },
        ))
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
    let mut text = TextBlock::new().text(label).font_size(10.0);
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
                .content(TextBlock::new().text(label).font_size(11.0).opacity(0.75))
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
        // F3:行间距改由卡片自带(虚拟行无 sibling spacing;对应原 StackPanel spacing 14)。
        .margin(Thickness::new(0.0, 0.0, 0.0, 14.0))
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
                            .text(if state.loading {
                                "加载中…"
                            } else {
                                "加载更多回复"
                            })
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
fn related_status_view(text: &str, on_retry: Option<&Callback<()>>, compact_card: bool) -> View {
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
    let mut box_border = Border::new()
        .padding(Thickness::xy(10.0, 10.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(StackPanel::new().spacing(8.0).children(children));
    box_border = if compact_card {
        box_border.horizontal_alignment(HorizontalAlignment::Stretch)
    } else {
        box_border.width(RELATED_CARD_WIDTH)
    };
    box_border.into()
}

fn related_panel(input: &PlayerInput, compact_card: bool, source: &VirtualSource) -> View {
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
                compact_card,
            ));
        } else {
            cards.push(related_status_view("相关推荐加载中…", None, compact_card));
        }
    } else if related.items.is_empty() {
        cards.push(related_status_view("暂无相关视频", None, compact_card));
    } else {
        // F3:同评论区——整表卡片换虚拟源行,同 rev 复用即零行重跑;
        // 卡间距(原 StackPanel spacing 10)改由相关卡自带下边距补齐。
        cards.push(ItemsRepeater::new().virtual_source(source.clone()).into());
    }

    StackPanel::new().spacing(10.0).children(cards).into()
}

fn related_card(item: &VideoItem, compact_card: bool, on_open: &Callback<String>) -> View {
    let bvid = item.bvid.clone();
    let on_click = on_open.clone();
    let (thumb_w, thumb_h) = if compact_card {
        (116.0, 65.0)
    } else {
        (132.0, RELATED_COVER_HEIGHT)
    };
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
    // Mobile 改用原生 Border 点击释放避免 ResourceOverrides 装箱崩溃；Desktop 右栏显式定宽满栏。
    let mut card = Border::new()
        .padding(Thickness::xy(8.0, 8.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        // F3:行间距改由卡片自带(对应原 StackPanel spacing 10)。
        .margin(Thickness::new(0.0, 0.0, 0.0, 10.0))
        .on_pointer_released(move |_| on_click.call(bvid.clone()));
    card = if compact_card {
        card.horizontal_alignment(HorizontalAlignment::Stretch)
    } else {
        card.width(RELATED_CARD_WIDTH)
    };
    card.content(
        Grid::new()
            .columns([GridLength::Pixel(thumb_w), GridLength::Star(1.0)])
            .children((
                Grid::new()
                    .height(thumb_h)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
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
                    .spacing(RELATED_TEXT_SPACING)
                    .children((
                        TextBlock::new()
                            .text(item.title.clone())
                            .font_size(if compact_card { 12.5 } else { 13.0 })
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_wrapping(TextWrapping::Wrap)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(2)
                            .height(RELATED_TITLE_HEIGHT),
                        TextBlock::new()
                            .text(item.author.clone())
                            .font_size(11.0)
                            .opacity(0.6)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(1)
                            .height(RELATED_META_HEIGHT),
                        TextBlock::new()
                            .text(format!("▶ {}", theme::fmt_count(item.play_count)))
                            .font_size(11.0)
                            .opacity(0.55)
                            .height(RELATED_META_HEIGHT),
                    )),
            )),
    )
    .into()
}

pub(crate) fn play_pause_button(
    playing: bool,
    compact: bool,
    on_dark: bool,
    on_toggle: Callback<()>,
) -> View {
    if compact {
        Border::new()
            .width(32.0)
            .height(32.0)
            .corner_radius(6.0)
            .background(hud_chip_background(on_dark))
            .border_brush(hud_chip_border(on_dark))
            .border_thickness(if on_dark { 0.0 } else { 1.0 })
            .on_pointer_released(move |_| on_toggle.call(()))
            .content(
                FontIcon::new()
                    .glyph(if playing { "\u{E769}" } else { "\u{E768}" })
                    .font_size(13.0)
                    .foreground(hud_foreground(on_dark))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center),
            )
            .into()
    } else {
        AppBarButton::new()
            .icon(Icon::symbol(if playing {
                Symbol::Pause
            } else {
                Symbol::Play
            }))
            .on_click(on_toggle)
            .into()
    }
}

pub(crate) fn mute_toggle_button(
    muted: bool,
    compact: bool,
    on_dark: bool,
    on_toggle_mute: Callback<()>,
) -> View {
    if compact {
        Border::new()
            .width(28.0)
            .height(28.0)
            .corner_radius(6.0)
            .background(hud_chip_background(on_dark))
            .border_brush(hud_chip_border(on_dark))
            .border_thickness(if on_dark { 0.0 } else { 1.0 })
            .on_pointer_released(move |_| on_toggle_mute.call(()))
            .content(
                FontIcon::new()
                    .glyph(if muted { "\u{E74F}" } else { "\u{E767}" })
                    .font_size(12.0)
                    .foreground(hud_foreground(on_dark))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center),
            )
            .into()
    } else {
        AppBarButton::new()
            .icon(Icon::symbol(if muted {
                Symbol::Mute
            } else {
                Symbol::Volume
            }))
            .on_click(on_toggle_mute)
            .into()
    }
}

pub(crate) fn danmaku_toggle_button(
    enabled: bool,
    compact: bool,
    on_dark: bool,
    on_toggle: Callback<()>,
) -> View {
    let label = if enabled { "弹幕 开" } else { "弹幕 关" };
    if compact {
        Border::new()
            .height(28.0)
            .corner_radius(6.0)
            .padding(Thickness::xy(8.0, 0.0))
            .background(if enabled {
                Brush::Theme(ThemeBrush::Accent)
            } else if on_dark {
                hud_chip_background(true)
            } else {
                Brush::Theme(ThemeBrush::CardBackground)
            })
            .border_brush(if enabled {
                Brush::Theme(ThemeBrush::Accent)
            } else {
                hud_chip_border(on_dark)
            })
            .border_thickness(if enabled || on_dark { 0.0 } else { 1.0 })
            .on_pointer_released(move |_| on_toggle.call(()))
            .content(
                TextBlock::new()
                    .text(label)
                    .font_size(11.0)
                    .foreground(if enabled || on_dark {
                        Brush::Solid(Color::rgb(255, 255, 255))
                    } else {
                        Brush::Theme(ThemeBrush::PrimaryText)
                    })
                    .vertical_alignment(VerticalAlignment::Center),
            )
            .into()
    } else {
        Button::new()
            .style(if enabled {
                ButtonStyle::Accent
            } else {
                ButtonStyle::Subtle
            })
            .content(label)
            .on_click(on_toggle)
            .into()
    }
}

/// F1 反馈:悬浮播控在视频(深色画面)上需要半透明黑底衬 + 白前景;普通
/// 播放条仍走主题色。纯函数便于单测锁定配色契约。
pub(crate) fn hud_chip_background(on_dark: bool) -> Brush {
    if on_dark {
        Brush::Solid(Color::argb(90, 0, 0, 0))
    } else {
        Brush::Theme(ThemeBrush::CardBackground)
    }
}

pub(crate) fn hud_chip_border(on_dark: bool) -> Brush {
    if on_dark {
        Brush::Solid(Color::argb(0, 0, 0, 0))
    } else {
        Brush::Theme(ThemeBrush::CardStroke)
    }
}

pub(crate) fn hud_foreground(on_dark: bool) -> Brush {
    if on_dark {
        Brush::Solid(Color::rgb(255, 255, 255))
    } else {
        Brush::Theme(ThemeBrush::PrimaryText)
    }
}

/// HUD 底衬条颜色:66% 黑(用户建议「透明或默认黑色背景」的折中:保持
/// 可读性同时不遮挡画面);圆角 10、内边距随紧凑态收缩。
pub(crate) fn hud_scrim_color() -> Color {
    Color::argb(168, 0, 0, 0)
}

pub(crate) fn fullscreen_toggle_button(
    is_fullscreen: bool,
    compact: bool,
    on_dark: bool,
    on_toggle: Callback<()>,
) -> View {
    let auto_id = if is_fullscreen {
        "player-exit-fullscreen"
    } else {
        "player-enter-fullscreen"
    };
    if compact {
        let on_pointer = on_toggle.clone();
        Border::new()
            .width(28.0)
            .height(28.0)
            .corner_radius(6.0)
            .background(hud_chip_background(on_dark))
            .border_brush(hud_chip_border(on_dark))
            .border_thickness(if on_dark { 0.0 } else { 1.0 })
            .on_pointer_released(move |_| on_pointer.call(()))
            .content(
                Button::new()
                    .automation_id(auto_id)
                    .style(ButtonStyle::Subtle)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .vertical_alignment(VerticalAlignment::Stretch)
                    .on_click(on_toggle)
                    .content(
                        FontIcon::new()
                            .glyph(if is_fullscreen {
                                "\u{E73F}"
                            } else {
                                "\u{E740}"
                            })
                            .font_size(12.0)
                            .foreground(hud_foreground(on_dark))
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .vertical_alignment(VerticalAlignment::Center),
                    ),
            )
            .into()
    } else {
        AppBarButton::new()
            .automation_id(auto_id)
            .icon(Icon::symbol(if is_fullscreen {
                Symbol::BackToWindow
            } else {
                Symbol::FullScreen
            }))
            .on_click(on_toggle)
            .into()
    }
}

pub(crate) fn seek_slider(position: u32, duration_secs: u32, on_seek: Callback<f64>) -> Slider {
    // 拖拽提示(label)显示真实播放时间,而非 1..N 的裸数值。
    Slider::new()
        .thumb_tooltip(ThumbTooltip::Duration)
        .minimum(0.0)
        .maximum(f64::from(duration_secs.max(1)))
        .value(f64::from(position))
        .on_value_changed(on_seek)
}

/// 音量条(悬停滚轮每格 ±5);delta 走 shell 的 `VolumeDelta` 钳制语义,避免闭包快照过期。
pub(crate) fn volume_slider(
    width: f64,
    volume: u32,
    on_volume: &Callback<f64>,
    on_volume_wheel: &Callback<i32>,
) -> Slider {
    let on_volume_wheel = on_volume_wheel.clone();
    Slider::new()
        .width(width)
        .minimum(0.0)
        .maximum(100.0)
        .value(f64::from(volume))
        .on_value_changed(on_volume.clone())
        .on_pointer_wheel_changed(move |info: PointerEventInfo| {
            let step = volume_wheel_step(info.wheel_delta);
            if step != 0 {
                on_volume_wheel.call(step);
            }
        })
        .vertical_alignment(VerticalAlignment::Center)
}

/// 滚轮一格的音量步长:正向 +5、负向 -5、无 delta(非滚轮指针事件)不动。
fn volume_wheel_step(wheel_delta: i32) -> i32 {
    wheel_delta.signum() * 5
}

pub(crate) fn time_label(position: u32, duration_secs: u32, font_size: f64) -> TextBlock {
    TextBlock::new()
        .text(format!(
            "{} / {}",
            theme::fmt_duration(position),
            theme::fmt_duration(duration_secs)
        ))
        .font_size(font_size)
        .opacity(0.7)
        .vertical_alignment(VerticalAlignment::Center)
}

/// 播放页画质选择器（F2）：ComboBox 呈现当前档，菜单按 dash 实际下发档降序；
/// 少于 2 档（或未加载）返回 None 不渲染。紧凑/竖屏布局省去“画质”页眉，
/// 避免单行播控出现冗余解释性文本与高度错位。
pub(crate) fn quality_selector(
    menu: &[(i64, String)],
    current_qn: i64,
    on_select: &Callback<i64>,
    compact: bool,
) -> Option<View> {
    if menu.len() < 2 {
        return None;
    }
    let menu: Vec<(i64, String)> = menu.to_vec();
    let selected = menu
        .iter()
        .position(|(qn, _)| *qn == current_qn && current_qn > 0)
        .unwrap_or(0);
    let on_change = on_select.clone();
    let mut combo = ComboBox::new()
        .automation_id("player-quality-selector")
        .items_source(menu.iter().map(|(_, label)| label.clone()))
        .selected_index(selected)
        .on_selection_changed(move |index: Option<usize>| {
            if let Some((qn, _)) = index.and_then(|idx| menu.get(idx)) {
                on_change.call(*qn);
            }
        })
        .min_width(if compact { 88.0 } else { 108.0 })
        .vertical_alignment(VerticalAlignment::Center);
    if !compact {
        combo = combo.header("画质");
    }
    Some(combo.into())
}

/// 竖屏/单列播放页单行紧凑播控条。
pub(crate) fn single_column_transport(input: &PlayerInput, margin_x: f64) -> View {
    let play_btn = play_pause_button(input.playing, true, false, input.on_toggle.clone());
    let mute_btn = mute_toggle_button(input.volume == 0, true, false, input.on_toggle_mute.clone());
    let danmaku_btn = danmaku_toggle_button(
        input.danmaku_enabled,
        true,
        false,
        input.on_toggle_danmaku.clone(),
    );
    let fullscreen_btn =
        fullscreen_toggle_button(false, true, false, input.on_enter_fullscreen.clone());

    Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .margin(Thickness::new(margin_x, 4.0, margin_x, 0.0))
        .children((
            play_btn,
            seek_slider(input.position, input.duration_secs, input.on_seek.clone())
                .grid_column(1)
                .margin(Thickness::xy(8.0, 0.0))
                .vertical_alignment(VerticalAlignment::Center),
            StackPanel::new()
                .grid_column(2)
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children({
                    let mut items: Vec<View> =
                        vec![time_label(input.position, input.duration_secs, 11.0).into()];
                    if let Some(q) = quality_selector(
                        &input.quality_menu,
                        input.current_qn,
                        &input.on_select_quality,
                        true,
                    ) {
                        items.push(q);
                    }
                    if let Some(p) = pages_selector(
                        &input.pages_menu,
                        input.current_cid,
                        &input.on_select_page,
                        true,
                    ) {
                        items.push(p);
                    }
                    items.push(
                        volume_slider(
                            48.0,
                            input.volume,
                            &input.on_volume,
                            &input.on_volume_wheel,
                        )
                        .into(),
                    );
                    items.push(mute_btn);
                    items.push(danmaku_btn);
                    items.push(fullscreen_btn);
                    items
                }),
        ))
        .into()
}

/// 分P选择器（U3）：ComboBox 呈现 "P{n} 标题"；少于 2 个分P返回 None。
pub(crate) fn pages_selector(
    menu: &[(u64, String)],
    current_cid: u64,
    on_select: &Callback<u64>,
    compact: bool,
) -> Option<View> {
    if menu.len() < 2 {
        return None;
    }
    let menu: Vec<(u64, String)> = menu.to_vec();
    let selected = menu
        .iter()
        .position(|(cid, _)| *cid == current_cid && current_cid > 0);
    let on_change = on_select.clone();
    Some(
        ComboBox::new()
            .header("分P")
            .items_source(menu.iter().map(|(_, label)| label.clone()))
            .selected_index(selected.unwrap_or(0))
            .on_selection_changed(move |index: Option<usize>| {
                if let Some((cid, _)) = index.and_then(|idx| menu.get(idx)) {
                    on_change.call(*cid);
                }
            })
            .min_width(if compact { 72.0 } else { 96.0 })
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    )
}

fn half_pane_transport(input: &HalfPaneInput, pane_width: f64) -> View {
    let slider_w = half_pane_volume_slider_width(pane_width);
    let show_percent = slider_w > 44.0;
    let play_btn = play_pause_button(input.playing, true, false, input.on_toggle.clone());
    let mute_btn = mute_toggle_button(input.volume == 0, true, false, input.on_toggle_mute.clone());
    let danmaku_btn = danmaku_toggle_button(
        input.danmaku_enabled,
        true,
        false,
        input.on_toggle_danmaku.clone(),
    );
    let fullscreen_btn = fullscreen_toggle_button(false, true, false, input.on_fullscreen.clone());

    let mut right_items: Vec<View> = vec![mute_btn];
    right_items.push(volume_slider(slider_w, input.volume, &input.on_volume, &input.on_volume_wheel).into());
    if show_percent {
        right_items.push(
            TextBlock::new()
                .text(format!("{}%", input.volume))
                .font_size(11.0)
                .opacity(0.6)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }
    right_items.push(danmaku_btn);
    right_items.push(fullscreen_btn);

    StackPanel::new()
        .margin(Thickness::new(12.0, 6.0, 12.0, 4.0))
        .spacing(4.0)
        .children((
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
                            time_label(input.position, input.duration_secs, 11.0),
                        )),
                    StackPanel::new()
                        .grid_column(2)
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children(right_items),
                )),
        ))
        .into()
}

/// 首页右侧半窗预览输入（复用同一 `VideoEngine`，自带独立播放控制条、弹幕开关、最左侧带关闭/拖动柄/展开图标）。
#[derive(Clone)]
pub struct HalfPaneInput {
    pub item: VideoItem,
    pub author_avatar: Option<EncodedImage>,
    pub my_avatar: Option<EncodedImage>,
    pub desc: Option<String>,
    pub playing: bool,
    pub position: u32,
    pub duration_secs: u32,
    pub volume: u32,
    pub danmaku_enabled: bool,
    pub mount_tick: u32,
    pub width: f64,
    pub max_width: f64,
    pub max_height: f64,
    pub notice: Option<String>,
    /// 解析/缓冲中:半窗视频面叠加环形加载指示(与全播放页同源状态)。
    pub loading: bool,
    pub engine: Arc<VideoEngine>,
    pub native: Option<Arc<NativePlayer>>,
    pub stage: Option<Arc<DanmakuStage>>,
    pub comments: CommentsSnapshot,
    pub comment_cb: CommentCallbacks,
    pub on_toggle: Callback<()>,
    pub on_seek: Callback<f64>,
    pub on_volume: Callback<f64>,
    /// 音量条滚轮(±5/格,复用 shell `VolumeDelta` 钳制)。
    pub on_volume_wheel: Callback<i32>,
    pub on_toggle_mute: Callback<()>,
    pub on_toggle_danmaku: Callback<()>,
    pub on_fullscreen: Callback<()>,
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
            && self.playing == other.playing
            && self.position == other.position
            && self.duration_secs == other.duration_secs
            && self.volume == other.volume
            && self.danmaku_enabled == other.danmaku_enabled
            && self.mount_tick == other.mount_tick
            && (self.width - other.width).abs() < 0.5
            && (self.max_width - other.max_width).abs() < 0.5
            && (self.max_height - other.max_height).abs() < 0.5
            && self.notice == other.notice
            && self.loading == other.loading
            && Arc::ptr_eq(&self.engine, &other.engine)
            && opt_arc_ptr_eq(&self.native, &other.native)
            && opt_arc_ptr_eq(&self.stage, &other.stage)
            && self.comments == other.comments
            && self.comment_cb == other.comment_cb
            && self.on_toggle == other.on_toggle
            && self.on_seek == other.on_seek
            && self.on_volume == other.on_volume
            && self.on_volume_wheel == other.on_volume_wheel
            && self.on_toggle_mute == other.on_toggle_mute
            && self.on_toggle_danmaku == other.on_toggle_danmaku
            && self.on_fullscreen == other.on_fullscreen
            && self.on_expand == other.on_expand
            && self.on_close == other.on_close
            && self.on_resize == other.on_resize
    }
}

pub struct HalfPanePreview {
    drag_origin: Option<(f64, f64)>,
    live_width: Option<f64>,
    max_width: f64,
    double_click: DoubleClickTracker,
    /// F3:半窗评论区虚拟源缓存(常驻 compact 档;与 PlayerPage 同契约)。
    comments_cache: VirtualSourceCache,
}

#[derive(Clone, PartialEq)]
pub enum HalfPaneMessage {
    DragStart(f64, f64),
    DragMove(f64),
    DragEnd,
    VideoPressed(f64, f64, bool),
}

impl HalfPanePreview {
    /// 按 rev 刷新半窗评论虚拟源(常驻 compact 档)。
    fn refresh_comments_source(&mut self, input: &HalfPaneInput) {
        let snapshot = &input.comments;
        let rev = snapshot.rev;
        let data = Arc::clone(&snapshot.data);
        let cb = input.comment_cb.clone();
        self.comments_cache.get(VARIANT_COMPACT, rev, || {
            comments_virtual_source(&data, &cb, true, rev)
        });
    }

    /// view 侧取评论虚拟源(命中缓存;未命中现建兜底)。
    fn comments_source(&self, input: &HalfPaneInput) -> VirtualSource {
        let snapshot = &input.comments;
        self.comments_cache
            .peek(VARIANT_COMPACT, snapshot.rev)
            .unwrap_or_else(|| {
                comments_virtual_source(&snapshot.data, &input.comment_cb, true, snapshot.rev)
            })
    }
}

impl Component for HalfPanePreview {
    type Input = HalfPaneInput;
    type Message = HalfPaneMessage;

    fn create(input: &HalfPaneInput, _context: &ComponentContext<Self>) -> Self {
        let mut pane = Self {
            drag_origin: None,
            live_width: None,
            max_width: input.max_width,
            double_click: DoubleClickTracker::new(),
            comments_cache: VirtualSourceCache::default(),
        };
        pane.refresh_comments_source(input);
        pane
    }

    fn input_changed(&mut self, input: &HalfPaneInput, _context: &ComponentContext<Self>) {
        self.refresh_comments_source(input);
        self.max_width = input.max_width;
        if let Some(w) = self.live_width {
            self.live_width = Some(clamp_half_pane_width(w, self.max_width));
        }
    }

    fn update(&mut self, message: HalfPaneMessage, context: &ComponentContext<Self>) {
        match message {
            HalfPaneMessage::DragStart(window_x, current_w) => {
                if window_x.is_finite() {
                    self.drag_origin = Some((window_x, current_w));
                }
            }
            HalfPaneMessage::DragMove(window_x) => {
                if window_x.is_finite()
                    && let Some((start_x, start_w)) = self.drag_origin
                {
                    let next_w =
                        clamp_half_pane_width(start_w + (start_x - window_x), self.max_width);
                    self.live_width = Some(next_w);
                }
            }
            HalfPaneMessage::DragEnd => {
                self.drag_origin = None;
            }
            HalfPaneMessage::VideoPressed(x, y, left_pressed) => {
                if self
                    .double_click
                    .record_press(Instant::now(), x, y, left_pressed)
                {
                    context.input().on_fullscreen.call(());
                }
            }
        }
    }

    fn view(&self, input: &HalfPaneInput, context: &mut ViewContext<Self>) -> View {
        let pane_width =
            clamp_half_pane_width(self.live_width.unwrap_or(input.width), input.max_width);
        let video_height = compute_half_pane_video_height(pane_width, input.max_height);

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
            player_surface_view(
                &input.engine,
                input.native.as_ref(),
                input.stage.as_ref(),
                input.mount_tick,
            )
        };
        let surface = surface_with_loading(surface, input.loading);
        let surface = surface_with_double_click(
            surface,
            context
                .callback(|ev: PointerEventInfo| {
                    HalfPaneMessage::VideoPressed(
                        ev.window_x,
                        ev.window_y,
                        ev.is_left_button_pressed,
                    )
                })
                .into(),
        );

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
                        FontIcon::new().glyph("\u{E76F}").font_size(14.0).opacity(
                            if self.drag_origin.is_some() {
                                1.0
                            } else {
                                0.55
                            },
                        ),
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
                            .content(FontIcon::new().glyph("\u{E76B}").font_size(14.0))
                            .on_click(input.on_expand.clone()),
                    )),
            );

        let info_and_comments = StackPanel::new()
            .spacing(12.0)
            .margin(Thickness::new(14.0, 8.0, 14.0, 20.0))
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
                // 简介：内联展示，无展开态（见 format_desc_body 注释）。
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text("简介")
                        .font_size(12.0)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .opacity(0.75),
                    TextBlock::new()
                        .text(format_desc_body(&input.item.bvid, input.desc.as_deref()))
                        .font_size(12.0)
                        .opacity(0.85)
                        .text_wrapping(TextWrapping::Wrap),
                )),
                comments_panel_with(
                    &input.comments,
                    input.my_avatar.as_ref(),
                    &input.comment_cb,
                    &input.comment_cb.composer_draft,
                    true,
                    &self.comments_source(input),
                ),
            ));

        let main_pane = Grid::new()
            .grid_column(1)
            .rows([GridLength::Auto, GridLength::Auto, GridLength::Star(1.0)])
            .children((
                Border::new()
                    .height(video_height)
                    .margin(Thickness::new(12.0, 12.0, 12.0, 0.0))
                    .corner_radius(8.0)
                    .content(surface),
                Grid::new()
                    .grid_row(1)
                    .children([half_pane_transport(input, pane_width)]),
                ScrollViewer::new().grid_row(2).content(info_and_comments),
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

        // 外层 Grid 无背景时默认对空白区域命中透明；仅拖拽中叠加透明遮罩捕获全局指针，
        // 同时保持 drawer 同 key 挂载，避免拖拽起止重建 VideoSurface（SwapChainPanel）。
        let mut layers = vec![keyed("half-pane-drawer", drawer)];
        if self.drag_origin.is_some() {
            let on_resize_release = input.on_resize.clone();
            let on_resize_cancel = input.on_resize.clone();
            let on_resize_lost = input.on_resize.clone();
            let final_w = pane_width;
            let end_msg_cancel = context.message(HalfPaneMessage::DragEnd);
            let end_msg_lost = context.message(HalfPaneMessage::DragEnd);
            layers.push(keyed(
                "half-pane-drag-scrim",
                Border::new()
                    .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                    .on_pointer_moved(
                        context.callback(|ev: PointerEventInfo| {
                            HalfPaneMessage::DragMove(ev.window_x)
                        }),
                    )
                    .on_pointer_released(context.callback(move |_: PointerEventInfo| {
                        on_resize_release.call(final_w);
                        HalfPaneMessage::DragEnd
                    }))
                    .on_pointer_canceled(move || {
                        on_resize_cancel.call(final_w);
                        end_msg_cancel.call(());
                    })
                    .on_pointer_capture_lost(move || {
                        on_resize_lost.call(final_w);
                        end_msg_lost.call(());
                    }),
            ));
        }
        Grid::new().keyed_children(layers).into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use qaq_bx_core::reply::ReplyItem;

    use super::{
        CommentCallbacks, CommentSort, CommentsData, HALF_PANE_MAX_WIDTH, HALF_PANE_MIN_WIDTH,
        PlayerTab, RELATED_CARD_WIDTH, RELATED_COVER_HEIGHT, RELATED_META_HEIGHT,
        RELATED_TEXT_SPACING, RELATED_TITLE_HEIGHT, RelatedPane, RIGHT_RAIL_WIDTH,
        VARIANT_COMPACT, VARIANT_DESKTOP, VirtualSourceCache, clamp_half_pane_width,
        comment_row_key, comments_tab_label, comments_virtual_source,
        compute_half_pane_video_height, desktop_transport_compact, format_desc_body,
        half_pane_volume_slider_width, volume_wheel_step, related_row_key,
    };

    #[test]
    fn desc_body_keeps_text_and_bvid_without_title() {
        let s = format_desc_body("BV1xx411c7mD", Some("  真实简介  "));
        assert!(s.starts_with("真实简介"), "应保留去首尾空白后的正文: {s}");
        assert!(s.ends_with("BV 号：BV1xx411c7mD"));
        assert!(!s.contains("简介\n\n真实简介\n\n真实简介"));
    }

    #[test]
    fn desc_body_falls_back_for_missing_or_placeholder_desc() {
        for case in [None, Some(""), Some("   "), Some("-")] {
            let s = format_desc_body("BV1", case);
            assert!(s.starts_with("暂无简介"), "缺省/占位简介应落占位文案: {s}");
        }
    }

    #[test]
    fn player_tab_defaults_and_comments_label() {
        assert_eq!(PlayerTab::default(), PlayerTab::Intro);
        assert_eq!(comments_tab_label(None), "评论");
        assert_eq!(comments_tab_label(Some(0)), "评论");
        assert_eq!(comments_tab_label(Some(285)), "评论 285");
        assert_eq!(comments_tab_label(Some(12_340)), "评论 1.2万");
    }

    /// 音量条滚轮步长:一格 ±5,非滚轮事件(0 delta)不动。
    #[test]
    fn volume_wheel_step_maps_one_notch_to_five_percent() {
        assert_eq!(volume_wheel_step(120), 5);
        assert_eq!(volume_wheel_step(-120), -5);
        assert_eq!(volume_wheel_step(28), 5);
        assert_eq!(volume_wheel_step(-28), -5);
        assert_eq!(volume_wheel_step(0), 0);
    }

    #[test]
    fn half_pane_width_and_video_height_respect_viewport_boundaries() {
        // 下界 / 上界 / 非法输入 / 窄桌面 max_width 约束
        assert_eq!(clamp_half_pane_width(200.0, 1000.0), HALF_PANE_MIN_WIDTH);
        assert_eq!(clamp_half_pane_width(999.0, 1000.0), HALF_PANE_MAX_WIDTH);
        assert_eq!(clamp_half_pane_width(800.0, 756.0), 756.0);
        assert_eq!(clamp_half_pane_width(800.0, 300.0), HALF_PANE_MIN_WIDTH);
        assert_eq!(clamp_half_pane_width(f64::NAN, 800.0), HALF_PANE_MIN_WIDTH);

        // 短窗口（640 DIP 高 → content_height ≈ 504）拖拽到 820 宽时视频高度不得挤占评论区
        let short_win_h = compute_half_pane_video_height(820.0, 504.0);
        assert_eq!(short_win_h, 284.0);
        // 标准桌面（800 DIP 高 → content_height ≈ 664）保持 16:9 与 [160, 440] 上下界
        assert_eq!(compute_half_pane_video_height(480.0, 664.0), 234.0);
        assert_eq!(compute_half_pane_video_height(340.0, 664.0), 160.0);
        assert_eq!(compute_half_pane_video_height(820.0, 664.0), 425.0);

        // 侧栏与桌面播控条窄宽度档位切换:滑条任何宽度都显示,宽度自适应
        assert!(half_pane_volume_slider_width(340.0) > 0.0, "竖屏最窄档也必须出滑条");
        assert!(half_pane_volume_slider_width(480.0) > half_pane_volume_slider_width(340.0));
        assert!(desktop_transport_compact(416.0)); // 860 DIP 桌面下界左栏宽度
        assert!(!desktop_transport_compact(836.0)); // 1280 DIP 标准桌面左栏宽度
    }

    /// 相关推荐卡等高回归：文本列三槽全为常量定高，卡片高度与标题
    /// 1 行/2 行无关；且定高槽必须容得下最坏字体行高（Noto Sans SC ≈1.48em），
    /// 防止再退回「内容驱动行高 → 卡片长短不一」。
    #[test]
    fn related_card_slots_are_content_independent() {
        const LINE_EM: f64 = 1.48;
        // 标题两行必须放入定高槽（max_lines(2) 封顶行数，槽高封顶卡高）。
        assert!(
            2.0 * 13.0 * LINE_EM <= RELATED_TITLE_HEIGHT,
            "标题槽应容两行 13px 文本"
        );
        // UP 主 / 播放量各一行必须放入元信息槽。
        assert!(
            11.0 * LINE_EM <= RELATED_META_HEIGHT,
            "元信息槽应容一行 11px 文本"
        );
        // 卡高 = max(封面, 文本列定高)，两侧皆常量 → 与数据内容无关；
        // 且必须同时容下封面与文本列，定高不会产生裁切。
        let text_col =
            RELATED_TITLE_HEIGHT + RELATED_META_HEIGHT * 2.0 + RELATED_TEXT_SPACING * 2.0;
        let card_body = RELATED_COVER_HEIGHT.max(text_col);
        assert!(card_body >= RELATED_COVER_HEIGHT, "封面完整可见");
        assert!(card_body >= text_col, "文本列完整可见");
        // 宽度同理：卡宽由常量 RELATED_CARD_WIDTH 定死（右栏栅格列 − ScrollViewer
        // 缘距），与标题长度无关；封列 132 + 间距 10 + 边框/内衬 18 之外须容文本列。
        assert!(
            RELATED_CARD_WIDTH == RIGHT_RAIL_WIDTH - 4.0 - 16.0,
            "卡宽应恰好满栏"
        );
        let chrome = 1.0 * 2.0 + 8.0 * 2.0; // 边框 + Border 内衬
        let min_text_width = 120.0; // 短标题/UP 主仍要可读的保底宽
        assert!(
            chrome + 132.0 + 10.0 + min_text_width <= RELATED_CARD_WIDTH,
            "定宽 {RELATED_CARD_WIDTH} 应容下封列+文本列"
        );
    }

    /// F1 反馈契约:全屏 HUD 底衬为半透明黑、深色态控件走白前景;普通
    /// 播放条保持主题配色不受影响。
    /// F2 契约:菜单少于 2 档不渲染;≥2 档时选中项与回调命中正确档;
    /// 紧凑/竖屏布局省去"画质"页眉,桌面非紧凑档保留。
    #[test]
    fn quality_selector_requires_two_tiers_and_maps_selection() {
        let cb = super::Callback::new(|_: i64| {});
        assert!(super::quality_selector(&[], 0, &cb, false).is_none());
        assert!(
            super::quality_selector(&[(80, "1080P".into())], 80, &cb, false).is_none(),
            "单档不渲染选择器"
        );
        let menu = vec![
            (116, "1080P60".into()),
            (80, "1080P".into()),
            (64, "720P".into()),
        ];
        let desktop_view = super::quality_selector(&menu, 80, &cb, false).expect("桌面选择器应渲染");
        let desktop_dbg = format!("{desktop_view:?}");
        assert!(desktop_dbg.contains("player-quality-selector"));
        assert!(desktop_dbg.contains("画质"), "桌面非紧凑档保留画质页眉");

        let compact_view = super::quality_selector(&menu, 80, &cb, true).expect("紧凑选择器应渲染");
        let compact_dbg = format!("{compact_view:?}");
        assert!(compact_dbg.contains("player-quality-selector"));
        assert!(compact_dbg.contains("1080P"));
        assert!(
            !compact_dbg.contains("画质"),
            "竖屏/紧凑布局不得渲染多余的“画质”解释性页眉"
        );
    }

    #[test]
    fn portrait_single_column_transport_omits_quality_header() {
        let engine = super::VideoEngine::new();
        let item = super::VideoItem {
            bvid: "BV1GJ411x7h7".into(),
            aid: 1,
            cid: 2,
            title: "test".into(),
            author: "up".into(),
            author_mid: 3,
            author_face_url: None,
            duration_secs: 60,
            play_count: 10,
            danmaku_count: 5,
            like_count: None,
            coin_count: None,
            favorite_count: None,
            share_count: None,
            cover: None,
            pic_url: None,
        };
        let noop_unit = super::Callback::new(|()| {});
        let noop_f64 = super::Callback::new(|_: f64| {});
        let layout = super::ViewportLayout::initial(true);
        assert!(layout.is_single_column(), "竖屏模式应走单列布局");
        let input = super::PlayerInput {
            item,
            author_avatar: None,
            my_avatar: None,
            desc: None,
            playing: true,
            position: 11,
            volume: 60,
            danmaku_enabled: true,
            quality_menu: vec![(80, "1080P 高清".into()), (32, "480P 清晰".into())],
            current_qn: 80,
            pages_menu: Vec::new(),
            current_cid: 2,
            layout,
            mount_tick: 1,
            duration_secs: 256,
            notice: None,
            loading: false,
            engine,
            native: None,
            stage: None,
            comments: super::CommentsSnapshot::default(),
            comment_cb: test_callbacks(),
            on_open_video: super::Callback::new(|_: String| {}),
            on_related_retry: noop_unit.clone(),
            on_toggle: noop_unit.clone(),
            on_seek: noop_f64.clone(),
            on_volume: noop_f64,
            on_volume_wheel: super::Callback::new(|_: i32| {}),
            on_toggle_mute: noop_unit.clone(),
            on_toggle_danmaku: noop_unit.clone(),
            on_select_quality: super::Callback::new(|_: i64| {}),
            on_select_page: super::Callback::new(|_: u64| {}),
            on_enter_fullscreen: noop_unit,
            related: super::RelatedPane::default(),
            demo_override: None,
        };
        let transport = super::single_column_transport(&input, input.layout.page_margin_x());
        let dbg = format!("{transport:?}");
        assert!(dbg.contains("player-quality-selector"));
        assert!(dbg.contains("1080P 高清"));
        assert!(
            !dbg.contains("画质"),
            "竖屏模式视频页面播控条不得出现“画质”二字"
        );
    }

    #[test]
    fn hud_dark_chips_use_translucent_black_and_white_foreground() {
        let scrim = super::hud_scrim_color();
        // 66% 黑(α=168):既可读又不糊画面。
        assert_eq!(
            scrim,
            super::Color::argb(168, 0, 0, 0),
            "scrim 必须是 66% 纯黑"
        );

        for dark in [true, false] {
            let cb = super::Callback::new(|_| {});
            let _ = super::play_pause_button(true, true, dark, cb.clone());
            let _ = super::mute_toggle_button(false, true, dark, cb.clone());
            let _ = super::danmaku_toggle_button(dark, true, dark, cb.clone());
            let _ = super::fullscreen_toggle_button(true, true, dark, cb);
        }
        // 深色态配色契约(供全屏页引用):白前景 + 黑底衬芯片。
        assert!(
            matches!(super::hud_foreground(true), super::Brush::Solid(_)),
            "深色态前景应为白色实刷"
        );
        assert!(matches!(
            super::hud_chip_background(true),
            super::Brush::Solid(_)
        ));
    }

    #[test]
    fn transport_bars_with_fullscreen_button_fit_minimum_widths() {
        // Mobile 390 DIP 单行播控：两侧边距 12×2=24 → 可用 366 DIP
        let mobile_inner = 390.0 - 24.0;
        let mobile_fixed = 32.0 + 16.0 + 70.0 + 6.0 + 28.0 + 6.0 + 56.0 + 6.0 + 28.0;
        assert!(
            mobile_inner - mobile_fixed >= 100.0,
            "390 DIP 下进度条应保有足够拖动宽度"
        );

        // 860 DIP 桌面下界左栏（left_inner_w = 416 DIP）紧凑播控含小窗与全屏按钮
        let desktop_min_left = 416.0;
        let desktop_compact_used =
            32.0 + 8.0 + 70.0 + 28.0 + 6.0 + 64.0 + 6.0 + 56.0 + 6.0 + 48.0 + 6.0 + 28.0;
        assert!(
            desktop_compact_used <= desktop_min_left,
            "860 DIP 下紧凑播控不得溢出"
        );

        // HalfPane 最小宽度 340 DIP（扣左条 40 与内边距 24 → 276 DIP）
        let half_min_inner = HALF_PANE_MIN_WIDTH - 40.0 - 24.0;
        let half_used = 32.0 + 8.0 + 70.0 + 28.0 + 6.0 + 56.0 + 6.0 + 28.0;
        assert!(
            half_used <= half_min_inner,
            "340 DIP 半窗播控含全屏按钮不得溢出"
        );

        let cb = windows_reactor::Callback::new(|()| {});
        let _ = super::fullscreen_toggle_button(false, false, false, cb.clone());
        let _ = super::fullscreen_toggle_button(true, true, false, cb);
    }

    // ---------- F3:评论/相关列表行虚拟化 ----------

    fn reply_item(rpid: u64) -> ReplyItem {
        ReplyItem {
            rpid,
            oid: 0,
            root: 0,
            parent: 0,
            mid: 0,
            uname: format!("user{rpid}"),
            avatar: String::new(),
            level: 1,
            message: format!("msg{rpid}"),
            ctime: 0,
            like: 0,
            rcount: 0,
            time_desc: String::new(),
            location: String::new(),
            is_up_top: false,
            up_liked: false,
            up_replied: false,
            replies: Vec::new(),
        }
    }

    fn test_callbacks() -> CommentCallbacks {
        CommentCallbacks {
            on_sort: super::Callback::new(|_: CommentSort| {}),
            on_load_more: super::Callback::new(|()| {}),
            on_retry: super::Callback::new(|()| {}),
            on_toggle_sub: super::Callback::new(|_: u64| {}),
            on_load_more_sub: super::Callback::new(|_: u64| {}),
            on_draft: super::Callback::new(|_: std::rc::Rc<str>| {}),
            on_submit: super::Callback::new(|()| {}),
            composer_notice: None,
            can_post: false,
            composer_draft: String::new(),
        }
    }

    #[test]
    fn virtual_source_cache_reuses_per_rev_and_rebuilds_on_change() {
        let mut cache = VirtualSourceCache::default();
        let data = Arc::new(CommentsData::default());
        let cb = test_callbacks();
        let desktop = |rev: u64| comments_virtual_source(&data, &cb, false, rev);
        let first = cache.get(VARIANT_DESKTOP, 7, || desktop(7));
        let again = cache.get(VARIANT_DESKTOP, 7, || desktop(7));
        assert!(
            first == again,
            "同 (档位, rev) 必须复用同一实例(跨 pass 判等短路的根基)"
        );
        let rebuilt = cache.get(VARIANT_DESKTOP, 8, || desktop(8));
        assert!(first != rebuilt, "rev 变化必须重建(新 Rc 实例)");
        let compact = cache.get(VARIANT_COMPACT, 7, || {
            comments_virtual_source(&data, &cb, true, 7)
        });
        assert!(first != compact, "档位之间互相隔离");
        assert!(
            cache.get(VARIANT_DESKTOP, 8, || desktop(8)) == rebuilt,
            "另一档刷新不得干扰本档缓存"
        );
    }

    #[test]
    fn comment_row_keys_unique_across_and_within_segments() {
        // 置顶段与普通段可能复用同一 rpid(旧 c-top-/c- 前缀契约):不得撞键。
        assert_ne!(comment_row_key(0, 1, 42), comment_row_key(1, 1, 42));
        // 行序入键:bilibili 异常返回段内重复 rpid 也不会触发 reactor 重复键 panic。
        assert_ne!(comment_row_key(0, 2, 42), comment_row_key(1, 2, 42));
    }

    /// composer 布局回归(v0.2.0 事故):容器必须是三列 Grid,输入框/发布钮
    /// 带 Grid.Column 1/2;误挂垂直 StackPanel 时附加属性被静默忽略,头像/
    /// 输入框/发布钮竖堆三行。notice 必须落第 2 行输入框列。
    #[test]
    fn composer_is_three_column_grid_with_notice_on_second_row() {
        let mut cb = test_callbacks();
        let plain = format!("{:?}", super::comment_composer(None, &cb, ""));
        // 容器契约:根节点必须是 Grid,且不得混入 StackPanel(竖堆回归)。
        assert!(
            plain.starts_with("View(Declaration { kind: Grid,"),
            "composer 容器必须是 Grid: {plain}"
        );
        assert!(
            !plain.contains("StackPanel"),
            "composer 不得退回 StackPanel(竖堆三行回归)"
        );
        // 三列 [Auto, Star, Auto]:输入框列必须弹性占满剩余宽。
        assert!(plain.contains("id: GridColumns"), "必须声明三列");
        assert!(plain.contains("Star(1.0)"), "输入框列必须为 Star 弹性列");
        // 列号绑定到正确元素:TextBox 段落含 Grid.Column=1,Button 段落含 2。
        let (_, rest) = plain
            .split_once("kind: TextBox")
            .expect("composer 应含输入框");
        let (textbox_seg, button_seg) = rest
            .split_once("kind: Button")
            .expect("composer 应含发布钮");
        assert!(
            textbox_seg.contains("id: GridColumn, value: I32(1)"),
            "输入框必须落第 2 列"
        );
        assert!(
            button_seg.contains("id: GridColumn, value: I32(2)"),
            "发布钮必须落第 3 列"
        );
        assert!(
            !plain.contains("id: GridRow,"),
            "无 notice 时不得出现第 2 行元素"
        );
        // 未登录:置灰 + 登录占位文案。
        assert!(plain.contains("登录后即可发布评论"));
        assert!(plain.contains("IsEnabled, value: Bool(false)"));

        // notice 必须落第 2 行输入框列(Grid.Row=1 + Grid.Column=1)。
        cb.composer_notice = Some("评论失败,请重试".into());
        let noticed = format!("{:?}", super::comment_composer(None, &cb, ""));
        let notice_seg = noticed
            .rsplit_once("Declaration { kind: TextBlock")
            .expect("notice 应为末尾 TextBlock")
            .1;
        assert!(
            notice_seg.contains("id: GridRow, value: I32(1)"),
            "notice 必须落第 2 行: {notice_seg}"
        );
        assert!(
            notice_seg.contains("id: GridColumn, value: I32(1)"),
            "notice 必须缩进到输入框列"
        );
        assert!(notice_seg.contains("评论失败,请重试"));

        // 已登录:Accent 发布钮 + 友善占位文案,按钮不再置灰。
        cb.composer_notice = None;
        cb.can_post = true;
        let posted = format!("{:?}", super::comment_composer(None, &cb, "草稿"));
        assert!(posted.contains("发一条友善的评论…"));
        assert!(posted.contains("ButtonStyle(Accent)"));
        assert!(!posted.contains("IsEnabled, value: Bool(false)"));
        assert!(posted.contains("Text, value: String(\"草稿\")"));
    }

    #[test]
    fn related_row_keys_unique_per_row() {
        assert_ne!(related_row_key(0, "BV1"), related_row_key(1, "BV1"));
        assert_ne!(related_row_key(0, "BV1"), related_row_key(0, "BV2"));
    }

    #[test]
    fn comments_virtual_source_len_and_rebuild_distinctness() {
        let data = Arc::new(CommentsData {
            top_replies: vec![reply_item(1), reply_item(2)],
            replies: vec![reply_item(3), reply_item(1)],
            ..CommentsData::default()
        });
        let cb = test_callbacks();
        let source = comments_virtual_source(&data, &cb, false, 9);
        assert_eq!(source.len(), 4);
        // 机制锁定:同 rev 现建两次必然不等(Rc 闭包指针不同)——这正是调用方
        // 必须经 VirtualSourceCache 缓存复用、不能在 view 里现建的原因。
        assert_ne!(source, comments_virtual_source(&data, &cb, false, 9));
    }

    #[test]
    fn related_pane_rev_participates_in_equality() {
        let mut pane = RelatedPane::default();
        let clone = pane.clone();
        assert_eq!(pane, clone);
        pane.rev = pane.rev.wrapping_add(1);
        assert_ne!(
            pane, clone,
            "rev 必须参与判等:封面回填/原位 push 依赖它触发换代"
        );
    }

    #[test]
    fn half_pane_input_tracks_native_and_stage_identity() {
        let engine = super::VideoEngine::new();
        let native = Arc::new(super::NativePlayer::new());
        let stage = super::DanmakuStage::new();
        let item = super::VideoItem {
            bvid: "BV1GJ411x7h7".into(),
            aid: 1,
            cid: 2,
            title: "test".into(),
            author: "up".into(),
            author_mid: 3,
            author_face_url: None,
            duration_secs: 60,
            play_count: 10,
            danmaku_count: 5,
            like_count: None,
            coin_count: None,
            favorite_count: None,
            share_count: None,
            cover: None,
            pic_url: None,
        };
        let noop_unit = super::Callback::new(|()| {});
        let noop_f64 = super::Callback::new(|_: f64| {});
        let base = super::HalfPaneInput {
            item,
            author_avatar: None,
            my_avatar: None,
            desc: None,
            playing: true,
            position: 0,
            duration_secs: 60,
            volume: 80,
            danmaku_enabled: true,
            mount_tick: 1,
            width: 480.0,
            max_width: 800.0,
            max_height: 600.0,
            notice: None,
            loading: false,
            engine,
            native: None,
            stage: None,
            comments: super::CommentsSnapshot::default(),
            comment_cb: test_callbacks(),
            on_toggle: noop_unit.clone(),
            on_seek: noop_f64.clone(),
            on_volume: noop_f64.clone(),
            on_volume_wheel: super::Callback::new(|_: i32| {}),
            on_toggle_mute: noop_unit.clone(),
            on_toggle_danmaku: noop_unit.clone(),
            on_fullscreen: noop_unit.clone(),
            on_expand: noop_unit.clone(),
            on_close: noop_unit,
            on_resize: noop_f64,
        };
        assert!(base == base.clone());

        let with_native = super::HalfPaneInput {
            native: Some(native),
            ..base.clone()
        };
        assert!(base != with_native, "native 通道接入必须触发半窗表面重建");

        let with_stage = super::HalfPaneInput {
            stage: Some(stage),
            ..with_native.clone()
        };
        assert!(
            with_native != with_stage,
            "stage 接入必须触发半窗弹幕透明层同步更新"
        );
    }
}
