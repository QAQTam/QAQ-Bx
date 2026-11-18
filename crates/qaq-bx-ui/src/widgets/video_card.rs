//! 视频卡片与自适应网格（`docs/ui-design.md` §3 + `layout::CardMetrics`）：
//! - 先分配固定槽位框架（`HomeSlot::Skeleton`），数据/封面就绪后原地填充，避免整页重排与滚动抖动；
//! - 每张卡片为独立 `VideoCard` 子组件（本地局部状态 + `PartialEq` 跳过未变卡）；
//! - 桌面态支持鼠标悬停两段式中心缩放动画（70ms 微缩蓄力 0.96 → 150ms 扩张 1.045），Mobile 紧凑态关闭悬停动画。

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use windows_reactor::*;

use crate::data::VideoItem;
use crate::layout::CardMetrics;
use crate::theme;

const CONTRACT_DURATION: Duration = Duration::from_millis(70);
const EXPAND_DURATION: Duration = Duration::from_millis(150);
const RESET_DURATION: Duration = Duration::from_millis(140);
const CONTRACT_SCALE: f64 = 0.96;
const EXPAND_SCALE: f64 = 1.045;

/// 推荐流网格槽位（先占骨架框架，后续按槽位键原地替换为真实视频卡）。
#[derive(Clone, Debug, PartialEq)]
pub enum HomeSlot {
    Skeleton { slot_key: String },
    Video { slot_key: String, item: VideoItem },
}

impl HomeSlot {
    pub fn slot_key(&self) -> &str {
        match self {
            Self::Skeleton { slot_key } | Self::Video { slot_key, .. } => slot_key,
        }
    }

    pub fn bvid(&self) -> Option<&str> {
        match self {
            Self::Skeleton { .. } => None,
            Self::Video { item, .. } => Some(item.bvid.as_str()),
        }
    }
}

/// 构建推荐流按行虚拟化数据源（仅在槽位/度量/封面实质变更时重建，避免滚动事件触发整页行刷新）。
pub fn home_virtual_source(
    slots: &[HomeSlot],
    metrics: CardMetrics,
    row_revision: u64,
    focused_idx: Option<usize>,
    on_open: &Callback<String>,
    on_hover_preresolve: &Callback<String>,
    on_row_active: &Callback<(u64, usize, bool)>,
    unplayable: &HashMap<String, String>,
) -> VirtualSource {
    let cols = metrics.cols.max(1);
    let rows: Rc<Vec<Rc<Vec<HomeSlot>>>> = Rc::new(
        slots
            .chunks(cols)
            .map(|chunk| Rc::new(chunk.to_vec()))
            .collect(),
    );
    let row_count = rows.len();
    let on_open = on_open.clone();
    let on_hover = on_hover_preresolve.clone();
    let on_row = on_row_active.clone();
    let unplayable = Rc::new(unplayable.clone());
    VirtualSource::new(
        row_revision,
        row_count,
        move |row_idx| (row_revision << 24) ^ (row_idx as u64),
        move |row_idx| {
            let row_slots = Rc::clone(&rows[row_idx]);
            let focused_col = focused_idx.and_then(|idx| {
                let start = row_idx * cols;
                let end = start + row_slots.len();
                (idx >= start && idx < end).then(|| idx - start)
            });
            View::component::<FeedRow>(FeedRowInput {
                row_revision,
                row_idx,
                slots: row_slots,
                metrics,
                focused_col,
                on_open: on_open.clone(),
                on_hover_preresolve: on_hover.clone(),
                on_row_active: on_row.clone(),
                unplayable: Rc::clone(&unplayable),
            })
        },
    )
}

/// 首页虚拟化行网格：把缓存的 `VirtualSource` 交给 `ItemsRepeater`，
/// 仅在视口附近创建 WinUI 控件并在划出后自动回收，防止长列表 COM 对象与解码像素无限膨胀。
pub fn virtual_video_grid(source: &VirtualSource, margin_x: f64) -> View {
    ItemsRepeater::new()
        .margin(Thickness::new(margin_x, 0.0, margin_x, 24.0))
        .virtual_source(source.clone())
        .into()
}

