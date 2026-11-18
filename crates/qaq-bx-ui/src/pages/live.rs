//! 直播页：guest 可用的 `room/v1/Area/getRoomList` 真实数据卡片网格（U4 接线）。
//!
//! 状态由 `AppShell` 单源持有（`LivePageState`）;封面经 `ImageCache` 异步填充
//! (`LiveCoverReady` 回填,与视频卡同款有界 LRU);点击卡片 → `LiveRoomSelected`
//! → 房间流直链交 mpv 播放。

use std::sync::Arc;

use qaq_bx_core::live::LiveRoom;
use windows_reactor::*;

use crate::layout::ViewportLayout;
use crate::theme;

/// UI 侧直播间卡:核心展示模型 + 已解码封面(`ImageCache` 异步回填)。
#[derive(Clone, Debug, PartialEq)]
pub struct LiveCard {
    pub room: LiveRoom,
    pub cover: Option<EncodedImage>,
}

/// 直播页展示状态（`AppShell` 单源持有）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LivePageState {
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    pub cards: Vec<LiveCard>,
    /// 下一次请求页号（成功才推进;空页 → 终态）。
    pub next_page: u32,
    pub reached_end: bool,
}

pub fn live_page(
    state: &LivePageState,
    layout: ViewportLayout,
    on_open: &Callback<(u64, String, String)>,
    on_more: &Callback<()>,
) -> View {
    let subtitle = if let Some(err) = &state.error {
        if state.loaded_once {
            format!("{} 个直播间 · 下一页加载失败：{err}", state.cards.len())
        } else {
            format!("加载失败：{err}")
        }
    } else if state.loading && state.cards.is_empty() {
        "加载中…".to_string()
    } else {
        format!("{} 个直播间", state.cards.len())
    };

    let footer_pill: Option<View> = if state.error.is_some() {
        Some(status_pill("重试", Some(on_more.clone()), false))
    } else if state.reached_end {
        Some(status_pill("没有更多了", None, false))
    } else if state.loading {
        Some(status_pill("加载更多…", None, true))
    } else if state.loaded_once {
        Some(status_pill("加载更多", Some(on_more.clone()), false))
    } else {
        None
    };

    let margin_x = layout.page_margin_x();
    let header_top = if layout.is_mobile() { 12.0 } else { 24.0 };
    let header = Border::new()
        .padding(Thickness::new(margin_x, header_top, margin_x, 12.0))
        .grid_row(0)
        .content(
            StackPanel::new().spacing(4.0).children((
                theme::text("直播")
                    .font_size(layout.page_title_size())
                    .font_weight(FontWeight::BOLD),
                theme::text(subtitle)
                    .font_size(if layout.is_mobile() { 12.0 } else { 13.0 })
                    .opacity(0.6),
            )),
        );

    let mut content_items: Vec<View> = vec![rooms_grid(&state.cards, layout.cards, on_open)];
    if let Some(pill) = footer_pill {
        content_items.push(pill);
    }

    let grid = ScrollViewer::new().grid_row(1).content(
        Border::new()
            .padding(Thickness::new(margin_x, 4.0, margin_x, 28.0))
            .content(StackPanel::new().spacing(16.0).children(content_items)),
    );

    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((View::from(header), View::from(grid)))
        .into()
}

/// 直播间卡网格：按列数精确分行(与 `video_grid` 同款防折行策略)。
pub fn rooms_grid(
    cards: &[LiveCard],
    metrics: crate::layout::CardMetrics,
    on_open: &Callback<(u64, String, String)>,
) -> View {
    let cols = metrics.cols.max(1);
    let rows: Vec<KeyedView> = cards
        .chunks(cols)
        .enumerate()
        .map(|(row_idx, chunk)| {
            let cells: Vec<KeyedView> = chunk
                .iter()
                .map(|card| {
                    KeyedView::new(
                        format!("live:{}", card.room.roomid),
                        room_card(card, on_open, metrics.card_width),
                    )
                })
                .collect();
            KeyedView::new(
                format!("live-row:{row_idx}:{}", cols),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(metrics.gap)
                    .keyed_children(cells),
            )
        })
        .collect();
    StackPanel::new().spacing(metrics.gap).keyed_children(rows).into()
}

