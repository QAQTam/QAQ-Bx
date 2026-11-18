//! 应用壳外框渲染件（从 `shell.rs` 拆出，遵循低耦合约束）：
//! - 响应式 `TitleBar`（Expanded 内嵌搜索框 / Medium 搜索浮窗按钮 / Mobile 仅通知铃铛）；
//! - 悬浮搜索条（Medium 窄桌面一键呼出，提交后跳 `Route::Search`）；
//! - Mobile 底部 5-Tab 导航栏与「更多」2×3 悬浮网格（含 6 个扩展目的地）。

use std::rc::Rc;

use windows_reactor::*;

use crate::layout::ViewportLayout;
use crate::router::Route;

/// Mobile 底栏「更多」悬浮网格目的地（6 项：补全 WatchLater，2×3 整齐排列）。
pub const COMPACT_MORE_ROUTES: [Route; 6] = [
    Route::Following,
    Route::Live,
    Route::Favorites,
    Route::History,
    Route::WatchLater,
    Route::Settings,
];

pub fn title_search_box_width(window_width: f64) -> f64 {
    if window_width.is_finite() {
        (window_width - 620.0).clamp(200.0, 360.0)
    } else {
        360.0
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_title_bar(
    layout: ViewportLayout,
    title_bar_ref: &ElementRef<TitleBar>,
    portrait_mode: bool,
    on_toggle_dock: Callback<()>,
    app_icon_png: &'static [u8],
    app_name: &str,
    route_subtitle: &str,
    can_back: bool,
    logged_in: bool,
    total_unread: u64,
    search_draft: &str,
    // 搜索默认词(U2;None → 通用占位)。
    search_default_word: Option<&str>,
    on_back: Callback<()>,
    on_toggle_pane: Callback<()>,
    on_search_draft: Callback<Rc<str>>,
    on_search_submit: Callback<()>,
    on_toggle_search_overlay: Callback<()>,
    on_open_notifications: Callback<()>,
) -> TitleBar {
    let mut bar = TitleBar::new()
        .preferred_height(WindowTitleBarHeight::Tall)
        // 拖框卡顿治理(perf-architecture-review §8.2):关掉 WinUI TitleBar 控件
        // 的逐布局 pass 拖拽区全树重算;控件仍会在自身尺寸/属性变化时刷新,
        // 覆盖我们档位切换(搜索框进/出 Content)的主要路径。
        .auto_refresh_drag_regions(false)
        // §8.2 收口:声明面挂 element_ref,shell 在内容结构档位翻转(窗口静止后)
        // 经此句柄补算一次拖拽区,兜住不经 TitleBar 自身尺寸/属性变化的残余路径。
        .element_ref(title_bar_ref)
        .icon(Icon::image_data(EncodedImage::from_static(app_icon_png)))
        .title(app_name)
        .subtitle(if layout.is_mobile() {
            ""
        } else {
            route_subtitle
        })
        .is_back_button_visible(true)
        .is_back_button_enabled(can_back)
        .is_pane_toggle_button_visible(!layout.is_mobile())
        .on_back_requested(on_back)
        .on_pane_toggle_requested(on_toggle_pane)
        .grid_row(0);

    if layout.shows_inline_title_search() {
        // 全局内敛搜索框：Border 壳 + 无边框透明 TextBox + 内嵌放大镜 Button，PreviewKeyDown Enter 即提交。
        let search_w = title_search_box_width(layout.window_width);
        let on_enter = {
            let on_submit = on_search_submit.clone();
            RoutedCallback::new(move |ev: KeyEventInfo| {
                if crate::shell::search_enter_submits(&ev) {
                    on_submit.call(());
                    true
                } else {
                    false
                }
            })
        };
        bar = bar.content(
            Border::new()
                .width(search_w)
                .height(32.0)
                .corner_radius(6.0)
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .padding(Thickness::xy(2.0, 1.0))
                .on_preview_key_down(on_enter)
                .content(
                    Grid::new()
                        .columns([GridLength::Star(1.0), GridLength::Auto])
                        .children((
                            TextBox::new()
                                .grid_column(0)
                                .placeholder_text(search_default_word.unwrap_or("搜索视频、UP 主"))
                                .text(search_draft)
                                .background(Brush::Solid(Color::argb(0, 0, 0, 0)))
                                .border_thickness(0.0)
                                .on_text_changed(on_search_draft),
                            Button::new()
                                .grid_column(1)
                                .style(ButtonStyle::Subtle)
                                .height(26.0)
                                .vertical_alignment(VerticalAlignment::Center)
                                .on_click(on_search_submit)
                                .content(SymbolIcon::new().symbol(Symbol::Find)),
                        )),
                ),
        );
    }

    let mut right_items: Vec<View> = Vec::new();
    if portrait_mode {
        // 竖屏模式:系统最大化钮已被 presenter 级没收(SetIsMaximizable(false),
        // 控件将其置灰),这里补「贴边最大化」钮——右缘贴边 × 工作区全高,再点还原。
        // 放 right_items 最右(紧邻 caption 钮);right_header 是单值 relation,
        // 必须并入下方统一构建,否则会被搜索/通知钮覆盖。
        right_items.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .on_click(on_toggle_dock)
                .content(SymbolIcon::new().symbol(Symbol::DockRight))
                .into(),
        );
    }
    if layout.shows_title_search_button() {
        right_items.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .on_click(on_toggle_search_overlay)
                .content(SymbolIcon::new().symbol(Symbol::Find))
                .into(),
        );
    }
    if logged_in {
        let mut bell_parts: Vec<View> = vec![SymbolIcon::new().symbol(Symbol::Mail).into()];
        if total_unread > 0 {
            bell_parts.push(
                InfoBadge::new()
                    .value(total_unread.min(99) as i32)
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            );
        }
        right_items.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .on_click(on_open_notifications)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children(bell_parts),
                )
                .into(),
        );
    }
    if !right_items.is_empty() {
        bar = bar.right_header(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(4.0)
                .children(right_items),
        );
    }
    // xamlroot-probe:焦点锚点挂进 left_header(probe feature 下生效)。
    #[cfg(feature = "probe")]
    let bar = crate::probe::attach_focus_button(bar);
    bar
}

