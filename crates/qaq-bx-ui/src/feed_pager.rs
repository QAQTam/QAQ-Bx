//! 首页推荐流视口自适应分页与封面窗口模型。
//!
//! 结合固定卡片网格几何参数与 `ItemsRepeater` 当前实际挂载行（`HomeRowActive`）推导：
//! 1. 当前窗口每行列数 `cols` 与整屏可见容量；
//! 2. 对齐整行与 B 站单次 `ps<=30` 上限的逻辑页规模（默认预热 ~48–72 条）；
//! 3. 按当前锚点行上下缓冲计算离屏封面清理窗口（`visible_cover_window_by_row`）。

use crate::layout::CardMetrics;
#[cfg(test)]
use crate::layout::{ContentLayoutTier, ShellChromeMode};
#[cfg(test)]
use crate::theme;

#[cfg(test)]
const CELL_HEIGHT: f64 = theme::CARD_HEIGHT + theme::CARD_GAP;

const DEFAULT_VIS_ROWS: usize = 3;
const TARGET_SCREENS: f64 = 2.0;
const MIN_LOGICAL_PAGE: usize = 48;
const MIN_MOBILE_LOGICAL_PAGE: usize = 24;
const MAX_LOGICAL_PAGE: usize = 72;
/// 预取提前时间窗口（覆盖 1~2 轮 rcmd RTT + 首行封面启动开销）。
#[cfg(test)]
const LEAD_TIME_SECS: f64 = 0.8;

/// 依据桌面视口宽度计算网格每行精确列数。
#[cfg(test)]
pub fn columns(viewport_width: f64) -> usize {
    CardMetrics::for_viewport(
        ShellChromeMode::Desktop,
        ContentLayoutTier::Expanded,
        viewport_width,
    )
    .cols
}

/// 依据卡片度量与视口高度计算整屏可见行数。
pub fn visible_rows_for(metrics: CardMetrics, viewport_height: f64) -> usize {
    if !viewport_height.is_finite() || viewport_height <= 0.0 {
        return DEFAULT_VIS_ROWS;
    }
    let cell_h = metrics.cell_height().max(1.0);
    (viewport_height / cell_h).ceil().clamp(1.0, 12.0) as usize
}

/// 依据桌面默认卡片高度计算整屏可见行数。
#[cfg(test)]
pub fn visible_rows(viewport_height: f64) -> usize {
    visible_rows_for(CardMetrics::default(), viewport_height)
}

/// 结合卡片度量与视口高度计算整行对齐的逻辑页条数。
pub fn logical_page_target_for(metrics: CardMetrics, viewport_height: f64) -> usize {
    let cols = metrics.cols.max(1);
    let vis_rows = visible_rows_for(metrics, viewport_height);
    let desired = ((cols * vis_rows) as f64 * TARGET_SCREENS).ceil() as usize;
    let min_page = if metrics.compact_card {
        MIN_MOBILE_LOGICAL_PAGE
    } else {
        MIN_LOGICAL_PAGE
    };
    let clamped = desired.clamp(min_page, MAX_LOGICAL_PAGE);
    align_up_to_cols(clamped, cols).min(MAX_LOGICAL_PAGE.max(cols))
}

/// 计算与列数整行对齐的桌面逻辑页条数（1280x800→48，1600x900→50，1920x1080→56，2560x1440→72）。
#[cfg(test)]
pub fn logical_page_target(viewport_width: f64, viewport_height: f64) -> usize {
    let metrics = CardMetrics::for_viewport(
        ShellChromeMode::Desktop,
        ContentLayoutTier::Expanded,
        viewport_width,
    );
    logical_page_target_for(metrics, viewport_height)
}

/// 结合当前列表尾行缺口微调下一批槽位数，尽量凑成完整矩形行。
pub fn aligned_next_batch_for(
    current_slots: usize,
    metrics: CardMetrics,
    viewport_height: f64,
) -> usize {
    let cols = metrics.cols.max(1);
    let base = logical_page_target_for(metrics, viewport_height);
    let rem = current_slots % cols;
    if rem == 0 {
        base
    } else {
        let fill = cols - rem;
        (base + fill).min(MAX_LOGICAL_PAGE + cols)
    }
}

