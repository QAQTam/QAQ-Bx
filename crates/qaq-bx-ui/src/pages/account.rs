//! 个人中心与通知中心（登录后视图）：账号资料、关系计数、五类通知与私信会话。

use qaq_bx_core::account::{AccountStat, RelationKind, RelationUser};
use qaq_bx_core::notify::{
    AtNotice, LikeNotice, NoticeLink, ReplyNotice, SystemNotice, UnreadSummary, WhisperMessage,
    WhisperSession,
};
use windows_reactor::*;

use crate::auth::{UserInfo, avatar_thumbnail_url};
use crate::image_cache::ImageCache;
use crate::layout::ViewportLayout;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotifyTab {
    #[default]
    Reply,
    At,
    Like,
    System,
    Whisper,
}

impl NotifyTab {
    pub const ALL: [Self; 5] = [
        Self::Reply,
        Self::At,
        Self::Like,
        Self::System,
        Self::Whisper,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Reply => "回复",
            Self::At => "@我",
            Self::Like => "赞",
            Self::System => "系统",
            Self::Whisper => "私信",
        }
    }

    pub const fn unread(self, summary: UnreadSummary) -> u64 {
        match self {
            Self::Reply => summary.reply,
            Self::At => summary.at,
            Self::Like => summary.like,
            Self::System => summary.system,
            Self::Whisper => summary.whisper,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenTarget {
    pub bvid: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct ListSliceState<T> {
    pub items: Vec<T>,
    pub loading: bool,
    pub loaded_once: bool,
    pub has_more: bool,
    pub error: Option<String>,
}

impl<T> Default for ListSliceState<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            loading: false,
            loaded_once: false,
            has_more: false,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WhisperThreadState {
    pub messages: Vec<WhisperMessage>,
    pub loading: bool,
    pub error: Option<String>,
}

pub struct AccountViewData<'a> {
    pub layout: ViewportLayout,
    pub whisper_detail_open: bool,
    pub user: &'a UserInfo,
    pub stat: Option<AccountStat>,
    pub unread: UnreadSummary,
    pub followings: &'a ListSliceState<RelationUser>,
    pub followers: &'a ListSliceState<RelationUser>,
    pub active_tab: NotifyTab,
    pub replies: &'a ListSliceState<ReplyNotice>,
    pub ats: &'a ListSliceState<AtNotice>,
    pub likes: &'a ListSliceState<LikeNotice>,
    pub systems: &'a ListSliceState<SystemNotice>,
    pub sessions: &'a ListSliceState<WhisperSession>,
    pub selected_talker: Option<(u64, i32)>,
    pub selected_thread: Option<&'a WhisperThreadState>,
    pub avatar_cache: &'a ImageCache,
    pub logout_confirm: bool,
    pub auth_notice: Option<&'a str>,
}

pub struct AccountCallbacks {
    pub on_refresh: Callback<()>,
    pub on_logout: Callback<()>,
    pub on_cancel_logout: Callback<()>,
    pub on_open_relations: Callback<RelationKind>,
    pub on_refresh_relations: Callback<RelationKind>,
    pub on_load_more_relations: Callback<RelationKind>,
    pub on_select_tab: Callback<NotifyTab>,
    pub on_load_more: Callback<NotifyTab>,
    pub on_retry_tab: Callback<NotifyTab>,
    pub on_select_session: Callback<(u64, i32)>,
    pub on_close_whisper_detail: Callback<()>,
    pub on_refresh_thread: Callback<(u64, i32)>,
    pub on_open_video: Callback<OpenTarget>,
    /// U5:快捷入口(登录态列表页接线后的页内直达)。
    pub on_open_history: Callback<()>,
    pub on_open_watch_later: Callback<()>,
    pub on_open_favorites: Callback<()>,
}

pub fn account_page(data: AccountViewData<'_>, cb: AccountCallbacks) -> View {
    if data.layout.uses_whisper_drilldown()
        && data.active_tab == NotifyTab::Whisper
        && data.whisper_detail_open
        && let Some(detail) = whisper_mobile_detail_page(&data, &cb)
    {
        return detail;
    }

    if data.layout.is_single_column() {
        let margin_x = data.layout.page_margin_x();
        let profile = profile_section(&data, &cb);
        let notifications = notification_section(&data, &cb, false);
        return ScrollViewer::new()
            .content(
                Border::new()
                    .padding(Thickness::new(margin_x, 12.0, margin_x, 24.0))
                    .content(
                        StackPanel::new()
                            .spacing(16.0)
                            .children((profile, notifications)),
                    ),
            )
            .into();
    }

    let profile = profile_section(&data, &cb);
    let notifications = notification_section(&data, &cb, true);

    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            Border::new()
                .padding(Thickness::new(36.0, 24.0, 36.0, 16.0))
                .grid_row(0)
                .content(profile),
            Border::new()
                .padding(Thickness::new(36.0, 0.0, 36.0, 24.0))
                .grid_row(1)
                .content(notifications),
        ))
        .into()
}

