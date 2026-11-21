//! #7 卡片悬停预览卡(Store 风格,全局单例 PeekCard)。
//!
//! 卡片自身不再持有预览状态:只把「进入/离开(含窗口坐标边界)」上报给 Shell,
//! 由 Shell 的 [`PeekMachine`] 统一计时(进入 300ms 预取简介 → 1s 弹出 → 离开
//! 宽限 200ms),并在根布局末层的覆盖 `Canvas` 里挂载**全局唯一**的 PeekCard。
//! 不原地展开(避免 ItemsRepeater 重排,#4 教训);不用 Popup/Flyout/TeachingTip;
//! 定位用 pointer 事件自带的窗口相对 DIP(与根 Canvas 同坐标空间,免
//! `TransformToVisual`;焦点触发路径则由 Shell 经 winrt_ext 解析焦点元素矩形),
//! 屏幕边缘翻转由 [`peek_placement`] 纯函数决定。
//! 动画走 reactor 的 composition 隐式过渡(`scale_transition`/`opacity_transition`
//! → `SetScaleTransition`/`SetOpacityTransition`,GPU 合成线程,不占 UI 线程);
//! 挂载帧先落 Scale 1.0/Opacity 0,16ms 后的翻牌帧过渡到 1.15/1.0(中心缩放,
//! 视觉尺寸按 1.15 计入边缘翻转)。
//! `reduce_animations` 设置或系统动画开关关闭时跳过两帧过渡,直接以终态显示。

use std::time::Duration;

use windows_reactor::*;

use crate::data::VideoItem;
use crate::theme;

/// 悬停进入后预取简介的延迟。
pub const PEEK_PREFETCH: Duration = Duration::from_millis(300);
/// 悬停停留多久后弹出预览卡。
pub const PEEK_POP: Duration = Duration::from_millis(1000);
/// 离开卡片/预览卡后的收起宽限。
pub const PEEK_GRACE: Duration = Duration::from_millis(200);
/// 挂载帧与翻牌帧之间的间隔(让首帧先发布终值前的起点属性)。
pub const PEEK_ANIM_SETTLE: Duration = Duration::from_millis(16);
/// Composition 隐式过渡时长。
pub const PEEK_ANIM_DURATION: Duration = Duration::from_millis(180);
/// 弹出终态的视觉放大倍率(中心缩放)。
pub const PEEK_POP_SCALE: f64 = 1.15;
/// PeekCard 布局宽(不含缩放)。
pub const PEEK_WIDTH: f64 = 380.0;
/// PeekCard 布局高(不含缩放)。内容预算:pad24 + 封面200 + 标题44 + 简介51
/// + 统计18 + 按钮44 + 间距40 ≈ 421,取 440 留余量,防子元素溢出边框。
pub const PEEK_HEIGHT: f64 = 440.0;
/// 与悬停卡片的垂直间距。
pub const PEEK_GAP: f64 = 8.0;
/// 窗口边缘安全边距。
pub const PEEK_EDGE_MARGIN: f64 = 12.0;

/// 悬停卡片在窗口坐标(DIP)中的矩形,由卡片从 pointer 事件推导。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 卡片 → Shell 的悬停上报(进入/离开;焦点触发的 rect 由 Shell 在弹出时解析)。
#[derive(Clone, Debug, PartialEq)]
pub struct HoverReport {
    /// 进 `Message` 通道,必须 `Send`(spawn_background 约束),用 String 不用 `Rc<str>`。
    pub bvid: String,
    pub entering: bool,
    pub rect: Option<CardRect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeekPhase {
    Idle,
    /// 已上报进入,预取/弹出计时在飞。
    Pending,
    /// 预览卡已弹出可见。
    Open,
    /// 已离开,宽限倒计时中(预览卡仍挂载,防止移入预览卡时闪断)。
    Hiding,
}

/// 离开事件对定时器的处置指令(由 Shell 执行)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeekExitAction {
    /// 与本预览无关(旧卡片/已在收起)。
    Ignored,
    /// 弹出前取消:作废预取与弹出定时器(快速掠过即走此分支,绝不弹出)。
    CancelPending,
    /// 弹出后离开:以 `seq` 起 200ms 宽限定时器。
    StartGrace(u64),
}

