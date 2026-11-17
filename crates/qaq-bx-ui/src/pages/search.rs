//! 搜索页：综合搜索结果网格。
//!
//! 桌面全态（`>= 860` DIP）统一走标题栏全局内敛搜索框(输入+按钮一体,Enter 即提交),
//! 页内不再放置重复输入框；窄屏（`< 860` DIP）标题栏隐藏内嵌搜索框时保留页内搜索栏。
//! 状态由 `AppShell` 单源持有(`SearchUiState`);结果卡复用 `video_grid`,
//! 封面经 `ImageCache` 异步填充(`CoverReady` 兜底回填)。空态/错误态就地展示。

use std::rc::Rc;

use crate::data::VideoItem;
use crate::layout::ViewportLayout;
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

pub fn search_page(
    state: &SearchUiState,
    on_draft: &Callback<Rc<str>>,
    on_submit: &Callback<()>,
    on_open: &Callback<String>,
    layout: ViewportLayout,
) -> View {
    let margin_x = layout.page_margin_x();
    let header_top = if layout.is_mobile() { 12.0 } else { 18.0 };

    let header_sub: View = if layout.shows_inline_title_search() {
        // 桌面全态统一走标题栏全局内敛搜索框，页内仅展示提示行。
        theme::text("使用顶部搜索框搜索视频、UP 主，回车即可")
            .font_size(12.0)
            .opacity(0.55)
            .into()
    } else {
        let on_enter = {
            let on_submit = on_submit.clone();
            RoutedCallback::new(move |ev: KeyEventInfo| {
                if crate::shell::search_enter_submits(&ev) {
                    on_submit.call(());
                    true
                } else {
                    false
                }
            })
        };
        let submit_btn = {
            let on_submit = on_submit.clone();
            Button::new()
                .style(ButtonStyle::Subtle)
                .on_click(move || on_submit.call(()))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children((
                            View::from(SymbolIcon::new().symbol(Symbol::Find)),
                            View::from(TextBlock::new().text("搜索")),
                        )),
                )
        };
        Border::new()
            .on_preview_key_down(on_enter)
            .content(
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        View::from(
                            TextBox::new()
                                .placeholder_text("搜索视频、UP 主")
                                .text(state.draft.clone())
                                .on_text_changed({
                                    let on_draft = on_draft.clone();
                                    move |text: Rc<str>| on_draft.call(text)
                                }),
                        ),
                        View::from(submit_btn.grid_column(1)),
                    )),
            )
            .into()
    };

    let header = Border::new()
        .padding(Thickness::new(margin_x, header_top, margin_x, 12.0))
        .grid_row(0)
        .content(
            StackPanel::new()
                .spacing(if layout.shows_inline_title_search() { 4.0 } else { 10.0 })
                .children((
                    theme::text("搜索")
                        .font_size(layout.page_title_size())
                        .font_weight(FontWeight::BOLD),
                    header_sub,
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
                Border::new()
                    .padding(Thickness::new(margin_x, 4.0, margin_x, 36.0))
                    .content(
                        StackPanel::new().spacing(8.0).children((
                            theme::text(subtitle)
                                .font_size(if layout.is_mobile() { 12.0 } else { 13.0 })
                                .opacity(0.6),
                            video_card::video_grid(&state.videos, layout.cards, on_open),
                        )),
                    ),
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