/// 顶部悬浮搜索栏（用于 Medium 半屏态免挤压 TitleBar 触发搜索）。
pub fn floating_search_layers(
    draft: &str,
    on_draft: Callback<Rc<str>>,
    on_submit: Callback<()>,
    on_close: Callback<()>,
) -> Vec<KeyedView> {
    let scrim = KeyedView::new(
        "search-scrim",
        Border::new()
            .background(Brush::Solid(Color::argb(64, 0, 0, 0)))
            .on_pointer_pressed({
                let on_close = on_close.clone();
                move |_: PointerEventInfo| on_close.call(())
            }),
    );

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

    let card = KeyedView::new(
        "search-popup",
        Border::new()
            .max_width(480.0)
            .margin(Thickness::new(16.0, 10.0, 16.0, 0.0))
            .padding(Thickness::xy(12.0, 10.0))
            .corner_radius(12.0)
            .background(ThemeBrush::SolidBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .vertical_alignment(VerticalAlignment::Top)
            .on_preview_key_down(on_enter)
            .content(
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        TextBox::new()
                            .placeholder_text("搜索视频、UP 主")
                            .text(draft)
                            .on_text_changed(on_draft),
                        Button::new()
                            .grid_column(1)
                            .style(ButtonStyle::Accent)
                            .content("搜索")
                            .on_click(on_submit),
                        Button::new()
                            .grid_column(2)
                            .style(ButtonStyle::Subtle)
                            .content(FontIcon::new().glyph("\u{E711}").font_size(12.0))
                            .on_click(on_close),
                    )),
            ),
    );

    vec![scrim, card]
}

