//! 登录态列表页（U5 接线：历史 / 稍后再看 / 收藏）。
//!
//! 数据由 `AppShell` 单源持有（`UserListState` / `FavPageState`）；封面经
//! `ImageCache` 异步回填（与直播卡同款 LRU + 有界并发）；点击卡片 → shell
//! `open_video` 数据源链（user_index 命中即带标题/封面进播放页）。
//! 三个页面共用同一套卡片网格；未登录由 shell 渲染 `login_gate_page`。

use windows_reactor::*;

use crate::layout::ViewportLayout;
use crate::theme;

/// 统一展示卡：核心字段 + 已解码封面 + 页面专属元信息行。
#[derive(Clone, Debug, PartialEq)]
pub struct UserVideoCard {
    pub bvid: String,
    pub title: String,
    pub cover_url: String,
    pub cover: Option<EncodedImage>,
    pub up_name: String,
    pub duration_secs: u32,
    pub play_count: u64,
    /// 观看进度（秒；`-1` = 看完）。仅历史页下发。
    pub progress_secs: Option<i64>,
    /// 副行文本（观看时间 / 加入时间 / 收藏时间 + UP 主）。
    pub meta_line: String,
    /// 进度条填充比例（0..1；`None` 不渲染条）。
    pub progress_ratio: Option<f64>,
    /// 进度角标文本（「已看完」/「看到 12:34」）；无则不渲染。
    pub progress_badge: Option<String>,
}

/// 列表页展示状态（历史与稍后再看共用；收藏的资源列表复用同构）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserListState {
    pub loading: bool,
    pub loaded_once: bool,
    pub error: Option<String>,
    pub cards: Vec<UserVideoCard>,
    /// 下一次请求页号（成功才推进）。
    pub next_page: u32,
    pub reached_end: bool,
    /// 服务端总数（稍后再看 count；历史端点无总数则 `None`）。
    pub total: Option<u64>,
}

impl UserListState {
    /// 页头副标题（加载态/错误态/计数态统一口径，与直播/热门页同款）。
    pub fn subtitle(&self, empty_hint: &str) -> String {
        if let Some(err) = &self.error {
            if self.loaded_once {
                format!("{} 条 · 下一页加载失败：{err}", self.cards.len())
            } else {
                format!("加载失败：{err}")
            }
        } else if self.loading && self.cards.is_empty() {
            "加载中…".to_string()
        } else if !self.loaded_once {
            empty_hint.to_string()
        } else if let Some(total) = self.total {
            format!("{} 条 · 共 {total}", self.cards.len())
        } else {
            format!("{} 条", self.cards.len())
        }
    }
}

/// 收藏夹 chip 行的展示模型（`id` = media_id）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavFolderChip {
    pub id: u64,
    pub title: String,
    pub media_count: u64,
}

/// 收藏页展示状态（夹列表 → 资源列表两级）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FavPageState {
    pub folders_loading: bool,
    pub folders_loaded: bool,
    pub folders_error: Option<String>,
    pub folders: Vec<FavFolderChip>,
    /// 当前选中夹（默认首夹；0 = 未选）。
    pub selected: u64,
    /// 当前夹的资源列表（与历史页同构翻页）。
    pub resources: UserListState,
}

/// 未登录门控页：标题 + 说明 + 登录按钮（跳 Route::Login）。
pub fn login_gate_page(title: &str, hint: &str, layout: ViewportLayout, on_login: &Callback<()>) -> View {
    let margin_x = layout.page_margin_x();
    let top = if layout.is_mobile() { 12.0 } else { 24.0 };
    let login_cb = on_login.clone();
    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            Border::new()
                .padding(Thickness::new(margin_x, top, margin_x, 12.0))
                .grid_row(0)
                .content(
                    theme::text(title)
                        .font_size(layout.page_title_size())
                        .font_weight(FontWeight::BOLD),
                ),
            StackPanel::new()
                .grid_row(1)
                .spacing(12.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    theme::text(hint).font_size(13.0).opacity(0.6),
                    Button::new()
                        .content("去登录")
                        .on_click(move || login_cb.call(())),
                )),
        ))
        .into()
}

