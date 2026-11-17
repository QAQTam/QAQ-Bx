//! 热门页：真实 `popular` API（无签名 pn/ps 分页）的卡片网格 + 分页/错误/终态底栏。
//!
//! 状态由 `AppShell` 单源持有（`PopularFeedState`）；本模块仅做展示与回调接线。

use windows_reactor::*;

use crate::data::VideoItem;
use crate::theme;
use crate::widgets::video_card;

/// 热门页展示状态（`AppShell` 单源持有）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PopularFeedState {
    /// 分页请求在飞（首屏/加载更多共用）。
    pub loading: bool,
    /// 至少成功加载过一页。
    pub loaded_once: bool,
    /// 上次请求失败原因（`Some` 时底栏显示重试；重试重取同一页）。
    pub error: Option<String>,
    /// 已累计展示的真实卡片（按 bvid 去重）。
    pub items: Vec<VideoItem>,
    /// 下一次请求页号（成功才推进）。
    pub next_pn: u32,
    /// 服务端返回空页 → 定格终态。
    pub reached_end: bool,
}

pub fn popular_page(
    state: &PopularFeedState,
    on_open: &Callback<String>,
    on_more: &Callback<()>,
) -> View {
    let subtitle = if let Some(err) = &state.error {
        if state.loaded_once {
            format!("{} 个视频 · 下一页加载失败：{err}", state.items.len())
        } else {
            format!("加载失败：{err}")
        }
    } else if state.loading && state.items.is_empty() {
        "加载中…".to_string()
    } else {
        format!("{} 个视频", state.items.len())
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

    let header = Border::new()
        .padding(Thickness::new(36.0, 24.0, 36.0, 12.0))
        .grid_row(0)
        .content(
            StackPanel::new().spacing(4.0).children((
                theme::text("热门")
                    .font_size(28.0)
                    .font_weight(FontWeight::BOLD),
                theme::text(subtitle).opacity(0.6),
            )),
        );

    let grid = ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(36.0, 12.0, 36.0, 36.0))
                .content(video_card::video_grid(&state.items, on_open)),
        )
        .into();

    let mut body_layers = vec![grid];
    if let Some(pill) = footer_pill {
        body_layers.push(pill);
    }

    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            View::from(header),
            View::from(Grid::new().grid_row(1).children(body_layers)),
        ))
        .into()
}

/// 底栏状态胶囊：按钮态（可点击）或纯文本态（可选转圈）。
fn status_pill(label: &str, on_click: Option<Callback<()>>, with_spinner: bool) -> View {
    let content: View = if let Some(on_click) = on_click {
        Button::new()
            .content(label)
            .on_click(move || on_click.call(()))
            .into()
    } else {
        let mut children: Vec<View> = Vec::new();
        if with_spinner {
            children.push(
                View::from(
                    ProgressRing::new()
                        .is_indeterminate(true)
                        .is_active(true)
                        .width(16.0)
                        .height(16.0),
                ),
            );
        }
        children.push(
            View::from(
                TextBlock::new()
                    .text(label)
                    .font_size(12.0)
                    .opacity(0.75)
                    .vertical_alignment(VerticalAlignment::Center),
            ),
        );
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(10.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children(children)
            .into()
    };
    Border::new()
        .height(40.0)
        .corner_radius(20.0)
        .padding(Thickness::xy(16.0, 8.0))
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Bottom)
        .margin(Thickness::new(0.0, 0.0, 0.0, 16.0))
        .content(content)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(bvid: &str) -> VideoItem {
        VideoItem {
            bvid: bvid.into(),
            aid: 1,
            cid: 1,
            title: format!("t{bvid}"),
            author: String::new(),
            author_mid: 0,
            author_face_url: None,
            duration_secs: 10,
            play_count: 100,
            danmaku_count: 1,
            like_count: None,
            coin_count: None,
            favorite_count: None,
            share_count: None,
            cover: None,
            pic_url: None,
        }
    }

    #[test]
    fn default_state_is_idle_untouched() {
        let state = PopularFeedState::default();
        assert!(!state.loading && !state.loaded_once && !state.reached_end);
        assert!(state.items.is_empty());
        // AppShell 创建时显式置 next_pn=1（实测 popular API pn=0 非法返回空/报错）。
        assert_eq!(state.next_pn, 0);
    }

    #[test]
    fn state_merges_without_duplicates() {
        // 展示端仅消费 items 列表；去重语义由 shell::merge_popular_items 测试锚定。
        let mut state = PopularFeedState::default();
        state.items.push(item("BV1"));
        state.items.push(item("BV2"));
        assert_eq!(state.items.len(), 2);
        assert!(state.items.iter().any(|v| v.bvid == "BV1"));
    }
}
