//! 视口自适应布局与卡片度量（纯值层，零 WinUI 依赖）。
//!
//! 拆分为正交两维：
//! - `ShellChromeMode`（`< 860.0` DIP —— 恰好退回 2 列阈值 —— 即切 `Mobile` 形态：底栏 5-Tab、禁用半窗、双列等宽铺满卡、56px 迷你条、双页签吸顶播放页）；
//! - `ContentLayoutTier`（`< 600` Compact / `600..860` Medium / `>= 860` Expanded，使 600..859 半屏 Mobile 仍可在顶栏右上呼出悬浮搜索，`< 600` 则保底只留通知图标防拥挤）。

use qaq_bx_core::settings::WindowBounds;

/// 客户区最小宽高下限（DIP，交由 `WindowConstraints` 强制钳制）。
pub const MIN_WINDOW_WIDTH: f64 = 390.0;
pub const MIN_WINDOW_HEIGHT: f64 = 640.0;

/// 超窄顶栏内容阈值（`< 600.0` DIP 隐藏顶栏搜索按钮防挤压系统三键）。
pub const COMPACT_CONTENT_BREAKPOINT: f64 = 600.0;
/// 渲染 2 列即切入真 Mobile 形态的精确断点（桌面 36px 边距下 3 列下限 `56+72+236*3+24 = 860.0`）。
pub const MOBILE_BREAKPOINT: f64 = 860.0;
pub const EXPANDED_BREAKPOINT: f64 = 860.0;
/// 竖屏模式（实验）客户区宽度上限：底栏出现边界 − 1 DIP，严格锁定在 Mobile 宽度带 `[390, 859]`。
pub const PORTRAIT_MAX_WIDTH_DIPS: f64 = MOBILE_BREAKPOINT - 1.0;
/// 竖屏模式（实验）初始默认客户区尺寸（9:16 竖屏手感；可向右拖宽至 859 或贴边进入 3 列）。
pub const DEFAULT_PORTRAIT_WIDTH: f64 = 480.0;
pub const DEFAULT_PORTRAIT_HEIGHT: f64 = 860.0;

/// 默认桌面窗口尺寸。
pub const DEFAULT_WINDOW_WIDTH: f64 = 1280.0;
pub const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;
/// 桌面端左侧 `NavigationView` 收起态窄栏占用宽度。
pub const DESKTOP_NAV_RAIL_WIDTH: f64 = 56.0;

/// 桌面卡宽曲线下限(F5):列数按此最小宽推导。
pub const DESKTOP_CARD_MIN_WIDTH: f64 = 200.0;
/// 桌面卡文本区高度(标题两行 + UP 行 + 内外边距;封面之外预留)。
pub const DESKTOP_CARD_TEXT_ALLOWANCE: f64 = 107.0;
/// 桌面卡宽曲线上限(F5):超宽视口不再无限拉伸。
pub const DESKTOP_CARD_MAX_WIDTH: f64 = 420.0;
/// 兼容引用:1280 DIP 内容宽下的曲线取值(≈236,历史基线)。
pub const DESKTOP_CARD_WIDTH: f64 = 236.0;
pub const DESKTOP_CARD_GAP: f64 = 12.0;
/// 兼容引用:1280 DIP 曲线下的封面高(精确 16:9 取整)。
pub const DESKTOP_CARD_COVER_HEIGHT: f64 = 133.0;
pub const DESKTOP_CARD_HEIGHT: f64 = 240.0;
pub const DESKTOP_MARGIN_X: f64 = 36.0;

pub const MOBILE_MARGIN_X: f64 = 12.0;
pub const MOBILE_CARD_GAP: f64 = 8.0;
/// Mobile 3 列切入参考最小卡宽（`599` DIP 仍为 2 列，`600` DIP 起自动升为 3 列，`859` DIP 3 列 273px）。
pub const MOBILE_CARD_MIN_WIDTH: f64 = 186.5;
pub const MOBILE_CARD_TEXT_ALLOWANCE: f64 = 84.0;