/// 纯计时状态机:所有 tick 都带进入代次,旧代次 tick 一律作废。
pub struct PeekMachine {
    phase: PeekPhase,
    seq: u64,
    bvid: Option<String>,
    rect: Option<CardRect>,
}

impl Default for PeekMachine {
    fn default() -> Self {
        Self {
            phase: PeekPhase::Idle,
            seq: 0,
            bvid: None,
            rect: None,
        }
    }
}

impl PeekMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// 卡片上报进入。返回 `Some(seq)` = 需要以该代次(重)挂预取与弹出定时器;
    /// `None` = 无需动作(同卡已可见,或宽限期内回到同卡直接复活)。
    pub fn on_card_enter(&mut self, bvid: String, rect: Option<CardRect>) -> Option<u64> {
        match self.phase {
            // 宽限期内回到同一张卡:立刻复活,不重开 1s 计时(移入预览卡/
            // 卡片边缘抖动都不该闪断)。
            PeekPhase::Hiding if self.bvid.as_deref() == Some(bvid.as_str()) => {
                self.phase = PeekPhase::Open;
                if rect.is_some() {
                    self.rect = rect;
                }
                None
            }
            // 已可见的同卡:仅刷新锚点。
            PeekPhase::Open if self.bvid.as_deref() == Some(bvid.as_str()) => {
                if rect.is_some() {
                    self.rect = rect;
                }
                None
            }
            // Pending 同卡:重复进入(Enter 抖动),定时器原样续用。
            PeekPhase::Pending if self.bvid.as_deref() == Some(bvid.as_str()) => Some(self.seq),
            _ => {
                self.seq = self.seq.wrapping_add(1);
                self.phase = PeekPhase::Pending;
                self.bvid = Some(bvid);
                self.rect = rect;
                Some(self.seq)
            }
        }
    }

    /// 卡片上报离开。
    pub fn on_card_exit(&mut self, bvid: &str) -> PeekExitAction {
        if self.bvid.as_deref() != Some(bvid) || self.phase == PeekPhase::Idle {
            return PeekExitAction::Ignored;
        }
        match self.phase {
            PeekPhase::Pending | PeekPhase::Idle => {
                self.phase = PeekPhase::Idle;
                self.bvid = None;
                self.rect = None;
                PeekExitAction::CancelPending
            }
            PeekPhase::Open => {
                self.phase = PeekPhase::Hiding;
                self.seq = self.seq.wrapping_add(1);
                PeekExitAction::StartGrace(self.seq)
            }
            PeekPhase::Hiding => PeekExitAction::Ignored,
        }
    }

    /// 指针移入预览卡本体:宽限期内撤销收起。返回是否撤销了收起定时器。
    pub fn on_peek_enter(&mut self) -> bool {
        if self.phase == PeekPhase::Hiding {
            self.phase = PeekPhase::Open;
            return true;
        }
        false
    }

    /// 指针离开预览卡本体(预览卡盖在卡片上方,离开预览卡通常已在卡片外)。
    pub fn on_peek_exit(&mut self) -> Option<u64> {
        if self.phase == PeekPhase::Open {
            self.phase = PeekPhase::Hiding;
            self.seq = self.seq.wrapping_add(1);
            return Some(self.seq);
        }
        None
    }

    /// 300ms 预取 tick:仅 Pending 且代次匹配才放行 bvid。
    pub fn on_prefetch_tick(&mut self, seq: u64) -> Option<String> {
        if self.phase == PeekPhase::Pending && seq == self.seq {
            self.bvid.clone()
        } else {
            None
        }
    }

    /// 1s 弹出 tick:Pending → Open。焦点路径 rect 为 None 时由 Shell 解析后
    /// [`attach_rect`](Self::attach_rect);解析不到则 [`cancel`](Self::cancel)。
    pub fn on_pop_tick(&mut self, seq: u64) -> Option<String> {
        if self.phase == PeekPhase::Pending && seq == self.seq {
            self.phase = PeekPhase::Open;
            return self.bvid.clone();
        }
        None
    }

    /// 焦点弹出锚定:补写解析出的卡片矩形。
    pub fn attach_rect(&mut self, rect: CardRect) {
        if self.phase == PeekPhase::Open || self.phase == PeekPhase::Pending {
            self.rect = Some(rect);
        }
    }

    /// 200ms 宽限到期:收起。返回 `true` = 预览卡需要卸载(视图变化)。
    pub fn on_grace_tick(&mut self, seq: u64) -> bool {
        if self.phase == PeekPhase::Hiding && seq == self.seq {
            self.phase = PeekPhase::Idle;
            self.bvid = None;
            self.rect = None;
            return true;
        }
        false
    }

    /// 外部作废(路由切换/全屏/换一换/滚动回收/尺寸变化):立即收起。
    /// 返回 `true` = 原本挂载,视图需要变化。
    pub fn cancel(&mut self) -> bool {
        let was_visible = matches!(self.phase, PeekPhase::Open | PeekPhase::Hiding);
        self.phase = PeekPhase::Idle;
        self.bvid = None;
        self.rect = None;
        self.seq = self.seq.wrapping_add(1);
        was_visible
    }

    /// 预览卡当前是否需要挂载(Open 与宽限期 Hiding 都保持挂载)。
    pub fn is_mounted(&self) -> bool {
        matches!(self.phase, PeekPhase::Open | PeekPhase::Hiding) && self.rect.is_some()
    }

    /// 挂载内容(bvid + 锚点矩形);Shell 视图层与探针断言用。
    pub fn mounted_content(&self) -> Option<(String, CardRect)> {
        if !self.is_mounted() {
            return None;
        }
        Some((self.bvid.clone()?, self.rect?))
    }

    pub fn phase_open(&self) -> bool {
        matches!(self.phase, PeekPhase::Open | PeekPhase::Hiding)
    }

    /// 是否处于宽限期 Hiding(淡出帧值用)。
    pub fn hiding(&self) -> bool {
        self.phase == PeekPhase::Hiding
    }
}

