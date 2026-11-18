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
pub const MOBILE_CARD_TEXT_ALLOWANCE: f64 = 77.0;

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
                let cols = 2usize;
                let margin_x = MOBILE_MARGIN_X;
                let gap = MOBILE_CARD_GAP;
                let card_width = ((content_width - margin_x * 2.0 - gap) / cols as f64)
                    .floor()
                    .clamp(160.0, 414.0);
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
        let win_w = if width.is_finite() && width > 0.0 {
            width.max(MIN_WINDOW_WIDTH)
        } else {
            DEFAULT_WINDOW_WIDTH
        };
        let win_h = if height.is_finite() && height > 0.0 {
            height.max(MIN_WINDOW_HEIGHT)
        } else {
            DEFAULT_WINDOW_HEIGHT
        };
        let chrome = ShellChromeMode::from_width(win_w);
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

    /// 窗口拖拽防抖：列数变化、卡态切换或单卡宽变化 ≥ 2 DIP 时才重建 `VirtualSource`。
    pub fn should_rebuild_grid(self, prev: CardMetrics) -> bool {
        self.cards.cols != prev.cols
            || self.cards.compact_card != prev.compact_card
            || (self.cards.card_width - prev.card_width).abs() >= 2.0
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
        assert_eq!(m390.cards.card_height, 178.0);
        assert_eq!(m390.player_video_height(), 206.0);

        let m599 = ViewportLayout::from_window_size(599.0, 800.0);
        assert!(m599.is_mobile());
        assert_eq!(m599.tier, ContentLayoutTier::Compact);
        assert_eq!(m599.cards.cols, 2);
        assert_eq!(m599.cards.card_width, 283.0);
        assert!(!m599.shows_title_search_button());

        // 600..859：同样处于 2 列区间，直接切为等宽铺满 Mobile 壳层，并保留顶栏右上悬浮搜索按钮
        let m600 = ViewportLayout::from_window_size(600.0, 800.0);
        assert_eq!(m600.chrome, ShellChromeMode::Mobile);
        assert_eq!(m600.tier, ContentLayoutTier::Medium);
        assert!(m600.cards.compact_card);
        assert_eq!(m600.cards.cols, 2);
        assert_eq!(m600.cards.card_width, 284.0);
        assert!(m600.is_single_column());
        assert!(m600.shows_title_search_button());
        assert!(!m600.shows_inline_title_search());

        let m640 = ViewportLayout::from_window_size(640.0, 900.0);
        assert_eq!(m640.chrome, ShellChromeMode::Mobile);
        assert_eq!(m640.tier, ContentLayoutTier::Medium);
        assert_eq!(m640.cards.cols, 2);
        assert_eq!(m640.cards.card_width, 304.0);
        assert_eq!(m640.player_video_height(), 347.0);
        assert!(m640.is_single_column());

        let m859 = ViewportLayout::from_window_size(859.0, 800.0);
        assert!(m859.is_mobile());
        assert_eq!(m859.cards.cols, 2);
        assert_eq!(m859.cards.card_width, 413.0);

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
        // Noto Sans SC 常见行高约 1.48em；标题 2 行(12.5px) + UP 1 行(11px) + 内外边距与间距
        let m = ViewportLayout::from_window_size(390.0, 640.0).cards;
        let line_em = 1.48;
        let needed_text_h = (2.0 * 12.5 * line_em) + (11.0 * line_em) + 4.0 + 14.0 + 2.0;
        assert!(
            m.card_height - m.cover_height >= needed_text_h,
            "紧凑卡文本预留({})应容下标题+UP({})",
            m.card_height - m.cover_height,
            needed_text_h
        );
    }

    #[test]
    fn grid_rebuild_debounce_and_back_navigation_predicates() {
        let base = ViewportLayout::from_window_size(390.0, 760.0);
        let tiny = ViewportLayout::from_window_size(391.0, 760.0);
        assert!(!tiny.should_rebuild_grid(base.cards));

        let wider = ViewportLayout::from_window_size(396.0, 760.0);
        assert!(wider.should_rebuild_grid(base.cards));

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
}
