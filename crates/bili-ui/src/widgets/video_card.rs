//! 视频卡片与自适应网格（`docs/ui-design.md` §3）：
//! - 先分配固定槽位框架（`HomeSlot::Skeleton`），数据/封面就绪后原地填充，避免整页重排与滚动抖动；
//! - 每张卡片为独立 `VideoCard` 子组件（本地局部状态 + `PartialEq` 跳过未变卡）；
//! - 鼠标悬停呈现两段式中心缩放动画（70ms 微缩蓄力 0.96 → 150ms 扩张 1.045），动画确认后再触发预解析（不自动播放）。

use std::rc::Rc;
use std::time::Duration;

use windows_reactor::*;

use crate::data::VideoItem;
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

/// 构建推荐流按行虚拟化数据源（仅在槽位/列数/封面实质变更时重建，避免滚动事件触发整页行刷新）。
pub fn home_virtual_source(
    slots: &[HomeSlot],
    cols: usize,
    row_revision: u64,
    focused_idx: Option<usize>,
    on_open: &Callback<String>,
    on_hover_preresolve: &Callback<String>,
    on_row_active: &Callback<(u64, usize, bool)>,
) -> VirtualSource {
    let cols = cols.max(1);
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
                focused_col,
                on_open: on_open.clone(),
                on_hover_preresolve: on_hover.clone(),
                on_row_active: on_row.clone(),
            })
        },
    )
}

/// 首页虚拟化行网格：把缓存的 `VirtualSource` 交给 `ItemsRepeater`，
/// 仅在视口附近创建 WinUI 控件并在划出后自动回收，防止长列表 COM 对象与解码像素无限膨胀。
pub fn virtual_video_grid(source: &VirtualSource) -> View {
    ItemsRepeater::new()
        .margin(Thickness::new(36.0, 0.0, 36.0, 24.0))
        .virtual_source(source.clone())
        .into()
}

/// 兼容旧调用（如热门页）：直接把静态列表转为网格，沿用同款收缩-扩张悬停交互。
pub fn video_grid(items: &[VideoItem], on_open: &Callback<String>) -> View {
    let slots: Vec<HomeSlot> = items
        .iter()
        .cloned()
        .map(|item| HomeSlot::Video {
            slot_key: format!("static:{}", item.bvid),
            item,
        })
        .collect();
    let cells = slots.iter().map(|slot| {
        KeyedView::new(
            slot.slot_key().to_string(),
            View::component::<VideoCard>(VideoCardInput {
                slot: slot.clone(),
                is_focused: false,
                on_open: on_open.clone(),
                on_hover_preresolve: None,
            }),
        )
    });
    VariableSizedWrapGrid::new()
        .orientation(Orientation::Horizontal)
        .item_width(theme::CARD_WIDTH + theme::CARD_GAP)
        .item_height(theme::CARD_HEIGHT + theme::CARD_GAP)
        .keyed_children(cells)
        .into()
}

#[derive(Clone, PartialEq)]
pub struct FeedRowInput {
    pub row_revision: u64,
    pub row_idx: usize,
    pub slots: Rc<Vec<HomeSlot>>,
    pub focused_col: Option<usize>,
    pub on_open: Callback<String>,
    pub on_hover_preresolve: Callback<String>,
    pub on_row_active: Callback<(u64, usize, bool)>,
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
                    is_focused: input.focused_col == Some(col_idx),
                    on_open: input.on_open.clone(),
                    on_hover_preresolve: Some(input.on_hover_preresolve.clone()),
                }),
            )
        });
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(theme::CARD_GAP)
            .height(theme::CARD_HEIGHT)
            .margin(Thickness::new(0.0, 0.0, 0.0, theme::CARD_GAP))
            .keyed_children(cells)
            .into()
    }
}

#[derive(Clone, PartialEq)]
pub struct VideoCardInput {
    pub slot: HomeSlot,
    pub is_focused: bool,
    pub on_open: Callback<String>,
    pub on_hover_preresolve: Option<Callback<String>>,
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