/// ✕ 手动关闭的静默范围:仅被关闭的那张卡,直到指针离开它或进入别的卡。
/// 指针仍停在原卡上时不立刻再弹(防「关了又弹」);换卡/离开即恢复,
/// 不做会话级全局静默(旧 `peek_dismissed` 教训:✕ 之后整个首页不再弹出)。
#[derive(Default)]
pub struct PeekDismissal {
    bvid: Option<String>,
}

impl PeekDismissal {
    /// ✕ 关闭当前预览:登记被静默的稿件(无挂载内容时等价复位)。
    pub fn close(&mut self, bvid: Option<String>) {
        self.bvid = bvid;
    }

    /// 进入上报:返回是否放行。进入别的卡即清除静默。
    pub fn on_enter(&mut self, bvid: &str) -> bool {
        if self.bvid.as_deref() == Some(bvid) {
            return false;
        }
        self.bvid = None;
        true
    }

    /// 离开上报:离开被静默的卡即解除(再次悬停可重新弹出)。
    pub fn on_exit(&mut self, bvid: &str) {
        if self.bvid.as_deref() == Some(bvid) {
            self.bvid = None;
        }
    }

    /// 外部复位(点卡进播放页/预览内播放/设置开关)。
    pub fn reset(&mut self) {
        self.bvid = None;
    }
}

/// 预览卡落点:画布上的布局左上角(视觉盒按 1.15 中心缩放计入边缘避让)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    /// 上方放不下、翻到卡片下方。
    pub flipped_below: bool,
}

