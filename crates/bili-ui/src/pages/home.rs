//! 推荐页：页头（含“换一换”）+ 骨架槽位自适应网格 + 轻量底栏状态条。
//!
//! 状态由 `AppShell` 单源持有（`FeedSession` 逻辑页聚合 + 槽位填充 + 终态锁定）。
//! 无感加载：通过 `ScrollViewer::on_view_changed` 精确计算可视区底部条数与滚动速度，
//! 在接近当前页半程前先分配骨架框架并异步拉取下一逻辑页，再原地填数据并批刷封面。

use windows_reactor::*;

use crate::widgets::video_card;

/// 推荐页展示状态（`AppShell` 单源持有）。
#[derive(Clone)]
pub struct HomeState {
    /// 流代次（换一换递增，驱动 `ScrollViewer` 键控回到顶部；增量翻页不变）。
    pub stream_gen: u64,
    /// 当前自适应列数（用于按行虚拟化分组与行键版本控制）。
    pub grid_cols: usize,
    /// 请求进行中（首屏/换一换/翻页共用）。
    pub loading: bool,
    /// 上次请求失败原因（`Some` 时底栏显示重试按钮并暂停自动预取）。
    pub error: Option<String>,
    /// 已成功加载的逻辑页数（含首屏）。
    pub page_count: u32,
    /// 当前已填入的真实去重视频总数（不含未填充骨架槽位）。
    pub video_count: usize,
    /// 流耗尽标记（`true` 时定格为最后一页、收起占位骨架并停止自动续拉）。
    pub reached_end: bool,
}

impl Default for HomeState {
    fn default() -> Self {
        Self {
            stream_gen: 1,
            grid_cols: 4,
            loading: false,
            error: None,
            page_count: 0,
            video_count: 0,
            reached_end: false,
        }
    }
}

const FOOTER_HEIGHT: f64 = 40.0;

pub fn home_page(
    source: &VirtualSource,
    state: &HomeState,
    on_refresh: &Callback<()>,
    on_retry: &Callback<()>,
) -> View {
    let subtitle = if let Some(err) = &state.error {
        format!("加载失败：{err}")
    } else if state.loading && state.video_count == 0 {
        "正在预热首屏推荐…".to_string()
    } else if state.reached_end {
        format!(
            "{} 个视频 · 共 {} 页（已加载最后一页）",
            state.video_count,
            state.page_count.max(1)
        )
    } else if state.loading {
        format!(
            "{} 个视频 · 已加载 {} 页 · 正在预取下一逻辑页…",
            state.video_count, state.page_count
        )
    } else {
        format!(
            "{} 个视频 · 已加载 {} 页（去重后）",
            state.video_count, state.page_count
        )
    };

    let refresh_btn = {
        let on_refresh = on_refresh.clone();
        Button::new()
            .content("换一换")
            .vertical_alignment(VerticalAlignment::Center)
            .on_click(move || on_refresh.call(()))
    };

    let footer_pill: Option<View> = if state.error.is_some() {
        let on_retry = on_retry.clone();
        Some(
            Border::new()
                .height(FOOTER_HEIGHT)
                .corner_radius(20.0)
                .padding(Thickness::xy(16.0, 6.0))
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Bottom)
                .margin(Thickness::new(0.0, 0.0, 0.0, 16.0))
                .content(
                    Button::new()
                        .content("加载失败，点击重试")
                        .on_click(move || on_retry.call(())),
                )
                .into(),
        )
    } else if state.reached_end {
        Some(
            Border::new()
                .height(FOOTER_HEIGHT)
                .corner_radius(20.0)
                .padding(Thickness::xy(16.0, 8.0))
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Bottom)
                .margin(Thickness::new(0.0, 0.0, 0.0, 16.0))
                .content(
                    TextBlock::new()
                        .text(format!(
                            "已经到底了 · 共 {} 个视频（第 {} 页为最后一页）",
                            state.video_count,
                            state.page_count.max(1)
                        ))
                        .font_size(12.0)
                        .opacity(0.75)
                        .vertical_alignment(VerticalAlignment::Center),
                )
                .into(),
        )
    } else if state.loading && state.video_count > 0 {
        Some(
            Border::new()
                .height(FOOTER_HEIGHT)
                .corner_radius(20.0)
                .padding(Thickness::xy(16.0, 8.0))
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Bottom)
                .margin(Thickness::new(0.0, 0.0, 0.0, 16.0))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(10.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            View::from(
                                ProgressRing::new()
                                    .is_indeterminate(true)
                                    .is_active(true)
                                    .width(16.0)
                                    .height(16.0),
                            ),
                            View::from(
                                TextBlock::new()
                                    .text("正在准备更多推荐…")
                                    .font_size(12.0)
                                    .opacity(0.75),
                            ),
                        )),
                )
                .into(),
        )
    } else {
        None
    };

    let header = Border::new()
        .padding(Thickness::new(36.0, 24.0, 36.0, 12.0))
        .grid_row(0)
        .content(StackPanel::new().spacing(4.0).children((
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    TextBlock::new()
                        .text("推荐")
                        .font_size(28.0)
                        .font_weight(FontWeight::BOLD)
                        .vertical_alignment(VerticalAlignment::Center),
                    View::from(refresh_btn.grid_column(1)),
                )),
            TextBlock::new().text(subtitle).opacity(0.6),
        )));

    // ScrollViewer 直接承载 ItemsRepeater 且不订阅 ViewChanged，防止 WinUI 滚动队列阻塞同步虚拟化测量。
    let scroll_host: View = Grid::new()
        .keyed_children([keyed(
            format!("home-scroll-{}", state.stream_gen),
            ScrollViewer::new().content(video_card::virtual_video_grid(source)),
        )])
        .into();

    let mut body_layers = vec![scroll_host];
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