/// 通用列表页骨架（历史/稍后再看：标题行 + 卡片网格 + 底部分页胶囊）。
pub fn list_page(
    state: &UserListState,
    title: &str,
    empty_hint: &str,
    layout: ViewportLayout,
    on_open: &Callback<String>,
    on_more: &Callback<()>,
) -> View {
    let subtitle = state.subtitle(empty_hint);
    let footer_pill: Option<View> = if state.error.is_some() && state.loaded_once {
        Some(status_pill("重试", Some(on_more.clone()), false))
    } else if state.reached_end && !state.cards.is_empty() {
        Some(status_pill("没有更多了", None, false))
    } else if state.loading && !state.cards.is_empty() {
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
                theme::text(title)
                    .font_size(layout.page_title_size())
                    .font_weight(FontWeight::BOLD),
                theme::text(subtitle)
                    .font_size(if layout.is_mobile() { 12.0 } else { 13.0 })
                    .opacity(0.6),
            )),
        );

    let mut content_items: Vec<View> = vec![cards_grid(&state.cards, layout.cards, on_open)];
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

pub fn history_page(
    state: &UserListState,
    layout: ViewportLayout,
    on_open: &Callback<String>,
    on_more: &Callback<()>,
) -> View {
    list_page(state, "历史", "暂无历史记录", layout, on_open, on_more)
}

pub fn watch_later_page(
    state: &UserListState,
    layout: ViewportLayout,
    on_open: &Callback<String>,
    on_more: &Callback<()>,
) -> View {
    list_page(state, "稍后再看", "暂无内容", layout, on_open, on_more)
}

/// 收藏页：夹 chip 横排（标题行下）+ 当前夹资源网格 + 分页胶囊。
pub fn favorites_page(
    state: &FavPageState,
    layout: ViewportLayout,
    on_select_folder: &Callback<u64>,
    on_open: &Callback<String>,
    on_more: &Callback<()>,
) -> View {
    let subtitle = state.resources.subtitle("选择收藏夹查看内容");
    let footer_pill: Option<View> = if state.resources.error.is_some() && state.resources.loaded_once
    {
        Some(status_pill("重试", Some(on_more.clone()), false))
    } else if state.resources.reached_end && !state.resources.cards.is_empty() {
        Some(status_pill("没有更多了", None, false))
    } else if state.resources.loading && !state.resources.cards.is_empty() {
        Some(status_pill("加载更多…", None, true))
    } else if state.resources.loaded_once {
        Some(status_pill("加载更多", Some(on_more.clone()), false))
    } else {
        None
    };

    let margin_x = layout.page_margin_x();
    let header_top = if layout.is_mobile() { 12.0 } else { 24.0 };

    let mut header_stack: Vec<View> = vec![
        View::from(
            theme::text("收藏")
                .font_size(layout.page_title_size())
                .font_weight(FontWeight::BOLD),
        ),
        View::from(
            theme::text(subtitle)
                .font_size(if layout.is_mobile() { 12.0 } else { 13.0 })
                .opacity(0.6),
        ),
    ];
    if state.folders_error.is_some() {
        header_stack.push(View::from(
            theme::text(format!(
                "收藏夹加载失败：{}",
                state.folders_error.as_deref().unwrap_or_default()
            ))
            .font_size(12.0)
            .opacity(0.75),
        ));
    }
    if !state.folders.is_empty() {
        let chips: Vec<KeyedView> = state
            .folders
            .iter()
            .map(|folder| {
                let selected = folder.id == state.selected;
                let payload = folder.id;
                let on_click = {
                    let cb = on_select_folder.clone();
                    move || cb.call(payload)
                };
                KeyedView::new(
                    format!("fav-folder:{}", folder.id),
                    Button::new()
                        .style(if selected {
                            ButtonStyle::Default
                        } else {
                            ButtonStyle::Subtle
                        })
                        .content(format!("{} ({})", folder.title, folder.media_count))
                        .on_click(on_click),
                )
            })
            .collect();
        header_stack.push(
            ScrollViewer::new()
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Hidden)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .margin(Thickness::new(0.0, 4.0, 0.0, 0.0))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .keyed_children(chips),
                )
                .into(),
        );
    } else if state.folders_loading {
        header_stack.push(View::from(
            theme::text("收藏夹加载中…").font_size(12.0).opacity(0.6),
        ));
    }

    let header = Border::new()
        .padding(Thickness::new(margin_x, header_top, margin_x, 12.0))
        .grid_row(0)
        .content(StackPanel::new().spacing(4.0).children(header_stack));

    let mut content_items: Vec<View> =
        vec![cards_grid(&state.resources.cards, layout.cards, on_open)];
    if let Some(pill) = footer_pill {
        content_items.push(pill);
    }
    if state.resources.loaded_once && state.resources.cards.is_empty() && !state.resources.loading {
        content_items.push(View::from(
            theme::text("该收藏夹暂无视频")
                .font_size(13.0)
                .opacity(0.55)
                .horizontal_alignment(HorizontalAlignment::Center),
        ));
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

/// 卡片网格：按列数精确分行（与 live/热门页同款防折行策略）。
pub fn cards_grid(
    cards: &[UserVideoCard],
    metrics: crate::layout::CardMetrics,
    on_open: &Callback<String>,
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
                        format!("uv:{}", card.bvid),
                        user_card(card, on_open, metrics.card_width),
                    )
                })
                .collect();
            KeyedView::new(
                format!("uv-row:{row_idx}:{}", cols),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(metrics.gap)
                    .keyed_children(cells),
            )
        })
        .collect();
    StackPanel::new().spacing(metrics.gap).keyed_children(rows).into()
}