/// Android Material 3 × Fluent Mobile 底部 5-Tab 导航栏（推荐 / 热门 / 搜索 / 我的 / 更多）。
pub fn compact_bottom_bar(
    active_route: Route,
    more_open: bool,
    on_navigate: impl Fn(Route) -> Callback<()> + Clone,
    on_toggle_more: Callback<()>,
) -> Grid {
    let primary: [(Route, &str); 4] = [
        (Route::Home, "推荐"),
        (Route::Popular, "热门"),
        (Route::Search, "搜索"),
        (Route::Login, "我的"),
    ];
    let more_active = more_open || COMPACT_MORE_ROUTES.contains(&active_route);

    let tab_btn =
        |col: usize, symbol: Symbol, label: &str, selected: bool, on_click: Callback<()>| {
            let pill_bg = if selected {
                Brush::Theme(ThemeBrush::Accent)
            } else {
                Brush::Solid(Color::argb(0, 0, 0, 0))
            };
            let icon_fg = if selected {
                Brush::Solid(Color::rgb(255, 255, 255))
            } else {
                Brush::Theme(ThemeBrush::PrimaryText)
            };
            Button::new()
                .grid_column(col as i32)
                .style(ButtonStyle::Subtle)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .vertical_alignment(VerticalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Center)
                .vertical_content_alignment(VerticalAlignment::Center)
                .on_click(on_click)
                .content(
                    StackPanel::new().spacing(3.0).children((
                        Border::new()
                            .width(48.0)
                            .height(28.0)
                            .corner_radius(14.0)
                            .background(pill_bg)
                            .horizontal_alignment(HorizontalAlignment::Center)
                            .content(
                                FontIcon::new()
                                    .glyph(symbol_glyph(symbol))
                                    .font_size(14.0)
                                    .foreground(icon_fg)
                                    .opacity(if selected { 1.0 } else { 0.72 })
                                    .horizontal_alignment(HorizontalAlignment::Center)
                                    .vertical_alignment(VerticalAlignment::Center),
                            ),
                        TextBlock::new()
                            .text(label)
                            .font_size(11.0)
                            .font_weight(if selected {
                                FontWeight::SEMI_BOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .foreground(if selected {
                                ThemeBrush::AccentText
                            } else {
                                ThemeBrush::PrimaryText
                            })
                            .opacity(if selected { 1.0 } else { 0.72 })
                            .horizontal_alignment(HorizontalAlignment::Center),
                    )),
                )
        };

    let mut children: Vec<View> = primary
        .iter()
        .enumerate()
        .map(|(col, &(route, label))| {
            tab_btn(
                col,
                route.icon(),
                label,
                active_route == route,
                on_navigate(route),
            )
            .into()
        })
        .collect();

    children.push(tab_btn(4, Symbol::More, "更多", more_active, on_toggle_more).into());

    Grid::new().children((Border::new()
        .height(64.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0))
        .content(
            Grid::new()
                .columns([
                    GridLength::Star(1.0),
                    GridLength::Star(1.0),
                    GridLength::Star(1.0),
                    GridLength::Star(1.0),
                    GridLength::Star(1.0),
                ])
                .children(children),
        ),))
}

/// Mobile 底栏右侧向上展开的 2×3 悬浮菜单（覆盖全部 6 个二级目的地）。
pub fn compact_more_popup(
    active_route: Route,
    on_navigate: impl Fn(Route) -> Callback<()> + Clone,
) -> View {
    let tiles: Vec<View> = COMPACT_MORE_ROUTES
        .iter()
        .enumerate()
        .map(|(idx, &route)| {
            let selected = active_route == route;
            let row = (idx / 2) as i32;
            let col = (idx % 2) as i32;
            Button::new()
                .grid_row(row)
                .grid_column(col)
                .style(if selected {
                    ButtonStyle::Accent
                } else {
                    ButtonStyle::Subtle
                })
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Left)
                .on_click(on_navigate(route))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .margin(Thickness::xy(4.0, 4.0))
                        .children((
                            FontIcon::new()
                                .glyph(symbol_glyph(route.icon()))
                                .font_size(14.0)
                                .vertical_alignment(VerticalAlignment::Center),
                            TextBlock::new()
                                .text(route.label())
                                .font_size(12.5)
                                .vertical_alignment(VerticalAlignment::Center),
                        )),
                )
                .into()
        })
        .collect();

    Border::new()
        .width(248.0)
        .margin(Thickness::new(0.0, 0.0, 12.0, 10.0))
        .padding(Thickness::xy(8.0, 8.0))
        .corner_radius(12.0)
        .background(ThemeBrush::SolidBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .horizontal_alignment(HorizontalAlignment::Right)
        .vertical_alignment(VerticalAlignment::Bottom)
        .content(
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                .rows([GridLength::Auto, GridLength::Auto, GridLength::Auto])
                .column_spacing(6.0)
                .row_spacing(6.0)
                .children(tiles),
        )
        .into()
}

pub fn symbol_glyph(symbol: Symbol) -> &'static str {
    match symbol {
        Symbol::Home => "\u{E80F}",
        Symbol::Like => "\u{E8E1}",
        Symbol::Find => "\u{E721}",
        Symbol::Account => "\u{E77B}",
        Symbol::More => "\u{E712}",
        Symbol::People => "\u{E716}",
        Symbol::Camera => "\u{E722}",
        Symbol::Favorite => "\u{E734}",
        Symbol::Clock => "\u{E823}",
        Symbol::TwoPage => "\u{E8F1}",
        Symbol::Setting => "\u{E713}",
        _ => "\u{E712}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_more_routes_covers_all_secondary_destinations_without_primary_overlap() {
        let primary = [Route::Home, Route::Popular, Route::Search, Route::Login];
        assert_eq!(COMPACT_MORE_ROUTES.len(), 6);
        assert!(COMPACT_MORE_ROUTES.contains(&Route::WatchLater));
        assert!(COMPACT_MORE_ROUTES.contains(&Route::Settings));
        for r in COMPACT_MORE_ROUTES {
            assert!(!primary.contains(&r), "{r:?} 不应与底部主 Tab 重叠");
        }
    }

    #[test]
    fn title_search_box_width_clamps_on_narrow_desktop() {
        assert_eq!(title_search_box_width(860.0), 240.0);
        assert_eq!(title_search_box_width(700.0), 200.0);
        assert_eq!(title_search_box_width(1280.0), 360.0);
    }
}
