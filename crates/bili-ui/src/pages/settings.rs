//! 设置页：由 AppShell 单源持有并实时持久化到 `data/settings.json`。

use bili_core::settings::{AppSettings, CodecPref, QualityPref};
use windows_reactor::*;

use crate::theme;

pub fn settings_page(
    settings: &AppSettings,
    is_vip: bool,
    settings_path: &str,
    save_status: Option<&str>,
    on_change: &Callback<AppSettings>,
) -> View {
    let quality_labels: Vec<&'static str> = QualityPref::ALL.iter().map(|q| q.label()).collect();
    let codec_labels: Vec<&'static str> = CodecPref::ALL.iter().map(|c| c.label()).collect();
    let opacity_pct = (settings.danmaku_opacity * 100.0).round().clamp(10.0, 100.0) as i32;

    let effective_qn = settings.quality.effective_max_qn(is_vip);
    let qn_hint = if settings.quality.qn() > effective_qn {
        format!(
            "当前账号非大会员，播放最高将以 1080P (qn={effective_qn}) 解析（登录后大会员账号自动生效 {}）",
            settings.quality.label()
        )
    } else {
        format!("当前将按 {} (上限 qn={effective_qn}) 请求播放地址", settings.quality.label())
    };

    let info_title = if save_status.is_some() {
        "配置持久化状态"
    } else {
        "已接入持久化"
    };
    let info_msg = save_status
        .map(str::to_string)
        .unwrap_or_else(|| format!("设置实时同步并原子写入：{settings_path}"));
    let info_severity = if save_status.is_some_and(|s| s.contains("失败") || s.contains("回退")) {
        InfoBarSeverity::Warning
    } else {
        InfoBarSeverity::Success
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
                .header("首选清晰度")
                .items_source(quality_labels)
                .selected_index(settings.quality.index())
                .on_selection_changed(on_quality),
            ComboBox::new()
                .header("编码偏好")
                .items_source(codec_labels)
                .selected_index(settings.codec.index())
                .on_selection_changed(on_codec),
            ToggleSwitch::new()
                .header("进入播放页自动播放")
                .on_content("开")
                .off_content("关")
                .is_on(settings.autoplay)
                .on_toggled(on_autoplay),
            ToggleSwitch::new()
                .header("硬件解码优先（D3D11VA）")
                .on_content("开")
                .off_content("关")
                .is_on(settings.hardware_decode)
                .on_toggled(on_hardware),
        )),
    )
    .grid_column(0);

    let danmaku_card = card(
        StackPanel::new().spacing(16.0).children((
            Slider::new()
                .header(format!("默认音量（{}%）", settings.volume))
                .minimum(0.0)
                .maximum(100.0)
                .value(f64::from(settings.volume))
                .on_value_changed(on_volume),
            ToggleSwitch::new()
                .header("弹幕显示")
                .on_content("开")
                .off_content("关")
                .is_on(settings.danmaku_enabled)
                .on_toggled(on_danmaku),
            Slider::new()
                .header(format!("弹幕不透明度（{}%）", opacity_pct))
                .minimum(10.0)
                .maximum(100.0)
                .value(f64::from(opacity_pct))
                .on_value_changed(on_opacity),
            NumberBox::new()
                .header("滚动弹幕最大轨道数（1–20）")
                .minimum(1.0)
                .maximum(20.0)
                .small_change(1.0)
                .spin_button_placement_mode(NumberBoxSpinButtonPlacementMode::Inline)
                .value(settings.danmaku_max_lanes as f64)
                .on_value_changed(on_lanes),
            InfoBar::new()
                .title(info_title)
                .message(info_msg)
                .severity(info_severity)
                .is_open(true)
                .is_closable(false),
        )),
    )
    .grid_column(1);

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 36.0))
                .content(
                    StackPanel::new().spacing(16.0).children((
                        theme::page_header("设置", &qn_hint),
                        Grid::new()
                            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                            .column_spacing(20.0)
                            .children((playback_card, danmaku_card)),
                    )),
                ),
        )
        .into()
}