/// 计算当前视窗底部为止已经曝光到了第几条视频（`0..=total_slots`）。
#[cfg(test)]
pub fn seen_bottom_count(
    offset_y: f64,
    viewport_width: f64,
    viewport_height: f64,
    total_slots: usize,
) -> usize {
    if total_slots == 0 {
        return 0;
    }
    let cols = columns(viewport_width);
    let vis_h = if viewport_height.is_finite() && viewport_height > 0.0 {
        viewport_height
    } else {
        DEFAULT_VIS_ROWS as f64 * CELL_HEIGHT
    };
    let bottom_y = (offset_y.max(0.0) + vis_h).max(CELL_HEIGHT);
    let bottom_row = (bottom_y / CELL_HEIGHT).ceil().max(1.0) as usize;
    bottom_row.saturating_mul(cols).min(total_slots)
}

/// 离屏封面纹理裁剪窗口：仅保留距当前视口上下各 `buffer_screens` 屏范围内的卡片引用。
#[cfg(test)]
pub fn visible_cover_window(
    offset_y: f64,
    viewport_width: f64,
    viewport_height: f64,
    total_slots: usize,
    buffer_screens: usize,
) -> std::ops::Range<usize> {
    if total_slots == 0 {
        return 0..0;
    }
    let cols = columns(viewport_width);
    let vis_rows = visible_rows(viewport_height);
    let margin_rows = vis_rows.saturating_mul(buffer_screens.max(1));
    let top_row = (offset_y.max(0.0) / CELL_HEIGHT).floor() as usize;
    let bottom_row = top_row.saturating_add(vis_rows);
    let start_idx = top_row.saturating_sub(margin_rows).saturating_mul(cols);
    let end_idx = bottom_row
        .saturating_add(margin_rows)
        .saturating_mul(cols)
        .min(total_slots);
    start_idx.min(end_idx)..end_idx
}

/// 半页触发线 + 速度提前量：返回当视窗下方剩余条数 `<= 阈值` 时应拉取下一逻辑页。
#[cfg(test)]
pub fn prefetch_threshold(page_target: usize, cols: usize, speed_items_per_sec: f64) -> usize {
    let half_page = (page_target / 2).max(cols * 2);
    let lead_items = if speed_items_per_sec.is_finite() && speed_items_per_sec > 0.0 {
        (speed_items_per_sec * LEAD_TIME_SECS).ceil() as usize
    } else {
        0
    };
    half_page.max(lead_items)
}

#[cfg(test)]
pub fn should_prefetch(
    offset_y: f64,
    viewport_width: f64,
    viewport_height: f64,
    total_slots: usize,
    speed_items_per_sec: f64,
) -> bool {
    let cols = columns(viewport_width);
    let target = logical_page_target(viewport_width, viewport_height);
    let seen = seen_bottom_count(offset_y, viewport_width, viewport_height, total_slots);
    let ahead = total_slots.saturating_sub(seen);
    ahead <= prefetch_threshold(target, cols, speed_items_per_sec)
}

fn align_up_to_cols(n: usize, cols: usize) -> usize {
    let cols = cols.max(1);
    n.div_ceil(cols) * cols
}