/// 按精确行分组渲染静态视频网格（热门页/搜索页共用），规避 `VariableSizedWrapGrid` 末列间距导致的早折行。
pub fn video_grid(items: &[VideoItem], metrics: CardMetrics, on_open: &Callback<String>) -> View {
    let cols = metrics.cols.max(1);
    let rows: Vec<KeyedView> = items
        .chunks(cols)
        .enumerate()
        .map(|(row_idx, chunk)| {
            let cells: Vec<KeyedView> = chunk
                .iter()
                .map(|item| {
                    let slot = HomeSlot::Video {
                        slot_key: format!("static:{}", item.bvid),
                        item: item.clone(),
                    };
                    KeyedView::new(
                        slot.slot_key().to_string(),
                        View::component::<VideoCard>(VideoCardInput {
                            slot,
                            metrics,
                            is_focused: false,
                            on_open: on_open.clone(),
                            on_hover_preresolve: None,
                            unplayable_badge: None,
                        }),
                    )
                })
                .collect();
            let row_key = chunk
                .first()
                .map(|i| format!("row:{row_idx}:{}:{}", cols, i.bvid))
                .unwrap_or_else(|| format!("row:{row_idx}"));
            KeyedView::new(
                row_key,
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(metrics.gap)
                    .height(metrics.card_height)
                    .keyed_children(cells),
            )
        })
        .collect();

    StackPanel::new()
        .spacing(metrics.gap)
        .keyed_children(rows)
        .into()
}

#[derive(Clone, PartialEq)]
pub struct FeedRowInput {
    pub row_revision: u64,
    pub row_idx: usize,
    pub slots: Rc<Vec<HomeSlot>>,
    pub metrics: CardMetrics,
    pub focused_col: Option<usize>,
    pub on_open: Callback<String>,
    pub on_hover_preresolve: Callback<String>,
    pub on_row_active: Callback<(u64, usize, bool)>,
    /// bvid → 不可播原因（充电/付费未解锁；命中即卡面打标）。
    pub unplayable: Rc<HashMap<String, String>>,
}

pub struct FeedRow;

#[derive(Clone, PartialEq)]
pub enum RowMessage {}

impl Component for FeedRow {
    type Input = FeedRowInput;
    type Message = RowMessage;

    fn create(_input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self
    }

    fn update(&mut self, message: Self::Message, _context: &ComponentContext<Self>) {
        match message {}
    }

    fn view(&self, input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        let rev = input.row_revision;
        let row_idx = input.row_idx;
        let on_active = input.on_row_active.clone();
        context.use_effect("row-active", (rev, row_idx), move || {
            on_active.call((rev, row_idx, true));
            let on_recycle = on_active;
            Some(Box::new(move || on_recycle.call((rev, row_idx, false))))
        });
        let cells = input.slots.iter().enumerate().map(|(col_idx, slot)| {
            KeyedView::new(
                slot.slot_key().to_string(),
                View::component::<VideoCard>(VideoCardInput {
                    slot: slot.clone(),
                    metrics: input.metrics,
                    is_focused: input.focused_col == Some(col_idx),
                    on_open: input.on_open.clone(),
                    on_hover_preresolve: Some(input.on_hover_preresolve.clone()),
                    unplayable_badge: slot
                        .bvid()
                        .and_then(|bvid| input.unplayable.get(bvid).cloned()),
                }),
            )
        });
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(input.metrics.gap)
            .height(input.metrics.card_height)
            .margin(Thickness::new(0.0, 0.0, 0.0, input.metrics.gap))
            .keyed_children(cells)
            .into()
    }
}

