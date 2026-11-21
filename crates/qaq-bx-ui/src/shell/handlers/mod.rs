//! 域分发路由:将 Message 变体路由到各域的 handle()。本 match 穷尽覆盖全部变体且无
//! `_` 兜底臂 —— Message 新增变体而未路由时在此编译失败,替代原 giant match 的穷尽性。
//! 各域文件只含自己的臂(`_ => {}` 兜内层未命中,仅当路由表与域臂不同步时才可能触发)。

mod settings;
mod auth;
mod account_notify;
mod search;
mod window;
mod gamepad;
mod smtc;
mod comments;
mod danmaku;
mod lists;
mod peek;
mod home_feed;
mod cover_pipeline;
mod nav;
mod player_ctl;
mod player_media;

// 跨域共享的状态类型与纯函数(由各域文件再导出,供 mod.rs/tests 经 `use super::*` 可见):
pub(in crate::shell) use self::comments::CommentsSessionState;
pub(in crate::shell) use self::home_feed::{apply_feed_page, mount_pulse_active, spawn_feed_batch};
pub(in crate::shell) use self::lists::spawn_popular_fetch;
pub(in crate::shell) use self::player_media::{
    CachedMedia, missing_media_track, missing_native_video_track, MissingTrack, mpv_load_gate_settled,
    mpv_should_show_loading, native_load_gate_settled, native_load_stalled, native_should_show_loading,
    pages_menu_from, spawn_resolve_media,
};
pub(in crate::shell) use self::window::{ResizeStorm, ResizeStormTrack};

use super::*;

#[derive(Clone, Copy, Debug)]
enum Domain {
    Settings, Auth, AccountNotify, Search, Window, Gamepad, Smtc, Comments, Danmaku, Lists, Peek, HomeFeed, CoverPipeline, Nav, PlayerCtl, PlayerMedia,
}