fn user_card(card: &UserVideoCard, on_open: &Callback<String>, card_width: f64) -> View {
    let cover_h = (card_width * 9.0 / 16.0).round();
    let payload = card.bvid.clone();
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

    // 封面层:右下时长徽标 + 可选进度角标(左下)+ 进度条(底部)。
    let mut cover_overlays: Vec<View> = Vec::new();
    cover_overlays.push(
        Border::new()
            .margin(Thickness::new(0.0, 0.0, 8.0, 8.0))
            .horizontal_alignment(HorizontalAlignment::Right)
            .vertical_alignment(VerticalAlignment::Bottom)
            .corner_radius(10.0)
            .padding(Thickness::xy(7.0, 2.0))
            .background(Brush::Solid(Color::argb(170, 0, 0, 0)))
            .content(
                TextBlock::new()
                    .text(theme::fmt_duration(card.duration_secs))
                    .font_size(10.5)
                    .foreground(Brush::Solid(Color::rgb(255, 255, 255))),
            )
            .into(),
    );
    if let Some(badge) = &card.progress_badge {
        cover_overlays.push(
            Border::new()
                .margin(Thickness::new(8.0, 0.0, 0.0, 8.0))
                .horizontal_alignment(HorizontalAlignment::Left)
                .vertical_alignment(VerticalAlignment::Bottom)
                .corner_radius(10.0)
                .padding(Thickness::xy(7.0, 2.0))
                .background(Brush::Solid(Color::argb(170, 0, 0, 0)))
                .content(
                    TextBlock::new()
                        .text(badge.clone())
                        .font_size(10.5)
                        .foreground(Brush::Solid(Color::rgb(255, 255, 255))),
                )
                .into(),
        );
    }
    let progress_bar: View = match card.progress_ratio {
        Some(ratio) => ProgressBar::new()
            .minimum(0.0)
            .maximum(1.0)
            .value(ratio.clamp(0.0, 1.0))
            .height(3.0)
            .vertical_alignment(VerticalAlignment::Bottom)
            .into(),
        None => Border::new().height(0.0).into(),
    };
    let cover_layer = Grid::new().children((
        cover,
        StackPanel::new()
            .grid_row(0)
            .vertical_alignment(VerticalAlignment::Bottom)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .children(cover_overlays),
        progress_bar,
    ));

    // 封面 + 文本区;Border 点击释放(避开 ResourceOverrides 崩溃,与 live 卡同)。
    Border::new()
        .width(card_width)
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .on_pointer_released(move |_| on_click())
        .content(
            StackPanel::new().children((
                cover_layer,
                StackPanel::new()
                    .margin(Thickness::xy(10.0, 8.0))
                    .spacing(3.0)
                    .children((
                        TextBlock::new()
                            .text(card.title.clone())
                            .font_size(12.5)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(2),
                        TextBlock::new()
                            .text(card.meta_line.clone())
                            .font_size(11.0)
                            .opacity(0.65)
                            .text_trimming(TextTrimming::CharacterEllipsis)
                            .max_lines(1),
                    )),
            )),
        )
        .into()
}

