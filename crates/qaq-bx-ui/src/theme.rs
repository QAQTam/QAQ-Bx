//! Fluent 设计 token 与通用小件（`docs/ui-design.md` §0/§2/§7）。
//!
//! 颜色一律走 `ThemeBrush`（跟随系统主题与强调色），此处只收敛布局数值。

use std::time::{SystemTime, UNIX_EPOCH};

use windows_reactor::*;

/// 东八区（B 站服务端标准时区）相对 UTC 秒偏移。
const CST_OFFSET_SECS: i64 = 8 * 3600;
const SECS_PER_DAY: i64 = 86_400;

/// 应用默认字体（Windows 11 25H2 内置）。
pub const DEFAULT_FONT_FAMILY: &str = "Noto Sans SC";

/// 构造应用默认字体的 `TextBlock`。
pub fn text(value: impl AsRef<str>) -> TextBlock {
    TextBlock::new()
        .font_family(DEFAULT_FONT_FAMILY)
        .text(value.as_ref())
}

/// 卡片间距。
#[cfg(test)]
pub const CARD_GAP: f64 = crate::layout::DESKTOP_CARD_GAP;
/// 卡片总高（封面 + 文本区）。
#[cfg(test)]
pub const CARD_HEIGHT: f64 = crate::layout::DESKTOP_CARD_HEIGHT;

/// 支持按断点缩放字号的页头。
pub fn page_header_sized(title: &str, subtitle: &str, title_size: f64) -> View {
    StackPanel::new()
        .spacing(4.0)
        .children((
            text(title)
                .font_size(title_size)
                .font_weight(FontWeight::BOLD),
            text(subtitle).opacity(0.6),
        ))
        .into()
}

/// 头像渲染：已解码位图走 `Image::source_data`，未就绪/空 URL 回退 `PersonPicture` 姓名首字。
pub fn avatar_view(img: Option<&EncodedImage>, name: &str, size: f64, radius: f64) -> Border {
    let inner: View = match img {
        Some(encoded) => Image::new()
            .source_data(encoded.clone())
            .width(size)
            .height(size)
            .stretch(Stretch::UniformToFill)
            .into(),
        None => {
            let mut pic = PersonPicture::new().width(size).height(size);
            if !name.is_empty() {
                pic = pic.display_name(name);
            }
            pic.into()
        }
    };
    Border::new()
        .width(size)
        .height(size)
        .corner_radius(radius)
        .background(ThemeBrush::SolidBackground)
        .content(inner)
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

/// 硬币计数格式化（整数不带 `.0`，小数保留一位）。
pub fn fmt_coins(coins: f64) -> String {
    if !coins.is_finite() || coins <= 0.0 {
        return "0".to_string();
    }
    let rounded = (coins * 10.0).round() / 10.0;
    if (rounded - rounded.round()).abs() < 0.05 {
        format!("{}", rounded.round() as u64)
    } else {
        format!("{rounded:.1}")
    }
}

/// 毫秒 unix 转 `YYYY-MM-DD`（东八区）；`0` 返回空串。
pub fn fmt_date_ms(unix_ms: u64) -> String {
    if unix_ms == 0 {
        return String::new();
    }
    let secs = (unix_ms / 1000) as i64;
    let (y, m, d, _, _) = cst_ymdhms(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 秒 unix 转简洁时间（当年省年份，今天省日期：`今天 HH:MM` / `MM-DD HH:MM` / `YYYY-MM-DD HH:MM`）。
pub fn fmt_time(unix_secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    fmt_time_with_now(unix_secs, now)
}

pub fn fmt_time_with_now(unix_secs: i64, now_secs: i64) -> String {
    if unix_secs <= 0 {
        return String::new();
    }
    let (y, m, d, hh, mm) = cst_ymdhms(unix_secs);
    let (ny, nm, nd, _, _) = cst_ymdhms(now_secs);
    if y == ny && m == nm && d == nd {
        format!("今天 {hh:02}:{mm:02}")
    } else if y == ny {
        format!("{m:02}-{d:02} {hh:02}:{mm:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
    }
}

/// 裁剪系统通知 `"YYYY-MM-DD HH:MM:SS"` 秒位 → `"YYYY-MM-DD HH:MM"`。
pub fn fmt_sys_time(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 16 && t.as_bytes().get(13) == Some(&b':') {
        t[..16].to_string()
    } else {
        t.to_string()
    }
}

fn cst_ymdhms(unix_secs: i64) -> (i64, u32, u32, u32, u32) {
    let local = unix_secs.saturating_add(CST_OFFSET_SECS);
    let days = local.div_euclid(SECS_PER_DAY);
    let rem = local.rem_euclid(SECS_PER_DAY);
    let hh = (rem / 3600) as u32;
    let mm = ((rem % 3600) / 60) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, hh, mm)
}

/// Howard Hinnant 公历转换（输入自 1970-01-01 起的整天数）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m, d)
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

    #[test]
    fn dates_and_coins_format_cleanly() {
        // 1700000000 = 2023-11-15 06:13:20 +08:00
        assert_eq!(fmt_date_ms(1_700_000_000_000), "2023-11-15");
        assert_eq!(fmt_date_ms(0), "");
        assert_eq!(fmt_time_with_now(1_700_000_000, 1_700_003_600), "今天 06:13");
        assert_eq!(fmt_time_with_now(1_700_000_000, 1_700_200_000), "11-15 06:13");
        assert_eq!(fmt_time_with_now(1_700_000_000, 1_795_000_000), "2023-11-15 06:13");
        assert_eq!(fmt_sys_time("2026-11-15 15:43:07"), "2026-11-15 15:43");
        assert_eq!(fmt_coins(125.1), "125.1");
        assert_eq!(fmt_coins(42.0), "42");
    }
}
