//! 搜索页：综合搜索结果网格。
//!
//! 搜索入口全站唯一——标题栏全局内敛搜索框(输入+按钮一体,Enter 即提交),
//! 本页不再放置页内输入框。状态由 `AppShell` 单源持有(`SearchUiState`);
//! 结果卡复用 `video_grid`,封面经 `ImageCache` 异步填充(`CoverReady` 兜底回填)。
//! 空态/错误态就地展示。

use crate::data::VideoItem;
use crate::theme;
use crate::widgets::video_card;
use windows_reactor::*;

/// 搜索页展示状态(`AppShell` 单源持有)。
#[derive(Clone, Default)]
pub struct SearchUiState {
    /// 全局搜索框草稿(每次 TextChanged 更新;本页只读展示)。
    pub draft: String,
    /// 已提交查询(当前结果对应的词;空串表示尚未搜索)。
    pub query: String,
    pub loading: bool,
    pub error: Option<String>,
    pub videos: Vec<VideoItem>,
    /// 服务端报告的总命中数(`numResults`)。
    pub total: Option<u64>,
}

pub fn search_page(state: &SearchUiState, on_open: &Callback<String>, compact: bool) -> View {
    let margin_x = if compact { 12.0 } else { 36.0 };

    // 页头只留标题与当前查询上下文;发起/修改关键词一律走标题栏全局搜索框。
    let header = Border::new()
        .padding(Thickness::new(margin_x, 18.0, margin_x, 12.0))
        .grid_row(0)
        .content(
            StackPanel::new().spacing(4.0).children((
                theme::text("搜索")
                    .font_size(28.0)
                    .font_weight(FontWeight::BOLD),
                theme::text("使用顶部搜索框搜索视频、UP 主,回车即可").font_size(12.0).opacity(0.55),
            )),
        );

    let body: View = if state.loading && state.videos.is_empty() {
        ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(28.0)
            .height(28.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    } else if let Some(err) = &state.error {
        StackPanel::new()
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                TextBlock::new().text(format!("搜索失败：{err}")).opacity(0.8),
                TextBlock::new()
                    .text("可调整关键词后重新搜索")
                    .font_size(12.0)
                    .opacity(0.6),
            ))
            .into()
    } else if state.videos.is_empty() {
        let hint = if state.query.is_empty() {
            "输入关键词搜索视频".to_string()
        } else {
            format!("没有找到与「{}」相关的内容", state.query)
        };
        TextBlock::new()
            .text(hint)
            .opacity(0.6)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    } else {
        let subtitle = match state.total {
            Some(total) => format!("「{}」约 {} 个结果", state.query, theme::fmt_count(total)),
            None => format!("「{}」的结果", state.query),
        };
        ScrollViewer::new()
            .content(
                StackPanel::new().spacing(8.0).children((
                    theme::text(subtitle).opacity(0.6),
                    video_card::video_grid(&state.videos, on_open),
                )),
            )
            .into()
    };

    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            View::from(header),
            View::from(Grid::new().grid_row(1).children(vec![body])),
        ))
        .into()
}
