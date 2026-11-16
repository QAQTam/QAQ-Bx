//! 应用壳：TitleBar + NavigationView + 路由 + 迷你播放条 + 登录状态机
//! （`docs/ui-design.md` §1/§5；登录桥接见 `crate::auth`）。
//! 播放与登录状态在此单源持有，页面组件纯展示 + 回调上报。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bili_core::account::{AccountStat, NavClient, RelationKind, RelationPage, RelationUser};
use bili_core::danmaku::{DanmakuClient, DanmakuElem};
use bili_core::feed::{fetch_rcmd_batch, FeedCard, FeedClient, FeedKind, FeedSession};
use bili_core::notify::{
    AtNotice, FeedCursor, LikeNotice, ReplyNotice, SystemNotice, UnreadSummary, WhisperMessage,
    WhisperSession,
};
use bili_core::player::{PlayerClient, StreamChoice, VideoMeta};
use bili_core::reply::{ReplyClient, ReplyCursor, ReplyItem, ReplyPage, ReplySort, SubReplyPage};
use bili_core::settings::{AppSettings, FileSettingsStore, SETTINGS_FILE};
use bili_video::engine::{OverlayDanmaku, VideoConfig, VideoEngine};
use windows_reactor::*;

use crate::auth::{
    avatar_thumbnail_url, Auth, AuthState, FlowCell, LoginFlowState, PollOutcome, ProbeReport,
};
use crate::data::{self, VideoItem};
use crate::feed_pager;
use crate::gamepad::{self, GamepadAction, GamepadEvent};
use crate::image_cache::ImageCache;
use crate::pages;
use crate::pages::account::{
    AccountCallbacks, AccountViewData, ListSliceState, NotifyTab, OpenTarget, WhisperThreadState,
};
use crate::pages::player::{
    CommentCallbacks, CommentSort, CommentsData, CommentsSnapshot, SubReplyState,
};
use crate::router::{self, Route, NAV_ROUTES};
use crate::runtime::{block_on, cover_semaphore};
use crate::smtc::{SmtcCommand, SmtcController};
use crate::widgets::mini_player;
use crate::widgets::video_card::{self, HomeSlot};

/// 封面下载完成回填 UI 的批处理合并窗口（避免每到 1 张封面重绘整页）。
const COVER_FLUSH_INTERVAL: Duration = Duration::from_millis(80);
/// Hover 预解析缓存有效期（CDN 签名时效内安全复用）。
const PRERESOLVE_TTL: Duration = Duration::from_secs(180);
/// 窄屏（竖屏手机/掌机）自适应断点。
const COMPACT_BREAKPOINT: f64 = 760.0;
/// Hover 预解析最多缓存条目数。
const MAX_PRERESOLVE_CACHE: usize = 16;
/// 关注/粉丝挂件每页拉取条数。
const RELATION_PAGE_SIZE: u32 = 20;
/// TitleBar 应用图标（48×48 透明圆角 PNG，编译期嵌入）。
const APP_ICON_48_PNG: &[u8] = include_bytes!("../../../packaging/store/QAQ-Bx-48.png");
/// 同时在飞的 Hover 预解析上限（防连续快速扫过触发风控）。
const MAX_PRERESOLVE_INFLIGHT: usize = 4;
/// 离屏封面纹理裁剪窗口（保留当前行上下各 4 屏，更远仅保留轻量元数据）。
const COVER_WINDOW_BUFFER_SCREENS: usize = 4;
/// 距离列表底部不足几行时预拉下一逻辑页（留出 ~1 屏无感缓冲）。
const HOME_PREFETCH_LEAD_ROWS: usize = 2;
/// 单流最大加载卡片上限（配合按行虚拟化与封面回收，长流滚动低内存）。
const MAX_HOME_SLOTS: usize = 1_200;
/// 播放状态刷新节拍（引擎快照轮询;0.25s 保证进度条顺滑）。
const PLAYBACK_TICK: Duration = Duration::from_millis(250);
/// 设置滑块（音量/透明度）去抖落盘窗口。
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// 升级 https 并对 B 站图床追加 `@480w_270h_1c.jpg` 缩略图后缀，
/// 把单张封面流量与 WinUI 解码像素面积压缩到原图的 ~1/16–1/46。
pub fn thumbnail_url(raw: &str) -> String {
    let url = raw.replacen("http://", "https://", 1);
    if url.contains(".hdslb.com/") && !url.contains('@') && !url.contains('?') {
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".webp")
        {
            return format!("{url}@480w_270h_1c.jpg");
        }
    }
    url
}

/// FeedCard → 展示模型（`goto != av` 已由 `FeedSession::absorb` 过滤）。
fn video_item(card: &FeedCard) -> VideoItem {
    VideoItem {
        bvid: card.bvid.clone().unwrap_or_default(),
        aid: card.id,
        cid: card.cid.unwrap_or(0),
        title: card.title.clone().unwrap_or_default(),
        author: card.owner.as_ref().map_or_else(String::new, |owner| owner.name.clone()),
        author_mid: card.owner.as_ref().map_or(0, |owner| owner.mid),
        author_face_url: card
            .owner
            .as_ref()
            .and_then(|owner| owner.face.as_deref())
            .and_then(normalize_face_url),
        duration_secs: card.duration.unwrap_or_default().clamp(0, i64::from(u32::MAX)) as u32,
        play_count: card.stat.as_ref().map_or(0, |stat| stat.view.unsigned_abs()),
        danmaku_count: card.stat.as_ref().map_or(0, |stat| stat.danmaku.unsigned_abs()),
        cover: None,
        pic_url: card.pic.as_deref().map(thumbnail_url),
    }
}

fn normalize_face_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(if let Some(rest) = trimmed.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        trimmed.replacen("http://", "https://", 1)
    })
}

const SUB_REPLY_PAGE_SIZE: u32 = 10;

#[derive(Default)]
struct ReplyBucket {
    loading: bool,
    loaded_once: bool,
    has_more: bool,
    error: Option<String>,
    cursor: Option<ReplyCursor>,
    total_count: Option<u64>,
    top_replies: Vec<ReplyItem>,
    replies: Vec<ReplyItem>,
    seen_rpids: HashSet<u64>,
}

#[derive(Default)]
struct CommentsSessionState {
    aid: u64,
    sort: CommentSort,
    upper_mid: u64,
    hot: ReplyBucket,
    latest: ReplyBucket,
    sub_replies: HashMap<u64, SubReplyState>,
}

impl CommentsSessionState {
    fn reset(&mut self, aid: u64, upper_mid: u64) {
        self.aid = aid;
        self.sort = CommentSort::Hot;
        self.upper_mid = upper_mid;
        self.hot = ReplyBucket::default();
        self.latest = ReplyBucket::default();
        self.sub_replies.clear();
    }

    fn bucket(&self, sort: CommentSort) -> &ReplyBucket {
        match sort {
            CommentSort::Hot => &self.hot,
            CommentSort::Latest => &self.latest,
        }
    }

    fn bucket_mut(&mut self, sort: CommentSort) -> &mut ReplyBucket {
        match sort {
            CommentSort::Hot => &mut self.hot,
            CommentSort::Latest => &mut self.latest,
        }
    }

    fn references_avatar(&self, thumb_url: &str) -> bool {
        let bucket = self.bucket(self.sort);
        let in_main = bucket
            .top_replies
            .iter()
            .chain(bucket.replies.iter())
            .any(|item| !item.avatar.is_empty() && avatar_thumbnail_url(&item.avatar) == thumb_url);
        if in_main {
            return true;
        }
        self.sub_replies.values().any(|sub| {
            sub.expanded
                && sub
                    .items
                    .iter()
                    .any(|item| !item.avatar.is_empty() && avatar_thumbnail_url(&item.avatar) == thumb_url)
        })
    }