// ---------------------------------------------------------------------------
// 卡片文本列严格不变量（首页/热门/搜索共用 `VideoCard`，#10）
// ---------------------------------------------------------------------------
// 旧实现缺陷：标题 TextBlock 只设 `max_lines(2)` 不定高，实际占高随标题字数
// 在 1 行/2 行之间浮动；StackPanel 顺序流式布局把下方 UP 主行、播放·弹幕统计
// 行跟着顶下去/升上来（同屏卡片信息行错位，即用户截图症状）。
// 现约定：文本列三行（标题/UP 主/统计）全部使用**定高槽**，槽高只由字号与
// Noto Sans SC 最坏行高系数推导，与标题实际字符数、换行数**无关**；UI 层
// (`widgets/video_card.rs`) 必须从此处取槽值并配显式 `line_height`，禁止内联
// 字面量，保证「数据无关」是结构性质而非目测对齐。

/// Noto Sans SC 最坏行高系数（em）；单测沿用播放页相关推荐卡同口径。
pub const CARD_LINE_EM: f64 = 1.48;
/// 标题定高槽恒预留的行数（1 行标题也在槽内顶对齐，余量沉底不外溢）。
pub const CARD_TITLE_MAX_LINES: f64 = 2.0;
/// 标题字号：桌面 13.0，紧凑/竖屏 13.5（适配 PC 视距与 2~3 列卡宽）。
pub const CARD_TITLE_FONT_DESKTOP: f64 = 13.0;
pub const CARD_TITLE_FONT_COMPACT: f64 = 13.5;
/// UP 主行字号（纳入不变量推导）。
pub const CARD_AUTHOR_FONT_DESKTOP: f64 = 12.0;
pub const CARD_AUTHOR_FONT_COMPACT: f64 = 12.0;
/// 播放量·弹幕统计行字号。
pub const CARD_STATS_FONT: f64 = 11.0;
/// 文本列行距（与 StackPanel::spacing 同源，供高度推导）。
pub const CARD_TEXT_SPACING: f64 = 4.0;
/// 文本列容器上/下缘距（与 video_card.rs margins 同值，供高度推导）。
pub const CARD_TEXT_MARGIN_DESKTOP_TOP: f64 = 10.0;
pub const CARD_TEXT_MARGIN_DESKTOP_BOTTOM: f64 = 12.0;
pub const CARD_TEXT_MARGIN_COMPACT_TOP: f64 = 8.0;
pub const CARD_TEXT_MARGIN_COMPACT_BOTTOM: f64 = 10.0;

/// 一行定高（Noto Sans SC 最坏行高向上取整到整 DIP）。
pub fn card_line_slot(font: f64) -> f64 {
    (font * CARD_LINE_EM).ceil()
}

/// 卡片文本列纯值层几何：所有行的顶边 y 与总高只由本结构算术推导，
/// 标题长度无输入通道 → 位置不变量由构造成立。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardTextLayout {
    pub compact: bool,
}

impl CardTextLayout {
    pub fn title_font(self) -> f64 {
        if self.compact {
            CARD_TITLE_FONT_COMPACT
        } else {
            CARD_TITLE_FONT_DESKTOP
        }
    }

    pub fn author_font(self) -> f64 {
        if self.compact {
            CARD_AUTHOR_FONT_COMPACT
        } else {
            CARD_AUTHOR_FONT_DESKTOP
        }
    }

    pub fn stats_font(self) -> f64 {
        CARD_STATS_FONT
    }

    pub fn spacing(self) -> f64 {
        CARD_TEXT_SPACING
    }

    pub fn top_margin(self) -> f64 {
        if self.compact {
            CARD_TEXT_MARGIN_COMPACT_TOP
        } else {
            CARD_TEXT_MARGIN_DESKTOP_TOP
        }
    }

    pub fn bottom_margin(self) -> f64 {
        if self.compact {
            CARD_TEXT_MARGIN_COMPACT_BOTTOM
        } else {
            CARD_TEXT_MARGIN_DESKTOP_BOTTOM
        }
    }

    /// 标题定高槽：恒为两行最坏行高，任何标题内容不改变本值。
    pub fn title_slot(self) -> f64 {
        (CARD_TITLE_MAX_LINES * self.title_font() * CARD_LINE_EM).ceil()
    }

    /// 槽内单行行高（×2 恰铺满 title_slot，1 行标题顶对齐余量留白）。
    pub fn title_line_height(self) -> f64 {
        self.title_slot() / CARD_TITLE_MAX_LINES
    }

    pub fn author_slot(self) -> f64 {
        card_line_slot(self.author_font())
    }

    pub fn stats_slot(self) -> f64 {
        card_line_slot(self.stats_font())
    }

