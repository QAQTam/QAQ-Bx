//! 列表页滚动自动续页：纯判定逻辑（视图层触发 + shell 门控共用，零 IO）。
//!
//! 视图层：`ScrollViewer::on_view_changed` 上报滚动几何 → `near_bottom`
//! 判定接近底部即投递 `ListAutoMore(target)`；shell 侧经 `should_auto_page`
//! 门控（未加载过首页/在飞/终态/错误态一律不自动发请求，手动胶囊仍可用）。

use windows_reactor::ScrollEventInfo;

/// 距底多少像素内触发预取（约一行卡片高度,滚动到底前无感续页）。
pub const AUTO_PAGE_LEAD_PX: f64 = 600.0;

/// 自动续页目标页（与 shell 五张可翻页列表一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoPage {
    History,
    WatchLater,
    Favorites,
    Popular,
    Live,
}

/// 滚动几何是否接近底部（extent/viewport 未就绪时一律 false,防布局初期误触发）。
pub fn near_bottom(ev: &ScrollEventInfo, lead_px: f64) -> bool {
    ev.extent_height > 0.0
        && ev.viewport_height > 0.0
        && ev.offset_y + ev.viewport_height >= ev.extent_height - lead_px
}

/// shell 门控：仅「已成功加载过首页 + 无在飞请求 + 未到终态 + 无错误」才自动续页。
/// 错误态留给手动「重试」胶囊,避免滚动事件反复打失败请求。
pub fn should_auto_page(
    loaded_once: bool,
    loading: bool,
    reached_end: bool,
    has_error: bool,
) -> bool {
    loaded_once && !loading && !reached_end && !has_error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(offset_y: f64, viewport: f64, extent: f64) -> ScrollEventInfo {
        ScrollEventInfo {
            offset_x: 0.0,
            offset_y,
            viewport_width: 1280.0,
            viewport_height: viewport,
            extent_width: 1280.0,
            extent_height: extent,
            is_intermediate: false,
        }
    }

    #[test]
    fn near_bottom_boundaries() {
        // 距底 601px:不触发。
        assert!(!near_bottom(&ev(0.0, 800.0, 2001.0), 600.0));
        // 恰好进入提前量:触发。
        assert!(near_bottom(&ev(601.0, 800.0, 2001.0), 600.0));
        // 已到底:触发。
        assert!(near_bottom(&ev(1201.0, 800.0, 2001.0), 600.0));
        // 内容不足一屏但还有下一页(宽屏少行):offset=0 也触发,自动填满屏。
        assert!(near_bottom(&ev(0.0, 800.0, 500.0), 600.0));
    }

    #[test]
    fn near_bottom_rejects_unready_geometry() {
        assert!(!near_bottom(&ev(0.0, 0.0, 2000.0), 600.0), "viewport 未就绪");
        assert!(!near_bottom(&ev(0.0, 800.0, 0.0), 600.0), "extent 未就绪");
    }

    #[test]
    fn auto_page_gate_truth_table() {
        assert!(should_auto_page(true, false, false, false));
        assert!(!should_auto_page(false, false, false, false), "首页未成功不自动");
        assert!(!should_auto_page(true, true, false, false), "在飞不重复发");
        assert!(!should_auto_page(true, false, true, false), "终态不再发");
        assert!(!should_auto_page(true, false, false, true), "错误态留给手动重试");
    }
}