pub(super) fn dispatch(m: &mut AppShell, message: Message, context: &ComponentContext<AppShell>) -> bool {
    let target = match &message {
        Message::ClearCaches => Domain::Settings,
        Message::CachesCleared(..) => Domain::Settings,
        Message::UpdateSettings(..) => Domain::Settings,
        Message::FlushSettingsSave => Domain::Settings,
        Message::SettingsSaved(..) => Domain::Settings,
        Message::AuthProbed { .. } => Domain::Auth,
        Message::AuthStartLogin => Domain::Auth,
        Message::LoginQrReady(..) => Domain::Auth,
        Message::LoginStartFailed(..) => Domain::Auth,
        Message::LoginPollTick => Domain::Auth,
        Message::LoginPolled(..) => Domain::Auth,
        Message::AuthCancelLogin => Domain::Auth,
        Message::AuthLogout => Domain::Auth,
        Message::AuthCancelLogout => Domain::Auth,
        Message::AuthLoggedOut(..) => Domain::Auth,
        Message::OpenNotifications => Domain::AccountNotify,
        Message::AccountRefresh => Domain::AccountNotify,
        Message::AccountStatLoaded { .. } => Domain::AccountNotify,
        Message::UnreadLoaded { .. } => Domain::AccountNotify,
        Message::SelectNotifyTab(..) => Domain::AccountNotify,
        Message::LoadMoreNotifyTab(..) => Domain::AccountNotify,
        Message::RetryNotifyTab(..) => Domain::AccountNotify,
        Message::RepliesLoaded { .. } => Domain::AccountNotify,
        Message::AtsLoaded { .. } => Domain::AccountNotify,
        Message::LikesLoaded { .. } => Domain::AccountNotify,
        Message::SystemsLoaded { .. } => Domain::AccountNotify,
        Message::SessionsLoaded { .. } => Domain::AccountNotify,
        Message::SelectWhisperSession(..) => Domain::AccountNotify,
        Message::CloseWhisperDetail => Domain::AccountNotify,
        Message::RefreshWhisperThread(..) => Domain::AccountNotify,
        Message::WhisperThreadLoaded { .. } => Domain::AccountNotify,
        Message::OpenRelations(..) => Domain::AccountNotify,
        Message::RefreshRelations(..) => Domain::AccountNotify,
        Message::LoadMoreRelations(..) => Domain::AccountNotify,
        Message::RelationsLoaded { .. } => Domain::AccountNotify,
        Message::SearchDraftChanged(..) => Domain::Search,
        Message::SearchSubmitted => Domain::Search,
        Message::SearchResults { .. } => Domain::Search,
        Message::SearchFailed { .. } => Domain::Search,
        Message::SearchDefaultReady(..) => Domain::Search,
        Message::SearchPickDefault => Domain::Search,
        Message::PortraitSynced { .. } => Domain::Window,
        Message::TogglePortraitDock => Domain::Window,
        Message::WindowResizeSettled => Domain::Window,
        Message::ShellInvalidateLayout => Domain::Window,
        Message::WindowResized(..) => Domain::Window,
        Message::GamepadInput(..) => Domain::Gamepad,
        Message::SmtcAction(..) => Domain::Smtc,
        Message::CommentDraftChanged(..) => Domain::Comments,
        Message::SubmitComment => Domain::Comments,
        Message::CommentSubmitted { .. } => Domain::Comments,
        Message::SetCommentSort(..) => Domain::Comments,
        Message::LoadMoreComments => Domain::Comments,
        Message::RetryComments => Domain::Comments,
        Message::CommentsLoaded { .. } => Domain::Comments,
        Message::ToggleSubReplies(..) => Domain::Comments,
        Message::LoadMoreSubReplies(..) => Domain::Comments,
        Message::SubRepliesLoaded { .. } => Domain::Comments,
        Message::DanmakuPart { .. } => Domain::Danmaku,
        Message::DanmakuFailed { .. } => Domain::Danmaku,
        Message::LiveRoomsLoaded { .. } => Domain::Lists,
        Message::LiveCoverReady { .. } => Domain::Lists,
        Message::LiveRoomSelected(..) => Domain::Lists,
        Message::LiveReady { .. } => Domain::Lists,
        Message::LiveMore => Domain::Lists,
        Message::HistoryLoaded { .. } => Domain::Lists,
        Message::HistoryMore => Domain::Lists,
        Message::WatchLaterLoaded { .. } => Domain::Lists,
        Message::WatchLaterMore => Domain::Lists,
        Message::FavFoldersLoaded { .. } => Domain::Lists,
        Message::FavSelectFolder(..) => Domain::Lists,
        Message::FavResourcesLoaded { .. } => Domain::Lists,
        Message::FavMore => Domain::Lists,
        Message::UserCoverReady { .. } => Domain::Lists,
        Message::PopularMore => Domain::Lists,
        Message::PopularLoaded(..) => Domain::Lists,
        Message::ListAutoMore(..) => Domain::Lists,
        Message::PreResolveVideo(..) => Domain::Peek,
        Message::MediaPreResolved { .. } => Domain::Peek,
        Message::CardHover(..) => Domain::Peek,
        Message::PeekPrefetchTick(..) => Domain::Peek,
        Message::PeekPopTick(..) => Domain::Peek,
        Message::PeekGraceTick(..) => Domain::Peek,
        Message::PeekAnimTick(..) => Domain::Peek,
        Message::PeekPointerEntered => Domain::Peek,
        Message::PeekPointerExited => Domain::Peek,
        Message::PeekDescReady { .. } => Domain::Peek,
        Message::PeekPlay(..) => Domain::Peek,
        Message::PeekWatchLater(..) => Domain::Peek,
        Message::PeekWatchLaterDone { .. } => Domain::Peek,
        Message::PeekClose => Domain::Peek,
        Message::HomeRefresh => Domain::HomeFeed,
        Message::HomeRowActive { .. } => Domain::HomeFeed,
        Message::HomeRetry => Domain::HomeFeed,
        Message::HomeLoaded { .. } => Domain::HomeFeed,
        Message::HomeFailed { .. } => Domain::HomeFeed,
        Message::CoverReady { .. } => Domain::CoverPipeline,
        Message::FlushCovers => Domain::CoverPipeline,
        Message::AvatarLoaded { .. } => Domain::CoverPipeline,
        Message::FlushPlaybackAvatars => Domain::CoverPipeline,
        Message::Back => Domain::Nav,
        #[cfg(feature = "selftest")]
        Message::SelfTestSync => Domain::Nav,
        Message::NavTagSelected(..) => Domain::Nav,
        Message::PaneOpenChanged(..) => Domain::Nav,
        Message::TogglePane => Domain::Nav,
        Message::ToggleMoreMenu => Domain::Nav,
        Message::CloseMoreMenu => Domain::Nav,
        Message::NavigateCompact(..) => Domain::Nav,
        Message::ToggleSearchOverlay => Domain::Nav,
        Message::CloseSearchOverlay => Domain::Nav,
        Message::DemoTick => Domain::Nav,
        Message::ToastActivated(..) => Domain::Nav,
        Message::OpenVideo(..) => Domain::PlayerCtl,
        Message::TogglePlay => Domain::PlayerCtl,
        Message::Seek(..) => Domain::PlayerCtl,
        Message::SetVolume(..) => Domain::PlayerCtl,
        Message::ToggleMute => Domain::PlayerCtl,
        Message::ToggleDanmaku => Domain::PlayerCtl,
        Message::EnterFullscreen => Domain::PlayerCtl,
        Message::ExitFullscreen => Domain::PlayerCtl,
        Message::ToggleFullscreen => Domain::PlayerCtl,
        Message::FullscreenSynced { .. } => Domain::PlayerCtl,
        Message::FullscreenProbeResult { .. } => Domain::PlayerCtl,
        Message::PlayerHotkey(..) => Domain::PlayerCtl,
        Message::ExpandHalfPane => Domain::PlayerCtl,
        Message::CloseHalfPane => Domain::PlayerCtl,
        Message::ResizeHalfPane(..) => Domain::PlayerCtl,
        Message::RelatedLoaded { .. } => Domain::PlayerCtl,
        Message::RelatedRetry => Domain::PlayerCtl,
        Message::OpenVideoTarget(..) => Domain::PlayerCtl,
        Message::PositionTick => Domain::PlayerMedia,
        Message::MediaReady { .. } => Domain::PlayerMedia,
        Message::SelectQuality(..) => Domain::PlayerMedia,
        Message::SelectPage(..) => Domain::PlayerMedia,
        Message::PageStreamReady { .. } => Domain::PlayerMedia,
        Message::MediaFailed { .. } => Domain::PlayerMedia,
        Message::VideoMetaLoaded { .. } => Domain::PlayerMedia,
    };
    let skip_tail = match target {
        Domain::Settings => m.handle_settings(message, context),
        Domain::Auth => m.handle_auth(message, context),
        Domain::AccountNotify => m.handle_account_notify(message, context),
        Domain::Search => m.handle_search(message, context),
        Domain::Window => m.handle_window(message, context),
        Domain::Gamepad => m.handle_gamepad(message, context),
        Domain::Smtc => m.handle_smtc(message, context),
        Domain::Comments => m.handle_comments(message, context),
        Domain::Danmaku => m.handle_danmaku(message, context),
        Domain::Lists => m.handle_lists(message, context),
        Domain::Peek => m.handle_peek(message, context),
        Domain::HomeFeed => m.handle_home_feed(message, context),
        Domain::CoverPipeline => m.handle_cover_pipeline(message, context),
        Domain::Nav => m.handle_nav(message, context),
        Domain::PlayerCtl => m.handle_player_ctl(message, context),
        Domain::PlayerMedia => m.handle_player_media(message, context),
    };
    if !skip_tail {
        // 原 update() giant match 之后的尾后语句;臂内顶层 return(→ true)会跳过 —— 语义保持。
        m.reconcile_playback_visibility(context);
        // #7 兜底作废:路由/浮层/全屏/紧凑档翻转后仍在飞的预览统一收起。
        if m.peek.phase_open() && !m.peek_allowed() {
            m.peek_hide_now();
        }
        m.refresh_probe();
    }
    skip_tail
}