fn profile_section(data: &AccountViewData<'_>, cb: &AccountCallbacks) -> View {
    let user = data.user;
    let compact = data.layout.is_single_column();
    let is_mobile = data.layout.is_mobile();
    let header_actions = {
        let mut items: Vec<View> = Vec::new();
        if !(is_mobile && data.logout_confirm) {
            if let Ok(btn) = HyperlinkButton::new()
                .content("主页")
                .navigate_uri(user.space_url.as_str())
            {
                items.push(btn.into());
            }
            items.push(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content("刷新")
                    .on_click(cb.on_refresh.clone())
                    .into(),
            );
        }
        if data.logout_confirm {
            items.push(
                Button::new()
                    .style(ButtonStyle::Accent)
                    .content("确认退出")
                    .on_click(cb.on_logout.clone())
                    .into(),
            );
            items.push(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content("取消")
                    .on_click(cb.on_cancel_logout.clone())
                    .into(),
            );
        } else {
            items.push(
                Button::new()
                    .content("退出")
                    .on_click(cb.on_logout.clone())
                    .into(),
            );
        }
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(if is_mobile { 4.0 } else { 8.0 })
            .vertical_alignment(VerticalAlignment::Center)
            .children(items)
    };

    let page_title_row = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .children((
            TextBlock::new()
                .text("个人中心")
                .font_size(data.layout.page_title_size())
                .font_weight(FontWeight::BOLD)
                .vertical_alignment(VerticalAlignment::Center),
            header_actions.grid_column(1),
        ));

    let avatar_size = if is_mobile { 52.0 } else { 64.0 };
    let avatar = theme::avatar_view(user.avatar.as_ref(), &user.uname, avatar_size, 8.0);

    let mut badges: Vec<View> = vec![
        TextBlock::new()
            .text(user.uname.clone())
            .font_size(if is_mobile { 16.0 } else { 18.0 })
            .font_weight(FontWeight::SEMI_BOLD)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .max_lines(1)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        chip_badge(&format!("Lv{}", user.level), false),
    ];
    if user.vip.is_vip() {
        let label = if user.vip_label.is_empty() {
            user.vip.label()
        } else {
            user.vip_label.as_str()
        };
        badges.push(chip_badge(label, true));
    }
    if !is_mobile && !user.official_title.is_empty() {
        badges.push(
            TextBlock::new()
                .text(user.official_title.clone())
                .font_size(12.0)
                .opacity(0.65)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }

    let due_str = theme::fmt_date_ms(user.vip_due_date);
    let mut sub_items: Vec<View> = vec![
        TextBlock::new()
            .text(format!("UID {}", user.mid))
            .font_size(12.0)
            .opacity(0.6)
            .into(),
    ];
    if user.vip.is_vip() && !due_str.is_empty() {
        sub_items.push(
            TextBlock::new()
                .text(format!("到期 {due_str}"))
                .font_size(12.0)
                .opacity(0.6)
                .into(),
        );
    }

    let exp_view: View = match user.next_exp {
        Some(next) if next > 0 => {
            let bar = ProgressBar::new()
                .minimum(0.0)
                .maximum(next as f64)
                .value((user.current_exp.min(next)) as f64)
                .vertical_alignment(VerticalAlignment::Center);
            let label = TextBlock::new()
                .text(format!("{} / {}", user.current_exp, next))
                .font_size(11.0)
                .opacity(0.55)
                .vertical_alignment(VerticalAlignment::Center);
            if compact {
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(8.0)
                    .children((View::from(bar), View::from(label.grid_column(1))))
                    .into()
            } else {
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((bar.width(120.0), label))
                    .into()
            }
        }
        _ => TextBlock::new()
            .text(format!("经验 {}", user.current_exp))
            .font_size(11.0)
            .opacity(0.55)
            .into(),
    };

    let identity = StackPanel::new()
        .spacing(6.0)
        .margin(if compact {
            Thickness::new(12.0, 0.0, 0.0, 0.0)
        } else {
            Thickness::new(16.0, 0.0, 16.0, 0.0)
        })
        .vertical_alignment(VerticalAlignment::Center)
        .children((
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children(badges),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .children(sub_items),
            exp_view,
        ));

    let flyout_width = if is_mobile { 308.0 } else { 336.0 };
    let stat_val = |v: Option<u64>| v.map(theme::fmt_count).unwrap_or_else(|| "—".into());
    let btn_following = relation_stat_button(
        &stat_val(data.stat.map(|s| s.following)),
        "关注",
        RelationKind::Following,
        data.followings,
        data.stat.map(|s| s.following),
        data.avatar_cache,
        flyout_width,
        compact,
        cb,
    );
    let btn_follower = relation_stat_button(
        &stat_val(data.stat.map(|s| s.follower)),
        "粉丝",
        RelationKind::Follower,
        data.followers,
        data.stat.map(|s| s.follower),
        data.avatar_cache,
        flyout_width,
        compact,
        cb,
    );
    let tile_dynamic = stat_tile(
        &stat_val(data.stat.map(|s| s.dynamic_count)),
        "动态",
        Some(&format!("{}/dynamic", user.space_url)),
        compact,
    );
    let tile_coins = stat_tile(&theme::fmt_coins(user.coins), "硬币", None, compact);

    let profile_card = if compact {
        let top = Grid::new()
            .columns([GridLength::Auto, GridLength::Star(1.0)])
            .children((
                avatar.vertical_alignment(VerticalAlignment::Center),
                identity.grid_column(1),
            ));
        let divider = Border::new().height(1.0).background(ThemeBrush::CardStroke);
        let stats_grid = Grid::new()
            .columns([
                GridLength::Star(1.0),
                GridLength::Star(1.0),
                GridLength::Star(1.0),
                GridLength::Star(1.0),
            ])
            .children((
                Border::new().grid_column(0).content(btn_following),
                Border::new().grid_column(1).content(btn_follower),
                Border::new().grid_column(2).content(tile_dynamic),
                Border::new().grid_column(3).content(tile_coins),
            ));
        Border::new()
            .corner_radius(8.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(Thickness::xy(14.0, 14.0))
            .content(StackPanel::new().spacing(12.0).children((
                View::from(top),
                View::from(divider),
                View::from(stats_grid),
            )))
    } else {
        let stats_strip = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .vertical_alignment(VerticalAlignment::Center)
            .children((btn_following, btn_follower, tile_dynamic, tile_coins));
        Border::new()
            .corner_radius(8.0)
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(1.0)
            .padding(20.0)
            .content(
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((avatar, identity.grid_column(1), stats_strip.grid_column(2))),
            )
    };

    let mut stack_children: Vec<View> = vec![page_title_row.into(), profile_card.into()];
    // U5:登录态列表页快捷入口(与 space 主页的收藏/历史/稍后再看同语义)。
    stack_children.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content("历史")
                    .on_click(cb.on_open_history.clone()),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content("稍后再看")
                    .on_click(cb.on_open_watch_later.clone()),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .content("收藏")
                    .on_click(cb.on_open_favorites.clone()),
            ))
            .into(),
    );
    if let Some(reason) = data.auth_notice {
        stack_children.push(
            InfoBar::new()
                .title("提示")
                .message(reason)
                .severity(InfoBarSeverity::Warning)
                .is_open(true)
                .is_closable(false)
                .into(),
        );
    }
    StackPanel::new()
        .spacing(14.0)
        .children(stack_children)
        .into()
}