    fn to_snapshot(&self, rev: u64, avatar_cache: &ImageCache) -> CommentsSnapshot {
        let bucket = self.bucket(self.sort);
        let mut avatars = HashMap::new();
        let collect_avatar = |raw: &str, out: &mut HashMap<String, EncodedImage>| {
            if raw.is_empty() {
                return;
            }
            let thumb = avatar_thumbnail_url(raw);
            if !out.contains_key(&thumb)
                && let Some(img) = avatar_cache.get(&thumb)
            {
                out.insert(thumb, img);
            }
        };
        for item in bucket.top_replies.iter().chain(bucket.replies.iter()) {
            collect_avatar(&item.avatar, &mut avatars);
        }
        for sub in self.sub_replies.values() {
            if sub.expanded {
                for item in &sub.items {
                    collect_avatar(&item.avatar, &mut avatars);
                }
            }
        }
        let loading = bucket.loading && bucket.top_replies.is_empty() && bucket.replies.is_empty();
        let loading_more = bucket.loading && (!bucket.top_replies.is_empty() || !bucket.replies.is_empty());
        CommentsSnapshot {
            rev,
            data: Arc::new(CommentsData {
                aid: self.aid,
                sort: self.sort,
                upper_mid: self.upper_mid,
                total_count: bucket.total_count,
                loading,
                loading_more,
                loaded_once: bucket.loaded_once,
                has_more: bucket.has_more,
                error: bucket.error.clone(),
                top_replies: bucket.top_replies.clone(),
                replies: bucket.replies.clone(),
                sub_replies: self.sub_replies.clone(),
                avatars,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolveProfileKey {
    pub max_qn: i64,
    pub codec_idx: usize,
}

#[derive(Clone)]
struct CachedMedia {
    key: ResolveProfileKey,
    video_url: String,
    audio_url: String,
    meta: VideoMeta,
    fetched_at: Instant,
}

/// 结合设置与当前大会员缓存推导请求清晰度上限和编码优先级（不发 IO）。
fn resolve_params_for(
    settings: &AppSettings,
    nav: &NavClient,
) -> (i64, &'static [&'static str]) {
    let is_vip = nav
        .cached_profile()
        .is_some_and(|profile| profile.vip.is_vip());
    (
        settings.quality.effective_max_qn(is_vip),
        settings.codec.priority(),
    )
}

fn resolve_profile_key(settings: &AppSettings, nav: &NavClient) -> ResolveProfileKey {
    let (max_qn, _) = resolve_params_for(settings, nav);
    ResolveProfileKey {
        max_qn,
        codec_idx: settings.codec.index(),
    }
}

fn video_config_from(settings: &AppSettings) -> VideoConfig {
    VideoConfig {
        hardware_decode: settings.hardware_decode,
        danmaku_enabled: settings.danmaku_enabled,
        danmaku_opacity: settings.danmaku_opacity,
        danmaku_max_lanes: settings.danmaku_max_lanes,
    }
}

/// AppShell 输入:正常运行恒为默认值;selftest 下携带共享探针句柄,
/// 状态经 reactor `test` 模块的 Input 句柄流出(reactor 测试同款模式)。
#[derive(Clone, Default, PartialEq)]
pub struct ShellInput {
    /// selftest 共享探针句柄(正常路径恒为 `None`,零开销)。
    pub probe: Option<std::rc::Rc<std::cell::RefCell<ShellProbe>>>,
}

/// selftest 探针:AppShell 断言所需的最小状态快照。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellProbe {
    pub route_tag: String,
    pub history_len: usize,
    pub playback_gen: u64,
    pub now_playing_bvid: Option<String>,
    pub comments_aid: u64,
    pub comments_sort_latest: bool,
    pub hot_loaded: bool,
    pub hot_replies: usize,
    pub hot_total: Option<u64>,
    pub latest_loaded: bool,
    pub latest_replies: usize,
}

pub struct AppShell {
    auth_service: Arc<Auth>,
    /// 设置持久化存储（`data/settings.json`）+ 运行时唯一配置源。
    settings_store: Arc<FileSettingsStore>,
    settings: AppSettings,
    settings_notice: Option<String>,
    settings_save_timer: Option<ComponentTimer>,
    /// 后台任务代次（切视频/换一换/重探登录态时递增，拦截在飞旧结果串台）。
    playback_gen: u64,
    playback_epoch: Arc<AtomicU64>,
    feed_gen: u64,
    feed_epoch: Arc<AtomicU64>,
    auth_gen: u64,
    /// 视频评论区客户端（复用共享凭据与票据）。
    reply_client: Arc<ReplyClient>,
    /// 首页推荐流客户端（WBI 签名，凭据复用登录态）。
    feed_client: Arc<FeedClient>,
    /// 首页流会话状态机（与后台请求任务共享）。
    feed_session: Arc<Mutex<FeedSession>>,
    /// 首页槽位序列（先分配骨架槽位再原地填真实卡片，保证布局尺寸稳定不抖动）。
    home_slots: Vec<HomeSlot>,
    /// 缓存的首页虚拟化数据源（仅在 `home_slots`/`grid_cols` 变更时重建，避免滚动触发整页行刷新）。
    home_source: VirtualSource,
    home_on_open: Callback<String>,
    home_on_hover: Callback<String>,
    home_on_row_active: Callback<(u64, usize, bool)>,
    home_active_rows: BTreeSet<usize>,
    /// bvid → 当前 `home_slots` 下标（封面批刷 O(1) 原地更新）。
    home_slot_by_bvid: HashMap<String, usize>,
    /// bvid → 卡片（点卡进播放页与 Hover 预解析的数据源）。
    home_index: HashMap<String, VideoItem>,
    /// 自增槽位序号（确保同一轮换代次内新追加的骨架键全局唯一）。
    home_next_slot_seq: usize,
    /// 当前在飞批预分配的末尾骨架数量。
    home_pending_skeletons: usize,
    /// 首页展示状态（加载中/错误/页数/总视频数/终态）。
    home_state: pages::home::HomeState,
    /// 上次发起的请求类别与目标条数（失败重试重发同类别）。
    home_last_kind: FeedKind,
    home_last_target: usize,
    /// 换一换在预取请求在飞时点击的待办标记（在飞结果落地后改发 Refresh）。
    home_refresh_pending: bool,
    /// 最近一次虚拟化行挂载锚点与封面裁剪锚点行。
    home_anchor_row: usize,
    home_last_evict_row: usize,
    home_viewport_w: f64,
    home_viewport_h: f64,
    /// 封面异步回填批缓存 + 在飞去重表 + 合并刷帧定时器。
    pending_covers: HashMap<String, EncodedImage>,
    cover_inflight: HashSet<String>,
    cover_flush_timer: Option<ComponentTimer>,
    /// Hover 预解析短期缓存（仅缓存直链不自动播放）与在飞追踪。
    preresolved: HashMap<String, CachedMedia>,
    preresolved_order: VecDeque<String>,
    preresolve_inflight: HashMap<String, ResolveProfileKey>,
    pending_open_preresolve: Option<(String, u64, ResolveProfileKey)>,
    /// 封面位图缓存（url → 位图；回滚/重复卡复用，换一换清空）。
    image_cache: Arc<ImageCache>,
    /// 登录态（页脚 + 登录页共用）。
    auth: AuthState,
    /// 扫码流程展示状态。
    login_flow: LoginFlowState,
    /// 登录会话槽（与轮询后台线程共享）。
    flow: FlowCell,
    /// 登出二次确认（防误触作废服务端会话）。
    logout_confirm: bool,
    /// 登录页操作结果提示（如登出失败原因）。
    auth_notice: Option<String>,
    /// 个人中心与通知中心状态（登录后有效，登出/切换账号时递增 `notify_gen` 拦截旧回调）。
    notify_gen: u64,
    account_stat: Option<AccountStat>,
    unread_summary: UnreadSummary,
    followings: ListSliceState<RelationUser>,
    followings_next_pn: u32,
    followers: ListSliceState<RelationUser>,
    followers_next_pn: u32,
    notify_tab: NotifyTab,
    notify_replies: ListSliceState<ReplyNotice>,
    notify_replies_cursor: Option<FeedCursor>,
    notify_ats: ListSliceState<AtNotice>,
    notify_ats_cursor: Option<FeedCursor>,
    notify_likes: ListSliceState<LikeNotice>,
    notify_likes_cursor: Option<FeedCursor>,
    notify_systems: ListSliceState<SystemNotice>,
    notify_systems_cursor: Option<u64>,
    notify_sessions: ListSliceState<WhisperSession>,
    notify_sessions_end_ts: Option<u64>,
    selected_whisper: Option<(u64, i32)>,
    whisper_threads: HashMap<(u64, i32), WhisperThreadState>,
    avatar_gen: u64,
    avatar_cache: Arc<ImageCache>,
    avatar_inflight: HashMap<String, u64>,
    route: Route,
    history: Vec<Route>,
    pane_open: bool,
    now_playing: Option<VideoItem>,
    now_playing_desc: Option<String>,
    meta_fetch_inflight_seq: Option<u64>,
    comments_session: CommentsSessionState,
    /// selftest 探针句柄(正常路径恒为 `None`)。
    probe: Option<std::rc::Rc<std::cell::RefCell<ShellProbe>>>,
    comments_snapshot: CommentsSnapshot,
    comments_rev: u64,
    playback_avatar_dirty: bool,
    playback_avatar_timer: Option<ComponentTimer>,
    playing: bool,
    position: u32,
    mount_tick: u32,
    volume: u32,
    /// 引擎回报的媒体总长(秒;None = 尚未加载,用卡片数据兜底)。
    engine_duration: Option<f64>,
    /// 媒体失败/引擎错误提示(播放页显示一次)。
    player_notice: Option<String>,
    /// 视频引擎(SwapChainPanel 渲染 + mpv;惰性按需创建，关闭迷你条即释放全部解码/D3D 内存)。
    engine: Option<Arc<VideoEngine>>,
    /// 当前播放项的元信息(cid/aid/duration;弹幕拉取参数)。
    current_meta: Option<VideoMeta>,
    /// 播放状态节拍定时器：Drop 即取消投递，必须持有。
    timer: Option<ComponentTimer>,
    /// 登录轮询定时器（2s，独立于播放节拍）。
    login_timer: Option<ComponentTimer>,
    /// Windows 控制中心媒体控件（SMTC）控制器。
    smtc: Option<SmtcController>,
    smtc_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<SmtcCommand>>>>,
    smtc_bvid: Option<String>,
    smtc_playing: Option<bool>,
    /// Xbox 手柄（XInput）连接与激活状态。
    gamepad_active: bool,
    gamepad_rx: Arc<Mutex<std::sync::mpsc::Receiver<GamepadEvent>>>,
    home_focus_idx: usize,
    /// 首页右侧半窗预览是否展开。
    home_half_pane: bool,
    /// 当前窗口宽度（用于宽屏左侧栏 ↔ 窄屏底栏自适应切换）。
    window_width: f64,
    /// 窄屏底栏“更多”1×5 向上弹窗开关。
    more_menu_open: bool,
    /// MSIX / Store 打包运行时的 Windows App Notifications 激活注册句柄。
    _app_notifications: Option<crate::winrt_ext::AppNotificationGuard>,
    toast_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<()>>>>,
}

#[derive(Clone)]
pub enum Message {
    Back,
    /// selftest 空转消息:驱动探针在队列尾部刷新状态(生产路径零开销)。
    #[cfg(feature = "selftest")]
    SelfTestSync,
    NavTagSelected(Option<String>),
    PaneOpenChanged(bool),
    TogglePane,
    OpenVideo(String),
    TogglePlay,
    Seek(f64),
    SetVolume(f64),
    /// 引擎状态节拍(250ms;读引擎快照刷新 UI 单源)。
    PositionTick,
    /// 后台解析完成(分轨直链)→ 引擎 loadfile + 弹幕拉取（带代次校验防切视频串台）。
    MediaReady {
        seq: u64,
        video_url: String,
        audio_url: String,
        meta: VideoMeta,
    },
    /// 全片弹幕已解析(sort by progress)。
    DanmakuLoaded { seq: u64, elems: Vec<DanmakuElem> },
    /// 弹幕拉取失败(静默;播放不受影响)。
    DanmakuFailed { seq: u64, reason: String },
    /// 解析/播放准备失败(播放页提示)。
    MediaFailed { seq: u64, reason: String },
    /// 队列功能待接入（下一首占位）。
    MiniNext,
    /// 关闭迷你播放条：停止并销毁惰性 VideoEngine，归还播放底座与 D3D11 内存。
    ClosePlayer,
    /// 首页右侧半窗预览：展开进入完整播放页 / 关闭半窗 / 记忆宽度。
    ExpandHalfPane,
    CloseHalfPane,
    ResizeHalfPane(f64),
    ToggleMoreMenu,
    CloseMoreMenu,
    NavigateCompact(Route),
    /// Windows 控制中心媒体按钮回调（None 表示通道已关闭）。
    SmtcAction(Option<SmtcCommand>),
    /// Xbox 手柄事件回调（None 表示通道已关闭）。
    GamepadInput(Option<GamepadEvent>),
    /// Windows Toast 通知激活回调（false 表示通道关闭）。
    ToastActivated(bool),

    // ---- 首页推荐流与 Hover 预解析 ----
    /// 换一换（Refresh 语义：清流 + 预分配首批骨架 + 重发）。
    HomeRefresh,
    /// 首页网格某一行被虚拟化容器实际挂载或回收（无感续页 + 离屏封面窗口锚点）。
    HomeRowActive { rev: u64, row_idx: usize, active: bool },
    /// 窗口尺寸变化：同步自适应网格列数与每行视口缓冲。
    WindowResized(windows_reactor::WindowSize),
    /// 上次请求失败后的重试（重发上次类别）。
    HomeRetry,
    /// 逻辑页批抓取已吸收（`replace` = 换一换/首屏；`exhausted` = 真无新增定格最后一页）。
    HomeLoaded {
        seq: u64,
        cards: Vec<VideoItem>,
        exhausted: bool,
        replace: bool,
    },
    HomeFailed { seq: u64, reason: String },
    /// 封面异步加载完成（先攒进 pending_covers，再定时批量刷入 UI）。
    CoverReady {
        seq: u64,
        bvid: String,
        image: Option<EncodedImage>,
    },
    FlushCovers,
    /// 卡片悬停收缩-扩张动画确认后发起静默预解析（只存缓存不自动播放）。
    PreResolveVideo(String),
    MediaPreResolved {
        bvid: String,
        key: ResolveProfileKey,
        result: Result<(VideoMeta, StreamChoice), String>,
    },

    // ---- 设置持久化 ----
    UpdateSettings(AppSettings),
    FlushSettingsSave,
    SettingsSaved(Option<String>),

    // ---- 登录（桥接 crate::auth）----
    /// 启动/登录成功后的 nav 探测结果（含自动续期状态）。
    AuthProbed { seq: u64, report: ProbeReport },
    AuthStartLogin,
    LoginQrReady(windows_reactor::EncodedImage),
    LoginStartFailed(String),
    LoginPollTick,
    LoginPolled(PollOutcome),
    AuthCancelLogin,
    AuthLogout,
    AuthCancelLogout,
    /// None = 成功；Some = 失败原因。
    AuthLoggedOut(Option<String>),

    // ---- 个人中心与通知中心 ----
    OpenNotifications,
    OpenVideoTarget(OpenTarget),
    AccountRefresh,
    AccountStatLoaded {
        seq: u64,
        stat: Option<AccountStat>,
    },
    UnreadLoaded {
        seq: u64,
        summary: Option<UnreadSummary>,
    },
    SelectNotifyTab(NotifyTab),
    LoadMoreNotifyTab(NotifyTab),
    RetryNotifyTab(NotifyTab),
    RepliesLoaded {
        seq: u64,
        append: bool,
        res: Result<bili_core::notify::FeedPage<ReplyNotice>, String>,
    },
    AtsLoaded {
        seq: u64,
        append: bool,
        res: Result<bili_core::notify::FeedPage<AtNotice>, String>,
    },
    LikesLoaded {
        seq: u64,
        append: bool,
        res: Result<bili_core::notify::FeedPage<LikeNotice>, String>,
    },
    SystemsLoaded {
        seq: u64,
        append: bool,
        res: Result<bili_core::notify::SystemPage, String>,
    },
    SessionsLoaded {
        seq: u64,
        append: bool,
        res: Result<bili_core::notify::SessionPage, String>,
    },
    SelectWhisperSession((u64, i32)),
    RefreshWhisperThread((u64, i32)),
    WhisperThreadLoaded {
        seq: u64,
        talker_id: u64,
        session_type: i32,
        res: Result<Vec<WhisperMessage>, String>,
    },
    AvatarLoaded {
        avatar_gen: u64,
        inflight_epoch: u64,
        url: String,
        has_image: bool,
    },
    FlushPlaybackAvatars,
    VideoMetaLoaded {
        seq: u64,
        res: Result<VideoMeta, String>,
    },
    SetCommentSort(CommentSort),
    LoadMoreComments,
    RetryComments,
    CommentsLoaded {
        seq: u64,
        sort: CommentSort,
        append: bool,
        res: Result<ReplyPage, String>,
    },
    ToggleSubReplies(u64),
    LoadMoreSubReplies(u64),
    SubRepliesLoaded {
        seq: u64,
        root_rpid: u64,
        append: bool,
        res: Result<SubReplyPage, String>,
    },
    OpenRelations(RelationKind),
    RefreshRelations(RelationKind),
    LoadMoreRelations(RelationKind),
    RelationsLoaded {
        seq: u64,
        kind: RelationKind,
        append: bool,
        res: Result<RelationPage, String>,
    },
}

impl AppShell {
    /// selftest:将断言所需最小状态刷入共享探针(update 入口统一调用,
    /// 探针句柄为 `None` 时零开销直返)。
    fn refresh_probe(&self) {
        let Some(probe) = &self.probe else { return };
        let hot = &self.comments_session.hot;
        let latest = &self.comments_session.latest;
        let mut out = probe.borrow_mut();
        out.route_tag = self.route.tag().to_string();
        out.history_len = self.history.len();
        out.playback_gen = self.playback_gen;
        out.now_playing_bvid = self.now_playing.as_ref().map(|item| item.bvid.clone());
        out.comments_aid = self.comments_session.aid;
        out.comments_sort_latest = self.comments_session.sort == CommentSort::Latest;
        out.hot_loaded = hot.loaded_once;
        out.hot_replies = hot.replies.len();
        out.hot_total = hot.total_count;
        out.latest_loaded = latest.loaded_once;
        out.latest_replies = latest.replies.len();
    }

    fn open_video(&mut self, bvid: &str, context: &ComponentContext<Self>) {
        // 数据源：首页已加载卡 → 占位数据回退（冒烟路由 BILIPLAY_VIDEO 用）。
        let mut item = match self
            .home_index
            .get(bvid)
            .cloned()
            .or_else(|| data::find(bvid))
        {
            Some(item) => item,
            None => return,
        };
        if item.cover.is_none()
            && let Some(url) = &item.pic_url
        {
            item.cover = self.image_cache.get(url);
        }
        self.start_playback(item, context);
    }

    fn open_video_target(&mut self, target: OpenTarget, context: &ComponentContext<Self>) {
        if self.home_index.contains_key(&target.bvid) || data::find(&target.bvid).is_some() {
            self.open_video(&target.bvid, context);
            return;
        }
        let item = VideoItem {
            bvid: target.bvid.clone(),
            aid: 0,
            cid: 0,
            title: if target.title.trim().is_empty() {
                target.bvid
            } else {
                target.title
            },
            author: String::new(),
            author_mid: 0,
            author_face_url: None,
            duration_secs: 0,
            play_count: 0,
            danmaku_count: 0,
            cover: None,
            pic_url: None,
        };
        self.start_playback(item, context);
    }

    fn ensure_engine(&mut self) -> Arc<VideoEngine> {
        if let Some(engine) = &self.engine {
            return engine.clone();
        }
        let engine = VideoEngine::with_config(video_config_from(&self.settings));
        self.engine = Some(engine.clone());
        engine
    }

    fn current_row_revision(&self) -> u64 {
        self.home_state
            .stream_gen
            .wrapping_mul(31)
            ^ (self.home_state.grid_cols.max(1) as u64)
    }

    fn rebuild_home_source(&mut self) {
        let rev = self.current_row_revision();
        let focused = self.gamepad_active.then_some(self.home_focus_idx);
        self.home_source = video_card::home_virtual_source(
            &self.home_slots,
            self.home_state.grid_cols.max(1),
            rev,
            focused,
            &self.home_on_open,
            &self.home_on_hover,
            &self.home_on_row_active,
        );
    }

    fn spawn_cover_fetch(
        &mut self,
        bvid: String,
        url: String,
        seq: u64,
        context: &ComponentContext<Self>,
    ) {
        if !self.cover_inflight.insert(bvid.clone()) {
            return;
        }
        let auth = self.auth_service.clone();
        let cache = self.image_cache.clone();
        let epoch = self.feed_epoch.clone();
        context.spawn_background(move |_| {
            if epoch.load(Ordering::SeqCst) != seq {
                return Message::CoverReady {
                    seq,
                    bvid,
                    image: None,
                };
            }
            let image = block_on(async {
                let _permit = cover_semaphore().acquire().await.ok()?;
                if epoch.load(Ordering::SeqCst) != seq {
                    return None;
                }
                let img = auth.fetch_image_async(&url).await;
                if let Some(ref img) = img
                    && epoch.load(Ordering::SeqCst) == seq
                {
                    cache.put(url.clone(), img.clone());
                }
                img
            });
            Message::CoverReady { seq, bvid, image }
        });
    }

    /// 离屏封面去引用 & 缓存热恢复：距当前挂载行 ±(4+1) 屏之外置空 `item.cover` 释放 WinUI 解码纹理，
    /// 滚回时先查有界 480 张 LRU `ImageCache` 零网络秒级恢复，命中失败则自动兜底补拉。
    fn sync_offscreen_covers(&mut self, context: &ComponentContext<Self>) {
        let keep = feed_pager::visible_cover_window_by_row(
            self.home_anchor_row,
            self.home_state.grid_cols.max(1),
            self.home_viewport_h,
            self.home_slots.len(),
            COVER_WINDOW_BUFFER_SCREENS,
        );
        self.home_last_evict_row = self.home_anchor_row;
        let mut changed = false;
        let mut refetch = Vec::new();
        for (idx, slot) in self.home_slots.iter_mut().enumerate() {
            let HomeSlot::Video { item, .. } = slot else {
                continue;
            };
            if keep.contains(&idx) {
                if item.cover.is_none()
                    && let Some(url) = &item.pic_url
                {
                    if let Some(cached) = self.image_cache.get(url) {
                        item.cover = Some(cached);
                        changed = true;
                    } else {
                        refetch.push((item.bvid.clone(), url.clone()));
                    }
                }
            } else if item.cover.is_some() {
                item.cover = None;
                changed = true;
            }
        }
        if changed {
            self.rebuild_home_source();
        }
        let seq = self.feed_gen;
        for (bvid, url) in refetch {
            self.spawn_cover_fetch(bvid, url, seq, context);
        }
    }

    fn maybe_prefetch_next_page(&mut self, context: &ComponentContext<Self>) {
        if self.home_state.loading
            || self.home_state.error.is_some()
            || self.home_state.reached_end
            || self.home_refresh_pending
            || self.home_slots.len() >= MAX_HOME_SLOTS
        {
            return;
        }
        let cols = self.home_state.grid_cols.max(1);
        let total_rows = self.home_slots.len().div_ceil(cols);
        if total_rows == 0 || self.home_anchor_row + 1 + HOME_PREFETCH_LEAD_ROWS < total_rows {
            return;
        }
        let batch_size = feed_pager::aligned_next_batch(
            self.home_slots.len(),
            self.home_viewport_w,
            self.home_viewport_h,
        )
        .min(MAX_HOME_SLOTS.saturating_sub(self.home_slots.len()));
        if batch_size > 0 {
            self.allocate_skeleton_slots(batch_size);
            self.request_feed_batch(FeedKind::Next, batch_size, false, context);
        }
    }

    /// 进入播放页:递增播放代次 + 惰性创建引擎 + 优先命中 Hover 预解析缓存 + 否则后台解析(cid 直出仅 1 跳)。
    fn start_playback(&mut self, item: VideoItem, context: &ComponentContext<Self>) {
        if self.route == Route::Home
            && self.settings.home_open_mode == bili_core::settings::HomeOpenMode::HalfPane
        {
            self.home_half_pane = true;
        } else {
            self.home_half_pane = false;
            if self.route != Route::Player {
                self.history.push(self.route);
            }
            self.route = Route::Player;
        }
        self.advance_playback_gen();
        self.pending_open_preresolve = None;
        self.current_meta = None;
        self.now_playing_desc = None;
        self.meta_fetch_inflight_seq = None;
        self.playback_avatar_dirty = false;
        self.playback_avatar_timer = None;
        self.ensure_engine().set_danmaku(Vec::new());
        self.now_playing = Some(item.clone());
        self.playing = self.settings.autoplay;
        self.position = 0;
        self.engine_duration = None;
        self.player_notice = None;
        self.comments_session.reset(item.aid, item.author_mid);
        self.refresh_comments_snapshot();
        self.schedule_tick(context);
        self.kickoff_video_extras(self.playback_gen, context);

        let nav = self.auth_service.nav_client();
        let current_key = resolve_profile_key(&self.settings, &nav);

        // 1) 命中新鲜 Hover 预解析缓存：直接送入 mpv，0ms 等待解析。
        if let Some(cached) = self.take_fresh_preresolved(&item.bvid, current_key) {
            let seq = self.playback_gen;
            self.apply_resolved_media(cached.video_url, cached.audio_url, cached.meta, seq, context);
            return;
        }

        // 2) 若同名 Hover 预解析正在飞且配置匹配：直接接管在飞请求结果，不重复发 playurl。
        if self.preresolve_inflight.get(&item.bvid).copied() == Some(current_key) {
            self.pending_open_preresolve = Some((item.bvid.clone(), self.playback_gen, current_key));
            return;
        }

        spawn_resolve_media(
            &self.auth_service,
            &self.settings,
            item,
            self.playback_gen,
            context,
        );
    }

    fn advance_playback_gen(&mut self) -> u64 {
        self.playback_gen = self.playback_gen.wrapping_add(1);
        if self.playback_gen == 0 {
            self.playback_gen = 1;
        }
        self.playback_epoch.store(self.playback_gen, Ordering::SeqCst);
        self.playback_gen
    }

    fn refresh_comments_snapshot(&mut self) {
        self.comments_rev = self.comments_rev.wrapping_add(1);
        self.comments_snapshot = self
            .comments_session
            .to_snapshot(self.comments_rev, &self.avatar_cache);
    }

    fn schedule_playback_avatar_flush(&mut self, context: &ComponentContext<Self>) {
        if self.playback_avatar_timer.is_none() {
            self.playback_avatar_timer = Some(
                context.set_local_timeout(COVER_FLUSH_INTERVAL, || Message::FlushPlaybackAvatars),
            );
        }
    }

    fn absorb_video_meta(&mut self, meta: &VideoMeta, seq: u64, context: &ComponentContext<Self>) {
        if seq != self.playback_gen {
            return;
        }
        if let Some(desc) = &meta.desc
            && !desc.trim().is_empty()
        {
            self.now_playing_desc = Some(desc.clone());
        }
        if let Some(cur) = self.now_playing.as_mut() {
            if cur.aid == 0 && meta.aid > 0 {
                cur.aid = meta.aid;
            }
            if cur.cid == 0 && meta.cid > 0 {
                cur.cid = meta.cid;
            }
            if cur.title.is_empty() || cur.title == cur.bvid {
                cur.title = meta.title.clone();
            }
            if cur.duration_secs == 0 && meta.duration > 0 {
                cur.duration_secs = meta.duration as u32;
            }
            if let Some(owner) = &meta.owner {
                if cur.author_mid == 0 && owner.mid > 0 {
                    cur.author_mid = owner.mid;
                }
                if cur.author.is_empty() && !owner.name.is_empty() {
                    cur.author = owner.name.clone();
                }
                if cur.author_face_url.is_none() {
                    cur.author_face_url = owner.face.as_deref().and_then(normalize_face_url);
                }
            }
        }
        if self.comments_session.aid == 0 && meta.aid > 0 {
            self.comments_session.aid = meta.aid;
        }
        if self.comments_session.upper_mid == 0
            && let Some(owner) = &meta.owner
            && owner.mid > 0
        {
            self.comments_session.upper_mid = owner.mid;
        }
        self.refresh_comments_snapshot();
        self.kickoff_video_extras(seq, context);
    }

    fn kickoff_video_extras(&mut self, seq: u64, context: &ComponentContext<Self>) {
        if seq != self.playback_gen {
            return;
        }
        let Some(item) = self.now_playing.clone() else {
            return;
        };
        if let Some(face) = item.author_face_url.as_deref() {
            self.spawn_playback_avatars(std::iter::once(face), seq, context);
        }
        if item.aid > 0 {
            let sort = self.comments_session.sort;
            let bucket = self.comments_session.bucket(sort);
            if !bucket.loaded_once && !bucket.loading && bucket.error.is_none() {
                self.fetch_comments(sort, false, context);
            }
        }
        // cid 直出跳过 resolve() 内的 view，但缺 aid 或缺 UP 主头像时补拉一次 view。
        if item.cid > 0
            && (item.aid == 0 || item.author_face_url.is_none())
            && self.meta_fetch_inflight_seq != Some(seq)
        {
            self.meta_fetch_inflight_seq = Some(seq);
            let auth = self.auth_service.clone();
            let nav = auth.nav_client();
            let bvid = item.bvid;
            context.spawn_background(move |_| {
                let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                let res = block_on(client.video_meta(&bvid)).map_err(|e| e.to_string());
                Message::VideoMetaLoaded { seq, res }
            });
        }
    }

    fn fetch_comments(&mut self, sort: CommentSort, append: bool, context: &ComponentContext<Self>) {
        let aid = self.comments_session.aid;
        if aid == 0 {
            return;
        }
        let cursor = {
            let bucket = self.comments_session.bucket_mut(sort);
            if bucket.loading {
                return;
            }
            bucket.loading = true;
            bucket.error = None;
            if append { bucket.cursor.clone() } else { None }
        };
        self.refresh_comments_snapshot();

        let seq = self.playback_gen;
        let client = self.reply_client.clone();
        context.spawn_background(move |_| Message::CommentsLoaded {
            seq,
            sort,
            append,
            res: block_on(client.fetch_main(aid, ReplySort::from(sort), cursor.as_ref()))
                .map_err(|e| e.to_string()),
        });
    }

    fn fetch_sub_replies(&mut self, root_rpid: u64, append: bool, context: &ComponentContext<Self>) {
        let aid = self.comments_session.aid;
        if aid == 0 || root_rpid == 0 {
            return;
        }
        let pn = {
            let entry = self
                .comments_session
                .sub_replies
                .entry(root_rpid)
                .or_default();
            if entry.loading {
                return;
            }
            entry.expanded = true;
            entry.loading = true;
            entry.error = None;
            if append && !entry.items.is_empty() {
                entry.next_pn.max(1)
            } else {
                1
            }
        };
        self.refresh_comments_snapshot();

        let seq = self.playback_gen;
        let client = self.reply_client.clone();
        context.spawn_background(move |_| Message::SubRepliesLoaded {
            seq,
            root_rpid,
            append: append && pn > 1,
            res: block_on(client.fetch_sub(aid, root_rpid, pn, SUB_REPLY_PAGE_SIZE))
                .map_err(|e| e.to_string()),
        });
    }

    fn apply_resolved_media(
        &mut self,
        video_url: String,
        audio_url: String,
        meta: VideoMeta,
        seq: u64,
        context: &ComponentContext<Self>,
    ) {
        self.absorb_video_meta(&meta, seq, context);
        let engine = self.ensure_engine();
        engine.set_volume(f64::from(self.volume));
        engine.load_with_options(&video_url, Some(&audio_url), self.settings.autoplay);
        self.playing = self.settings.autoplay;
        self.current_meta = Some(meta);
        self.fetch_danmaku(seq, context);
    }

    fn take_fresh_preresolved(
        &mut self,
        bvid: &str,
        expected_key: ResolveProfileKey,
    ) -> Option<CachedMedia> {
        let entry = self.preresolved.get(bvid)?.clone();
        if entry.key == expected_key && entry.fetched_at.elapsed() < PRERESOLVE_TTL {
            Some(entry)
        } else {
            self.preresolved.remove(bvid);
            None
        }
    }

    fn cache_preresolved(
        &mut self,
        bvid: String,
        key: ResolveProfileKey,
        meta: VideoMeta,
        choice: StreamChoice,
    ) {
        if self.preresolved.contains_key(&bvid) {
            self.preresolved_order.retain(|k| k != &bvid);
        } else if self.preresolved.len() >= MAX_PRERESOLVE_CACHE
            && let Some(evicted) = self.preresolved_order.pop_front()
        {
            self.preresolved.remove(&evicted);
        }
        self.preresolved_order.push_back(bvid.clone());
        self.preresolved.insert(
            bvid,
            CachedMedia {
                key,
                video_url: choice.video.base_url,
                audio_url: choice.audio.base_url,
                meta,
                fetched_at: Instant::now(),
            },
        );
    }

    /// 后台拉全片弹幕(view 端点为 protobuf 非计划内;分段数按 duration 推导)。
    fn fetch_danmaku(&mut self, seq: u64, context: &ComponentContext<Self>) {
        let Some(meta) = self.current_meta.clone() else {
            return;
        };
        let auth = self.auth_service.clone();
        let nav = auth.nav_client();
        context.spawn_background(move |_| {
            let client = DanmakuClient::with_nav(auth.http(), auth.credential_provider(), nav);
            let elems = block_on(client.all(
                meta.cid,
                meta.aid,
                u64::try_from(meta.duration.max(0)).unwrap_or(0),
            ));
            match elems {
                Ok(elems) => Message::DanmakuLoaded { seq, elems },
                Err(error) => Message::DanmakuFailed {
                    seq,
                    reason: error.to_string(),
                },
            }
        });
    }

    /// 去抖调度配置落盘（滑块拖动不频繁写盘，稳定后 300ms 原子写一次）。
    fn schedule_settings_save(&mut self, context: &ComponentContext<Self>) {
        self.settings_save_timer = Some(context.set_local_timeout(SETTINGS_SAVE_DEBOUNCE, || {
            Message::FlushSettingsSave
        }));
    }

    /// 播放状态节拍(常驻续拍:now_playing 存在即持续;Drop 取消)。
    fn schedule_tick(&mut self, context: &ComponentContext<Self>) {
        if self.timer.is_none() {
            self.timer = Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
        }
    }

    /// 登录轮询 2s 节拍链。
    fn schedule_login_tick(&mut self, context: &ComponentContext<Self>) {
        self.login_timer =
            Some(context.set_local_timeout(Duration::from_secs(2), || Message::LoginPollTick));
    }

    /// 展示总长(秒):引擎 duration 优先,卡片数据兜底。
    fn display_duration(&self, item: &VideoItem) -> u32 {
        self.engine_duration
            .map(|d| d.round().max(0.0) as u32)
            .unwrap_or(item.duration_secs)
    }

    fn listen_gamepad(rx: &Arc<Mutex<std::sync::mpsc::Receiver<GamepadEvent>>>, context: &ComponentContext<Self>) {
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let ev = rx.lock().ok().and_then(|guard| guard.recv().ok());
            Message::GamepadInput(ev)
        });
    }

    fn listen_toast(rx: &Arc<Mutex<std::sync::mpsc::Receiver<()>>>, context: &ComponentContext<Self>) {
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let ok = rx.lock().ok().is_some_and(|guard| guard.recv().is_ok());
            Message::ToastActivated(ok)
        });
    }

    fn focused_home_video(&self) -> Option<&VideoItem> {
        let mut videos = self.home_slots.iter().filter_map(|slot| match slot {
            HomeSlot::Video { item, .. } => Some(item),
            HomeSlot::Skeleton { .. } => None,
        });
        let count = self
            .home_slots
            .iter()
            .filter(|slot| matches!(slot, HomeSlot::Video { .. }))
            .count();
        if count == 0 {
            return None;
        }
        videos.nth(self.home_focus_idx.min(count - 1))
    }

    fn cycle_nav_route(&mut self, delta: isize) {
        let current_idx = NAV_ROUTES.iter().position(|&r| r == self.route).unwrap_or(0) as isize;
        let len = NAV_ROUTES.len() as isize;
        let next_idx = ((current_idx + delta).rem_euclid(len)) as usize;
        let next_route = NAV_ROUTES[next_idx];
        if next_route != self.route {
            self.history.push(self.route);
            self.route = next_route;
        }
    }

    fn handle_gamepad_action(&mut self, action: GamepadAction, context: &ComponentContext<Self>) {
        match action {
            GamepadAction::PrevTab => self.cycle_nav_route(-1),
            GamepadAction::NextTab => self.cycle_nav_route(1),
            GamepadAction::Back => {
                if self.route == Route::Player {
                    self.route = self.history.pop().unwrap_or(Route::Home);
                } else if let Some(prev) = self.history.pop() {
                    self.route = prev;
                }
            }
            GamepadAction::TogglePlay => {
                self.update(Message::TogglePlay, context);
            }
            GamepadAction::Refresh => {
                if self.route == Route::Home {
                    self.update(Message::HomeRefresh, context);
                    self.home_focus_idx = 0;
                }
            }
            GamepadAction::Confirm => {
                if self.route == Route::Player {
                    self.update(Message::TogglePlay, context);
                } else if self.route == Route::Home
                    && let Some(bvid) = self.focused_home_video().map(|v| v.bvid.clone())
                {
                    self.open_video(&bvid, context);
                    self.sync_smtc(context);
                }
            }
            GamepadAction::Left => {
                if self.route == Route::Player {
                    let target = f64::from(self.position.saturating_sub(10));
                    self.update(Message::Seek(target), context);
                } else if self.route == Route::Home {
                    let next = self.home_focus_idx.saturating_sub(1);
                    if next != self.home_focus_idx {
                        self.home_focus_idx = next;
                        self.rebuild_home_source();
                    }
                }
            }
            GamepadAction::Right => {
                if self.route == Route::Player {
                    let target = f64::from(self.position.saturating_add(10));
                    self.update(Message::Seek(target), context);
                } else if self.route == Route::Home {
                    let count = self
                        .home_slots
                        .iter()
                        .filter(|s| matches!(s, HomeSlot::Video { .. }))
                        .count();
                    if count > 0 {
                        let next = (self.home_focus_idx + 1).min(count - 1);
                        if next != self.home_focus_idx {
                            self.home_focus_idx = next;
                            self.rebuild_home_source();
                        }
                    }
                }
            }
            GamepadAction::Up => {
                if self.route == Route::Player {
                    let vol = f64::from((self.volume + 5).min(100));
                    self.update(Message::SetVolume(vol), context);
                } else if self.route == Route::Home {
                    let cols = self.home_state.grid_cols.max(1);
                    let next = self.home_focus_idx.saturating_sub(cols);
                    if next != self.home_focus_idx {
                        self.home_focus_idx = next;
                        self.rebuild_home_source();
                        let step = crate::theme::CARD_HEIGHT + crate::theme::CARD_GAP;
                        if !crate::winrt_ext::scroll_focused_viewer_by(-step) {
                            gamepad::inject_mouse_wheel(240);
                        }
                    }
                }
            }
            GamepadAction::Down => {
                if self.route == Route::Player {
                    let vol = f64::from(self.volume.saturating_sub(5));
                    self.update(Message::SetVolume(vol), context);
                } else if self.route == Route::Home {
                    let cols = self.home_state.grid_cols.max(1);
                    let count = self
                        .home_slots
                        .iter()
                        .filter(|s| matches!(s, HomeSlot::Video { .. }))
                        .count();
                    if count > 0 {
                        let next = (self.home_focus_idx + cols).min(count - 1);
                        if next != self.home_focus_idx {
                            self.home_focus_idx = next;
                            self.rebuild_home_source();
                            let step = crate::theme::CARD_HEIGHT + crate::theme::CARD_GAP;
                            if !crate::winrt_ext::scroll_focused_viewer_by(step) {
                                gamepad::inject_mouse_wheel(-240);
                            }
                        }
                    }
                }
            }
        }
    }

    fn listen_smtc(&self, context: &ComponentContext<Self>) {
        let Some(rx) = &self.smtc_rx else {
            return;
        };
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let cmd = rx.lock().ok().and_then(|guard| guard.recv().ok());
            Message::SmtcAction(cmd)
        });
    }

    fn sync_smtc(&mut self, context: &ComponentContext<Self>) {
        if let Some(item) = &self.now_playing {
            if self.smtc.is_none() {
                let (tx, rx) = std::sync::mpsc::channel();
                if let Some(ctrl) = SmtcController::try_init(tx) {
                    self.smtc = Some(ctrl);
                    self.smtc_rx = Some(Arc::new(Mutex::new(rx)));
                    self.listen_smtc(context);
                }
            }
            let Some(smtc) = &self.smtc else {
                return;
            };
            if self.smtc_bvid.as_deref() != Some(item.bvid.as_str()) {
                let cover_bytes = item.cover.as_ref().map(|img| img.as_bytes());
                smtc.update_metadata(&item.title, &item.author, cover_bytes);
                self.smtc_bvid = Some(item.bvid.clone());
            }
            if self.smtc_playing != Some(self.playing) {
                smtc.set_playing(self.playing);
                self.smtc_playing = Some(self.playing);
            }
        } else if self.smtc_bvid.is_some() || self.smtc_playing.is_some() {
            if let Some(smtc) = &self.smtc {
                smtc.set_stopped();
            }
            self.smtc_bvid = None;
            self.smtc_playing = None;
        }
    }

    fn half_pane_layer(
        &self,
        author_avatar: Option<EncodedImage>,
        my_avatar: Option<EncodedImage>,
        comment_cb: CommentCallbacks,
        context: &mut ViewContext<Self>,
    ) -> Option<KeyedView> {
        let (Some(item), Some(engine)) = (self.now_playing.as_ref(), self.engine.as_ref()) else {
            return None;
        };
        Some(keyed(
            format!("half-pane:{}", item.bvid),
            View::component::<pages::player::HalfPanePreview>(pages::player::HalfPaneInput {
                item: item.clone(),
                author_avatar,
                my_avatar,
                desc: self.now_playing_desc.clone(),
                mount_tick: self.mount_tick,
                width: self.settings.half_pane_width,
                max_width: self.window_width,
                notice: self.player_notice.clone(),
                engine: engine.clone(),
                comments: self.comments_snapshot.clone(),
                comment_cb,
                on_expand: context.message(Message::ExpandHalfPane),
                on_close: context.message(Message::CloseHalfPane),
                on_resize: context.callback(Message::ResizeHalfPane),
            }),
        ))
    }

    /// 首页视图（Home 路由与播放页无播放项时的兜底共用）。
    fn home_view(&self, context: &ViewContext<Self>) -> View {
        let compact = self.window_width < COMPACT_BREAKPOINT;
        pages::home::home_page(
            &self.home_source,
            &self.home_state,
            compact,
            &context.message(Message::HomeRefresh),
            &context.message(Message::HomeRetry),
        )
    }

    fn clear_login_session(&mut self) {
        *self.flow.lock().expect("登录会话槽中毒") = None;
        self.login_timer = None;
        self.login_flow = LoginFlowState::Idle;
    }

    /// 预分配固定尺寸骨架槽位框架（先框架、后填数据，保证布局几何稳定）。
    fn allocate_skeleton_slots(&mut self, count: usize) {
        let gen_id = self.feed_gen;
        for _ in 0..count {
            let seq = self.home_next_slot_seq;
            self.home_next_slot_seq += 1;
            self.home_slots.push(HomeSlot::Skeleton {
                slot_key: format!("slot-{gen_id}-{seq}"),
            });
        }
        self.home_pending_skeletons += count;
        self.rebuild_home_source();
    }

    /// 收起尾部尚未填充的骨架槽位（流耗尽定格最后一页或出错撤回时调用）。
    fn truncate_unfilled_skeletons(&mut self) {
        let before = self.home_slots.len();
        while matches!(self.home_slots.last(), Some(HomeSlot::Skeleton { .. })) {
            self.home_slots.pop();
        }
        self.home_pending_skeletons = 0;
        if self.home_slots.len() != before {
            self.rebuild_home_source();
        }
    }

    /// 发起一次逻辑页批请求：视口自适应目标条数 + 合批 `ps<=30` + 代次回投。
    fn request_feed_batch(
        &mut self,
        kind: FeedKind,
        target_count: usize,
        replace: bool,
        context: &ComponentContext<Self>,
    ) {
        if self.home_state.loading {
            return;
        }
        self.home_state.loading = true;
        self.home_state.error = None;
        self.home_last_kind = kind;
        self.home_last_target = target_count;
        spawn_feed_batch(
            &self.feed_client,
            &self.feed_session,
            kind,
            target_count,
            replace,
            self.feed_gen,
            context,
        );
    }

    fn schedule_cover_flush(&mut self, context: &ComponentContext<Self>) {
        if self.cover_flush_timer.is_none() {
            self.cover_flush_timer =
                Some(context.set_local_timeout(COVER_FLUSH_INTERVAL, || Message::FlushCovers));
        }
    }

    fn reset_account_data(&mut self) {
        self.notify_gen = self.notify_gen.wrapping_add(1);
        self.account_stat = None;
        self.unread_summary = UnreadSummary::default();
        self.followings = ListSliceState::default();
        self.followings_next_pn = 1;
        self.followers = ListSliceState::default();
        self.followers_next_pn = 1;
        self.notify_replies = ListSliceState::default();
        self.notify_replies_cursor = None;
        self.notify_ats = ListSliceState::default();
        self.notify_ats_cursor = None;
        self.notify_likes = ListSliceState::default();
        self.notify_likes_cursor = None;
        self.notify_systems = ListSliceState::default();
        self.notify_systems_cursor = None;
        self.notify_sessions = ListSliceState::default();
        self.notify_sessions_end_ts = None;
        self.selected_whisper = None;
        self.whisper_threads.clear();
        self.avatar_gen = self.avatar_gen.wrapping_add(1);
        self.avatar_cache.clear();
        self.avatar_inflight.clear();
    }

    fn fetch_account_summary(&mut self, context: &ComponentContext<Self>) {
        let seq = self.notify_gen;
        let auth = self.auth_service.clone();
        let nav = auth.nav_client();
        context.spawn_background(move |_| {
            let stat = block_on(nav.fetch_stat()).ok();
            Message::AccountStatLoaded { seq, stat }
        });
        let auth_u = self.auth_service.clone();
        context.spawn_background(move |_| {
            let summary = block_on(auth_u.notify_client().fetch_unread()).ok();
            Message::UnreadLoaded { seq, summary }
        });
    }

    fn relation_slot(&self, kind: RelationKind) -> (&ListSliceState<RelationUser>, u32) {
        match kind {
            RelationKind::Following => (&self.followings, self.followings_next_pn),
            RelationKind::Follower => (&self.followers, self.followers_next_pn),
        }
    }

    fn relation_slot_mut(
        &mut self,
        kind: RelationKind,
    ) -> (&mut ListSliceState<RelationUser>, &mut u32) {
        match kind {
            RelationKind::Following => (&mut self.followings, &mut self.followings_next_pn),
            RelationKind::Follower => (&mut self.followers, &mut self.followers_next_pn),
        }
    }

    fn fetch_relations(
        &mut self,
        kind: RelationKind,
        append: bool,
        context: &ComponentContext<Self>,
    ) {
        let AuthState::LoggedIn(user) = &self.auth else {
            return;
        };
        let mid = user.mid;
        let seq = self.notify_gen;
        let nav = self.auth_service.nav_client();
        let pn = {
            let (slice, next_pn) = self.relation_slot_mut(kind);
            if slice.loading {
                return;
            }
            slice.loading = true;
            slice.error = None;
            if append { (*next_pn).max(1) } else { 1 }
        };
        context.spawn_background(move |_| Message::RelationsLoaded {
            seq,
            kind,
            append,
            res: block_on(nav.fetch_relations(mid, kind, pn, RELATION_PAGE_SIZE))
                .map_err(|e| e.to_string()),
        });
    }

    fn ensure_account_loaded(&mut self, context: &ComponentContext<Self>) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        if self.account_stat.is_none() {
            self.fetch_account_summary(context);
        }
        self.ensure_active_tab_loaded(context);
    }

    fn ensure_active_tab_loaded(&mut self, context: &ComponentContext<Self>) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        let already = match self.notify_tab {
            NotifyTab::Reply => self.notify_replies.loaded_once || self.notify_replies.loading,
            NotifyTab::At => self.notify_ats.loaded_once || self.notify_ats.loading,
            NotifyTab::Like => self.notify_likes.loaded_once || self.notify_likes.loading,
            NotifyTab::System => self.notify_systems.loaded_once || self.notify_systems.loading,
            NotifyTab::Whisper => self.notify_sessions.loaded_once || self.notify_sessions.loading,
        };
        if !already {
            self.fetch_notify_tab(self.notify_tab, false, context);
        }
    }

    fn fetch_notify_tab(&mut self, tab: NotifyTab, append: bool, context: &ComponentContext<Self>) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        let seq = self.notify_gen;
        let client = self.auth_service.notify_client();
        match tab {
            NotifyTab::Reply => {
                if self.notify_replies.loading {
                    return;
                }
                self.notify_replies.loading = true;
                self.notify_replies.error = None;
                let cursor = if append { self.notify_replies_cursor } else { None };
                context.spawn_background(move |_| Message::RepliesLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_replies(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::At => {
                if self.notify_ats.loading {
                    return;
                }
                self.notify_ats.loading = true;
                self.notify_ats.error = None;
                let cursor = if append { self.notify_ats_cursor } else { None };
                context.spawn_background(move |_| Message::AtsLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_ats(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::Like => {
                if self.notify_likes.loading {
                    return;
                }
                self.notify_likes.loading = true;
                self.notify_likes.error = None;
                let cursor = if append { self.notify_likes_cursor } else { None };
                context.spawn_background(move |_| Message::LikesLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_likes(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::System => {
                if self.notify_systems.loading {
                    return;
                }
                self.notify_systems.loading = true;
                self.notify_systems.error = None;
                let cursor = if append { self.notify_systems_cursor } else { None };
                context.spawn_background(move |_| Message::SystemsLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_system_notifies(cursor)).map_err(|e| e.to_string()),
                });
            }
            NotifyTab::Whisper => {
                if self.notify_sessions.loading {
                    return;
                }
                self.notify_sessions.loading = true;
                self.notify_sessions.error = None;
                let end_ts = if append { self.notify_sessions_end_ts } else { None };
                context.spawn_background(move |_| Message::SessionsLoaded {
                    seq,
                    append,
                    res: block_on(client.fetch_sessions(end_ts)).map_err(|e| e.to_string()),
                });
            }
        }
    }

    fn fetch_whisper_thread(
        &mut self,
        talker_id: u64,
        session_type: i32,
        context: &ComponentContext<Self>,
    ) {
        if !matches!(self.auth, AuthState::LoggedIn(_)) {
            return;
        }
        let entry = self
            .whisper_threads
            .entry((talker_id, session_type))
            .or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.error = None;
        let seq = self.notify_gen;
        let client = self.auth_service.notify_client();
        context.spawn_background(move |_| Message::WhisperThreadLoaded {
            seq,
            talker_id,
            session_type,
            res: block_on(client.fetch_session_messages(talker_id, session_type))
                .map_err(|e| e.to_string()),
        });
    }

    fn spawn_notify_avatars<'a>(
        &mut self,
        urls: impl IntoIterator<Item = &'a str>,
        context: &ComponentContext<Self>,
    ) {
        self.spawn_avatars(urls, 0, context);
    }

    fn spawn_playback_avatars<'a>(
        &mut self,
        urls: impl IntoIterator<Item = &'a str>,
        playback_seq: u64,
        context: &ComponentContext<Self>,
    ) {
        self.spawn_avatars(urls, playback_seq, context);
    }

    fn spawn_avatars<'a>(
        &mut self,
        urls: impl IntoIterator<Item = &'a str>,
        inflight_epoch: u64,
        context: &ComponentContext<Self>,
    ) {
        let avatar_gen = self.avatar_gen;
        for raw in urls {
            if raw.is_empty() {
                continue;
            }
            let thumb = avatar_thumbnail_url(raw);
            if self.avatar_cache.get(&thumb).is_some() {
                continue;
            }
            if let Some(&existing_epoch) = self.avatar_inflight.get(&thumb) {
                // 0 表示持久有效的通知/关系头像；同代次也不重复发起；仅在旧播放视频被切走时允许接管。
                if existing_epoch == 0 || existing_epoch == inflight_epoch || inflight_epoch == 0 {
                    continue;
                }
            }
            self.avatar_inflight.insert(thumb.clone(), inflight_epoch);
            let auth = self.auth_service.clone();
            let cache = self.avatar_cache.clone();
            let playback_epoch = self.playback_epoch.clone();
            let url = thumb;
            context.spawn_background(move |_| {
                let has_image = block_on(async {
                    if inflight_epoch != 0
                        && playback_epoch.load(Ordering::SeqCst) != inflight_epoch
                    {
                        return false;
                    }
                    let Ok(_permit) = cover_semaphore().acquire().await else {
                        return false;
                    };
                    if inflight_epoch != 0
                        && playback_epoch.load(Ordering::SeqCst) != inflight_epoch
                    {
                        return false;
                    }
                    let Some(img) = auth.fetch_image_async(&url).await else {
                        return false;
                    };
                    cache.put(url.clone(), img);
                    true
                });
                Message::AvatarLoaded {
                    avatar_gen,
                    inflight_epoch,
                    url,
                    has_image,
                }
            });
        }
    }
}

fn apply_feed_page<T>(
    slice: &mut ListSliceState<T>,
    cursor_slot: &mut Option<FeedCursor>,
    append: bool,
    res: Result<bili_core::notify::FeedPage<T>, String>,
    avatar_of: impl Fn(&T) -> &str,
) -> Vec<String> {
    slice.loading = false;
    slice.loaded_once = true;
    match res {
        Ok(page) => {
            let urls: Vec<String> = page.items.iter().map(|i| avatar_of(i).to_string()).collect();
            if append {
                slice.items.extend(page.items);
            } else {
                slice.items = page.items;
            }
            *cursor_slot = page.next_cursor;
            slice.has_more = page.has_more;
            slice.error = None;
            urls
        }
        Err(err) => {
            slice.error = Some(err);
            Vec::new()
        }
    }
}

fn spawn_resolve_media(
    auth_service: &Arc<Auth>,
    settings: &AppSettings,
    item: VideoItem,
    seq: u64,
    context: &ComponentContext<AppShell>,
) {
    let auth = auth_service.clone();
    let nav = auth.nav_client();
    let (max_qn, codecs) = resolve_params_for(settings, &nav);
    context.spawn_background(move |_| {
        let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
        let res = block_on(client.resolve_with_cid(
            &item.bvid,
            item.aid,
            item.cid,
            &item.title,
            i64::from(item.duration_secs),
            max_qn,
            codecs,
        ));
        match res {
            Ok((meta, choice)) => Message::MediaReady {
                seq,
                video_url: choice.video.base_url,
                audio_url: choice.audio.base_url,
                meta,
            },
            Err(error) => Message::MediaFailed {
                seq,
                reason: error.to_string(),
            },
        }
    });
}

fn spawn_feed_batch(
    feed_client: &Arc<FeedClient>,
    feed_session: &Arc<Mutex<FeedSession>>,
    kind: FeedKind,
    target_count: usize,
    replace: bool,
    seq: u64,
    context: &ComponentContext<AppShell>,
) {
    let client = feed_client.clone();
    let session = feed_session.clone();
    context.spawn_background(move |_| {
        match block_on(fetch_rcmd_batch(&client, &session, kind, target_count)) {
            Ok(batch) => Message::HomeLoaded {
                seq,
                cards: batch.cards.iter().map(video_item).collect(),
                exhausted: batch.exhausted,
                replace,
            },
            Err(error) => Message::HomeFailed {
                seq,
                reason: error.to_string(),
            },
        }
    });
}

impl Component for AppShell {
    type Input = ShellInput;
    type Message = Message;

    fn create(input: &ShellInput, context: &ComponentContext<Self>) -> Self {
        crate::winrt_ext::apply_global_font(crate::theme::DEFAULT_FONT_FAMILY);
        let auth_service = Arc::new(Auth::new());
        let nav = auth_service.nav_client();
        let settings_store = Arc::new(FileSettingsStore::new(
            auth_service.data_dir().join(SETTINGS_FILE),
        ));
        let (settings, settings_notice) = settings_store.load_or_default();

        let playback_gen = 1u64;
        let playback_epoch = Arc::new(AtomicU64::new(playback_gen));
        let feed_gen = 1u64;
        let feed_epoch = Arc::new(AtomicU64::new(feed_gen));
        let auth_gen = 1u64;
        let reply_client = Arc::new(ReplyClient::with_nav(
            auth_service.http(),
            auth_service.credential_provider(),
            nav.clone(),
        ));

        // 启动即探测登录态（含自动 Cookie 续期 + `bili_ticket` 预热；后台单飞不重跑）。
        context.spawn_background({
            let auth = auth_service.clone();
            move |_| Message::AuthProbed {
                seq: auth_gen,
                report: auth.probe_blocking(),
            }
        });

        // 所有需要 nav 的客户端共用同一个 NavClient：用户资料 + WBI 密钥 + bili_ticket 一份缓存。
        let feed_client = Arc::new(FeedClient::with_nav(
            auth_service.http(),
            auth_service.credential_provider(),
            nav.clone(),
        ));
        let feed_session = Arc::new(Mutex::new(FeedSession::new()));
        // 视口自适应首批逻辑页（默认 1280x800 → 48 条）：先分配完整骨架槽位框架再拉数据。
        let initial_target = feed_pager::logical_page_target(0.0, 0.0);
        let mut home_slots = Vec::with_capacity(initial_target);
        for seq in 0..initial_target {
            home_slots.push(HomeSlot::Skeleton {
                slot_key: format!("slot-{feed_gen}-{seq}"),
            });
        }
        spawn_feed_batch(
            &feed_client,
            &feed_session,
            FeedKind::First,
            initial_target,
            true,
            feed_gen,
            context,
        );

        // 冒烟辅助：BILIPLAY_SMOKE_LOGIN=1 自动发起扫码流程（generate 只读，不登录）。
        let flow = Arc::new(Mutex::new(None));
        if std::env::var("BILIPLAY_SMOKE_LOGIN").as_deref() == Ok("1") {
            let auth = auth_service.clone();
            let cell = flow.clone();
            context.spawn_background(move |_| match auth.start_login_blocking(&cell) {
                Ok(qr) => Message::LoginQrReady(qr),
                Err(reason) => Message::LoginStartFailed(reason),
            });
        }

        // 冒烟辅助：BILIPLAY_ROUTE=<tag> 指定初始页面。
        let route = std::env::var("BILIPLAY_ROUTE")
            .ok()
            .and_then(|ref tag| Route::from_tag(tag));
        // 冒烟辅助：BILIPLAY_VIDEO=<bvid> 启动即进播放页(与点卡同管线)。
        let now_playing = std::env::var("BILIPLAY_VIDEO")
            .ok()
            .as_deref()
            .and_then(data::find);
        let route = if crate::notifications::launched_from_notification() {
            Route::Login
        } else {
            route
                .or((now_playing.is_some()).then_some(Route::Player))
                .unwrap_or(Route::Home)
        };

        // 冒烟播放与点卡同构:仅在 BILIPLAY_VIDEO 指定时才提前创建 VideoEngine；日常启动 0 播放内存占用。
        let mut timer = None;
        let mut engine = None;
        if let Some(item) = &now_playing {
            engine = Some(VideoEngine::with_config(video_config_from(&settings)));
            spawn_resolve_media(&auth_service, &settings, item.clone(), playback_gen, context);
            timer = Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
        }

        let initial_win_w = settings
            .window
            .map_or(1280.0, |w| f64::from(w.width).max(320.0));
        let volume = settings.volume;
        let home_on_open = context.callback(Message::OpenVideo);
        let home_on_hover = context.callback(Message::PreResolveVideo);
        let home_on_row_active =
            context.callback(|(rev, row_idx, active)| Message::HomeRowActive {
                rev,
                row_idx,
                active,
            });
        let initial_cols = feed_pager::columns(0.0);
        let initial_rev = feed_gen.wrapping_mul(31) ^ (initial_cols as u64);
        let home_source = video_card::home_virtual_source(
            &home_slots,
            initial_cols,
            initial_rev,
            None,
            &home_on_open,
            &home_on_hover,
            &home_on_row_active,
        );

        let gamepad_rx = Arc::new(Mutex::new(gamepad::spawn_gamepad_poller()));
        Self::listen_gamepad(&gamepad_rx, context);

        let (notif_tx, notif_rx) = std::sync::mpsc::channel::<()>();
        let notif_tx = Arc::new(Mutex::new(notif_tx));
        let app_notifications = crate::notifications::register_if_packaged(Arc::new(move || {
            if let Ok(tx) = notif_tx.lock() {
                let _ = tx.send(());
            }
        }));
        let toast_rx = app_notifications.is_some().then(|| {
            let rx = Arc::new(Mutex::new(notif_rx));
            Self::listen_toast(&rx, context);
            rx
        });

        Self {
            auth_service,
            settings_store,
            settings_notice,
            settings_save_timer: None,
            playback_gen,
            playback_epoch,
            feed_gen,
            feed_epoch,
            auth_gen,
            reply_client,
            feed_client,
            feed_session,
            home_slots,
            home_source,
            home_on_open,
            home_on_hover,
            home_on_row_active,
            home_active_rows: BTreeSet::new(),
            home_slot_by_bvid: HashMap::new(),
            home_index: HashMap::new(),
            home_next_slot_seq: initial_target,
            home_pending_skeletons: initial_target,
            home_state: pages::home::HomeState {
                stream_gen: feed_gen,
                grid_cols: feed_pager::columns(0.0),
                loading: true,
                ..Default::default()
            },
            home_last_kind: FeedKind::First,
            home_last_target: initial_target,
            home_refresh_pending: false,
            home_anchor_row: 0,
            home_last_evict_row: 0,
            home_viewport_w: 0.0,
            home_viewport_h: 0.0,
            pending_covers: HashMap::new(),
            cover_inflight: HashSet::new(),
            cover_flush_timer: None,
            preresolved: HashMap::new(),
            preresolved_order: VecDeque::new(),
            preresolve_inflight: HashMap::new(),
            pending_open_preresolve: None,
            image_cache: Arc::new(ImageCache::new()),
            auth: AuthState::Unknown,
            login_flow: LoginFlowState::Idle,
            flow,
            logout_confirm: false,
            auth_notice: None,
            notify_gen: 1,
            account_stat: None,
            unread_summary: UnreadSummary::default(),
            followings: ListSliceState::default(),
            followings_next_pn: 1,
            followers: ListSliceState::default(),
            followers_next_pn: 1,
            notify_tab: NotifyTab::default(),
            notify_replies: ListSliceState::default(),
            notify_replies_cursor: None,
            notify_ats: ListSliceState::default(),
            notify_ats_cursor: None,
            notify_likes: ListSliceState::default(),
            notify_likes_cursor: None,
            notify_systems: ListSliceState::default(),
            notify_systems_cursor: None,
            notify_sessions: ListSliceState::default(),
            notify_sessions_end_ts: None,
            selected_whisper: None,
            whisper_threads: HashMap::new(),
            avatar_gen: 1,
            avatar_cache: Arc::new(ImageCache::with_capacity(256)),
            avatar_inflight: HashMap::new(),
            route,
            history: Vec::new(),
            pane_open: true,
            now_playing,
            now_playing_desc: None,
            meta_fetch_inflight_seq: None,
            comments_session: CommentsSessionState::default(),
            probe: input.probe.clone(),
            comments_snapshot: CommentsSnapshot::default(),
            comments_rev: 0,
            playback_avatar_dirty: false,
            playback_avatar_timer: None,
            playing: false,
            position: 0,
            mount_tick: 0,
            volume,
            settings,
            engine_duration: None,
            player_notice: None,
            engine,
            current_meta: None,
            timer,
            login_timer: None,
            smtc: None,
            smtc_rx: None,
            smtc_bvid: None,
            smtc_playing: None,
            gamepad_active: false,
            gamepad_rx,
            home_focus_idx: 0,
            home_half_pane: false,
            window_width: initial_win_w,
            more_menu_open: false,
            _app_notifications: app_notifications,
            toast_rx,
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        self.refresh_probe();
        match message {
            Message::Back => {
                self.logout_confirm = false;
                if let Some(route) = self.history.pop() {
                    self.route = route;
                }
            }
            Message::NavTagSelected(tag) => {
                self.logout_confirm = false;
                if let Some(route) = tag.as_deref().and_then(Route::from_tag)
                    && route != self.route
                {
                    self.history.push(self.route);
                    self.route = route;
                    if route == Route::Login {
                        self.ensure_account_loaded(context);
                    }
                }
            }
            Message::PaneOpenChanged(open) => self.pane_open = open,
            Message::TogglePane => self.pane_open = !self.pane_open,
            #[cfg(feature = "selftest")]
            Message::SelfTestSync => {}
            Message::OpenVideo(bvid) => {
                self.open_video(&bvid, context);
                self.sync_smtc(context);
            }
            Message::OpenVideoTarget(target) => {
                self.open_video_target(target, context);
                self.sync_smtc(context);
            }
            Message::TogglePlay => {
                let Some(engine) = &self.engine else {
                    return;
                };
                let status = engine.status();
                if !status.has_media() {
                    return; // 媒体未加载(解析中/失败):按钮无操作对象。
                }
                if status.eof {
                    // 末帧重播:回零续播。
                    engine.seek(0.0);
                    engine.play();
                    self.playing = true;
                } else if self.playing {
                    engine.pause();
                    self.playing = false;
                } else {
                    engine.play();
                    self.playing = true;
                }
                self.sync_smtc(context);
            }
            Message::Seek(value) => {
                self.position = value.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                if let Some(engine) = &self.engine {
                    engine.seek(value.max(0.0));
                }
            }
            Message::SetVolume(value) => {
                let vol = value.round().clamp(0.0, 100.0) as u32;
                if vol != self.volume {
                    self.volume = vol;
                    if let Some(engine) = &self.engine {
                        engine.set_volume(f64::from(self.volume));
                    }
                }
                if self.settings.volume != vol {
                    self.settings.volume = vol;
                    self.schedule_settings_save(context);
                }
            }
            Message::PositionTick => {
                // 引擎快照 → UI 单源(乐观更新的 TogglePlay/Seek 在此被真实值校正)。
                if let Some(engine) = &self.engine {
                    let status = engine.status();
                    if status.has_media() {
                        self.position = status.time_pos.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                        self.playing = !status.pause && !status.eof;
                        if status.duration > 0.0 {
                            self.engine_duration = Some(status.duration);
                        }
                    }
                    // 仅当 SwapChain 尚未挂载成功时按 250ms 递增驱动 VideoSurface::try_mount()。
                    if self.route == Route::Player && !engine.attached() {
                        self.mount_tick = self.mount_tick.wrapping_add(1);
                    }
                    // 引擎错误(初始化/挂载/设备级)一次性上屏。
                    if self.player_notice.is_none()
                        && let Some(reason) = engine.error()
                    {
                        self.player_notice = Some(reason);
                    }
                }
                self.sync_smtc(context);
                // 续拍:仍持有播放项才继续(Back 到首页后 mini_player 也要状态)。
                if self.now_playing.is_some() {
                    self.timer =
                        Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
                } else {
                    self.timer = None;
                }
            }
            Message::MediaReady {
                seq,
                video_url,
                audio_url,
                meta,
            } => {
                if seq != self.playback_gen {
                    return; // 过期解析结果直接丢弃，防止切视频后串台覆盖
                }
                self.pending_open_preresolve = None;
                self.apply_resolved_media(video_url, audio_url, meta, seq, context);
            }
            Message::VideoMetaLoaded { seq, res } => {
                if seq != self.playback_gen {
                    return;
                }
                if self.meta_fetch_inflight_seq == Some(seq) {
                    self.meta_fetch_inflight_seq = None;
                }
                match res {
                    Ok(meta) => self.absorb_video_meta(&meta, seq, context),
                    Err(err) => {
                        if self.comments_session.aid == 0 {
                            let sort = self.comments_session.sort;
                            let bucket = self.comments_session.bucket_mut(sort);
                            bucket.loaded_once = true;
                            bucket.error = Some(err);
                            self.refresh_comments_snapshot();
                        }
                    }
                }
            }
            Message::SetCommentSort(sort) => {
                if self.comments_session.sort == sort {
                    return;
                }
                self.comments_session.sort = sort;
                self.refresh_comments_snapshot();
                let bucket = self.comments_session.bucket(sort);
                if !bucket.loaded_once && !bucket.loading {
                    self.fetch_comments(sort, false, context);
                }
            }
            Message::LoadMoreComments => {
                let sort = self.comments_session.sort;
                if self.comments_session.bucket(sort).has_more {
                    self.fetch_comments(sort, true, context);
                }
            }
            Message::RetryComments => {
                if self.comments_session.aid == 0 {
                    let seq = self.playback_gen;
                    self.meta_fetch_inflight_seq = None;
                    self.kickoff_video_extras(seq, context);
                } else {
                    let sort = self.comments_session.sort;
                    let bucket = self.comments_session.bucket(sort);
                    let append = !bucket.replies.is_empty() || !bucket.top_replies.is_empty();
                    self.fetch_comments(sort, append, context);
                }
            }
            Message::CommentsLoaded {
                seq,
                sort,
                append,
                res,
            } => {
                if seq != self.playback_gen {
                    return;
                }
                let (loaded_upper, avatar_urls): (u64, Vec<String>) = {
                    let bucket = self.comments_session.bucket_mut(sort);
                    bucket.loading = false;
                    bucket.loaded_once = true;
                    match res {
                        Ok(page) => {
                            let page_has_more = page.has_more();
                            let upper = page.upper_mid;
                            if !append {
                                bucket.top_replies.clear();
                                bucket.replies.clear();
                                bucket.seen_rpids.clear();
                            }
                            let mut urls = Vec::new();
                            if !append {
                                for item in page.top_replies {
                                    if bucket.seen_rpids.insert(item.rpid) {
                                        if !item.avatar.is_empty() {
                                            urls.push(item.avatar.clone());
                                        }
                                        bucket.top_replies.push(item);
                                    }
                                }
                            }
                            let mut added = 0usize;
                            for item in page.replies {
                                if bucket.seen_rpids.insert(item.rpid) {
                                    if !item.avatar.is_empty() {
                                        urls.push(item.avatar.clone());
                                    }
                                    bucket.replies.push(item);
                                    added += 1;
                                }
                            }
                            if page.cursor.all_count > 0 || bucket.total_count.is_none() {
                                bucket.total_count = Some(page.cursor.all_count);
                            }
                            let has_more = if append {
                                page_has_more && added > 0
                            } else {
                                page_has_more
                                    && (!bucket.replies.is_empty()
                                        || !bucket.top_replies.is_empty())
                            };
                            bucket.cursor = Some(page.cursor);
                            bucket.has_more = has_more;
                            bucket.error = None;
                            (upper, urls)
                        }
                        Err(err) => {
                            bucket.error = Some(err);
                            (0, Vec::new())
                        }
                    }
                };
                if self.comments_session.upper_mid == 0 && loaded_upper > 0 {
                    self.comments_session.upper_mid = loaded_upper;
                }
                self.refresh_comments_snapshot();
                self.spawn_playback_avatars(avatar_urls.iter().map(String::as_str), seq, context);
            }
            Message::ToggleSubReplies(rpid) => {
                let need_fetch = if let Some(entry) = self.comments_session.sub_replies.get_mut(&rpid)
                {
                    entry.expanded = !entry.expanded;
                    entry.expanded && entry.items.is_empty() && !entry.loading
                } else {
                    true
                };
                if need_fetch {
                    self.fetch_sub_replies(rpid, false, context);
                } else {
                    self.refresh_comments_snapshot();
                }
            }
            Message::LoadMoreSubReplies(rpid) => {
                self.fetch_sub_replies(rpid, true, context);
            }
            Message::SubRepliesLoaded {
                seq,
                root_rpid,
                append,
                res,
            } => {
                if seq != self.playback_gen {
                    return;
                }
                let avatar_urls: Vec<String> = {
                    let upper_mid = &mut self.comments_session.upper_mid;
                    let entry = self
                        .comments_session
                        .sub_replies
                        .entry(root_rpid)
                        .or_default();
                    entry.loading = false;
                    match res {
                        Ok(page) => {
                            if *upper_mid == 0 && page.upper_mid > 0 {
                                *upper_mid = page.upper_mid;
                            }
                            if !append {
                                entry.items.clear();
                            }
                            let mut seen: HashSet<u64> =
                                entry.items.iter().map(|i| i.rpid).collect();
                            let mut urls = Vec::new();
                            let mut added = 0usize;
                            for item in page.replies {
                                if seen.insert(item.rpid) {
                                    if !item.avatar.is_empty() {
                                        urls.push(item.avatar.clone());
                                    }
                                    entry.items.push(item);
                                    added += 1;
                                }
                            }
                            entry.total_count = page.total_count;
                            entry.next_pn = page.page_num.saturating_add(1);
                            entry.has_more = page.has_more && (!append || added > 0);
                            entry.error = None;
                            urls
                        }
                        Err(err) => {
                            entry.error = Some(err);
                            Vec::new()
                        }
                    }
                };
                self.refresh_comments_snapshot();
                self.spawn_playback_avatars(avatar_urls.iter().map(String::as_str), seq, context);
            }
            Message::DanmakuLoaded { seq, elems } => {
                if seq != self.playback_gen {
                    return;
                }
                // 转引擎镜像类型(避免 bili-video 依赖 bili-core),交由
                // 引擎线程在视频帧上叠画(暂停即冻结,与视频天然同步)。
                let items = elems
                    .into_iter()
                    .map(|e| OverlayDanmaku {
                        progress_ms: e.progress_ms,
                        mode: e.mode,
                        fontsize: e.fontsize,
                        color: e.color,
                        content: e.content,
                    })
                    .collect();
                if let Some(engine) = &self.engine {
                    engine.set_danmaku(items);
                }
            }
            Message::DanmakuFailed { seq, reason } => {
                // 弹幕拉取失败不影响播放;静默保留空层，仅记录当前代次诊断日志。
                if seq == self.playback_gen {
                    eprintln!("[bili-ui] 弹幕拉取失败（不影响播放）: {reason}");
                }
            }
            Message::MediaFailed { seq, reason } => {
                if seq != self.playback_gen {
                    return;
                }
                self.player_notice = Some(format!("视频解析失败:{reason}"));
                self.playing = false;
            }
            Message::MiniNext => {}
            Message::ClosePlayer => {
                self.advance_playback_gen();
                self.pending_open_preresolve = None;
                self.now_playing = None;
                self.now_playing_desc = None;
                self.meta_fetch_inflight_seq = None;
                self.playback_avatar_dirty = false;
                self.playback_avatar_timer = None;
                self.playing = false;
                self.position = 0;
                self.engine_duration = None;
                self.player_notice = None;
                self.current_meta = None;
                self.timer = None;
                self.comments_session.reset(0, 0);
                self.refresh_comments_snapshot();
                if let Some(engine) = self.engine.take() {
                    engine.stop();
                    drop(engine);
                }
                if self.route == Route::Player {
                    self.route = self.history.pop().unwrap_or(Route::Home);
                }
                self.home_half_pane = false;
                self.sync_smtc(context);
            }
            Message::ExpandHalfPane => {
                self.home_half_pane = false;
                if self.route != Route::Player {
                    self.history.push(self.route);
                    self.route = Route::Player;
                }
            }
            Message::CloseHalfPane => {
                self.home_half_pane = false;
            }
            Message::ResizeHalfPane(width) => {
                let clamped = width.clamp(340.0, 820.0);
                if (self.settings.half_pane_width - clamped).abs() >= 1.0 {
                    self.settings.half_pane_width = clamped;
                    self.schedule_settings_save(context);
                }
            }
            Message::SmtcAction(Some(cmd)) => {
                if let Some(engine) = &self.engine {
                    let status = engine.status();
                    if status.has_media() {
                        match cmd {
                            SmtcCommand::Play => {
                                if status.eof {
                                    engine.seek(0.0);
                                }
                                engine.play();
                                self.playing = true;
                            }
                            SmtcCommand::Pause => {
                                engine.pause();
                                self.playing = false;
                            }
                        }
                        self.sync_smtc(context);
                    }
                }
                self.listen_smtc(context);
            }
            Message::SmtcAction(None) => {}
            Message::ToastActivated(true) => {
                if let Some(rx) = self.toast_rx.clone() {
                    Self::listen_toast(&rx, context);
                }
                self.update(Message::OpenNotifications, context);
            }
            Message::ToastActivated(false) => {}
            Message::GamepadInput(Some(ev)) => {
                match ev {
                    GamepadEvent::Connected(connected) => {
                        if self.gamepad_active != connected {
                            self.gamepad_active = connected;
                            self.rebuild_home_source();
                        }
                    }
                    GamepadEvent::Action(action) => {
                        if !self.gamepad_active {
                            self.gamepad_active = true;
                            self.rebuild_home_source();
                        }
                        self.handle_gamepad_action(action, context);
                    }
                }
                Self::listen_gamepad(&self.gamepad_rx, context);
            }
            Message::GamepadInput(None) => {}

            // ---- 首页推荐流与 Hover 预解析 ----
            Message::HomeRefresh => {
                self.feed_gen = self.feed_gen.wrapping_add(1);
                self.feed_epoch.store(self.feed_gen, Ordering::SeqCst);
                self.image_cache.clear();
                self.pending_covers.clear();
                self.cover_inflight.clear();
                self.cover_flush_timer = None;
                self.home_anchor_row = 0;
                self.home_last_evict_row = 0;
                self.home_active_rows.clear();
                self.home_slots.clear();
                self.home_slot_by_bvid.clear();
                self.home_index.clear();
                self.home_next_slot_seq = 0;
                self.home_pending_skeletons = 0;
                self.home_state.stream_gen = self.feed_gen;
                self.home_state.grid_cols = feed_pager::columns(self.home_viewport_w);
                self.home_state.page_count = 0;
                self.home_state.video_count = 0;
                self.home_state.reached_end = false;
                self.home_state.error = None;

                let target = feed_pager::logical_page_target(self.home_viewport_w, self.home_viewport_h);
                self.allocate_skeleton_slots(target);

                if self.home_state.loading {
                    self.home_refresh_pending = true;
                    return;
                }
                self.home_refresh_pending = false;
                self.request_feed_batch(FeedKind::Refresh, target, true, context);
            }
            Message::HomeRowActive {
                rev,
                row_idx,
                active,
            } => {
                if rev != self.current_row_revision() {
                    return;
                }
                if active {
                    self.home_active_rows.insert(row_idx);
                } else {
                    self.home_active_rows.remove(&row_idx);
                }
                let cols = self.home_state.grid_cols.max(1);
                let total_rows = self.home_slots.len().div_ceil(cols);
                self.home_anchor_row = self
                    .home_active_rows
                    .iter()
                    .next_back()
                    .copied()
                    .unwrap_or(0)
                    .min(total_rows.saturating_sub(1));

                let vis_rows = feed_pager::visible_rows(self.home_viewport_h);
                let evict_stride = vis_rows.saturating_mul(2).max(4);
                if self.home_anchor_row.abs_diff(self.home_last_evict_row) >= evict_stride {
                    self.sync_offscreen_covers(context);
                }

                if active {
                    if std::env::var_os("BILIPLAY_TRACE_HOME").is_some() {
                        eprintln!(
                            "[bili-ui] row active: row={}/{} anchor={} active_cnt={} slots={} loading={}",
                            row_idx,
                            total_rows,
                            self.home_anchor_row,
                            self.home_active_rows.len(),
                            self.home_slots.len(),
                            self.home_state.loading
                        );
                    }
                    self.maybe_prefetch_next_page(context);
                }
            }
            Message::ToggleMoreMenu => {
                self.more_menu_open = !self.more_menu_open;
            }
            Message::CloseMoreMenu => {
                self.more_menu_open = false;
            }
            Message::NavigateCompact(target) => {
                self.more_menu_open = false;
                self.home_half_pane = false;
                self.logout_confirm = false;
                if self.route != target {
                    self.history.push(self.route);
                    self.route = target;
                    if target == Route::Login && self.auth_gen > 0 {
                        self.ensure_account_loaded(context);
                    }
                }
            }
            Message::WindowResized(size) => {
                self.window_width = size.width.max(320.0);
                if self.window_width >= COMPACT_BREAKPOINT {
                    self.more_menu_open = false;
                }
                let side_pad = if self.window_width < COMPACT_BREAKPOINT { 0.0 } else { 56.0 };
                let vw = (size.width - side_pad).max(320.0);
                let vh = (size.height - 136.0).max(320.0);
                self.home_viewport_w = vw;
                self.home_viewport_h = vh;
                let cols = feed_pager::columns(vw);
                if cols != self.home_state.grid_cols {
                    self.home_state.grid_cols = cols;
                    self.home_active_rows.clear();
                    self.home_anchor_row = 0;
                    self.home_last_evict_row = 0;
                    self.rebuild_home_source();
                }
            }
            Message::HomeRetry => {
                let replace = matches!(self.home_last_kind, FeedKind::First | FeedKind::Refresh);
                let target = if self.home_last_target > 0 {
                    self.home_last_target
                } else {
                    feed_pager::logical_page_target(self.home_viewport_w, self.home_viewport_h)
                };
                if replace {
                    self.home_slots.clear();
                    self.home_slot_by_bvid.clear();
                    self.home_index.clear();
                    self.home_next_slot_seq = 0;
                    self.home_pending_skeletons = 0;
                }
                self.allocate_skeleton_slots(target);
                self.request_feed_batch(self.home_last_kind, target, replace, context);
            }
            Message::HomeLoaded {
                seq,
                cards,
                exhausted,
                replace,
            } => {
                self.home_state.loading = false;
                if self.home_refresh_pending {
                    self.home_refresh_pending = false;
                    let target = self
                        .home_pending_skeletons
                        .max(feed_pager::logical_page_target(self.home_viewport_w, self.home_viewport_h));
                    self.request_feed_batch(FeedKind::Refresh, target, true, context);
                    return;
                }
                if seq != self.feed_gen {
                    return;
                }
                if replace && cards.is_empty() && !exhausted {
                    self.truncate_unfilled_skeletons();
                    self.home_state.error = Some("推荐流暂无内容，点击重试".to_string());
                    return;
                }
                self.home_state.error = None;
                if !cards.is_empty() {
                    self.home_state.page_count = if replace {
                        1
                    } else {
                        self.home_state.page_count + 1
                    };
                }

                // 原地填入已分配的骨架槽位框架；未填满尾部多余槽位一并收起。
                let mut cursor = self
                    .home_slots
                    .iter()
                    .position(|slot| matches!(slot, HomeSlot::Skeleton { .. }))
                    .unwrap_or(self.home_slots.len());

                for mut item in cards {
                    if let Some(url) = item.pic_url.clone() {
                        if let Some(cached) = self.image_cache.get(&url) {
                            item.cover = Some(cached);
                        } else {
                            self.spawn_cover_fetch(item.bvid.clone(), url, seq, context);
                        }
                    }

                    let bvid = item.bvid.clone();
                    self.home_index.insert(bvid.clone(), item.clone());

                    if cursor < self.home_slots.len() {
                        let slot_key = self.home_slots[cursor].slot_key().to_string();
                        self.home_slots[cursor] = HomeSlot::Video { slot_key, item };
                        self.home_slot_by_bvid.insert(bvid, cursor);
                        cursor += 1;
                    } else {
                        let next_seq = self.home_next_slot_seq;
                        self.home_next_slot_seq += 1;
                        let slot_key = format!("slot-{}-{}", self.feed_gen, next_seq);
                        self.home_slots.push(HomeSlot::Video { slot_key, item });
                        self.home_slot_by_bvid.insert(bvid, cursor);
                        cursor += 1;
                    }
                }

                // 收起该批未用到或流耗尽后的多余骨架槽位，防止底部出现死区。
                if cursor < self.home_slots.len() {
                    self.home_slots.truncate(cursor);
                }
                self.home_pending_skeletons = 0;
                self.home_state.video_count = self.home_slots.len();
                self.rebuild_home_source();
                if std::env::var_os("BILIPLAY_TRACE_HOME").is_some() {
                    eprintln!(
                        "[bili-ui] batch loaded: total_videos={}, page_count={}, exhausted={}",
                        self.home_state.video_count, self.home_state.page_count, exhausted
                    );
                }
                if exhausted || self.home_slots.len() >= MAX_HOME_SLOTS {
                    self.home_state.reached_end = true;
                } else {
                    let cols = self.home_state.grid_cols.max(1);
                    let total_rows = self.home_slots.len().div_ceil(cols);
                    self.home_anchor_row = self
                        .home_active_rows
                        .iter()
                        .next_back()
                        .copied()
                        .unwrap_or(0)
                        .min(total_rows.saturating_sub(1));
                    self.maybe_prefetch_next_page(context);
                }
            }
            Message::HomeFailed { seq, reason } => {
                self.home_state.loading = false;
                if self.home_refresh_pending {
                    self.home_refresh_pending = false;
                    let target = self
                        .home_pending_skeletons
                        .max(feed_pager::logical_page_target(self.home_viewport_w, self.home_viewport_h));
                    self.request_feed_batch(FeedKind::Refresh, target, true, context);
                    return;
                }
                if seq != self.feed_gen {
                    return;
                }
                self.truncate_unfilled_skeletons();
                self.home_state.error = Some(reason);
            }
            Message::CoverReady { seq, bvid, image } => {
                if seq != self.feed_gen {
                    return;
                }
                self.cover_inflight.remove(&bvid);
                let Some(image) = image else { return };
                self.pending_covers.insert(bvid, image);
                self.schedule_cover_flush(context);
            }
            Message::FlushCovers => {
                self.cover_flush_timer = None;
                if self.pending_covers.is_empty() {
                    return;
                }
                let keep = feed_pager::visible_cover_window_by_row(
                    self.home_anchor_row,
                    self.home_state.grid_cols.max(1),
                    self.home_viewport_h,
                    self.home_slots.len(),
                    COVER_WINDOW_BUFFER_SCREENS,
                );
                let batch = std::mem::take(&mut self.pending_covers);
                let mut changed = false;
                for (bvid, image) in batch {
                    if let Some(&idx) = self.home_slot_by_bvid.get(&bvid)
                        && keep.contains(&idx)
                        && let Some(HomeSlot::Video { item, .. }) = self.home_slots.get_mut(idx)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image);
                        changed = true;
                    }
                }
                if changed {
                    self.rebuild_home_source();
                }
            }
            Message::PreResolveVideo(bvid) => {
                let Some(item) = self.home_index.get(&bvid).cloned() else {
                    return;
                };
                if self
                    .now_playing
                    .as_ref()
                    .is_some_and(|cur| cur.bvid == bvid)
                {
                    return;
                }
                let nav = self.auth_service.nav_client();
                let key = resolve_profile_key(&self.settings, &nav);
                if self
                    .preresolved
                    .get(&bvid)
                    .is_some_and(|c| c.key == key && c.fetched_at.elapsed() < PRERESOLVE_TTL)
                {
                    return;
                }
                if self.preresolve_inflight.contains_key(&bvid)
                    || self.preresolve_inflight.len() >= MAX_PRERESOLVE_INFLIGHT
                {
                    return;
                }
                self.preresolve_inflight.insert(bvid.clone(), key);

                let auth = self.auth_service.clone();
                let (max_qn, codecs) = resolve_params_for(&self.settings, &nav);
                context.spawn_background(move |_| {
                    let client =
                        PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                    let res = block_on(client.resolve_with_cid(
                        &item.bvid,
                        item.aid,
                        item.cid,
                        &item.title,
                        i64::from(item.duration_secs),
                        max_qn,
                        codecs,
                    ))
                    .map_err(|e| e.to_string());
                    Message::MediaPreResolved {
                        bvid: item.bvid,
                        key,
                        result: res,
                    }
                });
            }
            Message::MediaPreResolved { bvid, key, result } => {
                self.preresolve_inflight.remove(&bvid);
                let matched_open = self
                    .pending_open_preresolve
                    .as_ref()
                    .is_some_and(|(pending_bvid, gen_id, pending_key)| {
                        pending_bvid == &bvid && *gen_id == self.playback_gen && *pending_key == key
                    });
                if matched_open {
                    self.pending_open_preresolve = None;
                }
                match result {
                    Ok((meta, choice)) => {
                        if std::env::var_os("BILIPLAY_TRACE_HOME").is_some() {
                            eprintln!(
                                "[bili-ui] preresolved cached (no autoplay): bvid={}, qn={}, route={:?}",
                                bvid, choice.video.id, self.route
                            );
                        }
                        if matched_open {
                            self.apply_resolved_media(
                                choice.video.base_url.clone(),
                                choice.audio.base_url.clone(),
                                meta.clone(),
                                self.playback_gen,
                                context,
                            );
                        }
                        self.cache_preresolved(bvid, key, meta, choice);
                    }
                    Err(reason) => {
                        if matched_open {
                            self.player_notice = Some(format!("视频解析失败:{reason}"));
                            self.playing = false;
                        }
                    }
                }
            }

            // ---- 设置 ----
            Message::UpdateSettings(next) => {
                let next = next.normalized();
                if next == self.settings {
                    return;
                }
                if next.volume != self.settings.volume {
                    self.volume = next.volume;
                    if let Some(engine) = &self.engine {
                        engine.set_volume(f64::from(next.volume));
                    }
                }
                if next.hardware_decode != self.settings.hardware_decode
                    && let Some(engine) = &self.engine
                {
                    engine.set_hardware_decode(next.hardware_decode);
                }
                if (next.danmaku_enabled != self.settings.danmaku_enabled
                    || (next.danmaku_opacity - self.settings.danmaku_opacity).abs() > f32::EPSILON
                    || next.danmaku_max_lanes != self.settings.danmaku_max_lanes)
                    && let Some(engine) = &self.engine
                {
                    engine.set_danmaku_config(
                        next.danmaku_enabled,
                        next.danmaku_opacity,
                        next.danmaku_max_lanes,
                    );
                }
                self.settings = next;
                self.schedule_settings_save(context);
            }
            Message::FlushSettingsSave => {
                self.settings_save_timer = None;
                let store = self.settings_store.clone();
                let snapshot = self.settings.clone();
                context.spawn_background(move |_| {
                    Message::SettingsSaved(store.save(&snapshot).err().map(|e| e.to_string()))
                });
            }
            Message::SettingsSaved(err) => {
                self.settings_notice = err.map(|e| format!("保存设置失败：{e}"));
            }

            // ---- 登录 ----
            Message::AuthProbed { seq, report } => {
                if seq != self.auth_gen {
                    return;
                }
                self.auth = report.state;
                if let Some(notice) = report.notice {
                    self.auth_notice = Some(notice);
                }
                if matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.fetch_account_summary(context);
                    if self.route == Route::Login {
                        self.ensure_active_tab_loaded(context);
                    }
                } else {
                    self.reset_account_data();
                }
            }
            Message::AuthStartLogin => {
                if matches!(
                    self.login_flow,
                    LoginFlowState::Starting
                        | LoginFlowState::AwaitingScan { .. }
                        | LoginFlowState::Scanned { .. }
                ) {
                    return;
                }
                self.logout_confirm = false;
                self.auth_notice = None;
                self.login_flow = LoginFlowState::Starting;
                let auth = self.auth_service.clone();
                let cell = self.flow.clone();
                context.spawn_background(move |_| match auth.start_login_blocking(&cell) {
                    Ok(qr) => Message::LoginQrReady(qr),
                    Err(reason) => Message::LoginStartFailed(reason),
                });
            }
            Message::LoginQrReady(qr) => {
                self.login_flow = LoginFlowState::AwaitingScan { qr };
                self.schedule_login_tick(context);
            }
            Message::LoginStartFailed(reason) => {
                self.login_flow = LoginFlowState::Failed(reason);
            }
            Message::LoginPollTick => {
                let auth = self.auth_service.clone();
                let cell = self.flow.clone();
                context.spawn_background(move |_| Message::LoginPolled(auth.poll_blocking(&cell)));
            }
            Message::LoginPolled(outcome) => match outcome {
                PollOutcome::WaitingScan => self.schedule_login_tick(context),
                PollOutcome::Scanned => {
                    self.login_flow = match &self.login_flow {
                        LoginFlowState::AwaitingScan { qr } | LoginFlowState::Scanned { qr } => {
                            LoginFlowState::Scanned { qr: qr.clone() }
                        }
                        other => other.clone(),
                    };
                    self.schedule_login_tick(context);
                }
                PollOutcome::Expired => {
                    self.login_timer = None;
                    self.login_flow = match &self.login_flow {
                        LoginFlowState::AwaitingScan { qr }
                        | LoginFlowState::Scanned { qr }
                        | LoginFlowState::Expired { qr } => LoginFlowState::Expired { qr: qr.clone() },
                        other => other.clone(),
                    };
                }
                PollOutcome::Success => {
                    self.clear_login_session();
                    self.reset_account_data();
                    // 会话已建立 → 递增认证代次重新探测用户信息（头像/昵称/续期状态）。
                    self.auth = AuthState::Unknown;
                    self.auth_gen = self.auth_gen.wrapping_add(1);
                    let auth = self.auth_service.clone();
                    let seq = self.auth_gen;
                    context.spawn_background(move |_| Message::AuthProbed {
                        seq,
                        report: auth.probe_blocking(),
                    });
                }
                PollOutcome::Error(reason) => {
                    self.login_timer = None;
                    self.login_flow = LoginFlowState::Failed(reason);
                }
            },
            Message::AuthCancelLogin => {
                self.logout_confirm = false;
                self.auth_notice = None;
                self.clear_login_session();
            }
            Message::AuthLogout => {
                // 二次确认：登出会作废服务端会话（不可逆）。
                if !self.logout_confirm {
                    self.logout_confirm = true;
                    return;
                }
                self.logout_confirm = false;
                self.auth_gen = self.auth_gen.wrapping_add(1);
                let auth = self.auth_service.clone();
                context.spawn_background(move |_| {
                    Message::AuthLoggedOut(auth.logout_blocking().err())
                });
            }
            Message::AuthCancelLogout => {
                self.logout_confirm = false;
            }
            Message::AuthLoggedOut(error) => match error {
                Some(reason) => self.auth_notice = Some(format!("退出失败：{reason}")),
                None => {
                    self.auth_gen = self.auth_gen.wrapping_add(1);
                    self.auth_notice = None;
                    self.auth = AuthState::Guest;
                    self.reset_account_data();
                }
            },

            // ---- 个人中心与通知中心 ----
            Message::OpenNotifications => {
                self.logout_confirm = false;
                if self.route != Route::Login {
                    self.history.push(self.route);
                    self.route = Route::Login;
                }
                if self.notify_tab.unread(self.unread_summary) == 0
                    && let Some(unread_tab) = NotifyTab::ALL
                        .into_iter()
                        .find(|tab| tab.unread(self.unread_summary) > 0)
                {
                    self.notify_tab = unread_tab;
                }
                self.ensure_account_loaded(context);
            }
            Message::AccountRefresh => {
                if !matches!(self.auth, AuthState::LoggedIn(_)) {
                    return;
                }
                let tab = self.notify_tab;
                let selected = self.selected_whisper;
                self.reset_account_data();
                self.notify_tab = tab;
                self.fetch_account_summary(context);
                self.fetch_notify_tab(tab, false, context);
                if tab == NotifyTab::Whisper
                    && let Some((tid, stype)) = selected
                {
                    self.selected_whisper = Some((tid, stype));
                    self.fetch_whisper_thread(tid, stype, context);
                }
            }
            Message::AccountStatLoaded { seq, stat } => {
                if seq == self.notify_gen && stat.is_some() {
                    self.account_stat = stat;
                }
            }
            Message::UnreadLoaded { seq, summary } => {
                if seq == self.notify_gen
                    && let Some(s) = summary
                {
                    self.unread_summary = s;
                }
            }
            Message::SelectNotifyTab(tab) => {
                self.notify_tab = tab;
                self.ensure_active_tab_loaded(context);
            }
            Message::LoadMoreNotifyTab(tab) => {
                self.fetch_notify_tab(tab, true, context);
            }
            Message::RetryNotifyTab(tab) => {
                let append = match tab {
                    NotifyTab::Reply => !self.notify_replies.items.is_empty(),
                    NotifyTab::At => !self.notify_ats.items.is_empty(),
                    NotifyTab::Like => !self.notify_likes.items.is_empty(),
                    NotifyTab::System => !self.notify_systems.items.is_empty(),
                    NotifyTab::Whisper => !self.notify_sessions.items.is_empty(),
                };
                self.fetch_notify_tab(tab, append, context);
            }
            Message::RepliesLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return;
                }
                let urls = apply_feed_page(
                    &mut self.notify_replies,
                    &mut self.notify_replies_cursor,
                    append,
                    res,
                    |i| &i.sender_avatar,
                );
                self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
            }
            Message::AtsLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return;
                }
                let urls = apply_feed_page(
                    &mut self.notify_ats,
                    &mut self.notify_ats_cursor,
                    append,
                    res,
                    |i| &i.sender_avatar,
                );
                self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
            }
            Message::LikesLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return;
                }
                let urls = apply_feed_page(
                    &mut self.notify_likes,
                    &mut self.notify_likes_cursor,
                    append,
                    res,
                    |i| &i.sender_avatar,
                );
                self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
            }
            Message::SystemsLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return;
                }
                self.notify_systems.loading = false;
                self.notify_systems.loaded_once = true;
                match res {
                    Ok(page) => {
                        if append {
                            let existing: HashSet<u64> =
                                self.notify_systems.items.iter().map(|i| i.id).collect();
                            for item in page.items {
                                if !existing.contains(&item.id) {
                                    self.notify_systems.items.push(item);
                                }
                            }
                        } else {
                            self.notify_systems.items = page.items;
                        }
                        self.notify_systems_cursor = page.next_cursor;
                        self.notify_systems.has_more = page.has_more;
                        self.notify_systems.error = None;
                    }
                    Err(err) => self.notify_systems.error = Some(err),
                }
            }
            Message::SessionsLoaded { seq, append, res } => {
                if seq != self.notify_gen {
                    return;
                }
                self.notify_sessions.loading = false;
                self.notify_sessions.loaded_once = true;
                match res {
                    Ok(page) => {
                        let urls: Vec<String> = page
                            .sessions
                            .iter()
                            .map(|s| s.talker_avatar.clone())
                            .collect();
                        if append {
                            self.notify_sessions.items.extend(page.sessions);
                        } else {
                            self.notify_sessions.items = page.sessions;
                        }
                        self.notify_sessions_end_ts = page.end_ts;
                        self.notify_sessions.has_more = page.has_more;
                        self.notify_sessions.error = None;
                        self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
                        if self.selected_whisper.is_none()
                            && let Some(first) = self.notify_sessions.items.first()
                        {
                            let key = (first.talker_id, first.session_type);
                            self.selected_whisper = Some(key);
                            if !self.whisper_threads.contains_key(&key) {
                                self.fetch_whisper_thread(key.0, key.1, context);
                            }
                        }
                    }
                    Err(err) => self.notify_sessions.error = Some(err),
                }
            }
            Message::SelectWhisperSession((talker_id, session_type)) => {
                self.selected_whisper = Some((talker_id, session_type));
                if !self.whisper_threads.contains_key(&(talker_id, session_type)) {
                    self.fetch_whisper_thread(talker_id, session_type, context);
                }
            }
            Message::RefreshWhisperThread((talker_id, session_type)) => {
                self.fetch_whisper_thread(talker_id, session_type, context);
            }
            Message::WhisperThreadLoaded {
                seq,
                talker_id,
                session_type,
                res,
            } => {
                if seq != self.notify_gen {
                    return;
                }
                let entry = self
                    .whisper_threads
                    .entry((talker_id, session_type))
                    .or_default();
                entry.loading = false;
                match res {
                    Ok(msgs) => {
                        entry.messages = msgs;
                        entry.error = None;
                    }
                    Err(err) => entry.error = Some(err),
                }
            }
            Message::AvatarLoaded {
                avatar_gen,
                inflight_epoch,
                url,
                has_image,
            } => {
                if avatar_gen != self.avatar_gen {
                    return;
                }
                if self.avatar_inflight.get(&url) == Some(&inflight_epoch) {
                    self.avatar_inflight.remove(&url);
                }
                if has_image
                    && self.now_playing.is_some()
                    && self.comments_session.references_avatar(&url)
                {
                    self.playback_avatar_dirty = true;
                    self.schedule_playback_avatar_flush(context);
                }
            }
            Message::FlushPlaybackAvatars => {
                self.playback_avatar_timer = None;
                if self.playback_avatar_dirty && self.now_playing.is_some() {
                    self.playback_avatar_dirty = false;
                    self.refresh_comments_snapshot();
                }
            }
            Message::OpenRelations(kind) => {
                let (slice, _) = self.relation_slot(kind);
                if !slice.loading && !slice.loaded_once {
                    self.fetch_relations(kind, false, context);
                }
            }
            Message::RefreshRelations(kind) => {
                self.fetch_relations(kind, false, context);
            }
            Message::LoadMoreRelations(kind) => {
                self.fetch_relations(kind, true, context);
            }
            Message::RelationsLoaded {
                seq,
                kind,
                append,
                res,
            } => {
                if seq != self.notify_gen {
                    return;
                }
                let urls: Option<Vec<String>> = {
                    let (slice, next_pn) = self.relation_slot_mut(kind);
                    slice.loading = false;
                    slice.loaded_once = true;
                    match res {
                        Ok(page) => {
                            let faces: Vec<String> =
                                page.items.iter().map(|u| u.face.clone()).collect();
                            if append {
                                let existing: HashSet<u64> =
                                    slice.items.iter().map(|u| u.mid).collect();
                                for u in page.items {
                                    if !existing.contains(&u.mid) {
                                        slice.items.push(u);
                                    }
                                }
                            } else {
                                slice.items = page.items;
                            }
                            *next_pn = page.next_pn;
                            slice.has_more = page.has_more;
                            slice.error = None;
                            if page.total > 0 {
                                let stat = self.account_stat.get_or_insert_with(Default::default);
                                match kind {
                                    RelationKind::Following => stat.following = page.total,
                                    RelationKind::Follower => stat.follower = page.total,
                                }
                            }
                            Some(faces)
                        }
                        Err(err) => {
                            slice.error = Some(err);
                            None
                        }
                    }
                };
                if let Some(urls) = urls {
                    self.spawn_notify_avatars(urls.iter().map(String::as_str), context);
                }
            }
        }
    }

    fn view(&self, _input: &ShellInput, context: &mut ViewContext<Self>) -> View {
        context.window_title(crate::notifications::APP_DISPLAY_NAME);
        let mut visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .client_size(1280.0, 800.0);
        if let Some(bounds) = self.settings.window {
            visuals = visuals.initial_placement(WindowPlacement {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
                maximized: bounds.maximized,
            });
        }
        context.window_visuals(visuals);
        context.on_window_size(context.callback(Message::WindowResized));

        let compact = self.window_width < COMPACT_BREAKPOINT;
        let on_open = context.callback(Message::OpenVideo);
        let is_vip = matches!(&self.auth, AuthState::LoggedIn(user) if user.vip.is_vip());
        let my_avatar = match &self.auth {
            AuthState::LoggedIn(user) => user.avatar.clone(),
            _ => None,
        };
        let comment_cb = CommentCallbacks {
            on_sort: context.callback(Message::SetCommentSort),
            on_load_more: context.message(Message::LoadMoreComments),
            on_retry: context.message(Message::RetryComments),
            on_toggle_sub: context.callback(Message::ToggleSubReplies),
            on_load_more_sub: context.callback(Message::LoadMoreSubReplies),
        };
        let author_avatar = self
            .now_playing
            .as_ref()
            .and_then(|item| item.author_face_url.as_deref())
            .map(avatar_thumbnail_url)
            .and_then(|url| self.avatar_cache.get(&url));

        let content: View = if self.route == Route::Player {
            match (self.now_playing.as_ref(), self.engine.as_ref()) {
                (Some(item), Some(engine)) => {
                    View::component::<pages::player::PlayerPage>(pages::player::PlayerInput {
                        item: item.clone(),
                        author_avatar: author_avatar.clone(),
                        my_avatar: my_avatar.clone(),
                        desc: self.now_playing_desc.clone(),
                        playing: self.playing,
                        position: self.position,
                        volume: self.volume,
                        compact,
                        mount_tick: self.mount_tick,
                        duration_secs: self.display_duration(item),
                        notice: self.player_notice.clone(),
                        engine: engine.clone(),
                        comments: self.comments_snapshot.clone(),
                        comment_cb: comment_cb.clone(),
                        on_toggle: context.message(Message::TogglePlay),
                        on_seek: context.callback(|value: f64| Message::Seek(value)),
                        on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                    })
                }
                _ => self.home_view(context),
            }
        } else if self.route == Route::Login {
            let (account_view, account_cb) = match &self.auth {
                AuthState::LoggedIn(user) => (
                    Some(AccountViewData {
                        user,
                        stat: self.account_stat,
                        unread: self.unread_summary,
                        followings: &self.followings,
                        followers: &self.followers,
                        active_tab: self.notify_tab,
                        replies: &self.notify_replies,
                        ats: &self.notify_ats,
                        likes: &self.notify_likes,
                        systems: &self.notify_systems,
                        sessions: &self.notify_sessions,
                        selected_talker: self.selected_whisper,
                        selected_thread: self
                            .selected_whisper
                            .and_then(|key| self.whisper_threads.get(&key)),
                        avatar_cache: &self.avatar_cache,
                        logout_confirm: self.logout_confirm,
                        auth_notice: self.auth_notice.as_deref(),
                    }),
                    Some(AccountCallbacks {
                        on_refresh: context.message(Message::AccountRefresh),
                        on_logout: context.message(Message::AuthLogout),
                        on_cancel_logout: context.message(Message::AuthCancelLogout),
                        on_open_relations: context.callback(Message::OpenRelations),
                        on_refresh_relations: context.callback(Message::RefreshRelations),
                        on_load_more_relations: context.callback(Message::LoadMoreRelations),
                        on_select_tab: context.callback(Message::SelectNotifyTab),
                        on_load_more: context.callback(Message::LoadMoreNotifyTab),
                        on_retry_tab: context.callback(Message::RetryNotifyTab),
                        on_select_session: context.callback(Message::SelectWhisperSession),
                        on_refresh_thread: context.callback(Message::RefreshWhisperThread),
                        on_open_video: context.callback(Message::OpenVideoTarget),
                    }),
                ),
                _ => (None, None),
            };
            pages::login::login_page(
                &self.auth,
                &self.login_flow,
                account_view,
                account_cb,
                context.message(Message::AuthStartLogin),
                context.message(Message::AuthCancelLogin),
            )
        } else if self.route == Route::Settings {
            let on_change = context.callback(Message::UpdateSettings);
            pages::settings::settings_page(
                &self.settings,
                is_vip,
                compact,
                self.settings_notice.as_deref(),
                &on_change,
            )
        } else if self.route == Route::Home {
            self.home_view(context)
        } else {
            router::route(self.route, on_open.clone())
        };

        // 切换目的地时退役原生控件（含播放页换视频的场景）。
        let key = if self.route == Route::Player {
            format!(
                "player:{}",
                self.now_playing.as_ref().map_or(String::new(), |item| item.bvid.clone())
            )
        } else {
            self.route.tag().to_string()
        };

        let menu_items: Vec<KeyedView> = NAV_ROUTES
            .iter()
            .map(|&route| {
                nav_item(
                    route.tag(),
                    route.label(),
                    Some(route.icon()),
                    self.route == route,
                )
            })
            .collect();

        // 页脚账号入口：反映登录态（头像位图 / 昵称 / 未登录）。
        let account = match &self.auth {
            AuthState::LoggedIn(user) => {
                let mut item = NavigationViewItem::new()
                    .tag(Route::Login.tag())
                    .is_selected(self.route == Route::Login)
                    .selects_on_invoked(true);
                item = match &user.avatar {
                    Some(img) => item.icon(Icon::image_data(img.clone())),
                    None => item.icon(Symbol::Account),
                };
                item.content(user.uname.clone())
            }
            AuthState::Unknown => NavigationViewItem::new()
                .tag(Route::Login.tag())
                .is_selected(self.route == Route::Login)
                .selects_on_invoked(true)
                .icon(Symbol::Account)
                .content("账号"),
            AuthState::Guest => NavigationViewItem::new()
                .tag(Route::Login.tag())
                .is_selected(self.route == Route::Login)
                .selects_on_invoked(true)
                .icon(Symbol::Account)
                .content("登录"),
        };
        let settings_item = NavigationViewItem::new()
            .tag(Route::Settings.tag())
            .is_selected(self.route == Route::Settings)
            .selects_on_invoked(true)
            .icon(Route::Settings.icon())
            .content(Route::Settings.label());

        let route_subtitle = if self.route == Route::Login {
            match &self.auth {
                AuthState::LoggedIn(_) => "个人中心",
                _ => "登录",
            }
        } else {
            self.route.label()
        };

        let mut title_bar = TitleBar::new()
            .preferred_height(WindowTitleBarHeight::Tall)
            .icon(Icon::image_data(EncodedImage::from_static(APP_ICON_48_PNG)))
            .title(crate::notifications::APP_DISPLAY_NAME)
            .subtitle(route_subtitle)
            .is_back_button_visible(true)
            .is_back_button_enabled(!self.history.is_empty())
            .is_pane_toggle_button_visible(!compact)
            .on_back_requested(context.message(Message::Back))
            .on_pane_toggle_requested(context.message(Message::TogglePane))
            .grid_row(0)
            .content(TextBox::new().placeholder_text("搜索"));

        if matches!(self.auth, AuthState::LoggedIn(_)) {
            let total_unread = self.unread_summary.total();
            let mut bell_parts: Vec<View> =
                vec![SymbolIcon::new().symbol(Symbol::Mail).into()];
            if total_unread > 0 {
                bell_parts.push(
                    InfoBadge::new()
                        .value(total_unread.min(99) as i32)
                        .vertical_alignment(VerticalAlignment::Center)
                        .into(),
                );
            }
            title_bar = title_bar.right_header(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Message::OpenNotifications))
                    .content(
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .children(bell_parts),
                    ),
            );
        }

        // 播放页与半窗预览自带视频画面 → 隐藏迷你条。
        let mini = if self.route != Route::Player
            && !(self.route == Route::Home && self.home_half_pane)
            && let Some(item) = self.now_playing.as_ref()
        {
            Some(mini_player::mini_player(
                item,
                self.playing,
                self.position,
                &context.message(Message::Seek(0.0)),
                &context.message(Message::TogglePlay),
                &context.message(Message::MiniNext),
                &context.message(Message::ClosePlayer),
                &context.callback(|value: f64| Message::Seek(value)),
            ))
        } else {
            None
        };

        if compact {
            let content_host = Grid::new().grid_row(1).keyed_children({
                let mut layers = if self.route == Route::Player {
                    vec![
                        keyed("home", self.home_view(context)),
                        keyed(
                            key,
                            Border::new()
                                .background(ThemeBrush::SolidBackground)
                                .content(content),
                        ),
                    ]
                } else if self.route == Route::Home && self.home_half_pane {
                    let mut list = vec![keyed(key, content)];
                    if let Some(layer) = self.half_pane_layer(
                        author_avatar.clone(),
                        my_avatar.clone(),
                        comment_cb.clone(),
                        context,
                    ) {
                        list.push(layer);
                    }
                    list
                } else {
                    vec![keyed(key, content)]
                };
                if self.gamepad_active {
                    let focus_title = self.focused_home_video().map(|v| v.title.as_str());
                    layers.push(keyed(
                        "gamepad-hint",
                        gamepad::gamepad_hint_bar(self.route, focus_title),
                    ));
                }
                if self.more_menu_open {
                    layers.push(keyed(
                        "more-scrim",
                        Border::new()
                            .background(Brush::Solid(Color::argb(1, 0, 0, 0)))
                            .on_pointer_pressed(context.callback(|_| Message::CloseMoreMenu)),
                    ));
                    layers.push(keyed(
                        "more-popup",
                        compact_more_popup(self.route, context),
                    ));
                }
                layers
            });

            let bottom_bar = compact_bottom_bar(self.route, self.more_menu_open, context);

            return if let Some(mini) = mini {
                Grid::new()
                    .rows([
                        GridLength::Auto,
                        GridLength::Star(1.0),
                        GridLength::Auto,
                        GridLength::Auto,
                    ])
                    .children((
                        title_bar,
                        content_host,
                        mini.grid_row(2),
                        bottom_bar.grid_row(3),
                    ))
                    .into()
            } else {
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((title_bar, content_host, bottom_bar.grid_row(2)))
                    .into()
            };
        }

        let navigation = NavigationView::new()
            .pane_display_mode(NavigationViewPaneDisplayMode::LeftCompact)
            .is_pane_toggle_button_visible(false)
            .is_back_button_visible(NavigationViewBackButtonVisible::Collapsed)
            .is_settings_visible(false)
            .always_show_header(false)
            .pane_title("biliplay")
            .is_pane_open(self.pane_open)
            .on_is_pane_open_changed(context.callback(Message::PaneOpenChanged))
            .on_selected_tag_changed(context.callback(|tag: Option<Rc<str>>| {
                Message::NavTagSelected(tag.map(|tag| tag.to_string()))
            }))
            .grid_row(1)
            .keyed_menu_items(menu_items)
            .footer_menu_items((account, settings_item))
            .content(Grid::new().keyed_children({
                let mut layers = if self.route == Route::Player {
                    vec![
                        keyed("home", self.home_view(context)),
                        keyed(
                            key,
                            Border::new()
                                .background(ThemeBrush::SolidBackground)
                                .content(content),
                        ),
                    ]
                } else if self.route == Route::Home && self.home_half_pane {
                    let mut list = vec![keyed(key, content)];
                    if let Some(layer) = self.half_pane_layer(
                        author_avatar.clone(),
                        my_avatar.clone(),
                        comment_cb.clone(),
                        context,
                    ) {
                        list.push(layer);
                    }
                    list
                } else {
                    vec![keyed(key, content)]
                };
                if self.gamepad_active {
                    let focus_title = self.focused_home_video().map(|v| v.title.as_str());
                    layers.push(keyed(
                        "gamepad-hint",
                        gamepad::gamepad_hint_bar(self.route, focus_title),
                    ));
                }
                layers
            }));

        if let Some(mini) = mini {
            Grid::new()
                .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                .children((title_bar, navigation, mini.grid_row(2)))
                .into()
        } else {
            Grid::new()
                .rows([GridLength::Auto, GridLength::Star(1.0)])
                .children((title_bar, navigation))
                .into()
        }
    }
}

