//! 迷你播放条（`docs/ui-design.md` §5，对照底部 MediaTransportControls）。
//! 由 AppShell 持有播放状态并注入回调；播放页打开时整条隐藏。

use windows_reactor::*;
use crate::data::VideoItem;
use crate::theme;

#[allow(clippy::too_many_arguments)]
pub fn mini_player(
    item: &VideoItem,
    playing: bool,
    position: u32,
    on_prev: &Callback<()>,
    on_toggle: &Callback<()>,
    on_next: &Callback<()>,
    on_close: &Callback<()>,
    on_seek: &Callback<f64>,
) -> Border {
    let thumb_inner: View = match &item.cover {
        Some(img) => Image::new()
            .source_data(img.clone())
            .stretch(Stretch::UniformToFill)
            .into(),
        None => TextBlock::new()
            .text("封面")
            .font_size(11.0)
            .opacity(0.35)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    };
    let thumb = Border::new()
        .corner_radius(4.0)
        .background(ThemeBrush::SolidBackground)
        .width(80.0)
        .height(45.0)
        .content(thumb_inner);
    let left = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(10.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            View::from(thumb),
            StackPanel::new()
                .spacing(2.0)
                .width(230.0)
                .children((
                    TextBlock::new()
                        .text(item.title.clone())
                        .font_size(12.0)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .max_lines(1),
                    TextBlock::new()
                        .text(item.author.clone())
                        .font_size(11.0)
                        .opacity(0.6)
                        .max_lines(1),
                )),
        ));

    let play_icon = if playing { Symbol::Pause } else { Symbol::Play };
    let center = StackPanel::new()
        .spacing(4.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            View::from(
                Slider::new()
                    .minimum(0.0)
                    .maximum(item.duration_secs as f64)
                    .value(position as f64)
                    .on_value_changed(on_seek.clone()),
            ),
            View::from(
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((
                    View::from(
                        TextBlock::new()
                            .text(theme::fmt_duration(position))
                            .font_size(11.0)
                            .opacity(0.6)
                            .vertical_alignment(VerticalAlignment::Center)
                    ),
                    View::from(
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(4.0)
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .grid_column(1)
                            .children((
                                AppBarButton::new()
                                    .icon(Icon::symbol(Symbol::Previous))
                                    .on_click(on_prev.clone()),
                                AppBarButton::new()
                                    .icon(Icon::symbol(play_icon))
                                    .on_click(on_toggle.clone()),
                                AppBarButton::new()
                                    .icon(Icon::symbol(Symbol::Next))
                                    .on_click(on_next.clone()),
                            ))
                    ),
                    View::from(
                        TextBlock::new()
                            .text(theme::fmt_duration(item.duration_secs))
                            .font_size(11.0)
                            .opacity(0.6)
                            .horizontal_alignment(HorizontalAlignment::Right)
                            .vertical_alignment(VerticalAlignment::Center)
                            .grid_column(2)
                    ),
                )),
            ),
        ));

    Border::new()
        .border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0))
        .border_brush(ThemeBrush::CardStroke)
        .background(ThemeBrush::CardBackground)
        .padding(Thickness::new(16.0, 8.0, 16.0, 8.0))
        .content(
            Grid::new()
                .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                .children((
                    View::from(left),
                    View::from(
                        center
                            .margin(Thickness::new(16.0, 0.0, 16.0, 0.0))
                            .grid_column(1),
                    ),
                    View::from(
                        AppBarButton::new()
                            .icon(Icon::symbol(Symbol::Cancel))
                            .vertical_alignment(VerticalAlignment::Center)
                            .grid_column(2)
                            .on_click(on_close.clone()),
                    ),
                )),
        )
}
