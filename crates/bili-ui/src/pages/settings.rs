//! 设置页：由 AppShell 单源持有并实时持久化到 `data/settings.json`。

use bili_core::settings::{AppSettings, CodecPref, QualityPref};
use windows_reactor::*;

use crate::theme;

pub fn settings_page(
    settings: &AppSettings,
    is_vip: bool,
    save_status: Option<&str>,
    on_change: &Callback<AppSettings>,
) -> View {
    let quality_labels: Vec<&'static str> = QualityPref::ALL.iter().map(|q| q.label()).collect();
    let codec_labels: Vec<&'static str> = CodecPref::ALL.iter().map(|c| c.label()).collect();
    let opacity_pct = (settings.danmaku_opacity * 100.0).round().clamp(10.0, 100.0) as i32;

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

    let card = |content: StackPanel| {
        Border::new()
            .corner_radius(8.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(24.0)
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
        NumberBox::new()
            .header("弹幕轨道上限")
            .minimum(1.0)
            .maximum(20.0)
            .small_change(1.0)
            .spin_button_placement_mode(NumberBoxSpinButtonPlacementMode::Inline)
            .value(settings.danmaku_max_lanes as f64)
            .on_value_changed(on_lanes)
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

    let danmaku_card = card(StackPanel::new().spacing(16.0).children(danmaku_items)).grid_column(1);

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                .content(
                    StackPanel::new().spacing(16.0).children((
                        theme::page_header("设置", qn_hint),
                        Grid::new()
                            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                            .column_spacing(20.0)
                            .children((playback_card, danmaku_card)),
                    )),
                ),
        )
        .into()
}
