//! 路由：导航页签与页面分发（Player 页由 shell 单独处理，不进导航）。

use windows_reactor::*;
use crate::pages;

/// 应用内目的地。`Player` / `Login` 不出现在导航栏（分别由卡片点击与页脚账号入口触发）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Popular,
    Following,
    Live,
    Favorites,
    History,
    WatchLater,
    Search,
    Login,
    Settings,
    Player,
}

/// 导航栏 menu_items（页脚另有账号 + 设置）。
pub const NAV_ROUTES: [Route; 7] = [
    Route::Home,
    Route::Popular,
    Route::Following,
    Route::Live,
    Route::Favorites,
    Route::History,
    Route::WatchLater,
];

impl Route {
    pub fn tag(self) -> &'static str {
        match self {
            Route::Home => "home",
            Route::Popular => "popular",
            Route::Following => "following",
            Route::Live => "live",
            Route::Favorites => "favorites",
            Route::History => "history",
            Route::WatchLater => "watch-later",
            Route::Search => "search",
            Route::Login => "login",
            Route::Settings => "settings",
            Route::Player => "player",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Route::Home => "推荐",
            Route::Popular => "热门",
            Route::Following => "关注",
            Route::Live => "直播",
            Route::Favorites => "收藏",
            Route::History => "历史",
            Route::WatchLater => "稍后再看",
            Route::Search => "搜索",
            Route::Login => "登录",
            Route::Settings => "设置",
            Route::Player => "正在播放",
        }
    }

    pub fn icon(self) -> Symbol {
        match self {
            Route::Home => Symbol::Home,
            Route::Popular => Symbol::Like,
            Route::Following => Symbol::People,
            Route::Live => Symbol::Camera,
            Route::Favorites => Symbol::Favorite,
            Route::History => Symbol::Clock,
            Route::WatchLater => Symbol::TwoPage,
                        Route::Search => Symbol::Find,
            Route::Login => Symbol::Account,
            Route::Settings => Symbol::Setting,
            Route::Player => Symbol::Play,
        }
    }

    /// `Player` / `Login` 永远不通过导航标签选中。
    pub fn from_tag(tag: &str) -> Option<Self> {
        [
            NAV_ROUTES.as_slice(),
            &[Route::Search, Route::Login, Route::Settings],
        ]
        .concat()
        .into_iter()
        .find(|route| route.tag() == tag)
    }
}

/// 非播放页分发（Home / Player / Login 由 AppShell 直接组装，分支仅为穷尽兜底）。
pub fn route(route: Route, on_open: Callback<String>) -> View {
    match route {
        Route::Popular => pages::popular::popular_page(on_open),
        Route::Following => pages::stub::stub_page(
            "关注",
            "登录后查看关注 UP 的最新动态",
            "数据源待接入",
            "动态 feed API 未实现；登录凭据已在本地（bili-plugin-login），接入后此页自动启用。",
        ),
        Route::Live => pages::stub::stub_page(
            "直播",
            "正在直播的推荐与关注",
            "数据源待接入",
            "直播 feed API 未实现，骨架阶段保留空态。",
        ),
        Route::Favorites => pages::stub::stub_page(
            "收藏",
            "登录账号的收藏夹",
            "数据源待接入",
            "收藏夹列表 API 未实现；凭据就绪后接入。",
        ),
        Route::History => pages::stub::stub_page(
            "历史",
            "观看历史与断点续播",
            "数据源待接入",
            "历史记录 API 未实现；断点续播依赖播放进度上报，随视频通道一并接入。",
        ),
        Route::WatchLater => pages::stub::stub_page(
            "稍后再看",
            "稍后再看列表",
            "数据源待接入",
            "稍后再看 API 未实现，骨架阶段保留空态。",
        ),
        Route::Search => pages::search::search_page(),
        // Home / Player / Login / Settings 由 shell 直接组装（共享壳层状态与回调）。
        Route::Home | Route::Player | Route::Login | Route::Settings => {
            pages::stub::stub_page("推荐", "推荐流加载中…", "", "")
        }
    }
}