#[derive(Clone, PartialEq)]
pub struct VideoCardInput {
    pub slot: HomeSlot,
    pub metrics: CardMetrics,
    pub is_focused: bool,
    pub on_open: Callback<String>,
    pub on_hover_preresolve: Option<Callback<String>>,
    /// 非空 = 该稿已被预解析判定为不可播（充电/付费未解锁），卡面打标。
    pub unplayable_badge: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoverPhase {
    Idle,
    Contracting,
    Expanded,
}

/// 纯状态机：悬停微缩 → 扩张 → 动画确认后才允许触发一次预解析；中途移出/换视频立废。
struct HoverMachine {
    phase: HoverPhase,
    has_animated: bool,
    seq: u64,
    last_bvid: Option<String>,
    preresolved_bvid: Option<String>,
}

impl HoverMachine {
    fn new(initial_bvid: Option<String>) -> Self {
        Self {
            phase: HoverPhase::Idle,
            has_animated: false,
            seq: 0,
            last_bvid: initial_bvid,
            preresolved_bvid: None,
        }
    }

    fn reset(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        self.phase = HoverPhase::Idle;
    }

    /// 返回 `bvid_changed`。
    fn sync_bvid(&mut self, next_bvid: Option<String>) -> bool {
        if self.last_bvid != next_bvid {
            self.last_bvid = next_bvid;
            self.preresolved_bvid = None;
            self.reset();
            true
        } else {
            false
        }
    }

    fn on_enter(&mut self) -> Option<u64> {
        if self.last_bvid.is_none() || self.phase != HoverPhase::Idle {
            return None;
        }
        self.seq = self.seq.wrapping_add(1);
        self.has_animated = true;
        self.phase = HoverPhase::Contracting;
        Some(self.seq)
    }

    fn on_expand(&mut self, seq: u64) -> Option<u64> {
        if seq != self.seq || self.phase != HoverPhase::Contracting {
            return None;
        }
        self.phase = HoverPhase::Expanded;
        Some(seq)
    }

    fn on_preresolve(&mut self, seq: u64) -> Option<String> {
        if seq != self.seq || self.phase != HoverPhase::Expanded {
            return None;
        }
        let bvid = self.last_bvid.clone()?;
        if self.preresolved_bvid.as_deref() == Some(bvid.as_str()) {
            return None;
        }
        self.preresolved_bvid = Some(bvid.clone());
        Some(bvid)
    }

    fn on_exit(&mut self) {
        self.reset();
    }

    fn scale_transition(&self) -> Option<(f64, Duration)> {
        if !self.has_animated {
            return None;
        }
        Some(match self.phase {
            HoverPhase::Idle => (1.0, RESET_DURATION),
            HoverPhase::Contracting => (CONTRACT_SCALE, CONTRACT_DURATION),
            HoverPhase::Expanded => (EXPAND_SCALE, EXPAND_DURATION),
        })
    }
}

#[derive(Clone, PartialEq)]
pub enum Message {
    PointerEntered,
    PointerExited,
    ExpandTick(u64),
    PreresolveTick(u64),
}

pub struct VideoCard {
    hover: HoverMachine,
    timer: Option<ComponentTimer>,
}

impl Component for VideoCard {
    type Input = VideoCardInput;
    type Message = Message;

    fn create(input: &Self::Input, _context: &ComponentContext<Self>) -> Self {
        Self {
            hover: HoverMachine::new(input.slot.bvid().map(ToOwned::to_owned)),
            timer: None,
        }
    }

    fn input_changed(&mut self, input: &Self::Input, _context: &ComponentContext<Self>) {
        // 仅当槽位从骨架填充为视频，或换了新视频时才复位悬停；封面异步到图不打断悬停动画。
        if self
            .hover
            .sync_bvid(input.slot.bvid().map(ToOwned::to_owned))
        {
            self.timer = None;
        }
        if input.metrics.compact_card && self.timer.is_some() {
            self.hover.on_exit();
            self.timer = None;
        }
    }