/// 按当前虚拟化已实现行号推导离屏封面保留区间（不依赖容易被布局重排抖动的 ScrollViewer offset）。
pub fn visible_cover_window_by_row(
    anchor_row: usize,
    metrics: CardMetrics,
    viewport_height: f64,
    total_slots: usize,
    buffer_screens: usize,
) -> std::ops::Range<usize> {
    if total_slots == 0 {
        return 0..0;
    }
    let cols = metrics.cols.max(1);
    let vis_rows = visible_rows_for(metrics, viewport_height);
    // WinUI ItemsRepeater 在当前行上下各预热约 1-2 屏；上下各留 buffer + 1 屏缓冲保证零闪烁。
    let margin_rows = vis_rows.saturating_mul(buffer_screens.saturating_add(1).max(2));
    let start_idx = anchor_row.saturating_sub(margin_rows).saturating_mul(cols);
    let end_idx = anchor_row
        .saturating_add(vis_rows)
        .saturating_add(margin_rows)
        .saturating_mul(cols)
        .min(total_slots);
    start_idx.min(end_idx)..end_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_adaptive_cols_and_page_targets() {
        // 1280x800（扣除 nav rail 后 ~1232 宽）→ F5 曲线 5 列，预热 50 条（整行对齐）
        assert_eq!(columns(1232.0), 5);
        assert_eq!(logical_page_target(1232.0, 750.0), 50);

        // 1600x900 近似内容宽 1550 → F5 曲线 7 列
        let c1600 = columns(1550.0);
        assert_eq!(c1600, 7);
        assert_eq!(logical_page_target(1550.0, 860.0) % c1600, 0);

        // 1920x1080 全屏（~1870 内容宽）→ F5 曲线 8 列；卡高 227 → vis_rows=5，
        // 期望 80 条但被 72 条上限钳制。
        assert_eq!(columns(1870.0), 8);
        assert_eq!(logical_page_target(1870.0, 1000.0), 72);

        // 2560x1440 全屏（~2510 内容宽）→ F5 曲线 11 列，页规模钳 72 条上限
        assert_eq!(columns(2510.0), 11);
        assert_eq!(logical_page_target(2510.0, 1360.0), 72);
    }

    #[test]
    fn seen_bottom_and_half_page_prefetch_trigger() {
        // 1280x800: F5 曲线 cols=5, vis_h=750 → bottom_row=3 → 首屏底部已见 15 条
        assert_eq!(seen_bottom_count(0.0, 1232.0, 750.0, 48), 15);
        // 剩 33 条 > 半页 24 条 → 不触发
        assert!(!should_prefetch(0.0, 1232.0, 750.0, 48, 0.0));

        // 向下滚动 3 行：底部到第 6 行 → 已见 30 条，剩 18 条 ≤ 半页阈值 → 触发下一页
        assert_eq!(seen_bottom_count(756.0, 1232.0, 750.0, 48), 30);
        assert!(should_prefetch(756.0, 1232.0, 750.0, 48, 0.0));
    }

    #[test]
    fn fast_scroll_velocity_expands_prefetch_lead_threshold() {
        let slow = prefetch_threshold(48, 4, 0.0);
        let fast = prefetch_threshold(48, 4, 80.0); // 80 条/秒 * 0.8s = 64 > 24
        assert_eq!(slow, 24);
        assert_eq!(fast, 64);
    }

    #[test]
    fn visible_cover_window_tracks_buffer_screens_around_viewport() {
        assert_eq!(visible_cover_window(0.0, 1232.0, 750.0, 0, 4), 0..0);
        // F5 曲线 cols=5, vis_rows=3, buffer=2 → margin=6 rows; offset=2520 (row 10) → [4..19)*5 = 20..95
        let win = visible_cover_window(2520.0, 1232.0, 750.0, 120, 2);
        assert_eq!(win, 20..95);

        let by_row = visible_cover_window_by_row(15, CardMetrics::default(), 750.0, 160, 2);
        assert_eq!(by_row, 24..108);
    }

    #[test]
    fn mobile_metrics_drive_two_col_targets_and_odd_slot_alignment() {
        let m =
            CardMetrics::for_viewport(ShellChromeMode::Mobile, ContentLayoutTier::Compact, 390.0);
        assert_eq!(m.cols, 2);
        assert!(m.compact_card);
        assert_eq!(visible_rows_for(m, 528.0), 3);
        let target = logical_page_target_for(m, 528.0);
        assert_eq!(target, 24);
        assert_eq!(target % 2, 0);
        assert_eq!(aligned_next_batch_for(25, m, 528.0), 25);
    }
}