fn notification_section(
    data: &AccountViewData<'_>,
    cb: &AccountCallbacks,
    nested_scroll: bool,
) -> View {
    let compact = data.layout.is_single_column();
    let is_mobile = data.layout.is_mobile();
    let tabs: Vec<KeyedView> = NotifyTab::ALL
        .into_iter()
        .map(|tab| {
            let selected = tab == data.active_tab;
            let count = tab.unread(data.unread);
            let on_select = cb.on_select_tab.clone();

            let mut parts: Vec<View> = vec![
                TextBlock::new()
                    .text(tab.label())
                    .font_size(if is_mobile { 12.0 } else { 13.0 })
                    .font_weight(if selected {
                        FontWeight::SEMI_BOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            ];
            if count > 0 {
                parts.push(
                    InfoBadge::new()
                        .value(count.min(99) as i32)
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                );
            }
            let btn = Button::new()
                .style(if selected {
                    ButtonStyle::Default
                } else {
                    ButtonStyle::Subtle
                })
                .on_click(move || on_select.call(tab))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(5.0)
                        .children(parts),
                );
            KeyedView::new(format!("tab-{}", tab.label()), btn)
        })
        .collect();

    let active_loading = match data.active_tab {
        NotifyTab::Reply => data.replies.loading,
        NotifyTab::At => data.ats.loading,
        NotifyTab::Like => data.likes.loading,
        NotifyTab::System => data.systems.loading,
        NotifyTab::Whisper => {
            data.sessions.loading || data.selected_thread.is_some_and(|t| t.loading)
        }
    };

    let mut header_right: Vec<View> = Vec::new();
    if active_loading {
        header_right.push(
            ProgressRing::new()
                .is_indeterminate(true)
                .is_active(true)
                .width(16.0)
                .height(16.0)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    }

    let bar: View = if compact {
        let title_row = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .children((
                TextBlock::new()
                    .text("通知")
                    .font_size(16.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .vertical_alignment(VerticalAlignment::Center),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .children(header_right),
            ));
        let tab_scroller = ScrollViewer::new()
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Hidden)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(4.0)
                    .keyed_children(tabs),
            );
        StackPanel::new()
            .spacing(8.0)
            .margin(Thickness::new(0.0, 0.0, 0.0, 10.0))
            .children((View::from(title_row), View::from(tab_scroller)))
            .into()
    } else {
        Grid::new()
            .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
            .margin(Thickness::new(0.0, 0.0, 0.0, 12.0))
            .grid_row(0)
            .children((
                TextBlock::new()
                    .text("通知")
                    .font_size(18.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .vertical_alignment(VerticalAlignment::Center),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .margin(Thickness::new(16.0, 0.0, 0.0, 0.0))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .keyed_children(tabs),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(2)
                    .children(header_right),
            ))
            .into()
    };

    let body: View = match data.active_tab {
        NotifyTab::Reply => {
            feed_list_view(data.replies, NotifyTab::Reply, nested_scroll, cb, |item| {
                interaction_card(
                    item.sender_mid,
                    &item.sender_name,
                    &item.sender_avatar,
                    &item.action_label,
                    &theme::fmt_time(item.time),
                    &item.content,
                    item.quote.as_deref(),
                    item.bvid.as_deref(),
                    &item.url,
                    is_mobile,
                    data.avatar_cache,
                    &cb.on_open_video,
                )
            })
        }
        NotifyTab::At => feed_list_view(data.ats, NotifyTab::At, nested_scroll, cb, |item| {
            interaction_card(
                item.sender_mid,
                &item.sender_name,
                &item.sender_avatar,
                &item.action_label,
                &theme::fmt_time(item.time),
                &item.content,
                item.quote.as_deref(),
                item.bvid.as_deref(),
                &item.url,
                is_mobile,
                data.avatar_cache,
                &cb.on_open_video,
            )
        }),
        NotifyTab::Like => {
            like_list_view(data.likes, data.avatar_cache, nested_scroll, is_mobile, cb)
        }
        NotifyTab::System => {
            feed_list_view(data.systems, NotifyTab::System, nested_scroll, cb, |item| {
                system_card(item, is_mobile, &cb.on_open_video)
            })
        }
        NotifyTab::Whisper => {
            if data.layout.uses_whisper_drilldown() {
                whisper_session_list_only(data, cb)
            } else {
                whisper_split_view(data, cb)
            }
        }
    };

    if !nested_scroll {
        return StackPanel::new().spacing(4.0).children((bar, body)).into();
    }

    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((bar, Border::new().grid_row(1).content(body)))
        .into()
}

fn feed_list_view<T>(
    slice: &ListSliceState<T>,
    tab: NotifyTab,
    nested_scroll: bool,
    cb: &AccountCallbacks,
    render_item: impl Fn(&T) -> View,
) -> View {
    if slice.items.is_empty() {
        return empty_or_error_view(slice.loading, slice.error.as_deref(), tab, cb);
    }
    let mut rows: Vec<View> = slice.items.iter().map(render_item).collect();
    append_list_footer(
        &mut rows,
        slice.loading,
        slice.has_more,
        slice.error.as_deref(),
        tab,
        cb,
    );
    let stack = StackPanel::new().spacing(8.0).children(rows);
    if nested_scroll {
        ScrollViewer::new().content(stack).into()
    } else {
        stack.into()
    }
}

fn like_list_view(
    slice: &ListSliceState<LikeNotice>,
    avatar_cache: &ImageCache,
    nested_scroll: bool,
    is_mobile: bool,
    cb: &AccountCallbacks,
) -> View {
    if slice.items.is_empty() {
        return empty_or_error_view(slice.loading, slice.error.as_deref(), NotifyTab::Like, cb);
    }
    let has_latest = slice.items.iter().any(|i| i.is_latest);
    let has_earlier = slice.items.iter().any(|i| !i.is_latest);
    let mut rows: Vec<View> = Vec::new();
    let mut emitted_earlier_header = false;
    if has_latest && has_earlier {
        rows.push(section_caption("最新"));
    }
    for item in &slice.items {
        if has_latest && !item.is_latest && !emitted_earlier_header {
            emitted_earlier_header = true;
            rows.push(section_caption("更早"));
        }
        let sender = if item.total_count > 1 {
            format!("{} 等 {} 人", item.sender_name, item.total_count)
        } else {
            item.sender_name.clone()
        };
        rows.push(interaction_card(
            item.sender_mid,
            &sender,
            &item.sender_avatar,
            &item.action_label,
            &theme::fmt_time(item.time),
            &item.target_text,
            None,
            item.bvid.as_deref(),
            &item.url,
            is_mobile,
            avatar_cache,
            &cb.on_open_video,
        ));
    }
    append_list_footer(
        &mut rows,
        slice.loading,
        slice.has_more,
        slice.error.as_deref(),
        NotifyTab::Like,
        cb,
    );
    let stack = StackPanel::new().spacing(8.0).children(rows);
    if nested_scroll {
        ScrollViewer::new().content(stack).into()
    } else {
        stack.into()
    }
}

fn section_caption(label: &str) -> View {
    TextBlock::new()
        .text(label)
        .font_size(12.0)
        .font_weight(FontWeight::SEMI_BOLD)
        .opacity(0.6)
        .margin(Thickness::new(2.0, 4.0, 0.0, 0.0))
        .into()
}

#[allow(clippy::too_many_arguments)]
fn interaction_card(
    _sender_mid: u64,
    sender_name: &str,
    sender_avatar: &str,
    action_label: &str,
    time_text: &str,
    body_text: &str,
    quote: Option<&str>,
    bvid: Option<&str>,
    url: &str,
    is_mobile: bool,
    avatar_cache: &ImageCache,
    on_open_video: &Callback<OpenTarget>,
) -> View {
    let cached_img = (!sender_avatar.is_empty())
        .then(|| avatar_cache.get(&avatar_thumbnail_url(sender_avatar)))
        .flatten();
    let avatar_size = if is_mobile { 32.0 } else { 36.0 };
    let avatar = theme::avatar_view(cached_img.as_ref(), sender_name, avatar_size, 6.0);

    let top_row = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .column_spacing(6.0)
        .children((
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    TextBlock::new()
                        .text(sender_name)
                        .font_size(13.0)
                        .font_weight(FontWeight::SEMI_BOLD)
                        .max_width(if is_mobile { 120.0 } else { 240.0 })
                        .max_lines(1)
                        .text_trimming(TextTrimming::CharacterEllipsis),
                    TextBlock::new()
                        .text(action_label)
                        .font_size(12.0)
                        .opacity(0.6)
                        .max_lines(1),
                )),
            TextBlock::new()
                .text(time_text)
                .font_size(11.0)
                .opacity(0.5)
                .grid_column(1)
                .vertical_alignment(VerticalAlignment::Center),
        ));

    let mut content_col: Vec<View> = vec![top_row.into()];
    if !body_text.trim().is_empty() {
        content_col.push(
            TextBlock::new()
                .text(body_text.trim())
                .font_size(13.0)
                .text_wrapping(TextWrapping::Wrap)
                .max_lines(4)
                .into(),
        );
    }
    if let Some(q) = quote.filter(|s| !s.trim().is_empty()) {
        content_col.push(
            Border::new()
                .border_thickness(Thickness::new(2.0, 0.0, 0.0, 0.0))
                .border_brush(ThemeBrush::CardStroke)
                .padding(Thickness::new(8.0, 2.0, 0.0, 2.0))
                .content(
                    TextBlock::new()
                        .text(q.trim())
                        .font_size(12.0)
                        .opacity(0.6)
                        .text_wrapping(TextWrapping::Wrap)
                        .max_lines(2),
                )
                .into(),
        );
    }

    let action = link_action_button("播放", "查看", bvid, url, body_text, on_open_video);

    if is_mobile {
        if let Some(act) = action {
            content_col.push(
                Border::new()
                    .horizontal_alignment(HorizontalAlignment::Right)
                    .content(act)
                    .into(),
            );
        }
        return card_box(
            Thickness::xy(12.0, 10.0),
            Grid::new()
                .columns([GridLength::Auto, GridLength::Star(1.0)])
                .children((
                    avatar.vertical_alignment(VerticalAlignment::Top),
                    StackPanel::new()
                        .spacing(4.0)
                        .margin(Thickness::new(10.0, 0.0, 0.0, 0.0))
                        .grid_column(1)
                        .children(content_col),
                )),
        );
    }

    let mut cols: Vec<View> = vec![
        avatar.vertical_alignment(VerticalAlignment::Top).into(),
        StackPanel::new()
            .spacing(4.0)
            .margin(Thickness::new(12.0, 0.0, 12.0, 0.0))
            .grid_column(1)
            .children(content_col)
            .into(),
    ];
    let col_defs = if let Some(act) = action {
        cols.push(
            Border::new()
                .grid_column(2)
                .vertical_alignment(VerticalAlignment::Center)
                .content(act)
                .into(),
        );
        vec![GridLength::Auto, GridLength::Star(1.0), GridLength::Auto]
    } else {
        vec![GridLength::Auto, GridLength::Star(1.0)]
    };

    card_box(
        Thickness::xy(16.0, 12.0),
        Grid::new().columns(col_defs).children(cols),
    )
}