    fn update(&mut self, message: Self::Message, context: &ComponentContext<Self>) {
        if context.input().metrics.compact_card {
            self.hover.on_exit();
            self.timer = None;
            return;
        }
        match message {
            Message::PointerEntered => {
                if let Some(seq) = self.hover.on_enter() {
                    self.timer =
                        Some(context.set_local_timeout(CONTRACT_DURATION, move || {
                            Message::ExpandTick(seq)
                        }));
                }
            }
            Message::ExpandTick(seq) => {
                if let Some(seq) = self.hover.on_expand(seq) {
                    self.timer =
                        Some(context.set_local_timeout(EXPAND_DURATION, move || {
                            Message::PreresolveTick(seq)
                        }));
                }
            }
            Message::PreresolveTick(seq) => {
                if let Some(bvid) = self.hover.on_preresolve(seq) {
                    self.timer = None;
                    let input = context.input();
                    if let Some(cb) = &input.on_hover_preresolve {
                        if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                            eprintln!("[qaq-bx-ui] hover expanded -> preresolve {bvid}");
                        }
                        cb.call(bvid);
                    }
                }
            }
            Message::PointerExited => {
                self.hover.on_exit();
                self.timer = None;
            }
        }
    }

    fn view(&self, input: &Self::Input, context: &mut ViewContext<Self>) -> View {
        let metrics = input.metrics;
        let content: View = match &input.slot {
            HomeSlot::Skeleton { .. } => skeleton_card_view(metrics),
            HomeSlot::Video { item, .. } => {
                populated_card_view(
                    item,
                    input.is_focused,
                    metrics,
                    &input.on_open,
                    input.unplayable_badge.as_deref(),
                )
            }
        };

        if metrics.compact_card {
            return Border::new()
                .width(metrics.card_width)
                .height(metrics.card_height)
                .content(content)
                .into();
        }

        let mut animated_box = Border::new()
            .width(metrics.card_width)
            .height(metrics.card_height)
            .content(content);

        if let Some((scale, duration)) = self.hover.scale_transition() {
            animated_box = animated_box.scale_transition(duration).scale(scale);
        }

        // 外层固定命中盒（不缩放）：防止收缩阶段鼠标落在边缘外侧引发 Enter/Exit 抖动。
        let on_enter = context.callback(|_: PointerEventInfo| Message::PointerEntered);
        let on_exit = context.callback(|_: PointerEventInfo| Message::PointerExited);
        Border::new()
            .width(metrics.card_width)
            .height(metrics.card_height)
            .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
            .on_pointer_entered(on_enter)
            .on_pointer_exited(on_exit)
            .content(animated_box)
            .into()
    }
}

fn skeleton_card_view(metrics: CardMetrics) -> View {
    let pad = if metrics.compact_card { 8.0 } else { 12.0 };
    let bar = |width: f64, opacity: f64, top: f64| {
        Border::new()
            .width(width)
            .height(if metrics.compact_card { 9.0 } else { 10.0 })
            .margin(Thickness::new(pad, top, 0.0, 0.0))
            .corner_radius(2.0)
            .background(ThemeBrush::SolidBackground)
            .opacity(opacity)
    };
    let line1_w = (metrics.card_width * 0.72).round().max(90.0);
    let line2_w = (metrics.card_width * 0.46).round().max(60.0);
    Border::new()
        .width(metrics.card_width)
        .height(metrics.card_height)
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(
            StackPanel::new().children((
                Border::new()
                    .height(if metrics.compact_card {
                        (metrics.cover_height - 8.0).max(60.0)
                    } else {
                        metrics.cover_height
                    })
                    .margin(Thickness::new(pad, pad, pad, 0.0))
                    .corner_radius(4.0)
                    .background(ThemeBrush::SolidBackground)
                    .opacity(0.35),
                bar(line1_w, 0.45, if metrics.compact_card { 8.0 } else { 12.0 }),
                bar(line2_w, 0.3, if metrics.compact_card { 6.0 } else { 8.0 }),
            )),
        )
        .into()
}