/// 底部分页胶囊（与直播页同款）。
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

/// 历史/稍后再看行 → 展示卡(shell 侧组装;测试锚定进度/角标口径)。
pub fn card_from_watch(
    item: &qaq_bx_core::history::HistoryItem,
    time_label: &str,
    cover: Option<EncodedImage>,
) -> UserVideoCard {
    let finished = item.is_finished();
    let (badge, ratio) = if finished {
        (Some("已看完".to_string()), Some(1.0))
    } else if item.progress_secs > 0 && item.duration_secs > 0 {
        let secs = item.progress_secs.min(item.duration_secs);
        (
            Some(format!("看到 {}", theme::fmt_duration(secs as u32))),
            Some(secs as f64 / item.duration_secs as f64),
        )
    } else {
        (None, None)
    };
    let mut meta = String::new();
    if !time_label.is_empty() {
        meta.push_str(time_label);
    }
    if !item.up_name.is_empty() {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&item.up_name);
    }
    if item.page_count > 1 {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&format!("P{}/{}", item.page, item.page_count));
    }
    UserVideoCard {
        bvid: item.bvid.clone(),
        title: if item.title.is_empty() {
            item.bvid.clone()
        } else {
            item.title.clone()
        },
        cover_url: item.cover.clone(),
        cover,
        up_name: item.up_name.clone(),
        duration_secs: item.duration_secs.clamp(0, u32::MAX as i64) as u32,
        play_count: item.views,
        progress_secs: Some(item.progress_secs),
        meta_line: meta,
        progress_ratio: ratio,
        progress_badge: badge,
    }
}