/// 屏幕边缘翻转定位:优先卡片上方水平居中;上方越界翻到下方;仍越界贴顶/底
/// 钳位;水平始终钳进窗口安全区。全部 DIP,与 pointer 事件/根 Canvas 同空间。
pub fn peek_placement(
    anchor: CardRect,
    window: (f64, f64),
    peek: (f64, f64),
    scale: f64,
    margin: f64,
    gap: f64,
) -> Placement {
    // 中心缩放的视觉盒比布局盒大 (scaled-unscaled)/2 的四周余量。
    let v_w = peek.0 * scale;
    let v_h = peek.1 * scale;
    let pad_x = (v_w - peek.0) / 2.0;
    let pad_y = (v_h - peek.1) / 2.0;

    // 垂直:优先上方(视觉盒底缘 = 卡片顶缘 - gap;返回值为布局盒左上,
    // 视觉盒 = 布局盒四周各外扩 pad)。
    let mut flipped_below = false;
    let mut y = anchor.y - gap - v_h + pad_y;
    if y - pad_y < margin {
        flipped_below = true;
        y = anchor.y + anchor.h + gap + pad_y;
    }
    let max_y = (window.1 - margin - peek.1 - pad_y).max(margin + pad_y);
    if y + v_h - pad_y > window.1 - margin {
        y = max_y;
    }
    let min_y = margin + pad_y;
    if y < min_y {
        y = min_y;
    }

    // 水平:以卡片中线居中,再按视觉盒钳进窗口。
    let mut x = anchor.x + (anchor.w - peek.0) / 2.0;
    let max_x = (window.0 - margin - peek.0 - pad_x).max(margin + pad_x);
    if x + v_w - pad_x > window.0 - margin {
        x = max_x;
    }
    let min_x = margin + pad_x;
    if x < min_x {
        x = min_x;
    }
    Placement { x, y, flipped_below }
}


/// 弹出/收起两帧动画在某一帧应发布的视觉值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeekRender {
    pub scale: f64,
    pub opacity: f64,
    /// 是否声明 composition 隐式过渡(降级时为 false,直接终态)。
    pub transition: bool,
}

/// 两帧进场的帧值:`armed=false` 为挂载帧(视觉起点),`armed=true` 为翻牌帧;
/// `open` = 处于 Open 还是宽限期 Hiding(收起淡出);`reduced` = 降级无动画
/// (直接以 1.0 原尺寸显示,跳过放大与淡入)。
pub fn peek_render(open: bool, armed: bool, reduced: bool) -> PeekRender {
    if reduced {
        return PeekRender {
            scale: 1.0,
            opacity: 1.0,
            transition: false,
        };
    }
    if !armed {
        return PeekRender {
            scale: 1.0,
            opacity: 0.0,
            transition: true,
        };
    }
    PeekRender {
        scale: PEEK_POP_SCALE,
        opacity: if open { 1.0 } else { 0.0 },
        transition: true,
    }
}