fn populated_card_view(
    item: &VideoItem,
    is_focused: bool,
    metrics: CardMetrics,
    on_open: &Callback<String>,
    unplayable_badge: Option<&str>,
) -> View {
    let bvid = item.bvid.clone();
    let on_click = on_open.clone();
    let compact = metrics.compact_card;
    let cover_inner: View = match &item.cover {
        Some(img) => Image::new()
            .source_data(img.clone())
            .stretch(Stretch::UniformToFill)
            .into(),
        None => TextBlock::new()
            .text("封面加载中")
            .font_size(if compact { 11.0 } else { 12.0 })
            .opacity(0.35)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    };

    let mut cover_layers: Vec<View> = vec![
        Border::new()
            .corner_radius(8.0)
            .background(ThemeBrush::SolidBackground)
            .content(cover_inner)
            .into(),
    ];

    // 不可播角标(充电/付费未解锁):置顶盖住时长徽标,先于其它覆盖层渲染保证可见。
    if let Some(reason) = unplayable_badge {
        let badge_text = if qaq_bx_core::player::is_trial_downgrade_reason(reason) {
            "充电专稿"
        } else {
            "无法播放"
        };
        cover_layers.push(
            Border::new()
                .background(Brush::Solid(Color::argb(216, 12, 12, 12)))
                .border_brush(Brush::Solid(Color::argb(255, 196, 86, 43)))
                .border_thickness(1.0)
                .corner_radius(4.0)
                .padding(Thickness::xy(6.0, 2.0))
                .margin(Thickness::new(6.0, 6.0, 6.0, 0.0))
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Top)
                .content(
                    theme::text(badge_text)
                        .foreground(Brush::Solid(Color::rgb(255, 179, 92)))
                        .font_size(if compact { 10.0 } else { 11.0 }),
                )
                .into(),
        );
    }

    if compact {
        let overlay_bg = Brush::Solid(Color::argb(176, 16, 16, 16));
        let white = Brush::Solid(Color::rgb(255, 255, 255));
        // Mobile 左下角统计胶囊（播放量 + 弹幕数）
        cover_layers.push(
            Border::new()
                .background(overlay_bg.clone())
                .corner_radius(4.0)
                .padding(Thickness::xy(5.0, 2.0))
                .margin(Thickness::new(6.0, 0.0, 48.0, 6.0))
                .horizontal_alignment(HorizontalAlignment::Left)
                .vertical_alignment(VerticalAlignment::Bottom)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(5.0)
                        .children((
                            TextBlock::new()
                                .text(format!("▶ {}", theme::fmt_count(item.play_count)))
                                .foreground(white.clone())
                                .font_size(10.0),
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(2.0)
                                .children((
                                    FontIcon::new()
                                        .glyph("\u{E8BD}")
                                        .font_size(9.5)
                                        .foreground(white.clone())
                                        .vertical_alignment(VerticalAlignment::Center),
                                    TextBlock::new()
                                        .text(theme::fmt_count(item.danmaku_count))
                                        .foreground(white.clone())
                                        .font_size(10.0),
                                )),
                        )),
                )
                .into(),
        );
        // Mobile 右下角时长徽标
        cover_layers.push(
            Border::new()
                .background(overlay_bg)
                .corner_radius(4.0)
                .padding(Thickness::xy(5.0, 2.0))
                .margin(Thickness::new(0.0, 0.0, 6.0, 6.0))
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Bottom)
                .content(
                    theme::text(theme::fmt_duration(item.duration_secs))
                        .foreground(white)
                        .font_size(10.5),
                )
                .into(),
        );
    } else {
        cover_layers.push(
            Border::new()
                .background(ThemeBrush::SolidBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .corner_radius(4.0)
                .padding(Thickness::xy(6.0, 2.0))
                .margin(Thickness::new(8.0, 0.0, 8.0, 8.0))
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Bottom)
                .content(
                    theme::text(theme::fmt_duration(item.duration_secs))
                        .foreground(ThemeBrush::PrimaryText)
                        .font_size(11.0),
                )
                .into(),
        );
    }

    let cover = Grid::new()
        .height(metrics.cover_height)
        .children(cover_layers);

    let text_area = if compact {
        StackPanel::new()
            .margin(Thickness::new(8.0, 6.0, 8.0, 8.0))
            .spacing(4.0)
            .children((
                theme::text(item.title.clone())
                    .font_size(12.5)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .text_wrapping(TextWrapping::Wrap)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .max_lines(2),
                theme::text(item.author.clone())
                    .font_size(11.0)
                    .opacity(0.6)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .max_lines(1),
            ))
    } else {
        StackPanel::new()
            .margin(Thickness::new(12.0, 10.0, 12.0, 12.0))
            .spacing(4.0)
            .children((
                theme::text(item.title.clone())
                    .font_size(14.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .text_wrapping(TextWrapping::Wrap)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .max_lines(2),
                theme::text(item.author.clone())
                    .font_size(12.0)
                    .opacity(0.6)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .max_lines(1),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(10.0)
                    .children((
                        theme::text(format!("▶ {}", theme::fmt_count(item.play_count)))
                            .font_size(11.0)
                            .opacity(0.6),
                        theme::text(format!("弹幕 {}", theme::fmt_count(item.danmaku_count)))
                            .font_size(11.0)
                            .opacity(0.6),
                    )),
            ))
    };

    let card_body = Border::new()
        .background(ThemeBrush::CardBackground)
        .border_brush(if is_focused {
            ThemeBrush::Accent
        } else {
            ThemeBrush::CardStroke
        })
        .border_thickness(if is_focused { 2.5 } else { 1.0 })
        .corner_radius(8.0)
        .content(StackPanel::new().children((View::from(cover), View::from(text_area))));

    if compact {
        return card_body
            .width(metrics.card_width)
            .height(metrics.card_height)
            .on_pointer_released(move |_| on_click.call(bvid.clone()))
            .into();
    }

    Button::new()
        .style(ButtonStyle::Subtle)
        .width(metrics.card_width)
        .height(metrics.card_height)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .vertical_content_alignment(VerticalAlignment::Stretch)
        .on_click(move || on_click.call(bvid.clone()))
        .content(card_body)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_ignores_hover_until_video_filled() {
        let mut hm = HoverMachine::new(None);
        assert!(hm.on_enter().is_none());
        assert!(hm.scale_transition().is_none());

        assert!(hm.sync_bvid(Some("BV1test".into())));
        let seq = hm.on_enter().expect("填充视频后应支持悬停微缩");
        assert_eq!(
            hm.scale_transition(),
            Some((CONTRACT_SCALE, CONTRACT_DURATION))
        );
        assert_eq!(hm.on_expand(seq), Some(seq));
        assert_eq!(hm.scale_transition(), Some((EXPAND_SCALE, EXPAND_DURATION)));
        assert_eq!(hm.on_preresolve(seq).as_deref(), Some("BV1test"));
        // 同一卡片同次停驻不重复触发预解析。
        assert!(hm.on_preresolve(seq).is_none());
    }

    #[test]
    fn early_exit_and_cover_refresh_behave_cleanly() {
        let mut hm = HoverMachine::new(Some("BV1alpha".into()));
        let seq = hm.on_enter().unwrap();
        // 封面到达不改变 bvid，不得中断收缩/扩张动画。
        assert!(!hm.sync_bvid(Some("BV1alpha".into())));
        assert_eq!(hm.on_expand(seq), Some(seq));

        // 鼠标移出后旧代次定时器全部作废并复位为 1.0。
        hm.on_exit();
        assert_eq!(hm.scale_transition(), Some((1.0, RESET_DURATION)));
        assert!(hm.on_preresolve(seq).is_none());
    }
}
