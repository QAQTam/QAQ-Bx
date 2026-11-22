//! 设置页：由 AppShell 单源持有并实时持久化到 `data/settings.json`。

use std::rc::Rc;

use qaq_bx_core::settings::{
    AppSettings, CodecPref, DANMAKU_FONTS, HomeOpenMode, PlayerEngine, QualityPref,
};
use windows_reactor::*;

use crate::layout::ViewportLayout;
use crate::theme;

/// 存储卡展示态（shell 单源：磁盘缓存占用 + 上次清空结果提示）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StorageCardState {
    /// 磁盘图片缓存当前占用字节。
    pub cache_bytes: u64,
    /// 一键清空后的结果提示（「已清空…释放 …」）。
    pub notice: Option<String>,
}

/// 字节数人类可读格式化（B/KB/MB/GB,一位小数;shell 提示与本卡共用）。
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn settings_page(
    settings: &AppSettings,
    is_vip: bool,
    layout: ViewportLayout,
    save_status: Option<&str>,
    on_change: &Callback<AppSettings>,
    storage: &StorageCardState,
    on_clear_caches: &Callback<()>,
) -> View {
    let quality_labels: Vec<&'static str> = QualityPref::ALL.iter().map(|q| q.label()).collect();
    let codec_labels: Vec<&'static str> = CodecPref::ALL.iter().map(|c| c.label()).collect();
    let open_mode_labels: Vec<&'static str> = HomeOpenMode::ALL.iter().map(|m| m.label()).collect();
    let engine_labels: Vec<&'static str> =
        PlayerEngine::ALL.iter().map(|e| e.label()).collect();
    let opacity_pct = (settings.danmaku_opacity * 100.0)
        .round()
        .clamp(10.0, 100.0) as i32;
    let font_scale_pct = (settings.danmaku_font_scale * 100.0)
        .round()
        .clamp(50.0, 200.0) as i32;

    let effective_qn = settings.quality.effective_max_qn(is_vip);
    let qn_hint = if settings.quality.qn() > effective_qn {
        "非大会员画质上限 1080P"
    } else {
        settings.quality.label()
    };

    let on_quality = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |index: Option<usize>| {
            if let Some(q) = index.and_then(QualityPref::from_index)
                && q != base.quality
            {
                let mut next = base.clone();
                next.quality = q;
                cb.call(next);
            }
        }
    };

    let on_codec = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |index: Option<usize>| {
            if let Some(c) = index.and_then(CodecPref::from_index)
                && c != base.codec
            {
                let mut next = base.clone();
                next.codec = c;
                cb.call(next);
            }
        }
    };

    let on_danmaku = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.danmaku_enabled {
                let mut next = base.clone();
                next.danmaku_enabled = on;
                cb.call(next);
            }
        }
    };

    let on_opacity = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let pct = val.round().clamp(10.0, 100.0) as i32;
            if pct != opacity_pct {
                let mut next = base.clone();
                next.danmaku_opacity = (pct as f32) / 100.0;
                cb.call(next);
            }
        }
    };

    let on_lanes = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: Option<f64>| {
            if let Some(v) = val
                && v.is_finite()
            {
                let lanes = (v.round() as usize).clamp(1, 20);
                if lanes != base.danmaku_max_lanes {
                    let mut next = base.clone();
                    next.danmaku_max_lanes = lanes;
                    cb.call(next);
                }
            }
        }
    };

    let on_font = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |index: Option<usize>| {
            if let Some((family, _)) = index.and_then(|idx| DANMAKU_FONTS.get(idx))
                && *family != base.danmaku_font_family
            {
                let mut next = base.clone();
                next.danmaku_font_family = (*family).to_string();
                cb.call(next);
            }
        }
    };

    let on_font_scale = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let scale = (val / 100.0).clamp(0.5, 2.0) as f32;
            if (scale - base.danmaku_font_scale).abs() > f32::EPSILON {
                let mut next = base.clone();
                next.danmaku_font_scale = scale;
                cb.call(next);
            }
        }
    };

    // 弹幕样式三滑条 + 防挡字幕开关(官方 DanmuWebPlayerConfig 对齐;DM 批次)。
    let speed_plus_pct = (settings.danmaku_speed_plus * 100.0)
        .round()
        .clamp(50.0, 200.0) as i32;
    let density_pct = (settings.danmaku_density * 100.0)
        .round()
        .clamp(10.0, 100.0) as i32;

    let on_speed_plus = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let pct = val.round().clamp(50.0, 200.0);
            let speed = (pct / 100.0) as f32;
            if (speed - base.danmaku_speed_plus).abs() > f32::EPSILON {
                let mut next = base.clone();
                next.danmaku_speed_plus = speed;
                cb.call(next);
            }
        }
    };

    let on_area = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let area = val.round().clamp(10.0, 100.0) as u8;
            if area != base.danmaku_area {
                let mut next = base.clone();
                next.danmaku_area = area;
                cb.call(next);
            }
        }
    };

    let on_density = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let pct = val.round().clamp(10.0, 100.0);
            let density = (pct / 100.0) as f32;
            if (density - base.danmaku_density).abs() > f32::EPSILON {
                let mut next = base.clone();
                next.danmaku_density = density;
                cb.call(next);
            }
        }
    };

    let on_prevent_shade = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.danmaku_prevent_shade {
                let mut next = base.clone();
                next.danmaku_prevent_shade = on;
                cb.call(next);
            }
        }
    };

    // 弹幕屏蔽类型四选(网页版弹幕设置对齐):get/set 函数对生成回调。
    let mk_block_cb = |get: fn(&AppSettings) -> bool, set: fn(&mut AppSettings, bool)| {
        let base = settings.clone();
        let cb = on_change.clone();
        move |checked: Option<bool>| {
            let Some(on) = checked else {
                return;
            };
            if on != get(&base) {
                let mut next = base.clone();
                set(&mut next, on);
                cb.call(next);
            }
        }
    };
    let on_block_scroll =
        mk_block_cb(|s| s.danmaku_block_scroll, |s, v| s.danmaku_block_scroll = v);
    let on_block_bottom =
        mk_block_cb(|s| s.danmaku_block_bottom, |s, v| s.danmaku_block_bottom = v);
    let on_block_top = mk_block_cb(|s| s.danmaku_block_top, |s, v| s.danmaku_block_top = v);
    let on_block_color =
        mk_block_cb(|s| s.danmaku_block_color, |s, v| s.danmaku_block_color = v);

    let on_filter_rules = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |text: Rc<str>| {
            if text.as_ref() != base.danmaku_filter_rules {
                let mut next = base.clone();
                next.danmaku_filter_rules = text.to_string();
                cb.call(next);
            }
        }
    };

    let on_repeat = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.danmaku_block_repeat {
                let mut next = base.clone();
                next.danmaku_block_repeat = on;
                cb.call(next);
            }
        }
    };

    let on_weight = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let level = val.round().clamp(0.0, 10.0) as u32;
            if level != base.danmaku_weight_min {
                let mut next = base.clone();
                next.danmaku_weight_min = level;
                cb.call(next);
            }
        }
    };

    let on_autoplay = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.autoplay {
                let mut next = base.clone();
                next.autoplay = on;
                cb.call(next);
            }
        }
    };

    let on_hardware = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.hardware_decode {
                let mut next = base.clone();
                next.hardware_decode = on;
                cb.call(next);
            }
        }
    };

    let on_volume = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |val: f64| {
            let vol = val.round().clamp(0.0, 100.0) as u32;
            if vol != base.volume {
                let mut next = base.clone();
                next.volume = vol;
                cb.call(next);
            }
        }
    };

    let on_open_mode = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |idx: Option<usize>| {
            if let Some(mode) = idx.and_then(HomeOpenMode::from_index)
                && mode != base.home_open_mode
            {
                let mut next = base.clone();
                next.home_open_mode = mode;
                cb.call(next);
            }
        }
    };

    // M7:播放引擎(默认 mpv 兼容优先;原生 Windows Media = 系统媒体引擎)。
    // 改当次即生效:正在播的 VOD 由 shell 换引擎续播,直播不受影响。
    let on_engine = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |idx: Option<usize>| {
            if let Some(engine) = idx.and_then(PlayerEngine::from_index)
                && engine != base.player_engine
            {
                let mut next = base.clone();
                next.player_engine = engine;
                cb.call(next);
            }
        }
    };
    let on_portrait = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.portrait_mode {
                let mut next = base.clone();
                next.portrait_mode = on;
                cb.call(next);
            }
        }
    };
    let on_prefer_official_cdn = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.prefer_official_cdn {
                let mut next = base.clone();
                next.prefer_official_cdn = on;
                cb.call(next);
            }
        }
    };
    let on_multi_thread_preload = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.multi_thread_preload {
                let mut next = base.clone();
                next.multi_thread_preload = on;
                cb.call(next);
            }
        }
    };
    let on_hover_preview = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.hover_preview {
                let mut next = base.clone();
                next.hover_preview = on;
                cb.call(next);
            }
        }
    };
    let on_reduce_animations = {
        let base = settings.clone();
        let cb = on_change.clone();
        move |on: bool| {
            if on != base.reduce_animations {
                let mut next = base.clone();
                next.reduce_animations = on;
                cb.call(next);
            }
        }
    };

    let card_padding = if layout.is_mobile() { 16.0 } else { 24.0 };
    let card = |content: StackPanel| {
        Border::new()
            .corner_radius(8.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(card_padding)
            .content(content)
    };

    let playback_card = card(
        StackPanel::new().spacing(16.0).children((
            RadioButtons::new()
                .header("清晰度")
                .items_source(quality_labels)
                .selected_index(settings.quality.index())
                .on_selection_changed(on_quality),
            ComboBox::new()
                .header("视频编码")
                .items_source(codec_labels)
                .selected_index(settings.codec.index())
                .on_selection_changed(on_codec),
            ComboBox::new()
                .header("首页播放时")
                .items_source(open_mode_labels)
                .selected_index(settings.home_open_mode.index())
                .on_selection_changed(on_open_mode),
            ComboBox::new()
                .header("播放引擎")
                .automation_id("settings-player-engine")
                .items_source(engine_labels)
                .selected_index(settings.player_engine.index())
                .on_selection_changed(on_engine),
            ToggleSwitch::new()
                .header("自动播放")
                .on_content("开")
                .off_content("关")
                .is_on(settings.autoplay)
                .on_toggled(on_autoplay),
            ToggleSwitch::new()
                .header("硬件解码")
                .on_content("开")
                .off_content("关")
                .is_on(settings.hardware_decode)
                .on_toggled(on_hardware),
            ToggleSwitch::new()
                .header("优先官方 CDN")
                .automation_id("settings-prefer-official-cdn")
                .on_content("开")
                .off_content("关")
                .is_on(settings.prefer_official_cdn)
                .on_toggled(on_prefer_official_cdn),
            ToggleSwitch::new()
                .header("多线程并发预加载")
                .automation_id("settings-multi-thread-preload")
                .on_content("开")
                .off_content("关")
                .is_on(settings.multi_thread_preload)
                .on_toggled(on_multi_thread_preload),
        )),
    )
    .grid_column(0);

    let mut danmaku_items: Vec<View> = vec![
        Slider::new()
            .header(format!("默认音量 {}%", settings.volume))
            .minimum(0.0)
            .maximum(100.0)
            .value(f64::from(settings.volume))
            .on_value_changed(on_volume)
            .into(),
        ToggleSwitch::new()
            .header("弹幕")
            .on_content("开")
            .off_content("关")
            .is_on(settings.danmaku_enabled)
            .on_toggled(on_danmaku)
            .into(),
        Slider::new()
            .header(format!("弹幕透明度 {}%", opacity_pct))
            .minimum(10.0)
            .maximum(100.0)
            .value(f64::from(opacity_pct))
            .on_value_changed(on_opacity)
            .into(),
        ComboBox::new()
            .header("弹幕字体")
            .items_source(DANMAKU_FONTS.iter().map(|(_, label)| (*label).to_string()))
            .selected_index(
                DANMAKU_FONTS
                    .iter()
                    .position(|(family, _)| *family == settings.danmaku_font_family)
                    .unwrap_or(0),
            )
            .on_selection_changed(on_font)
            .into(),
        Slider::new()
            .header(format!("弹幕字号 {}%", font_scale_pct))
            .minimum(50.0)
            .maximum(200.0)
            .step_frequency(10.0)
            .value(f64::from(font_scale_pct))
            .on_value_changed(on_font_scale)
            .into(),
        NumberBox::new()
            .header("弹幕轨道上限")
            .minimum(1.0)
            .maximum(20.0)
            .small_change(1.0)
            .spin_button_placement_mode(NumberBoxSpinButtonPlacementMode::Inline)
            .value(settings.danmaku_max_lanes as f64)
            .on_value_changed(on_lanes)
            .into(),
        Slider::new()
            .automation_id("settings-danmaku-speed-plus")
            .header(format!("弹幕速度 {}%", speed_plus_pct))
            .minimum(50.0)
            .maximum(200.0)
            .step_frequency(10.0)
            .value(f64::from(speed_plus_pct))
            .on_value_changed(on_speed_plus)
            .into(),
        Slider::new()
            .automation_id("settings-danmaku-area")
            .header(format!("弹幕显示区域 {}%", settings.danmaku_area))
            .minimum(10.0)
            .maximum(100.0)
            .step_frequency(10.0)
            .value(f64::from(settings.danmaku_area))
            .on_value_changed(on_area)
            .into(),
        Slider::new()
            .automation_id("settings-danmaku-density")
            .header(format!("弹幕密度 {}%", density_pct))
            .minimum(10.0)
            .maximum(100.0)
            .step_frequency(10.0)
            .value(f64::from(density_pct))
            .on_value_changed(on_density)
            .into(),
        ToggleSwitch::new()
            .automation_id("settings-danmaku-prevent-shade")
            .header("弹幕防挡字幕")
            .on_content("开")
            .off_content("关")
            .is_on(settings.danmaku_prevent_shade)
            .on_toggled(on_prevent_shade)
            .into(),
        // 弹幕屏蔽(网页版弹幕设置对齐):类型四选 + 规则 + 去重 + 智能权重。
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(16.0)
            .children([
                CheckBox::new()
                    .content("滚动")
                    .is_checked(settings.danmaku_block_scroll)
                    .on_is_checked_changed(on_block_scroll.clone())
                    .into(),
                CheckBox::new()
                    .content("顶部")
                    .is_checked(settings.danmaku_block_top)
                    .on_is_checked_changed(on_block_top.clone())
                    .into(),
                CheckBox::new()
                    .content("底部")
                    .is_checked(settings.danmaku_block_bottom)
                    .on_is_checked_changed(on_block_bottom.clone())
                    .into(),
                CheckBox::new()
                    .content("彩色")
                    .is_checked(settings.danmaku_block_color)
                    .on_is_checked_changed(on_block_color.clone())
                    .into(),
            ])
            .into(),
        TextBox::new()
            .header("弹幕屏蔽词(每行一条;re: 前缀=正则,u: 前缀=用户)")
            .accepts_return(true)
            .placeholder_text("前方高能\nre:^\\d+$\nu:abcd1234")
            .text(settings.danmaku_filter_rules.as_str())
            .height(96.0)
            .on_text_changed(on_filter_rules)
            .into(),
        ToggleSwitch::new()
            .header("过滤重复弹幕")
            .on_content("开")
            .off_content("关")
            .is_on(settings.danmaku_block_repeat)
            .on_toggled(on_repeat)
            .into(),
        Slider::new()
            .header(format!(
                "智能屏蔽等级 {}(0=关)",
                settings.danmaku_weight_min
            ))
            .minimum(0.0)
            .maximum(10.0)
            .step_frequency(1.0)
            .value(f64::from(settings.danmaku_weight_min))
            .on_value_changed(on_weight)
            .into(),
        Button::new()
            .content("Panic!")
            .on_click(|| panic!("用户从设置页手动触发测试 panic"))
            .into(),
    ];
    if let Some(status) = save_status {
        danmaku_items.push(
            InfoBar::new()
                .title("设置异常")
                .message(status)
                .severity(InfoBarSeverity::Warning)
                .is_open(true)
                .is_closable(false)
                .into(),
        );
    }

    let danmaku_card = card(StackPanel::new().spacing(16.0).children(danmaku_items));
    let window_card = card(
        StackPanel::new().spacing(16.0).children((
            ToggleSwitch::new()
                .header("竖屏模式（实验）")
                .on_content("开")
                .off_content("关")
                .is_on(settings.portrait_mode)
                .on_toggled(on_portrait),
            ToggleSwitch::new()
                .header("悬停预览卡")
                .automation_id("settings-hover-preview")
                .on_content("开")
                .off_content("关")
                .is_on(settings.hover_preview)
                .on_toggled(on_hover_preview),
            ToggleSwitch::new()
                .header("减少动画")
                .automation_id("settings-reduce-animations")
                .on_content("开")
                .off_content("关")
                .is_on(settings.reduce_animations)
                .on_toggled(on_reduce_animations),
            TextBlock::new()
                .text("窗口宽度锁定在手机布局档位（底部导航栏保留），最大化按钮变为「贴边到屏幕右缘」，再点还原。立即生效，重启后保持。")
                .font_size(12.0)
                .opacity(0.6)
                .text_wrapping(TextWrapping::Wrap),
        )),
    );

    // 存储卡:磁盘缓存占用 + 一键清空(白名单:图片缓存与风控票据;凭据/设置不动)。
    let clear_cb = on_clear_caches.clone();
    let mut storage_items: Vec<View> = vec![
        theme::text("存储").font_size(16.0).font_weight(FontWeight::SEMI_BOLD).into(),
        TextBlock::new()
            .text(format!(
                "封面/头像磁盘缓存 {} · 上限 200 MB。清空后已加载图片仍显示,重新下载仅发生在下次浏览。",
                fmt_bytes(storage.cache_bytes)
            ))
            .font_size(12.0)
            .opacity(0.6)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
        Button::new()
            .content("清空缓存")
            .automation_id("settings-clear-caches")
            .on_click(move || clear_cb.call(()))
            .into(),
        TextBlock::new()
            .text("清理范围:封面/头像磁盘缓存与风控票据(自动重签)。登录凭据与设置不会被清除。")
            .font_size(11.5)
            .opacity(0.5)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    ];
    if let Some(notice) = &storage.notice {
        storage_items.push(
            InfoBar::new()
                .title("缓存已清理")
                .message(notice.clone())
                .severity(InfoBarSeverity::Success)
                .is_open(true)
                .is_closable(false)
                .into(),
        );
    }
    let storage_card = card(StackPanel::new().spacing(12.0).children(storage_items));

    let cards_body: View = if layout.is_single_column() {
        StackPanel::new()
            .spacing(16.0)
            .children((playback_card, danmaku_card, window_card, storage_card))
            .into()
    } else {
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
            .rows([GridLength::Auto, GridLength::Auto])
            .column_spacing(20.0)
            .row_spacing(20.0)
            .children((
                playback_card,
                danmaku_card.grid_column(1),
                window_card.grid_row(1),
                storage_card.grid_row(1).grid_column(1),
            ))
            .into()
    };

    let pad_x = layout.page_margin_x();
    let pad_top = if layout.is_mobile() { 12.0 } else { 20.0 };
    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(pad_x, pad_top, pad_x, 36.0))
                .content(StackPanel::new().spacing(16.0).children((
                    theme::page_header_sized("设置", qn_hint, layout.page_title_size()),
                    cards_body,
                ))),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(storage: &StorageCardState) -> String {
        let settings = AppSettings::default();
        let on_change = Callback::new(|_: AppSettings| {});
        let on_clear = Callback::new(|_: ()| {});
        let view = settings_page(
            &settings,
            false,
            ViewportLayout::default(),
            None,
            &on_change,
            storage,
            &on_clear,
        );
        format!("{view:?}")
    }

    #[test]
    fn fmt_bytes_scales() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn storage_card_renders_clear_button_and_usage() {
        let debug = render(&StorageCardState { cache_bytes: 5 * 1024 * 1024, notice: None });
        assert!(debug.contains("清空缓存"), "存储卡必须提供清空按钮");
        assert!(debug.contains("settings-clear-caches"), "E2E 需 automation id 定位");
        assert!(debug.contains("5.0 MB"), "须展示当前占用");
        assert!(debug.contains("登录凭据与设置不会被清除"), "白名单口径须对用户明示");
    }

    #[test]
    fn storage_card_shows_clear_result_notice() {
        let debug = render(&StorageCardState {
            cache_bytes: 0,
            notice: Some("已清空缓存,释放 5.0 MB".into()),
        });
        assert!(debug.contains("已清空缓存,释放 5.0 MB"));
        assert!(debug.contains("0 B"));
    }
}
