//! 设置页：由 AppShell 单源持有并实时持久化到 `data/settings.json`。

use std::rc::Rc;

use qaq_bx_core::settings::{
    AppSettings, CodecPref, DANMAKU_FONTS, HomeOpenMode, PlayerEngine, QualityPref,
};
use windows_reactor::*;

use crate::layout::ViewportLayout;
use crate::theme;

pub fn settings_page(
    settings: &AppSettings,
    is_vip: bool,
    layout: ViewportLayout,
    save_status: Option<&str>,
    on_change: &Callback<AppSettings>,
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
    let cards_body: View = if layout.is_single_column() {
        StackPanel::new()
            .spacing(16.0)
            .children((playback_card, danmaku_card))
            .into()
    } else {
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
            .column_spacing(20.0)
            .children((playback_card, danmaku_card.grid_column(1)))
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