/// 收藏视频行 → 展示卡。
pub fn card_from_fav(
    media: &qaq_bx_core::fav::FavMedia,
    time_label: &str,
    cover: Option<EncodedImage>,
) -> UserVideoCard {
    let mut meta = String::new();
    if !time_label.is_empty() {
        meta.push_str(time_label);
    }
    if !media.up_name.is_empty() {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&media.up_name);
    }
    UserVideoCard {
        bvid: media.bvid.clone(),
        title: if media.title.is_empty() {
            media.bvid.clone()
        } else {
            media.title.clone()
        },
        cover_url: media.cover.clone(),
        cover,
        up_name: media.up_name.clone(),
        duration_secs: media.duration_secs.clamp(0, u32::MAX as i64) as u32,
        play_count: media.plays,
        progress_secs: None,
        meta_line: meta,
        progress_ratio: None,
        progress_badge: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(bvid: &str) -> UserVideoCard {
        UserVideoCard {
            bvid: bvid.to_string(),
            title: format!("标题{bvid}"),
            cover_url: String::new(),
            cover: None,
            up_name: "UP".into(),
            duration_secs: 300,
            play_count: 100,
            progress_secs: None,
            meta_line: "昨天 · UP".into(),
            progress_ratio: None,
            progress_badge: None,
        }
    }

    fn watch(bvid: &str, progress: i64, duration: i64) -> qaq_bx_core::history::HistoryItem {
        qaq_bx_core::history::HistoryItem {
            aid: 1,
            bvid: bvid.to_string(),
            cid: 1,
            title: format!("标题{bvid}"),
            cover: String::new(),
            duration_secs: duration,
            progress_secs: progress,
            viewed_at: 1_795_000_000,
            added_at: None,
            page: 1,
            page_count: 1,
            up_name: "UP".into(),
            up_mid: 2,
            views: 100,
            danmakus: 5,
        }
    }

    #[test]
    fn default_state_is_idle() {
        let state = UserListState::default();
        assert!(!state.loading && !state.loaded_once && !state.reached_end);
        assert_eq!(state.next_page, 0);
        assert!(state.total.is_none());
        assert_eq!(state.subtitle("空"), "空");
    }

    #[test]
    fn subtitle_reflects_count_total_and_error() {
        let mut state = UserListState::default();
        state.cards.push(card("BV1"));
        state.loaded_once = true;
        assert_eq!(state.subtitle("空"), "1 条");
        state.total = Some(208);
        assert_eq!(state.subtitle("空"), "1 条 · 共 208");
        state.error = Some("网络错误".into());
        assert_eq!(state.subtitle("空"), "1 条 · 下一页加载失败：网络错误");
    }

    #[test]
    fn grids_render_with_and_without_progress_without_panic() {
        let layout = ViewportLayout::default();
        let cb = Callback::new(|_: String| {});
        let _ = cards_grid(&[], layout.cards, &cb);
        let mut c1 = card("BV1");
        c1.progress_ratio = Some(0.4);
        c1.progress_badge = Some("看到 02:00".into());
        let c2 = card("BV2");
        let mut c3 = card("BV3");
        c3.cover = Some(EncodedImage::new(vec![1, 2, 3]));
        let _ = cards_grid(&[c1, c2, c3], layout.cards, &cb);
    }

    #[test]
    fn login_gate_renders_and_routes() {
        let layout = ViewportLayout::default();
        let cb = Callback::new(|_: ()| {});
        let _ = login_gate_page("历史", "登录后查看", layout, &cb);
    }

    #[test]
    fn watch_card_progress_semantics() {
        // 看到一半
        let c = card_from_watch(&watch("BV1", 120, 300), "昨天", None);
        assert_eq!(c.progress_badge.as_deref(), Some("看到 02:00"));
        assert!((c.progress_ratio.unwrap() - 0.4).abs() < 1e-9);
        // 看完(-1 哨兵)
        let c = card_from_watch(&watch("BV1", -1, 300), "昨天", None);
        assert_eq!(c.progress_badge.as_deref(), Some("已看完"));
        assert_eq!(c.progress_ratio, Some(1.0));
        // 超进度钳满
        let c = card_from_watch(&watch("BV1", 400, 300), "昨天", None);
        assert_eq!(c.progress_ratio, Some(1.0));
        // 未开始(0 秒)无条
        let c = card_from_watch(&watch("BV1", 0, 300), "昨天", None);
        assert!(c.progress_badge.is_none() && c.progress_ratio.is_none());
        // 多P 进副行
        let mut it = watch("BV1", 10, 300);
        it.page_count = 5;
        it.page = 2;
        let c = card_from_watch(&it, "", None);
        assert!(c.meta_line.contains("P2/5"), "{}", c.meta_line);
        // 时长溢出钳位
        let mut it = watch("BV1", 10, i64::MAX);
        it.page_count = 1;
        assert_eq!(c_duration(&it), u32::MAX);
        fn c_duration(it: &qaq_bx_core::history::HistoryItem) -> u32 {
            card_from_watch(it, "", None).duration_secs
        }
    }

    #[test]
    fn fav_card_carries_meta_without_progress() {
        let media = qaq_bx_core::fav::FavMedia {
            id: 1,
            bvid: "BV1fav".into(),
            title: "收藏视频".into(),
            cover: String::new(),
            duration_secs: 57,
            up_name: "UP".into(),
            up_mid: 2,
            plays: 999,
            collects: 12,
            danmakus: 3,
            fav_at: 1_795_000_000,
        };
        let c = card_from_fav(&media, "3 天前", None);
        assert_eq!(c.bvid, "BV1fav");
        assert!(c.progress_ratio.is_none() && c.progress_badge.is_none());
        assert_eq!(c.play_count, 999);
        assert!(c.meta_line.contains("3 天前"));
    }

    #[test]
    fn favorites_page_renders_folders_and_empty_folder_notice() {
        let layout = ViewportLayout::default();
        let on_folder = Callback::new(|_: u64| {});
        let on_open = Callback::new(|_: String| {});
        let on_more = Callback::new(|_: ()| {});
        let mut state = FavPageState::default();
        state.folders = vec![
            FavFolderChip { id: 1, title: "默认收藏夹".into(), media_count: 7 },
            FavFolderChip { id: 2, title: "TEMP".into(), media_count: 153 },
        ];
        state.selected = 1;
        state.resources.loaded_once = true;
        let _ = favorites_page(&state, layout, &on_folder, &on_open, &on_more);
        state.resources.loading = false;
        let _ = favorites_page(&state, layout, &on_folder, &on_open, &on_more);
    }
}