/// 预览卡内容:封面 / 标题(2 行) / 简介(3 行,预取到货前占位) /
/// 播放量·弹幕数·UP 主 / 「播放」「稍后再看」。`on_*` 回调由 Shell 注入。
#[allow(clippy::too_many_arguments)]
pub fn peek_card_view(
    item: &VideoItem,
    desc: Option<&str>,
    notice: Option<&str>,
    placement: Placement,
    render: PeekRender,
    bvid: &str,
    on_enter: impl Into<PointerCallback>,
    on_exit: impl Into<PointerCallback>,
    on_play: &Callback<String>,
    on_watch_later: &Callback<String>,
    on_close: &Callback<()>,
) -> View {
    let scale = render.scale;
    let opacity = render.opacity;
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
    let desc_view: View = match desc {
        Some(text) if !text.trim().is_empty() => theme::text(text.trim().to_string())
            .font_size(12.5)
            .opacity(0.75)
            .text_wrapping(TextWrapping::Wrap)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .max_lines(3)
            .into(),
        _ => theme::text("简介加载中…")
            .font_size(12.5)
            .opacity(0.4)
            .max_lines(1)
            .into(),
    };
    let play_cb = on_play.clone();
    let later_cb = on_watch_later.clone();
    let close_cb = on_close.clone();
    let play_bvid = bvid.to_string();
    let later_bvid = bvid.to_string();

    let mut rows: Vec<View> = vec![
        View::from(
            Border::new()
                .height(200.0)
                .corner_radius(6.0)
                .background(ThemeBrush::SolidBackground)
                .content(cover_inner),
        ),
        View::from(
            theme::text(item.title.clone())
                .font_size(14.5)
                .font_weight(FontWeight::SEMI_BOLD)
                .text_wrapping(TextWrapping::Wrap)
                .text_trimming(TextTrimming::CharacterEllipsis)
                .max_lines(2),
        ),
        desc_view,
        View::from(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .children((
                    theme::text(format!("▶ {}", theme::fmt_count(item.play_count)))
                        .font_size(11.5)
                        .opacity(0.6),
                    theme::text(format!("弹幕 {}", theme::fmt_count(item.danmaku_count)))
                        .font_size(11.5)
                        .opacity(0.6),
                    theme::text(item.author.clone())
                        .font_size(11.5)
                        .opacity(0.6)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .max_lines(1),
                )),
        ),
    ];
    if let Some(notice) = notice {
        rows.push(View::from(
            theme::text(notice.to_string())
                .font_size(11.5)
                .foreground(Brush::Solid(Color::rgb(255, 179, 92))),
        ));
    }
    rows.push(View::from(
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(8.0)
            .children((
                View::from(
                    Button::new()
                        .content("播放")
                        .style(ButtonStyle::Accent)
                        .on_click(move || play_cb.call(play_bvid.clone())),
                ),
                View::from(
                    Button::new()
                        .content("稍后再看")
                        .style(ButtonStyle::Subtle)
                        .grid_column(1)
                        .on_click(move || later_cb.call(later_bvid.clone())),
                ),
            )),
    ));

    // 右上收起钮:键盘/手柄用户与降级模式下不依赖「离开宽限」也能关闭。
    let content = Grid::new().children((
        View::from(StackPanel::new().spacing(8.0).children(rows)),
        View::from(
            Button::new()
                .content("✕")
                .style(ButtonStyle::Subtle)
                .horizontal_alignment(HorizontalAlignment::Right)
                .vertical_alignment(VerticalAlignment::Top)
                .automation_name("关闭预览")
                .on_click(move || close_cb.call(())),
        ),
    ));
    // 卡体浮在无背景覆盖 Canvas 上:CardBackground 深色主题仅 ~5% 白,
    // 单独使用会透出下方视频网格;必须先垫不透明 Solid 底再叠 Card tint。
    let body = Border::new()
        .background(ThemeBrush::SolidBackground)
        .corner_radius(10.0)
        .content(
            Border::new()
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .corner_radius(10.0)
                .padding(Thickness::xy(12.0, 12.0))
                .content(content),
        );

    let placed = body
        .canvas_left(placement.x)
        .canvas_top(placement.y)
        .width(PEEK_WIDTH)
        .height(PEEK_HEIGHT);
    // 隐式过渡:挂载帧发布起点值,翻牌帧改终值触发 composition 过渡。
    let mut root = if render.transition {
        placed
            .scale_transition(PEEK_ANIM_DURATION)
            .opacity_transition(PEEK_ANIM_DURATION)
    } else {
        placed
    };
    root = root.scale(scale).opacity(opacity);
    root.on_pointer_entered(on_enter)
        .on_pointer_exited(on_exit)
        .automation_name("预览卡")
        .automation_id("peek-card")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CardRect {
        CardRect { x, y, w, h }
    }

    /// 验收:进入 1s 弹出、离开宽限 200ms、宽限内回卡不收起。
    #[test]
    fn hover_pop_and_grace_lifecycle() {
        let mut m = PeekMachine::new();
        let seq = m
            .on_card_enter("BV1a".into(), Some(rect(100.0, 200.0, 220.0, 160.0)))
            .expect("首次进入应挂起计时");
        assert_eq!(m.on_pop_tick(seq.wrapping_add(1)), None, "旧代次 tick 作废");
        assert_eq!(m.on_pop_tick(seq).as_deref(), Some("BV1a"));
        assert!(m.is_mounted());

        // 离开 → 宽限(仍挂载)。
        let grace = match m.on_card_exit("BV1a") {
            PeekExitAction::StartGrace(seq) => seq,
            other => panic!("期望宽限,实际 {other:?}"),
        };
        assert!(m.is_mounted(), "宽限期内预览卡不闪断");
        // 宽限内回到同卡:直接复活,撤销收起。
        assert_eq!(m.on_card_enter("BV1a".into(), None), None);
        assert!(!m.on_grace_tick(grace), "撤销后的宽限 tick 必须作废");
        assert!(m.phase_open());
    }

    /// 验收:快速掠过(每卡停留 <1s)绝不弹出。
    #[test]
    fn fast_sweep_never_pops() {
        let mut m = PeekMachine::new();
        for i in 0..10 {
            let bvid = format!("BV{i}");
            let seq = m
                .on_card_enter(bvid.clone(), Some(rect(0.0, 0.0, 100.0, 60.0)))
                .unwrap();
            assert_eq!(
                m.on_card_exit(&bvid),
                PeekExitAction::CancelPending,
                "Pending 期离开只撤定时器,不收起(本来就没弹)"
            );
            assert!(m.on_pop_tick(seq).is_none(), "被掠过卡的弹出 tick 全部作废");
            assert!(m.on_prefetch_tick(seq).is_none(), "预取 tick 同样作废");
            assert!(!m.is_mounted(), "快速掠过全程不得弹出");
        }
    }

    /// 验收:指针从卡片移入 PeekCard(先离开卡片再进入预览卡)不收起。
    #[test]
    fn moving_into_peek_card_keeps_it_open() {
        let mut m = PeekMachine::new();
        let seq = m
            .on_card_enter("BV1x".into(), Some(rect(0.0, 0.0, 100.0, 60.0)))
            .unwrap();
        m.on_pop_tick(seq).unwrap();
        let grace = match m.on_card_exit("BV1x") {
            PeekExitAction::StartGrace(seq) => seq,
            other => panic!("期望宽限,实际 {other:?}"),
        };
        assert!(m.on_peek_enter(), "宽限内移入预览卡应撤销收起");
        assert!(!m.on_grace_tick(grace));
        // 离开预览卡再收起。
        let grace2 = m.on_peek_exit().expect("离开预览卡重新起宽限");
        assert!(m.on_grace_tick(grace2));
        assert!(!m.is_mounted());
        // 与预览无关的旧卡离开不产生任何动作。
        assert_eq!(m.on_card_exit("BV1x"), PeekExitAction::Ignored);
    }

    /// 换卡:已弹出状态悬停新卡,预览卡立即让位给新卡的 Pending。
    #[test]
    fn switching_card_resets_to_pending() {
        let mut m = PeekMachine::new();
        let seq_a = m
            .on_card_enter("BVa".into(), Some(rect(0.0, 0.0, 100.0, 60.0)))
            .unwrap();
        m.on_pop_tick(seq_a).unwrap();
        let _ = m.on_card_exit("BVa");
        let seq_b = m
            .on_card_enter("BVb".into(), Some(rect(120.0, 0.0, 100.0, 60.0)))
            .expect("换卡重开计时");
        assert!(!m.is_mounted(), "新卡 Pending 期间旧预览不残留");
        assert_eq!(m.on_pop_tick(seq_b).as_deref(), Some("BVb"));
        assert_eq!(m.on_grace_tick(seq_a), false);
    }

    #[test]
    fn cancel_reports_visibility_and_voids_ticks() {
        let mut m = PeekMachine::new();
        let seq = m
            .on_card_enter("BVc".into(), Some(rect(0.0, 0.0, 10.0, 10.0)))
            .unwrap();
        m.on_pop_tick(seq).unwrap();
        assert!(m.cancel(), "可见状态被作废需触发视图变化");
        assert!(!m.cancel(), "已 Idle 再 cancel 无视图变化");
        let seq2 = m
            .on_card_enter("BVc".into(), Some(rect(0.0, 0.0, 10.0, 10.0)))
            .unwrap();
        assert_ne!(seq2, seq, "cancel 换代次");
        assert!(m.on_pop_tick(seq).is_none());
    }

    /// 验收:屏幕边缘翻转定位——居上、上沿越界翻下、左/右越界钳位、下沿越界钳底。
    #[test]
    fn placement_flips_at_top_and_clamps_at_side_edges() {
        let peek = (PEEK_WIDTH, PEEK_HEIGHT);
        let win = (1600.0, 1000.0);
        // 常规:视觉盒(420×1.15=483 高)能完整放进卡片上方的位置。
        let card = rect(700.0, 600.0, 220.0, 160.0);
        let p = peek_placement(card, win, peek, PEEK_POP_SCALE, PEEK_EDGE_MARGIN, PEEK_GAP);
        assert!(!p.flipped_below);
        let pad_y = (PEEK_HEIGHT * PEEK_POP_SCALE - PEEK_HEIGHT) / 2.0;
        assert!(p.y + PEEK_HEIGHT + pad_y <= 600.0 - PEEK_GAP + 0.001, "视觉盒底缘贴卡片顶缘上方 gap");
        assert!(
            (p.x + PEEK_WIDTH / 2.0 - (card.x + card.w / 2.0)).abs() < 0.001,
            "视觉中线应对齐卡片中线"
        );
        // 上沿越界:翻到卡片下方。
        let top = rect(700.0, 500.0, 220.0, 160.0);
        let p = peek_placement(top, win, peek, PEEK_POP_SCALE, PEEK_EDGE_MARGIN, PEEK_GAP);
        assert!(p.flipped_below, "上方放不下必须翻到下方");
        // 翻下后视觉顶缘 ≥ 卡片底缘 + gap,或已被窗口底部钳位(取较小者)。
        assert!(
            p.y - pad_y >= top.y + top.h + PEEK_GAP - 0.001
                || p.y <= win.1 - PEEK_EDGE_MARGIN - PEEK_HEIGHT - pad_y + 0.001
        );
        // 左缘第一列:不得越出窗口左边距。
        let left = rect(10.0, 600.0, 220.0, 160.0);
        let p = peek_placement(left, win, peek, PEEK_POP_SCALE, PEEK_EDGE_MARGIN, PEEK_GAP);
        let pad_x = (PEEK_WIDTH * PEEK_POP_SCALE - PEEK_WIDTH) / 2.0;
        assert!(p.x - pad_x >= PEEK_EDGE_MARGIN - 0.001, "左缘必须钳进窗口");
        // 右缘最后一列:不得越出窗口右边距。
        let right = rect(1600.0 - 230.0, 600.0, 220.0, 160.0);
        let p = peek_placement(right, win, peek, PEEK_POP_SCALE, PEEK_EDGE_MARGIN, PEEK_GAP);
        assert!(p.x + PEEK_WIDTH + pad_x <= 1600.0 - PEEK_EDGE_MARGIN + 0.001, "右缘钳位");
        // 上方与下方都放不下(矮窗):钳到安全区内而非丢弃弹出。
        let short_win = (1600.0, PEEK_HEIGHT * PEEK_POP_SCALE + 2.0 * PEEK_EDGE_MARGIN + 30.0);
        let p = peek_placement(
            rect(700.0, 20.0, 220.0, 160.0),
            short_win,
            peek,
            PEEK_POP_SCALE,
            PEEK_EDGE_MARGIN,
            PEEK_GAP,
        );
        assert!(
            p.y - pad_y >= PEEK_EDGE_MARGIN - 0.001
                && p.y + PEEK_HEIGHT + pad_y <= short_win.1 - PEEK_EDGE_MARGIN + 0.001,
            "钳位窗口安全区: {:?}",
            p
        );
    }

    /// 焦点弹出(enter 不带 rect)在解析锚点前不得挂载。
    #[test]
    fn focus_pop_without_rect_stays_unmounted() {
        let mut m = PeekMachine::new();
        let seq = m.on_card_enter("BVf".into(), None).unwrap();
        m.on_pop_tick(seq).unwrap();
        assert!(!m.is_mounted(), "没有锚点不挂载");
        m.attach_rect(rect(50.0, 50.0, 100.0, 60.0));
        assert!(m.is_mounted());
    }

    /// 验收:✕ 只静默当前卡——原卡停留不重弹,离开原卡或换卡即恢复。
    #[test]
    fn dismissal_scopes_to_closed_card_only() {
        let mut d = PeekDismissal::default();
        assert!(d.on_enter("BVa"), "未关闭过任何卡时放行");
        d.close(Some("BVa".into()));
        assert!(!d.on_enter("BVa"), "指针停在被关的原卡上不得再弹");
        assert!(!d.on_enter("BVa"), "重复进入同样被静默");
        assert!(d.on_enter("BVb"), "换卡立即恢复");
        // 换卡已清静默:回到原卡也放行。
        assert!(d.on_enter("BVa"));
        // 离开解除路径。
        d.close(Some("BVa".into()));
        d.on_exit("BVb");
        assert!(!d.on_enter("BVa"), "离开别的卡不解除原卡静默");
        d.on_exit("BVa");
        assert!(d.on_enter("BVa"), "离开原卡后再悬停应恢复");
        // 外部复位与空关闭。
        d.close(Some("BVa".into()));
        d.reset();
        assert!(d.on_enter("BVa"));
        d.close(None);
        assert!(d.on_enter("BVa"), "无挂载内容时关闭等价复位");
    }

    /// 验收:卡体背景必须是不透明 Solid 底 + Card tint 两层。预览卡浮在无背景
    /// 覆盖 Canvas 上,CardBackground(深色 ~5% 白)单独作底会透出下方视频网格。
    #[test]
    fn peek_body_paints_opaque_base_under_card_tint() {
        let item = VideoItem {
            bvid: "BV1t".into(),
            aid: 1,
            cid: 2,
            title: "标题".into(),
            author: "UP".into(),
            author_mid: 3,
            author_face_url: None,
            duration_secs: 60,
            play_count: 100,
            danmaku_count: 10,
            like_count: None,
            coin_count: None,
            favorite_count: None,
            share_count: None,
            cover: None,
            pic_url: None,
        };
        let view = peek_card_view(
            &item,
            None,
            None,
            Placement { x: 0.0, y: 0.0, flipped_below: false },
            peek_render(true, true, true),
            "BV1t",
            |_: PointerEventInfo| {},
            |_: PointerEventInfo| {},
            &Callback::new(|_: String| {}),
            &Callback::new(|_: String| {}),
            &Callback::new(|_: ()| {}),
        );
        let debug = format!("{view:?}");
        // 封面 Border 已占一处 SolidBackground;卡体底层是第二处。
        assert!(
            debug.matches("SolidBackground").count() >= 2,
            "卡体必须垫不透明 Solid 底,否则半透明 Card 刷透出下方网格"
        );
        assert!(debug.contains("CardBackground"), "Card tint 面层保留");
        assert!(debug.contains("CardStroke"), "描边保留");
    }

    /// 两帧进场帧值:挂载帧=视觉起点,翻牌帧=终态;宽限期淡出;降级直接显示。
    #[test]
    fn peek_render_frames_and_reduced_mode() {
        let mount = peek_render(true, false, false);
        assert_eq!((mount.scale, mount.opacity), (1.0, 0.0));
        assert!(mount.transition, "起点帧也要声明过渡,翻牌帧改值才会动画");
        let open = peek_render(true, true, false);
        assert_eq!((open.scale, open.opacity), (PEEK_POP_SCALE, 1.0));
        let hiding = peek_render(false, true, false);
        assert_eq!((hiding.scale, hiding.opacity), (PEEK_POP_SCALE, 0.0));
        let reduced = peek_render(true, false, true);
        assert_eq!((reduced.scale, reduced.opacity), (1.0, 1.0));
        assert!(!reduced.transition);
    }
}