    /// 返回 `(bvid_changed, should_cancel_timer)`。
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
        if self.hover.sync_bvid(input.slot.bvid().map(ToOwned::to_owned)) {
            self.timer = None;
        }
    }

    fn update(&mut self, message: Self::Message, context: &ComponentContext<Self>) {
        match message {
            Message::PointerEntered => {
                if let Some(seq) = self.hover.on_enter() {
                    self.timer = Some(
                        context.set_local_timeout(CONTRACT_DURATION, move || Message::ExpandTick(seq)),
                    );
                }
            }
            Message::ExpandTick(seq) => {
                if let Some(seq) = self.hover.on_expand(seq) {
                    self.timer = Some(
                        context.set_local_timeout(EXPAND_DURATION, move || Message::PreresolveTick(seq)),
                    );
                }
            }
            Message::PreresolveTick(seq) => {
                if let Some(bvid) = self.hover.on_preresolve(seq) {
                    self.timer = None;
                    let input = context.input();
                    if let Some(cb) = &input.on_hover_preresolve {
                        if std::env::var_os("BILIPLAY_TRACE_HOME").is_some() {
                            eprintln!("[bili-ui] hover expanded -> preresolve {bvid}");
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
        let content: View = match &input.slot {
            HomeSlot::Skeleton { .. } => skeleton_card_view(),
            HomeSlot::Video { item, .. } => populated_card_view(item, input.is_focused, &input.on_open),
        };

        let mut animated_box = Border::new()
            .width(theme::CARD_WIDTH)
            .height(theme::CARD_HEIGHT)
            .content(content);

        if let Some((scale, duration)) = self.hover.scale_transition() {
            animated_box = animated_box.scale_transition(duration).scale(scale);
        }

        // 外层固定命中盒（不缩放）：防止收缩阶段鼠标落在边缘外侧引发 Enter/Exit 抖动。
        let on_enter = context.callback(|_: PointerEventInfo| Message::PointerEntered);
        let on_exit = context.callback(|_: PointerEventInfo| Message::PointerExited);
        Border::new()
            .width(theme::CARD_WIDTH)
            .height(theme::CARD_HEIGHT)
            .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
            .on_pointer_entered(on_enter)
            .on_pointer_exited(on_exit)
            .content(animated_box)
            .into()
    }
}

fn skeleton_card_view() -> View {
    let bar = |width: f64, opacity: f64, top: f64| {
        Border::new()
            .width(width)
            .height(10.0)
            .margin(Thickness::new(12.0, top, 0.0, 0.0))
            .corner_radius(2.0)
            .background(ThemeBrush::SolidBackground)
            .opacity(opacity)
    };
    Border::new()
        .width(theme::CARD_WIDTH)
        .height(theme::CARD_HEIGHT)
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .content(
            StackPanel::new().children((
                Border::new()
                    .height(theme::CARD_COVER_HEIGHT)
                    .margin(Thickness::new(12.0, 12.0, 12.0, 0.0))
                    .corner_radius(4.0)
                    .background(ThemeBrush::SolidBackground)
                    .opacity(0.35),
                bar(170.0, 0.45, 12.0),
                bar(110.0, 0.3, 8.0),
            )),
        )
        .into()
}

fn populated_card_view(item: &VideoItem, is_focused: bool, on_open: &Callback<String>) -> View {
    let bvid = item.bvid.clone();
    let on_click = on_open.clone();
    let cover_inner: View = match &item.cover {
        Some(img) => Image::new()
            .source_data(img.clone())
            .stretch(Stretch::UniformToFill)
            .into(),
        None => TextBlock::new()
            .text("封面加载中")
            .font_size(12.0)
            .opacity(0.35)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    };
    let cover = Grid::new()
        .height(theme::CARD_COVER_HEIGHT)
        .children((
            Border::new()
                .corner_radius(8.0)
                .background(ThemeBrush::SolidBackground)
                .content(cover_inner),
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
                    TextBlock::new()
                        .text(theme::fmt_duration(item.duration_secs))
                        .foreground(ThemeBrush::PrimaryText)
                        .font_size(11.0),
                ),
        ));
    Button::new()
        .style(ButtonStyle::Subtle)
        .width(theme::CARD_WIDTH)
        .height(theme::CARD_HEIGHT)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .vertical_content_alignment(VerticalAlignment::Stretch)
        .on_click(move || on_click.call(bvid.clone()))
        .content(
            Border::new()
                .background(ThemeBrush::CardBackground)
                .border_brush(if is_focused {
                    ThemeBrush::Accent
                } else {
                    ThemeBrush::CardStroke
                })
                .border_thickness(if is_focused { 2.5 } else { 1.0 })
                .corner_radius(8.0)
                .content(
                    StackPanel::new().children((
                        View::from(cover),
                        StackPanel::new()
                            .margin(Thickness::new(12.0, 10.0, 12.0, 12.0))
                            .spacing(4.0)
                            .children((
                                TextBlock::new()
                                    .text(item.title.clone())
                                    .font_size(14.0)
                                    .font_weight(FontWeight::SEMI_BOLD)
                                    .text_wrapping(TextWrapping::Wrap)
                                    .max_lines(2),
                                TextBlock::new()
                                    .text(item.author.clone())
                                    .font_size(12.0)
                                    .opacity(0.6),
                                StackPanel::new()
                                    .orientation(Orientation::Horizontal)
                                    .spacing(10.0)
                                    .children((
                                        TextBlock::new()
                                            .text(format!("▶ {}", theme::fmt_count(item.play_count)))
                                            .font_size(11.0)
                                            .opacity(0.6),
                                        TextBlock::new()
                                            .text(format!("弹幕 {}", theme::fmt_count(item.danmaku_count)))
                                            .font_size(11.0)
                                            .opacity(0.6),
                                    )),
                            )),
                    )),
                ),
        )
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
        assert_eq!(hm.scale_transition(), Some((CONTRACT_SCALE, CONTRACT_DURATION)));
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