fn system_card(item: &SystemNotice, is_mobile: bool, on_open_video: &Callback<OpenTarget>) -> View {
    let top = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .column_spacing(8.0)
        .children((
            TextBlock::new()
                .text(item.title.clone())
                .font_size(14.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .text_wrapping(TextWrapping::Wrap),
            TextBlock::new()
                .text(theme::fmt_sys_time(&item.time_at))
                .font_size(11.0)
                .opacity(0.5)
                .grid_column(1)
                .vertical_alignment(VerticalAlignment::Center),
        ));

    let mut parts: Vec<View> = vec![top.into()];
    if !item.content.is_empty() {
        parts.push(
            TextBlock::new()
                .text(item.content.clone())
                .font_size(13.0)
                .opacity(0.85)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    if !item.links.is_empty() {
        let link_views: Vec<View> = item
            .links
            .iter()
            .filter_map(|link| notice_link_view(link, &item.title, on_open_video))
            .collect();
        if !link_views.is_empty() {
            parts.push(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children(link_views)
                    .into(),
            );
        }
    }
    let pad = if is_mobile {
        Thickness::xy(12.0, 10.0)
    } else {
        Thickness::xy(16.0, 12.0)
    };
    card_box(pad, StackPanel::new().spacing(6.0).children(parts))
}

fn whisper_session_list_only(data: &AccountViewData<'_>, cb: &AccountCallbacks) -> View {
    let sessions = data.sessions;
    if sessions.items.is_empty() {
        return empty_or_error_view(
            sessions.loading,
            sessions.error.as_deref(),
            NotifyTab::Whisper,
            cb,
        );
    }
    let mut session_rows: Vec<View> = sessions
        .items
        .iter()
        .map(|s| {
            session_row_view(
                s,
                data.selected_talker,
                data.avatar_cache,
                &cb.on_select_session,
            )
        })
        .collect();
    append_list_footer(
        &mut session_rows,
        sessions.loading,
        sessions.has_more,
        sessions.error.as_deref(),
        NotifyTab::Whisper,
        cb,
    );
    card_box(
        Thickness::uniform(8.0),
        StackPanel::new().spacing(4.0).children(session_rows),
    )
}

fn whisper_mobile_detail_page(data: &AccountViewData<'_>, cb: &AccountCallbacks) -> Option<View> {
    let (tid, stype) = data.selected_talker?;
    let sess = data
        .sessions
        .items
        .iter()
        .find(|s| s.talker_id == tid && s.session_type == stype)?;
    let fallback = WhisperThreadState {
        loading: true,
        ..Default::default()
    };
    let thread = data.selected_thread.unwrap_or(&fallback);
    let margin_x = data.layout.page_margin_x();

    let mut actions: Vec<View> = Vec::new();
    if !sess.is_system_account && sess.session_type == 1 && sess.talker_id > 0 {
        let space = format!("https://space.bilibili.com/{}", sess.talker_id);
        if let Ok(link) = HyperlinkButton::new()
            .content("主页")
            .navigate_uri(space.as_str())
        {
            actions.push(link.into());
        }
    }
    let on_ref = cb.on_refresh_thread.clone();
    actions.push(
        Button::new()
            .style(ButtonStyle::Subtle)
            .content("刷新")
            .on_click(move || on_ref.call((tid, stype)))
            .into(),
    );

    let top_bar = Grid::new()
        .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
        .column_spacing(8.0)
        .children((
            Button::new()
                .style(ButtonStyle::Subtle)
                .content("← 会话列表")
                .on_click(cb.on_close_whisper_detail.clone()),
            TextBlock::new()
                .grid_column(1)
                .text(sess.talker_name.clone())
                .font_size(15.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .max_lines(1)
                .text_trimming(TextTrimming::CharacterEllipsis)
                .vertical_alignment(VerticalAlignment::Center),
            StackPanel::new()
                .grid_column(2)
                .orientation(Orientation::Horizontal)
                .spacing(4.0)
                .children(actions),
        ));

    let body: View = whisper_thread_body(
        sess,
        thread,
        data.user.mid,
        true,
        &cb.on_refresh_thread,
        &cb.on_open_video,
    );

    Some(
        Border::new()
            .padding(Thickness::new(margin_x, 10.0, margin_x, 12.0))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .row_spacing(10.0)
                    .children((
                        top_bar,
                        Border::new()
                            .grid_row(1)
                            .corner_radius(8.0)
                            .background(ThemeBrush::CardBackground)
                            .border_brush(ThemeBrush::CardStroke)
                            .border_thickness(1.0)
                            .padding(12.0)
                            .content(body),
                    )),
            )
            .into(),
    )
}

fn whisper_split_view(data: &AccountViewData<'_>, cb: &AccountCallbacks) -> View {
    let sessions = data.sessions;
    if sessions.items.is_empty() {
        return empty_or_error_view(
            sessions.loading,
            sessions.error.as_deref(),
            NotifyTab::Whisper,
            cb,
        );
    }

    let mut session_rows: Vec<View> = sessions
        .items
        .iter()
        .map(|s| {
            session_row_view(
                s,
                data.selected_talker,
                data.avatar_cache,
                &cb.on_select_session,
            )
        })
        .collect();
    append_list_footer(
        &mut session_rows,
        sessions.loading,
        sessions.has_more,
        sessions.error.as_deref(),
        NotifyTab::Whisper,
        cb,
    );

    let left_pane = Border::new()
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .padding(8.0)
        .grid_column(0)
        .content(
            ScrollViewer::new().content(StackPanel::new().spacing(4.0).children(session_rows)),
        );

    let selected_session = data.selected_talker.and_then(|(tid, stype)| {
        sessions
            .items
            .iter()
            .find(|s| s.talker_id == tid && s.session_type == stype)
    });

    let right_content: View = match (selected_session, data.selected_thread) {
        (Some(sess), Some(thread)) => whisper_thread_view(
            sess,
            thread,
            data.user.mid,
            &cb.on_refresh_thread,
            &cb.on_open_video,
        ),
        (Some(sess), None) => whisper_thread_view(
            sess,
            &WhisperThreadState {
                loading: true,
                ..Default::default()
            },
            data.user.mid,
            &cb.on_refresh_thread,
            &cb.on_open_video,
        ),
        _ => TextBlock::new()
            .text("选择会话")
            .font_size(13.0)
            .opacity(0.5)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    };

    let right_pane = Border::new()
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .padding(16.0)
        .grid_column(1)
        .content(right_content);

    Grid::new()
        .columns([GridLength::Pixel(280.0), GridLength::Star(1.0)])
        .column_spacing(12.0)
        .children((left_pane, right_pane))
        .into()
}

fn session_row_view(
    sess: &WhisperSession,
    selected: Option<(u64, i32)>,
    avatar_cache: &ImageCache,
    on_select: &Callback<(u64, i32)>,
) -> View {
    let is_selected = selected == Some((sess.talker_id, sess.session_type));
    let cached = (!sess.talker_avatar.is_empty())
        .then(|| avatar_cache.get(&avatar_thumbnail_url(&sess.talker_avatar)))
        .flatten();
    let avatar = theme::avatar_view(cached.as_ref(), &sess.talker_name, 36.0, 6.0);

    let top = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .children((
            TextBlock::new()
                .text(sess.talker_name.clone())
                .font_size(13.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .max_lines(1),
            TextBlock::new()
                .text(theme::fmt_time(sess.last_timestamp))
                .font_size(11.0)
                .opacity(0.5)
                .grid_column(1)
                .margin(Thickness::new(6.0, 0.0, 0.0, 0.0)),
        ));

    let mut bottom_children: Vec<View> = vec![
        TextBlock::new()
            .text(sess.preview.clone())
            .font_size(12.0)
            .opacity(0.6)
            .max_lines(1)
            .into(),
    ];
    let bottom_cols = if sess.unread_count > 0 {
        bottom_children.push(
            InfoBadge::new()
                .value(sess.unread_count.min(99) as i32)
                .grid_column(1)
                .margin(Thickness::new(6.0, 0.0, 0.0, 0.0))
                .into(),
        );
        vec![GridLength::Star(1.0), GridLength::Auto]
    } else {
        vec![GridLength::Star(1.0)]
    };

    let row_body = Border::new().padding(Thickness::xy(8.0, 6.0)).content(
        Grid::new()
            .columns([GridLength::Auto, GridLength::Star(1.0)])
            .children((
                avatar,
                StackPanel::new()
                    .spacing(2.0)
                    .margin(Thickness::new(10.0, 0.0, 0.0, 0.0))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .children((
                        top,
                        Grid::new().columns(bottom_cols).children(bottom_children),
                    )),
            )),
    );

    let talker_id = sess.talker_id;
    let session_type = sess.session_type;
    let on_click = on_select.clone();
    Button::new()
        .style(if is_selected {
            ButtonStyle::Default
        } else {
            ButtonStyle::Subtle
        })
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .on_click(move || on_click.call((talker_id, session_type)))
        .content(row_body)
        .into()
}

fn whisper_thread_view(
    sess: &WhisperSession,
    thread: &WhisperThreadState,
    my_uid: u64,
    on_refresh: &Callback<(u64, i32)>,
    on_open_video: &Callback<OpenTarget>,
) -> View {
    let mut actions: Vec<View> = Vec::new();
    if !sess.is_system_account && sess.session_type == 1 && sess.talker_id > 0 {
        let space = format!("https://space.bilibili.com/{}", sess.talker_id);
        if let Ok(link) = HyperlinkButton::new()
            .content("主页")
            .navigate_uri(space.as_str())
        {
            actions.push(link.into());
        }
    }
    let tid = sess.talker_id;
    let stype = sess.session_type;
    let on_ref = on_refresh.clone();
    actions.push(
        Button::new()
            .style(ButtonStyle::Subtle)
            .content("刷新")
            .on_click(move || on_ref.call((tid, stype)))
            .into(),
    );

    let header = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .margin(Thickness::new(0.0, 0.0, 0.0, 12.0))
        .grid_row(0)
        .children((
            TextBlock::new()
                .text(sess.talker_name.clone())
                .font_size(15.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .vertical_alignment(VerticalAlignment::Center),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .grid_column(1)
                .children(actions),
        ));

    let body: View = whisper_thread_body(sess, thread, my_uid, false, on_refresh, on_open_video);

    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .children((header, Border::new().grid_row(1).content(body)))
        .into()
}

fn whisper_thread_body(
    sess: &WhisperSession,
    thread: &WhisperThreadState,
    my_uid: u64,
    compact: bool,
    on_refresh: &Callback<(u64, i32)>,
    on_open_video: &Callback<OpenTarget>,
) -> View {
    let tid = sess.talker_id;
    let stype = sess.session_type;
    if thread.loading && thread.messages.is_empty() {
        ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(24.0)
            .height(24.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    } else if let Some(err) = &thread.error
        && thread.messages.is_empty()
    {
        StackPanel::new()
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                TextBlock::new()
                    .text(err.clone())
                    .font_size(12.0)
                    .opacity(0.65),
                Button::new()
                    .content("重试")
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .on_click({
                        let cb = on_refresh.clone();
                        move || cb.call((tid, stype))
                    }),
            ))
            .into()
    } else if thread.messages.is_empty() {
        TextBlock::new()
            .text("暂无消息")
            .font_size(12.0)
            .opacity(0.55)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    } else {
        let bubbles: Vec<View> = thread
            .messages
            .iter()
            .map(|msg| whisper_msg_bubble(msg, my_uid, &sess.talker_name, compact, on_open_video))
            .collect();
        ScrollViewer::new()
            .content(StackPanel::new().spacing(10.0).children(bubbles))
            .into()
    }
}

fn whisper_msg_bubble(
    msg: &WhisperMessage,
    my_uid: u64,
    talker_name: &str,
    compact: bool,
    on_open_video: &Callback<OpenTarget>,
) -> View {
    let is_me = my_uid > 0 && msg.sender_uid == my_uid;
    let sender = if is_me { "我" } else { talker_name };
    let align = if is_me {
        HorizontalAlignment::Right
    } else {
        HorizontalAlignment::Left
    };

    let mut inner: Vec<View> = vec![
        TextBlock::new()
            .text(msg.text.clone())
            .font_size(13.0)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    ];
    if let Some(link) = &msg.link
        && let Some(link_btn) = notice_link_view(link, &msg.text, on_open_video)
    {
        inner.push(link_btn);
    }

    StackPanel::new()
        .spacing(4.0)
        .horizontal_alignment(align)
        .children((
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .horizontal_alignment(align)
                .children((
                    TextBlock::new().text(sender).font_size(11.0).opacity(0.6),
                    TextBlock::new()
                        .text(theme::fmt_time(msg.timestamp))
                        .font_size(11.0)
                        .opacity(0.45),
                )),
            Border::new()
                .corner_radius(6.0)
                .background(ThemeBrush::SolidBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(1.0)
                .padding(Thickness::xy(12.0, 8.0))
                .max_width(if compact { 280.0 } else { 480.0 })
                .content(StackPanel::new().spacing(6.0).children(inner)),
        ))
        .into()
}

// ---------- 公共视觉辅助件 ----------

fn chip_badge(text: &str, accent: bool) -> View {
    let mut border = Border::new()
        .corner_radius(4.0)
        .padding(Thickness::xy(6.0, 2.0))
        .background(ThemeBrush::SolidBackground)
        .border_thickness(1.0)
        .vertical_alignment(VerticalAlignment::Center);
    let mut label = TextBlock::new()
        .text(text)
        .font_size(11.0)
        .font_weight(FontWeight::SEMI_BOLD);
    if accent {
        border = border.border_brush(ThemeBrush::Accent);
        label = label.foreground(ThemeBrush::AccentText);
    } else {
        border = border.border_brush(ThemeBrush::CardStroke);
        label = label.opacity(0.8);
    }
    border.content(label).into()
}

fn stat_tile_content(value: &str, label: &str) -> StackPanel {
    StackPanel::new()
        .spacing(2.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .children((
            TextBlock::new()
                .text(value)
                .font_size(16.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .foreground(ThemeBrush::PrimaryText)
                .horizontal_alignment(HorizontalAlignment::Center),
            TextBlock::new()
                .text(label)
                .font_size(11.0)
                .foreground(ThemeBrush::PrimaryText)
                .opacity(0.6)
                .horizontal_alignment(HorizontalAlignment::Center),
        ))
}

fn stat_tile(value: &str, label: &str, url: Option<&str>, compact: bool) -> View {
    let content = stat_tile_content(value, label);
    if let Some(u) = url
        && let Ok(btn) = HyperlinkButton::new()
            .content(content.clone())
            .navigate_uri(u)
    {
        return if compact {
            btn.horizontal_alignment(HorizontalAlignment::Stretch)
                .into()
        } else {
            btn.into()
        };
    }
    let b = Border::new()
        .padding(Thickness::xy(6.0, 4.0))
        .content(content);
    if compact {
        b.horizontal_alignment(HorizontalAlignment::Stretch).into()
    } else {
        b.into()
    }
}

#[allow(clippy::too_many_arguments)]
fn relation_stat_button(
    value: &str,
    label: &str,
    kind: RelationKind,
    slice: &ListSliceState<RelationUser>,
    total: Option<u64>,
    avatar_cache: &ImageCache,
    flyout_width: f64,
    compact: bool,
    cb: &AccountCallbacks,
) -> View {
    let on_open = cb.on_open_relations.clone();
    let mut btn = Button::new()
        .style(ButtonStyle::Subtle)
        .content(
            Border::new()
                .padding(Thickness::xy(if compact { 0.0 } else { 8.0 }, 4.0))
                .content(stat_tile_content(value, label)),
        )
        .on_click(move || on_open.call(kind));
    if compact {
        btn = btn.horizontal_alignment(HorizontalAlignment::Stretch);
    }
    btn.flyout_with(
        Flyout::rich(relation_flyout_content(
            label,
            kind,
            slice,
            total,
            avatar_cache,
            flyout_width,
            cb,
        ))
        .placement(FlyoutPlacement::Bottom),
    )
}

fn relation_flyout_content(
    label: &str,
    kind: RelationKind,
    slice: &ListSliceState<RelationUser>,
    total: Option<u64>,
    avatar_cache: &ImageCache,
    flyout_width: f64,
    cb: &AccountCallbacks,
) -> View {
    let heading = match total {
        Some(n) => format!("{label} · {}", theme::fmt_count(n)),
        None => label.to_string(),
    };
    let on_refresh = cb.on_refresh_relations.clone();
    let header = Grid::new()
        .columns([GridLength::Star(1.0), GridLength::Auto])
        .children((
            TextBlock::new()
                .text(heading)
                .font_size(14.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .vertical_alignment(VerticalAlignment::Center),
            Button::new()
                .style(ButtonStyle::Subtle)
                .is_enabled(!slice.loading)
                .content("刷新")
                .grid_column(1)
                .on_click(move || on_refresh.call(kind)),
        ));

    let divider = Border::new().height(1.0).background(ThemeBrush::CardStroke);

    let on_open = cb.on_open_relations.clone();
    let on_more = cb.on_load_more_relations.clone();
    let row_width = (flyout_width - 28.0).max(240.0);
    let body: View = if slice.items.is_empty() {
        let inner = empty_or_error_with(
            slice.loading,
            slice.error.as_deref(),
            if slice.loaded_once {
                "暂无记录"
            } else {
                "加载"
            },
            move || on_open.call(kind),
        );
        Border::new().height(120.0).content(inner).into()
    } else {
        let mut keyed: Vec<KeyedView> = slice
            .items
            .iter()
            .map(|u| {
                KeyedView::new(
                    format!("u:{}", u.mid),
                    relation_user_row(u, row_width, avatar_cache),
                )
            })
            .collect();
        if let Some(footer) = list_footer_view(
            slice.loading,
            slice.has_more,
            slice.error.as_deref(),
            move || on_open.call(kind),
            move || on_more.call(kind),
        ) {
            keyed.push(KeyedView::new("footer", footer));
        }
        Border::new()
            .content(
                ScrollViewer::new()
                    .max_height(360.0)
                    .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(StackPanel::new().spacing(2.0).keyed_children(keyed)),
            )
            .into()
    };

    StackPanel::new()
        .width(flyout_width)
        .spacing(8.0)
        .children((header, divider, body))
        .into()
}

fn relation_user_row(user: &RelationUser, row_width: f64, avatar_cache: &ImageCache) -> View {
    let cached_img = (!user.face.is_empty())
        .then(|| avatar_cache.get(&avatar_thumbnail_url(&user.face)))
        .flatten();
    let avatar = theme::avatar_view(cached_img.as_ref(), &user.uname, 34.0, 6.0);

    let mut name_row: Vec<View> = vec![
        TextBlock::new()
            .text(user.uname.as_str())
            .font_size(13.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .foreground(ThemeBrush::PrimaryText)
            .max_width((row_width - 104.0).max(120.0))
            .max_lines(1)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if user.is_mutual() {
        name_row.push(chip_badge("互粉", false));
    }
    if user.special {
        name_row.push(chip_badge("特别", true));
    }

    let bio = if !user.sign.is_empty() {
        user.sign.as_str()
    } else {
        user.official_desc.as_str()
    };
    let mut info_col: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(name_row)
            .into(),
    ];
    if !bio.is_empty() {
        info_col.push(
            TextBlock::new()
                .text(bio)
                .font_size(11.0)
                .foreground(ThemeBrush::PrimaryText)
                .opacity(0.6)
                .max_lines(1)
                .text_trimming(TextTrimming::CharacterEllipsis)
                .into(),
        );
    }

    let row_grid = Grid::new()
        .width(row_width)
        .columns([GridLength::Auto, GridLength::Star(1.0)])
        .children((
            avatar.vertical_alignment(VerticalAlignment::Center),
            StackPanel::new()
                .spacing(2.0)
                .margin(Thickness::new(10.0, 0.0, 0.0, 0.0))
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .children(info_col),
        ));

    if let Ok(link) = HyperlinkButton::new()
        .content(row_grid.clone())
        .navigate_uri(user.space_url().as_str())
    {
        return link.into();
    }
    Border::new()
        .padding(Thickness::xy(8.0, 6.0))
        .content(row_grid)
        .into()
}

fn card_box(padding: Thickness, content: impl Into<View>) -> View {
    Border::new()
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .padding(padding)
        .content(content)
        .into()
}

fn link_action_button(
    play_label: &str,
    web_label: &str,
    bvid: Option<&str>,
    url: &str,
    title_hint: &str,
    on_open_video: &Callback<OpenTarget>,
) -> Option<View> {
    if let Some(bv) = bvid {
        let target = OpenTarget {
            bvid: bv.to_string(),
            title: title_hint.to_string(),
        };
        let cb = on_open_video.clone();
        return Some(
            Button::new()
                .style(ButtonStyle::Subtle)
                .content(play_label)
                .on_click(move || cb.call(target.clone()))
                .into(),
        );
    }
    if !url.is_empty()
        && let Ok(link) = HyperlinkButton::new().content(web_label).navigate_uri(url)
    {
        return Some(link.into());
    }
    None
}

fn notice_link_view(
    link: &NoticeLink,
    title_hint: &str,
    on_open_video: &Callback<OpenTarget>,
) -> Option<View> {
    link_action_button(
        &link.label,
        &link.label,
        link.bvid.as_deref(),
        &link.url,
        title_hint,
        on_open_video,
    )
}

fn empty_or_error_with(
    loading: bool,
    error: Option<&str>,
    empty_label: &str,
    on_retry: impl Fn() + 'static,
) -> View {
    if loading {
        return ProgressRing::new()
            .is_indeterminate(true)
            .is_active(true)
            .width(28.0)
            .height(28.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into();
    }
    if let Some(err) = error {
        return StackPanel::new()
            .spacing(10.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .children((
                TextBlock::new()
                    .text(err)
                    .font_size(12.0)
                    .opacity(0.65)
                    .horizontal_alignment(HorizontalAlignment::Center),
                Button::new()
                    .content("重试")
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .on_click(on_retry),
            ))
            .into();
    }
    TextBlock::new()
        .text(empty_label)
        .font_size(13.0)
        .opacity(0.55)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .into()
}

fn empty_or_error_view(
    loading: bool,
    error: Option<&str>,
    tab: NotifyTab,
    cb: &AccountCallbacks,
) -> View {
    let on_retry = cb.on_retry_tab.clone();
    empty_or_error_with(loading, error, "暂无内容", move || on_retry.call(tab))
}

fn list_footer_view(
    loading: bool,
    has_more: bool,
    error: Option<&str>,
    on_retry: impl Fn() + 'static,
    on_more: impl Fn() + 'static,
) -> Option<View> {
    if let Some(err) = error {
        return Some(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .margin(Thickness::xy(0.0, 6.0))
                .children((
                    TextBlock::new()
                        .text(err)
                        .font_size(12.0)
                        .opacity(0.65)
                        .vertical_alignment(VerticalAlignment::Center),
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .content("重试")
                        .on_click(on_retry),
                ))
                .into(),
        );
    }
    if has_more {
        return Some(
            Button::new()
                .style(ButtonStyle::Subtle)
                .content(if loading {
                    "加载中…"
                } else {
                    "加载更多"
                })
                .is_enabled(!loading)
                .horizontal_alignment(HorizontalAlignment::Center)
                .margin(Thickness::xy(0.0, 6.0))
                .on_click(on_more)
                .into(),
        );
    }
    None
}

fn append_list_footer(
    rows: &mut Vec<View>,
    loading: bool,
    has_more: bool,
    error: Option<&str>,
    tab: NotifyTab,
    cb: &AccountCallbacks,
) {
    let on_retry = cb.on_retry_tab.clone();
    let on_more = cb.on_load_more.clone();
    if let Some(footer) = list_footer_view(
        loading,
        has_more,
        error,
        move || on_retry.call(tab),
        move || on_more.call(tab),
    ) {
        rows.push(footer);
    }
}