fn room_card(card: &LiveCard, on_open: &Callback<(u64, String, String)>, card_width: f64) -> View {
    let room = &card.room;
    let cover_h = (card_width * 9.0 / 16.0).round();
    let payload = (room.roomid, room.title.clone(), room.uname.clone());
    let on_click = {
        let cb = on_open.clone();
        move || cb.call(payload.clone())
    };
    let cover: View = match &card.cover {
        Some(img) => Image::new()
            .source_data(img.clone())
            .width(card_width)
            .height(cover_h)
            .stretch(Stretch::UniformToFill)
            .into(),
        None => Border::new()
            .width(card_width)
            .height(cover_h)
            .background(Brush::Solid(Color::rgb(24, 24, 28)))
            .content(
                FontIcon::new()
                    .glyph("\u{E714}")
                    .font_size(28.0)
                    .foreground(Brush::Solid(Color::rgb(90, 90, 96)))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center),
            )
            .into(),
    };
    // 封面 + 左下人气徽标 + 文本区;Border 点击释放(避开 ResourceOverrides 崩溃)。
    Border::new()
        .width(card_width)
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .on_pointer_released(move |_| on_click())
        .content(
            StackPanel::new().children((
                Grid::new().children((
                    cover,
                    Border::new()
                        .margin(Thickness::new(8.0, 0.0, 0.0, 8.0))
                        .horizontal_alignment(HorizontalAlignment::Left)
                        .vertical_alignment(VerticalAlignment::Bottom)
                        .corner_radius(10.0)
                        .padding(Thickness::xy(7.0, 2.0))
                        .background(Brush::Solid(Color::argb(170, 0, 0, 0)))
                        .content(
                            TextBlock::new()
                                .text(format!("{} 人气", theme::fmt_count(room.online)))
                                .font_size(10.5)
                                .foreground(Brush::Solid(Color::rgb(255, 255, 255))),
                        ),
                )),
                StackPanel::new()
                    .margin(Thickness::xy(10.0, 8.0))
                    .spacing(3.0)
                    .children((
                        TextBlock::new()
                            .text(room.title.clone())
                            .font_size(12.5)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(2),
                        TextBlock::new()
                            .text(format!(
                                "{} · {}",
                                room.uname,
                                if room.area_name.is_empty() {
                                    room.parent_name.clone()
                                } else {
                                    room.area_name.clone()
                                }
                            ))
                            .font_size(11.0)
                            .opacity(0.65)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(1),
                    )),
            )),
        )
        .into()
}

/// 底部分页胶囊(与热门页同款)。
fn status_pill(label: &str, on_click: Option<Callback<()>>, with_spinner: bool) -> View {
    if let Some(on_click) = on_click {
        return Button::new()
            .content(label)
            .horizontal_alignment(HorizontalAlignment::Center)
            .on_click(move || on_click.call(()))
            .into();
    }
    let mut children: Vec<View> = Vec::new();
    if with_spinner {
        children.push(View::from(
            ProgressRing::new()
                .is_indeterminate(true)
                .is_active(true)
                .width(16.0)
                .height(16.0),
        ));
    }
    children.push(View::from(
        TextBlock::new()
            .text(label)
            .font_size(12.0)
            .opacity(0.75)
            .vertical_alignment(VerticalAlignment::Center),
    ));
    Border::new()
        .corner_radius(18.0)
        .padding(Thickness::xy(16.0, 8.0))
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .content(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children(children),
        )
        .into()
}

#[allow(dead_code)]
fn _unused_ar(_a: Arc<()>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(roomid: u64) -> LiveRoom {
        LiveRoom {
            roomid,
            title: format!("房间{roomid}"),
            uname: "UP".into(),
            online: 1000,
            cover: format!("https://cover/{roomid}.jpg"),
            area_name: "网游".into(),
            parent_name: "直播".into(),
        }
    }

    #[test]
    fn default_state_is_idle() {
        let state = LivePageState::default();
        assert!(!state.loading && !state.loaded_once && !state.reached_end);
        assert_eq!(state.next_page, 0);
        assert!(state.cards.is_empty());
    }

    #[test]
    fn rooms_grid_renders_with_and_without_covers_without_panic() {
        let layout = ViewportLayout::default();
        let cb = Callback::new(|_: (u64, String, String)| {});
        let _ = rooms_grid(&[], layout.cards, &cb);
        let cards = vec![
            LiveCard { room: room(1), cover: None },
            LiveCard { room: room(2), cover: Some(EncodedImage::new(vec![1, 2, 3])) },
            LiveCard { room: room(3), cover: None },
        ];
        let _ = rooms_grid(&cards, layout.cards, &cb);
    }
}