    /// UP 主行顶边（相对文本列容器顶）：纯常量，与标题换行无关。
    /// 当前仅测试模块用作布局不变量断言，非测试构建不参与编译。
    #[cfg(test)]
    pub fn author_top(self) -> f64 {
        self.top_margin() + self.title_slot() + self.spacing()
    }

    /// 统计行顶边（紧凑卡无统计行，本值仅桌面态有意义）。
    #[cfg(test)]
    pub fn stats_top(self) -> f64 {
        self.author_top() + self.author_slot() + self.spacing()
    }

    /// 文本列总占高（含上下缘距）；紧凑卡文本列只有标题+UP 两槽。
    #[cfg(test)]
    pub fn total_height(self) -> f64 {
        if self.compact {
            self.author_top() + self.author_slot() + self.bottom_margin()
        } else {
            self.stats_top() + self.stats_slot() + self.bottom_margin()
        }
    }

    /// 文本列可用额度（card_height − cover_height，两态均为常量）。
    #[cfg(test)]
    pub fn allowance(compact: bool) -> f64 {
        if compact {
            MOBILE_CARD_TEXT_ALLOWANCE
        } else {
            DESKTOP_CARD_TEXT_ALLOWANCE
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellChromeMode {
    Desktop,
    Mobile,
}

impl ShellChromeMode {
    pub fn from_width(window_width: f64) -> Self {
        if window_width.is_finite() && window_width > 0.0 && window_width < MOBILE_BREAKPOINT {
            Self::Mobile
        } else {
            Self::Desktop
        }
    }

    pub fn is_mobile(self) -> bool {
        self == Self::Mobile
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentLayoutTier {
    Compact,
    Medium,
    Expanded,
}

impl ContentLayoutTier {
    pub fn from_width(window_width: f64) -> Self {
        if !window_width.is_finite() || window_width <= 0.0 || window_width >= EXPANDED_BREAKPOINT {
            Self::Expanded
        } else if window_width < COMPACT_CONTENT_BREAKPOINT {
            Self::Compact
        } else {
            Self::Medium
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardMetrics {
    pub cols: usize,
    pub card_width: f64,
    pub cover_height: f64,
    pub card_height: f64,
    pub gap: f64,
    pub margin_x: f64,
    pub compact_card: bool,
}

impl Default for CardMetrics {
    fn default() -> Self {
        Self::desktop(DESKTOP_MARGIN_X, 4)
    }
}

impl CardMetrics {
    pub fn desktop(margin_x: f64, cols: usize) -> Self {
        Self {
            cols: cols.clamp(1, 12),
            card_width: DESKTOP_CARD_WIDTH,
            cover_height: DESKTOP_CARD_COVER_HEIGHT,
            card_height: DESKTOP_CARD_HEIGHT,
            gap: DESKTOP_CARD_GAP,
            margin_x,
            compact_card: false,
        }
    }

    /// 依据壳层形态、内容层密度与内容区可用宽度计算精确卡片度量。
    pub fn for_viewport(
        chrome: ShellChromeMode,
        _tier: ContentLayoutTier,
        content_width: f64,
    ) -> Self {
        if !content_width.is_finite() || content_width <= 0.0 {
            return Self::default();
        }
        match chrome {
            ShellChromeMode::Mobile => {
                let margin_x = MOBILE_MARGIN_X;
                let gap = MOBILE_CARD_GAP;
                let inner = (content_width - margin_x * 2.0).max(160.0 * 2.0 + gap);
                let cols = ((inner + gap) / (MOBILE_CARD_MIN_WIDTH + gap))
                    .floor()
                    .clamp(2.0, 3.0) as usize;
                let card_width = ((inner - gap * (cols as f64 - 1.0)) / cols as f64)
                    .floor()
                    .clamp(160.0, 320.0);
                let cover_height = (card_width * 9.0 / 16.0).round();
                let card_height = cover_height + MOBILE_CARD_TEXT_ALLOWANCE;
                Self {
                    cols,
                    card_width,
                    cover_height,
                    card_height,
                    gap,
                    margin_x,
                    compact_card: true,
                }
            }
            ShellChromeMode::Desktop => {
                let margin_x = DESKTOP_MARGIN_X;
                // F5 连续卡片大小曲线：列数由最小卡宽推导，卡宽把剩余宽度
                // 平均分满（整 DIP），封面高精确 16:9 取整 —— 消除：
                // ① 定宽 236 留下的大块行尾空白（窗口越宽留白越多）；
                // ② 封面高与 9/16 比例的偏差导致的 UniformToFill 亚像素裁切
                //    （高 DPI 下表现为封面边缘毛刺/闪烁）。
                let inner = (content_width - margin_x * 2.0).max(DESKTOP_CARD_MIN_WIDTH);
                let cols = ((inner + DESKTOP_CARD_GAP)
                    / (DESKTOP_CARD_MIN_WIDTH + DESKTOP_CARD_GAP))
                    .floor()
                    .clamp(1.0, 12.0) as usize;
                let card_width = ((inner - DESKTOP_CARD_GAP * (cols as f64 - 1.0)) / cols as f64)
                    .floor()
                    .clamp(DESKTOP_CARD_MIN_WIDTH, DESKTOP_CARD_MAX_WIDTH);
                let cover_height = (card_width * 9.0 / 16.0).round();
                let card_height = cover_height + DESKTOP_CARD_TEXT_ALLOWANCE;
                Self {
                    cols,
                    card_width,
                    cover_height,
                    card_height,
                    gap: DESKTOP_CARD_GAP,
                    margin_x,
                    compact_card: false,
                }
            }
        }
    }

    pub fn cell_height(self) -> f64 {
        self.card_height + self.gap
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportLayout {
    pub window_width: f64,
    pub window_height: f64,
    pub chrome: ShellChromeMode,
    pub tier: ContentLayoutTier,
    pub cards: CardMetrics,
}

impl Default for ViewportLayout {
    fn default() -> Self {
        Self::from_window_size(DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT)
    }
}

impl ViewportLayout {
    pub fn from_window_size(width: f64, height: f64) -> Self {
        Self::for_window(width, height, false)
    }

    pub fn for_window(width: f64, height: f64, portrait_mode: bool) -> Self {
        let default_w = if portrait_mode {
            DEFAULT_PORTRAIT_WIDTH
        } else {
            DEFAULT_WINDOW_WIDTH
        };
        let default_h = if portrait_mode {
            DEFAULT_PORTRAIT_HEIGHT
        } else {
            DEFAULT_WINDOW_HEIGHT
        };
        let raw_w = if width.is_finite() && width > 0.0 {
            width.max(MIN_WINDOW_WIDTH)
        } else {
            default_w
        };
        let win_w = if portrait_mode {
            raw_w.min(PORTRAIT_MAX_WIDTH_DIPS)
        } else {
            raw_w
        };
        let win_h = if height.is_finite() && height > 0.0 {
            height.max(MIN_WINDOW_HEIGHT)
        } else {
            default_h
        };
        let chrome = if portrait_mode {
            ShellChromeMode::Mobile
        } else {
            ShellChromeMode::from_width(win_w)
        };
        let tier = ContentLayoutTier::from_width(win_w);
        let side_pad = if chrome.is_mobile() {
            0.0
        } else {
            DESKTOP_NAV_RAIL_WIDTH
        };
        let content_w = (win_w - side_pad).max(320.0);
        let cards = CardMetrics::for_viewport(chrome, tier, content_w);
        Self {
            window_width: win_w,
            window_height: win_h,
            chrome,
            tier,
            cards,
        }
    }

    pub fn initial(portrait_mode: bool) -> Self {
        let (w, h) = if portrait_mode {
            (DEFAULT_PORTRAIT_WIDTH, DEFAULT_PORTRAIT_HEIGHT)
        } else {
            (DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT)
        };
        Self::for_window(w, h, portrait_mode)
    }

    pub fn is_mobile(self) -> bool {
        self.chrome.is_mobile()
    }

    pub fn is_single_column(self) -> bool {
        self.tier != ContentLayoutTier::Expanded
    }

    pub fn uses_whisper_drilldown(self) -> bool {
        self.tier != ContentLayoutTier::Expanded
    }

    pub fn content_width(self) -> f64 {
        let side_pad = if self.is_mobile() {
            0.0
        } else {
            DESKTOP_NAV_RAIL_WIDTH
        };
        (self.window_width - side_pad).max(320.0)
    }

    pub fn content_height(self) -> f64 {
        let chrome_h = if self.is_mobile() { 112.0 } else { 136.0 };
        (self.window_height - chrome_h).max(320.0)
    }

    pub fn page_margin_x(self) -> f64 {
        self.cards.margin_x
    }

    pub fn page_title_size(self) -> f64 {
        match self.tier {
            ContentLayoutTier::Compact => 22.0,
            ContentLayoutTier::Medium => 26.0,
            ContentLayoutTier::Expanded => 28.0,
        }
    }

    /// 播放页视频槽在单列模式下按 16:9 动态计算高度（含上下界防小屏侵占评论空间）。
    pub fn player_video_height(self) -> f64 {
        let avail_w = (self.content_width() - self.page_margin_x() * 2.0).max(240.0);
        let max_h = match self.tier {
            ContentLayoutTier::Compact => 260.0,
            ContentLayoutTier::Medium | ContentLayoutTier::Expanded => 360.0,
        };
        (avail_w * 9.0 / 16.0).round().clamp(180.0, max_h)
    }

    /// TitleBar 内嵌搜索框仅在 Expanded 展开，Medium 走浮窗触发按钮，Mobile 走底栏 Tab。
    pub fn shows_inline_title_search(self) -> bool {
        self.tier == ContentLayoutTier::Expanded
    }

    pub fn shows_title_search_button(self) -> bool {
        self.tier == ContentLayoutTier::Medium
    }

    /// 结构性重建判定(#4):仅列数 / 卡态变化。卡宽已不进行 key,宽度收口走
    /// `metrics_refresh_needed` 的轻量原地刷新(同 key 复用容器,diff 改属性)。
    pub fn should_rebuild_grid(self, prev: CardMetrics) -> bool {
        self.cards.cols != prev.cols || self.cards.compact_card != prev.compact_card
    }

    /// 卡宽收口变化 ≥ 2 DIP:去抖后需把新度量(行高/封面高/骨架条宽)下发一次
    /// 到稳定 key 的行上;列宽本身拖拽期间已由 Grid star 列连续跟随,无需等待。
    pub fn metrics_refresh_needed(self, prev: CardMetrics) -> bool {
        (self.cards.card_width - prev.card_width).abs() >= 2.0
    }
}

/// 标题栏/手柄返回键是否可用（私信单页钻取时未有历史路由也可退回会话列表）。
pub fn can_navigate_back(
    layout: ViewportLayout,
    in_account_route: bool,
    history_len: usize,
    whisper_detail_open: bool,
) -> bool {
    history_len > 0 || (layout.uses_whisper_drilldown() && in_account_route && whisper_detail_open)
}

/// 夹紧历史窗口边界，避免恢复过小外框导致首帧越界闪烁。
pub fn clamp_window_bounds(mut bounds: WindowBounds) -> WindowBounds {
    bounds.width = bounds.width.max(MIN_WINDOW_WIDTH as i32);
    bounds.height = bounds.height.max(MIN_WINDOW_HEIGHT as i32);
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints_separate_mobile_medium_and_expanded() {
        let m390 = ViewportLayout::from_window_size(390.0, 640.0);
        assert_eq!(m390.chrome, ShellChromeMode::Mobile);
        assert_eq!(m390.tier, ContentLayoutTier::Compact);
        assert_eq!(m390.cards.cols, 2);
        assert!(m390.cards.compact_card);
        assert_eq!(m390.cards.card_width, 179.0);
        assert_eq!(m390.cards.cover_height, 101.0);
        assert_eq!(m390.cards.card_height, 185.0);
        assert_eq!(m390.player_video_height(), 206.0);

        let m599 = ViewportLayout::from_window_size(599.0, 800.0);
        assert!(m599.is_mobile());
        assert_eq!(m599.tier, ContentLayoutTier::Compact);
        assert_eq!(m599.cards.cols, 2);
        assert_eq!(m599.cards.card_width, 283.0);
        assert!(!m599.shows_title_search_button());

        // 600..859：Medium 内容档起自动切入 Mobile 3 列（186..273px），保留顶栏右上悬浮搜索按钮
        let m600 = ViewportLayout::from_window_size(600.0, 800.0);
        assert_eq!(m600.chrome, ShellChromeMode::Mobile);
        assert_eq!(m600.tier, ContentLayoutTier::Medium);
        assert!(m600.cards.compact_card);
        assert_eq!(m600.cards.cols, 3);
        assert_eq!(m600.cards.card_width, 186.0);
        assert_eq!(m600.cards.cover_height, 105.0);
        assert!(m600.is_single_column());
        assert!(m600.shows_title_search_button());
        assert!(!m600.shows_inline_title_search());

        let m640 = ViewportLayout::from_window_size(640.0, 900.0);
        assert_eq!(m640.chrome, ShellChromeMode::Mobile);
        assert_eq!(m640.tier, ContentLayoutTier::Medium);
        assert_eq!(m640.cards.cols, 3);
        assert_eq!(m640.cards.card_width, 200.0);
        assert_eq!(m640.cards.cover_height, 113.0);
        assert_eq!(m640.player_video_height(), 347.0);
        assert!(m640.is_single_column());

        let m859 = ViewportLayout::from_window_size(859.0, 800.0);
        assert!(m859.is_mobile());
        assert_eq!(m859.cards.cols, 3);
        assert_eq!(m859.cards.card_width, 273.0);
        assert_eq!(m859.cards.cover_height, 154.0);

        // 860px 起桌面端正好排下 3 列标准卡（56+72+236*3+24），同步切回 Desktop Expanded
        let d860 = ViewportLayout::from_window_size(860.0, 800.0);
        assert_eq!(d860.chrome, ShellChromeMode::Desktop);
        assert_eq!(d860.tier, ContentLayoutTier::Expanded);
        assert!(!d860.cards.compact_card);
        assert_eq!(d860.cards.cols, 3);
        // F5 曲线:860 DIP(内容 804/内宽 732)3 列 → 卡宽恰为历史基线 236,封面 133。
        assert_eq!(d860.cards.card_width, 236.0);
        assert_eq!(d860.cards.cover_height, 133.0);
        assert!(!d860.is_single_column());
        assert!(d860.shows_inline_title_search());

        let d1280 = ViewportLayout::from_window_size(1280.0, 800.0);
        // F5 曲线:1224 内宽 5 列 → 卡宽 235(≈历史基线 236,行尾无空白)。
        assert_eq!(d1280.cards.cols, 5);
        assert_eq!(d1280.cards.card_width, 220.0);
        assert_eq!(d1280.cards.cover_height, 124.0);
        assert_eq!(d1280.cards.card_height, 124.0 + DESKTOP_CARD_TEXT_ALLOWANCE);
    }

    #[test]
    fn mobile_card_text_budget_fits_without_clipping() {
        // 不变量接线后预算以 CardTextLayout 定高槽推导：紧凑卡文本列总高
        // 必须放进 MOBILE_CARD_TEXT_ALLOWANCE，且各槽容得下最坏字体行高。
        let m = ViewportLayout::from_window_size(390.0, 640.0).cards;
        let t = CardTextLayout { compact: true };
        assert_eq!(m.card_height - m.cover_height, CardTextLayout::allowance(true));
        assert!(
            t.total_height() <= CardTextLayout::allowance(true),
            "紧凑卡文本预留({})应容下定高文本列({})",
            CardTextLayout::allowance(true),
            t.total_height()
        );
        assert!(2.0 * t.title_font() * 1.48 <= t.title_slot(), "标题槽应容两行最坏行高");
        assert!(t.author_font() * 1.48 <= t.author_slot(), "UP 槽应容一行最坏行高");
    }

    /// #10 首页/热门/搜索共用卡文本列**严格不变量**：
    /// ① 标题 1 行/2 行/超长都不得改变 UP 主行与统计行的顶边（顶边是纯算术
    ///    常量，标题内容没有输入通道）；
    /// ② 桌面态文本列总高必须放进 DESKTOP_CARD_TEXT_ALLOWANCE（不许靠加大
    ///    卡高来兜底）；
    /// ③ 标题字号已调小一档（14→13 / 12.5→12），槽值随之锁死防回退。
    #[test]
    fn card_text_rows_are_title_length_independent() {
        let desktop = CardTextLayout { compact: false };
        // 槽值锚点：改字号/行数/缘距必须同步本测试（故意写死以拦截回归）。
        assert_eq!(desktop.title_font(), 13.0, "桌面标题应为调小一档后的 13px");
        assert_eq!(desktop.title_slot(), 39.0);
        assert_eq!(desktop.title_line_height() * 2.0, 39.0, "两行须恰好铺满槽");
        assert_eq!(desktop.author_slot(), 18.0);
        assert_eq!(desktop.stats_slot(), 17.0);
        assert_eq!(desktop.author_top(), 10.0 + 39.0 + 4.0);
        assert_eq!(desktop.stats_top(), desktop.author_top() + 18.0 + 4.0);
        assert!(
            desktop.total_height() <= CardTextLayout::allowance(false),
            "桌面文本列总高({})应容下({})",
            desktop.total_height(),
            CardTextLayout::allowance(false)
        );

        let compact = CardTextLayout { compact: true };
        assert_eq!(compact.title_font(), 13.5, "紧凑标题应为 PC 视距友好的 13.5px");
        assert_eq!(compact.title_slot(), 40.0);
        assert_eq!(compact.author_slot(), 18.0);
        assert_eq!(compact.author_top(), 8.0 + 40.0 + 4.0);
        assert!(compact.total_height() <= CardTextLayout::allowance(true));

        // 不变量的「构造性」：CardTextLayout 上不存在标题输入通道；再跨整条
        // 桌面卡宽曲线验证信息行顶边与文本列总高逐点一致（卡宽同样不参与）。
        for width in [860.0, 1000.0, 1280.0, 1920.0, 3840.0] {
            let m = ViewportLayout::from_window_size(width, 900.0).cards;
            assert!(!m.compact_card, "{width} 应为桌面卡");
            let t = CardTextLayout { compact: false };
            assert_eq!(t.author_top(), desktop.author_top(), "{width}: UP 行顶边漂移");
            assert_eq!(t.stats_top(), desktop.stats_top(), "{width}: 统计行顶边漂移");
            assert!(t.total_height() <= m.card_height - m.cover_height);
        }
    }

    #[test]
    fn grid_rebuild_debounce_and_back_navigation_predicates() {
        let base = ViewportLayout::from_window_size(390.0, 760.0);
        let tiny = ViewportLayout::from_window_size(391.0, 760.0);
        assert!(!tiny.should_rebuild_grid(base.cards));

        let wider = ViewportLayout::from_window_size(396.0, 760.0);
        // #4:纯卡宽变化不再触发结构性重建,只需 metrics 收口轻量刷新一次。
        assert!(!wider.should_rebuild_grid(base.cards));
        assert!(wider.metrics_refresh_needed(base.cards));
        let nudge = ViewportLayout::from_window_size(391.0, 760.0);
        assert!(!nudge.metrics_refresh_needed(base.cards));
        let cols_change = ViewportLayout::from_window_size(880.0, 760.0);
        assert_ne!(cols_change.cards.cols, base.cards.cols);
        assert!(cols_change.should_rebuild_grid(base.cards));

        assert!(!can_navigate_back(base, true, 0, false));
        assert!(can_navigate_back(base, true, 0, true));
        assert!(!can_navigate_back(base, false, 0, true));
        assert!(can_navigate_back(
            ViewportLayout::from_window_size(1280.0, 800.0),
            true,
            1,
            false
        ));
    }

    #[test]
    fn window_bounds_clamp_enforces_minimum_footprint() {
        let clamped = clamp_window_bounds(WindowBounds {
            x: 10,
            y: 20,
            width: 240,
            height: 320,
            maximized: false,
        });
        assert_eq!(clamped.width, 390);
        assert_eq!(clamped.height, 640);
    }

    /// F5:曲线在整 DIP 上取值且封面严格 16:9(±0.5 DIP),任何内容宽 /
    /// DPI 缩放下封面不做 UniformToFill 亚像素裁切。
    #[test]
    fn desktop_card_curve_stays_integer_aligned_and_16_9() {
        // 仅桌面形态(≥860 DIP)走曲线;Mobile 曲线已有独立断言。
        for width in [860.0, 1000.0, 1280.0, 1600.0, 1920.0, 2560.0, 3840.0] {
            let m = ViewportLayout::from_window_size(width, 900.0).cards;
            assert!(
                m.card_width.fract() == 0.0,
                "{width}: 卡宽非整 DIP {}",
                m.card_width
            );
            assert!(m.cover_height.fract() == 0.0, "{width}: 封面非整 DIP");
            let ratio = m.cover_height / m.card_width;
            assert!(
                (ratio - 9.0 / 16.0).abs() <= 0.5 / m.card_width,
                "{width}: 封面比例偏离 16:9 超过 0.5 DIP({ratio})"
            );
            assert!(
                m.card_width >= DESKTOP_CARD_MIN_WIDTH && m.card_width <= DESKTOP_CARD_MAX_WIDTH,
                "{width}: 卡宽越界 {}",
                m.card_width
            );
            // 行内总宽恰好不超内容区(不留 >1 卡距的空白)。
            let inner = width - 36.0;
            assert!(
                m.cols as f64 * m.card_width + (m.cols as f64 - 1.0) * m.gap <= inner + 0.5,
                "{width}: 行宽溢出"
            );
            assert!(
                inner - (m.cols as f64 * m.card_width + (m.cols as f64 - 1.0) * m.gap)
                    < m.card_width + m.gap,
                "{width}: 行尾空白超过一张卡(定宽回归)"
            );
        }
    }

    #[test]
    fn portrait_mode_strictly_clamps_to_mobile_band_across_all_widths() {
        let init = ViewportLayout::initial(true);
        assert_eq!(init.window_width, DEFAULT_PORTRAIT_WIDTH);
        assert_eq!(init.window_height, DEFAULT_PORTRAIT_HEIGHT);
        assert_eq!(init.chrome, ShellChromeMode::Mobile);
        assert_eq!(init.tier, ContentLayoutTier::Compact);
        assert_eq!(init.cards.cols, 2);
        assert_eq!(init.cards.card_width, 224.0);
        assert_eq!(init.cards.cover_height, 126.0);
        assert!(init.cards.compact_card);
        assert!(init.is_mobile());
        assert!(init.is_single_column());
        assert!(!init.shows_inline_title_search());

        for width in [0.0, f64::NAN, 320.0, 390.0, 480.0, 599.0, 600.0, 640.0, 859.0, 860.0, 1280.0, 1920.0, 3840.0] {
            let l = ViewportLayout::for_window(width, 1080.0, true);
            assert!(
                l.window_width >= MIN_WINDOW_WIDTH && l.window_width <= PORTRAIT_MAX_WIDTH_DIPS,
                "width={width}: portrait window_width 越界 {}",
                l.window_width
            );
            assert_eq!(l.chrome, ShellChromeMode::Mobile, "width={width}: 必须恒为 Mobile");
            assert_ne!(l.tier, ContentLayoutTier::Expanded, "width={width}: 严禁进入 Expanded");
            let expected_cols = if l.tier == ContentLayoutTier::Compact { 2 } else { 3 };
            assert_eq!(l.cards.cols, expected_cols, "width={width}: 竖屏列数应随档位 2/3 自适应");
            assert!(l.cards.compact_card, "width={width}: 竖屏必须恒为紧凑卡");
            assert!(!l.shows_inline_title_search(), "width={width}: 竖屏严禁出现桌面内嵌搜索框");
        }
    }

    #[test]
    fn mobile_and_portrait_dpi_matrix_preserves_bounds_and_16_9() {
        let scales = [1.0_f64, 1.25, 1.5, 1.75, 2.0, 2.25];
        for scale in scales {
            // 任意 DPI 下向下取整到物理像素后再反推 DIP，必须严格小于 MOBILE_BREAKPOINT。
            let max_client_px = (PORTRAIT_MAX_WIDTH_DIPS * scale).floor();
            assert!(
                max_client_px / scale < MOBILE_BREAKPOINT,
                "scale={scale}: 竖屏上限物理像素越界"
            );
            let min_client_px = (MIN_WINDOW_WIDTH * scale).ceil();
            assert!(
                min_client_px / scale >= MIN_WINDOW_WIDTH,
                "scale={scale}: 竖屏下限物理像素越界"
            );

            for width in (390..=859).step_by(17).chain([390, 480, 599, 600, 750, 859]) {
                let layout = ViewportLayout::for_window(f64::from(width), 860.0, true);
                let m = layout.cards;
                assert_eq!(m.card_width.fract(), 0.0);
                assert_eq!(m.cover_height.fract(), 0.0);
                assert!(
                    (m.cover_height - m.card_width * 9.0 / 16.0).abs() <= 0.5 + 1e-9,
                    "width={width}: 封面高度偏离 16:9 超过 0.5 DIP ({}/{})",
                    m.cover_height,
                    m.card_width
                );
                let used = m.cols as f64 * m.card_width + (m.cols as f64 - 1.0) * m.gap;
                let inner = layout.content_width() - 2.0 * m.margin_x;
                assert!(used <= inner + 0.5, "width={width}: 行宽溢出");
                assert!(
                    CardTextLayout { compact: true }.total_height() <= m.card_height - m.cover_height,
                    "width={width}: 文本槽溢出"
                );
            }
        }
    }
}