fn nav_item(tag: &str, label: &str, icon: Option<Symbol>, selected: bool) -> KeyedView {
    let mut item = NavigationViewItem::new()
        .tag(tag)
        .is_selected(selected)
        .selects_on_invoked(true)
        .content(label);
    if let Some(symbol) = icon {
        item = item.icon(symbol);
    }
    KeyedView::new(tag.to_string(), item)
}

const COMPACT_MORE_ROUTES: [Route; 5] = [
    Route::Following,
    Route::Live,
    Route::Favorites,
    Route::History,
    Route::Settings,
];

/// Android Material 3 × Fluent 窄屏底部导航栏（4 个常驻 + 1 个“更多”向上展开 1×5 弹窗）。
fn compact_bottom_bar(
    active_route: Route,
    more_open: bool,
    context: &mut ViewContext<AppShell>,
) -> Grid {
    let primary: [(Route, &str); 4] = [
        (Route::Home, "推荐"),
        (Route::Popular, "热门"),
        (Route::Search, "搜索"),
        (Route::Login, "我的"),
    ];
    let more_active = more_open || COMPACT_MORE_ROUTES.contains(&active_route);

    let tab_btn = |col: usize, symbol: Symbol, label: &str, selected: bool, on_click: Callback<()>| {
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
                        .width(56.0)
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
                context.message(Message::NavigateCompact(route)),
            )
            .into()
        })
        .collect();

    children.push(
        tab_btn(
            4,
            Symbol::More,
            "更多",
            more_active,
            context.message(Message::ToggleMoreMenu),
        )
        .into(),
    );

    Grid::new()
        .children((Border::new()
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

/// 窄屏底栏右侧向上展开的 1×5 悬浮菜单。
fn compact_more_popup(active_route: Route, context: &mut ViewContext<AppShell>) -> View {
    let rows: Vec<View> = COMPACT_MORE_ROUTES
        .iter()
        .map(|&route| {
            let selected = active_route == route;
            Button::new()
                .style(if selected {
                    ButtonStyle::Accent
                } else {
                    ButtonStyle::Subtle
                })
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Left)
                .on_click(context.message(Message::NavigateCompact(route)))
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(10.0)
                        .margin(Thickness::xy(6.0, 4.0))
                        .children((
                            FontIcon::new()
                                .glyph(symbol_glyph(route.icon()))
                                .font_size(14.0)
                                .vertical_alignment(VerticalAlignment::Center),
                            TextBlock::new()
                                .text(route.label())
                                .font_size(13.0)
                                .vertical_alignment(VerticalAlignment::Center),
                        )),
                )
                .into()
        })
        .collect();

    Border::new()
        .width(168.0)
        .margin(Thickness::new(0.0, 0.0, 12.0, 10.0))
        .padding(Thickness::xy(6.0, 6.0))
        .corner_radius(12.0)
        .background(ThemeBrush::SolidBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .horizontal_alignment(HorizontalAlignment::Right)
        .vertical_alignment(VerticalAlignment::Bottom)
        .content(StackPanel::new().spacing(4.0).children(rows))
        .into()
}

fn symbol_glyph(symbol: Symbol) -> &'static str {
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
        Symbol::Setting => "\u{E713}",
        _ => "\u{E712}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bili_core::settings::{CodecPref, QualityPref};

    #[test]
    fn resolve_params_caps_guest_at_80_and_honors_codec_pref() {
        let nav = NavClient::new_anonymous(reqwest::Client::new());
        let mut settings = AppSettings::default();
        settings.quality = QualityPref::Q4K;
        settings.codec = CodecPref::Av1Experimental;

        let (max_qn, codecs) = resolve_params_for(&settings, &nav);
        assert_eq!(max_qn, 80);
        assert_eq!(codecs, &["av01", "hev1", "avc1"]);

        settings.quality = QualityPref::Q720;
        settings.codec = CodecPref::HevcFirst;
        let (max_qn, codecs) = resolve_params_for(&settings, &nav);
        assert_eq!(max_qn, 64);
        assert_eq!(codecs, &["hev1", "avc1", "av01"]);
    }

    #[test]
    fn video_config_maps_settings_fields() {
        let settings = AppSettings {
            hardware_decode: false,
            danmaku_enabled: false,
            danmaku_opacity: 0.42,
            danmaku_max_lanes: 6,
            ..AppSettings::default()
        };
        let cfg = video_config_from(&settings);
        assert!(!cfg.hardware_decode);
        assert!(!cfg.danmaku_enabled);
        assert!((cfg.danmaku_opacity - 0.42).abs() < f32::EPSILON);
        assert_eq!(cfg.danmaku_max_lanes, 6);
    }

    #[test]
    fn thumbnail_url_appends_cdn_resize_for_hdslb_images() {
        assert_eq!(
            thumbnail_url("http://i2.hdslb.com/bfs/archive/abc.jpg"),
            "https://i2.hdslb.com/bfs/archive/abc.jpg@480w_270h_1c.jpg"
        );
        // 已有参数/非图床保持不二次拼接。
        assert_eq!(
            thumbnail_url("https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp"),
            "https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp"
        );
    }

    #[test]
    fn video_item_carries_owner_mid_and_normalized_face_url() {
        let card = FeedCard {
            id: 80433022,
            goto: Some("av".into()),
            bvid: Some("BV1GJ411x7h7".into()),
            cid: Some(137649199),
            title: Some("MV".into()),
            owner: Some(bili_core::feed::Owner {
                mid: 486906719,
                name: "索尼音乐中国".into(),
                face: Some("http://i2.hdslb.com/bfs/face/up.jpg".into()),
            }),
            ..FeedCard::default()
        };
        let item = video_item(&card);
        assert_eq!(item.aid, 80433022);
        assert_eq!(item.author_mid, 486906719);
        assert_eq!(
            item.author_face_url.as_deref(),
            Some("https://i2.hdslb.com/bfs/face/up.jpg")
        );
    }

    #[test]
    fn comments_session_tracks_avatar_refs_and_builds_snapshot() {
        let mut session = CommentsSessionState::default();
        session.reset(80433022, 486906719);
        session.hot.replies.push(ReplyItem {
            rpid: 101,
            oid: 80433022,
            root: 0,
            parent: 0,
            mid: 12,
            uname: "评论用户".into(),
            avatar: "https://i0.hdslb.com/bfs/face/u1.jpg".into(),
            level: 6,
            message: "nice".into(),
            ctime: 1700000000,
            like: 5,
            rcount: 0,
            time_desc: "1小时前".into(),
            location: "上海".into(),
            is_up_top: false,
            up_liked: false,
            up_replied: false,
            replies: Vec::new(),
        });

        let thumb = avatar_thumbnail_url("https://i0.hdslb.com/bfs/face/u1.jpg");
        assert!(session.references_avatar(&thumb));
        assert!(!session.references_avatar("https://i0.hdslb.com/bfs/face/other.jpg@96w_96h_1c.jpg"));

        let cache = ImageCache::with_capacity(8);
        cache.put(thumb.clone(), EncodedImage::new(vec![1, 2, 3]));
        let snap = session.to_snapshot(7, &cache);
        assert_eq!(snap.rev, 7);
        assert_eq!(snap.data.replies.len(), 1);
        assert!(snap.data.avatars.contains_key(&thumb));
    }
}
