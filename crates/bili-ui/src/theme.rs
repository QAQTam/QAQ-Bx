//! Fluent 设计 token 与通用小件（`docs/ui-design.md` §0/§2/§7）。
//!
//! 颜色一律走 `ThemeBrush`（跟随系统主题与强调色），此处只收敛布局数值。

use windows_reactor::*;

/// 视频卡片基准宽度（一屏 4~5 列，8GB 机器友好）。
pub const CARD_WIDTH: f64 = 236.0;
/// 卡片间距。
pub const CARD_GAP: f64 = 12.0;
/// 封面高度（16:9）。
pub const CARD_COVER_HEIGHT: f64 = 133.0;
/// 卡片总高（封面 + 文本区）。
pub const CARD_HEIGHT: f64 = 240.0;

/// 通用页头：大标题 + 副标题（对照 ZuneMusic `PageHeader`）。
pub fn page_header(title: &str, subtitle: &str) -> View {
    StackPanel::new()
        .spacing(4.0)
        .children((
            TextBlock::new()
                .text(title)
                .font_size(28.0)
                .font_weight(FontWeight::BOLD),
            TextBlock::new().text(subtitle).opacity(0.6),
        ))
        .into()
}

/// `12:34` / `1:02:03`。
pub fn fmt_duration(secs: u32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// `1.2万` / `3.4亿`。
pub fn fmt_count(count: u64) -> String {
    if count >= 100_000_000 {
        trim_zero(format!("{:.1}亿", count as f64 / 1e8))
    } else if count >= 10_000 {
        trim_zero(format!("{:.1}万", count as f64 / 1e4))
    } else {
        count.to_string()
    }
}

fn trim_zero(value: String) -> String {
    value.replace(".0亿", "亿").replace(".0万", "万")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration() {
        assert_eq!(fmt_duration(0), "00:00");
        assert_eq!(fmt_duration(754), "12:34");
        assert_eq!(fmt_duration(3723), "1:02:03");
    }

    #[test]
    fn count() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(12_000), "1.2万");
        assert_eq!(fmt_count(10_000), "1万");
        assert_eq!(fmt_count(340_000_000), "3.4亿");
    }
}
