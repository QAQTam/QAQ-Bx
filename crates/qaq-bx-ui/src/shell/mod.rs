//! 应用壳：TitleBar + NavigationView + 路由 + 迷你播放条 + 登录状态机
//! （`docs/ui-design.md` §1/§5；登录桥接见 `crate::auth`）。
//! 播放与登录状态在此单源持有，页面组件纯展示 + 回调上报。
//!
//! ## 拆解布局(P3,见 docs/dev-notes/shell-decomposition-plan.md)
//! - `message.rs` Message 枚举(163 变体按域分组);`util.rs` 纯函数/纯类型;
//! - `handlers/` update 域分发:mod.rs 穷尽路由表 + 各域 handle(臂逐字自原 giant match 搬运);
//! - `tests.rs` 单测;`mod.rs` 留守 AppShell 结构体、create()/view() 与少量共通方法
//!   (refresh_probe/schedule_tick/视图构建器,P5 再拆 view/)。

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qaq_bx_core::account::{AccountStat, NavClient, RelationKind, RelationPage, RelationUser};
use qaq_bx_core::danmaku::{DanmakuClient, DanmakuSegment, SEGMENT_MS};
use qaq_bx_core::feed::{
    ArchiveCard, FeedCard, FeedClient, FeedKind, FeedSession, fetch_rcmd_batch,
};
use qaq_bx_core::fav::FavClient;
use qaq_bx_core::history::HistoryClient;
use qaq_bx_core::live::LiveClient;
use crate::pages::live::{LiveCard, LivePageState};
use crate::pages::user_lists::{
    FavFolderChip, FavPageState, UserListState, UserVideoCard,
};
use qaq_bx_core::notify::{
    AtNotice, FeedCursor, LikeNotice, ReplyNotice, SystemNotice, UnreadSummary, WhisperMessage,
    WhisperSession,
};
use qaq_bx_core::cdn::CdnHealthTracker;
use qaq_bx_core::player::{
    AudioStream, Dash, PlayerClient, ResolvedMedia, StreamChoice, VideoMeta, VideoPage,
    VideoStream, is_trial_downgrade_reason, quality_label, quality_menu, select_audio_excluding,
    select_stream_by_qn, select_stream_excluding,
};
use qaq_bx_core::reply::{ReplyClient, ReplyCursor, ReplyItem, ReplyPage, ReplySort, SubReplyPage};
use qaq_bx_core::search::{SearchClient, SearchVideo};
use qaq_bx_core::settings::{AppSettings, FileSettingsStore, PlayerEngine, SETTINGS_FILE};
use qaq_bx_core::stream_proxy::StreamProxy;
use qaq_bx_video::engine::{ContentRef, DanmakuFilter, OverlayDanmaku, VideoConfig, VideoEngine};
use qaq_bx_video::native::NativePlayer;
use qaq_bx_video::stage::DanmakuStage;
use windows::Media::MediaPlaybackStatus;
use windows_reactor::*;

use crate::auth::{
    Auth, AuthState, FlowCell, LoginFlowState, PollOutcome, ProbeReport, avatar_thumbnail_url,
};
use crate::data::VideoItem;
use crate::feed_pager;
use crate::gamepad::{self, GamepadAction, GamepadEvent};
use crate::image_cache::ImageCache;
use crate::layout::{self, ViewportLayout, can_navigate_back, clamp_window_bounds};
use crate::pages;
use crate::pages::account::{
    AccountCallbacks, AccountViewData, ListSliceState, NotifyTab, OpenTarget, WhisperThreadState,
};
use crate::pages::player::{
    CommentCallbacks, CommentSort, CommentsData, CommentsSnapshot, SubReplyState,
};
use crate::router::{self, NAV_ROUTES, Route};
use crate::runtime::{block_on, cover_semaphore};
use crate::smtc::{NativeSmtc, SmtcCommand, SmtcController};
use crate::widgets::chrome;
use crate::widgets::video_card::{self, HomeSlot};
use crate::widgets::peek::{self, CardRect, HoverReport, PeekDismissal, PeekExitAction};

/// 封面下载完成回填 UI 的批处理合并窗口（避免每到 1 张封面重绘整页）。
const COVER_FLUSH_INTERVAL: Duration = Duration::from_millis(80);
/// Hover 预解析缓存有效期（CDN 签名时效内安全复用）。
const PRERESOLVE_TTL: Duration = Duration::from_secs(180);
/// 热门页单次分页条数（实测 popular API ps=20 稳定）。
const POPULAR_PAGE_SIZE: u32 = 20;
/// Hover 预解析最多缓存条目数。
const MAX_PRERESOLVE_CACHE: usize = 16;
/// 关注/粉丝挂件每页拉取条数。
const RELATION_PAGE_SIZE: u32 = 20;
/// TitleBar 应用图标（48×48 透明圆角 PNG，编译期嵌入）。
const APP_ICON_48_PNG: &[u8] = include_bytes!("../../../../packaging/store/QAQ-Bx-48.png");
/// 同时在飞的 Hover 预解析上限（防连续快速扫过触发风控）。
const MAX_PRERESOLVE_INFLIGHT: usize = 4;
/// #7 简介预取缓存条目上限(超限整体清空,预览本就是弱一致缓存)。
const MAX_PEEK_DESC_CACHE: usize = 64;
/// 离屏封面纹理裁剪窗口（保留当前行上下各 4 屏，更远仅保留轻量元数据）。
const COVER_WINDOW_BUFFER_SCREENS: usize = 4;
/// 距离列表底部不足几行时预拉下一逻辑页（留出 ~1 屏无感缓冲）。
const HOME_PREFETCH_LEAD_ROWS: usize = 2;
/// 单流最大加载卡片上限（配合按行虚拟化与封面回收，长流滚动低内存）。
const MAX_HOME_SLOTS: usize = 1_200;
/// 播放状态刷新节拍（引擎快照轮询;0.25s 保证进度条顺滑）。
const PLAYBACK_TICK: Duration = Duration::from_millis(250);
/// P1 网格重建去抖:拖拽期间 VirtualSource 重建延迟到窗口静止后 150ms 一次。
const GRID_REBUILD_DEBOUNCE: Duration = Duration::from_millis(150);

/// 竖屏模式(实验):客户区宽度带上限 = 底栏出现边界 − 1(layout.rs
/// `ShellChromeMode::from_width` 对 860 用严格 `<`,859 保证 Mobile chrome
/// 与底栏在任意 DPI 取整下都在)。下限沿用 `MIN_WINDOW_WIDTH`。
const PORTRAIT_MAX_WIDTH_DIPS: f64 = layout::PORTRAIT_MAX_WIDTH_DIPS;

/// 设置滑块（音量/透明度）去抖落盘窗口。
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
/// M7:直播间播放项 bvid 前缀(真实稿件不会以此开头;直播恒 mpv 的判据)。
const LIVE_BVID_PREFIX: &str = "live:";

const SUB_REPLY_PAGE_SIZE: u32 = 10;

/// 弹幕分段时长(秒;与 core `danmaku::SEGMENT_MS` 锚,B 站 6 分钟/段)。
const DANMAKU_SEGMENT_SECS: u64 = SEGMENT_MS / 1000;

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
    /// 已提交的搜索词(空串 = 尚未搜索)。
    pub search_query: String,
    /// 当前搜索结果条数。
    pub search_videos: usize,
    /// 首条搜索结果 bvid(用于验证 open_video 的 search_index 回退)。
    pub search_first_bvid: Option<String>,
    /// 热门页已加载条数（真实 popular API 分页累计）。
    pub popular_items: usize,
    /// 首条热门卡 bvid(用于验证 open_video 的 popular_index 回退)。
    pub popular_first_bvid: Option<String>,
    /// 相关推荐已加载条数（真实 archive/related API）。
    pub related_items: usize,
    pub related_bvid: String,
    /// 热门页最近一次失败原因（selftest 定位用）。
    pub popular_error: Option<String>,
    /// 挂载脉冲计数(selftest:半屏/播放页 swapchain 未挂载时的重试驱动)。
    pub mount_tick: u32,
    /// 引擎 swapchain 是否已成功挂载到面板。
    pub engine_attached: bool,
    /// 视频加载指示(ProgressRing)是否点亮。
    pub video_loading: bool,
    /// 首页是否处于半屏预览态(route 合法地停在 Home)。
    pub home_half_pane: bool,
    /// 当前是否处于真 Mobile 壳层形态（window_width < 600.0 DIP）。
    pub is_mobile: bool,
    /// 当前首页自适应列数（Mobile=2，Desktop 按视口）。
    pub grid_cols: usize,
    /// 私信单页钻取详情是否展开。
    pub whisper_detail_open: bool,
    /// 窄桌面悬浮搜索浮窗是否开启。
    pub search_overlay_open: bool,
    /// 不可播稿件(充电/付费未解锁)登记条数(付费过滤 selftest 断言用)。
    pub unplayable_count: usize,
    /// 当前弹幕开关状态。
    pub danmaku_enabled: bool,
    /// 当前音量（0..=100）。
    pub volume: u32,
    /// 是否处于系统全屏播放状态。
    pub is_fullscreen: bool,
    /// 播放页画质菜单档位数(F2)。
    pub quality_count: usize,
    /// 当前生效清晰度代号(F2;0 = 未加载)。
    pub current_qn: i64,
    /// 直播页已加载房间数(U4)。
    pub live_rooms: usize,
    /// 直播页首房 roomid(U4;0 = 未加载)。
    pub live_first_roomid: u64,
    /// M7:生效播放引擎标签(env 强制覆盖 > 设置项;`mpv` / `native`)。
    pub player_engine: String,
    /// M7:系统媒体引擎是否正在驱动播放(控制/弹幕/视图三路分流单源)。
    pub native_playback: bool,
    /// 竖屏模式（实验）开关状态。
    pub portrait_mode: bool,
    /// 当前生效视口布局宽度（DIP）。
    pub window_width: f64,
    pub prefer_official_cdn: bool,
    pub multi_thread_preload: bool,
    /// #7:悬停预览卡是否挂载(Open 或宽限期)。
    pub peek_open: bool,
    /// #7:当前挂载预览卡的 bvid。
    pub peek_bvid: Option<String>,
    /// #7:首页已加载真实视频条数(peek 场景选点用)。
    pub home_videos: usize,
    /// #7:首页首个真实卡 bvid(peek 场景注入悬停用)。
    pub home_first_bvid: Option<String>,
    /// U5:历史页已加载条数(selftest 断言)。
    pub user_history_count: usize,
    /// U5:稍后再看页已加载条数(selftest 断言)。
    pub user_watch_later_count: usize,
    /// U5:收藏页已选夹与资源条数(selftest 断言)。
    pub user_fav_cards: usize,
    pub user_fav_folders: usize,
    /// U5:当前夹资源是否落定(成功或失败;夹可空,不能只门控条数)。
    pub user_fav_resources_settled: bool,
    /// U5:当前登录态(selftest user-lists 场景分流用)。
    pub lists_logged_in: bool,
    /// 存储卡:磁盘图片缓存占用字节(selftest 清缓存断言)。
    pub disk_cache_bytes: u64,
    /// 存储卡:上次一键清空结果提示(selftest 断言)。
    pub cache_notice: Option<String>,
    /// 热门页是否在飞(自动续页门控 selftest 断言)。
    pub popular_loading: bool,
}

pub struct AppShell {
    auth_service: Arc<Auth>,
    /// 设置持久化存储（`data/settings.json`）+ 运行时唯一配置源。
    settings_store: Arc<FileSettingsStore>,
    settings: AppSettings,
    settings_notice: Option<String>,
    settings_save_timer: Option<ComponentTimer>,
    /// 设置页「存储」卡:上次一键清空缓存的结果提示。
    cache_notice: Option<String>,
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
    /// #7:卡片悬停进入/离开上报回调(Preview 单例计时由 Shell 持有)。
    home_on_hover_report: Callback<HoverReport>,
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
    // ---- #7 悬停预览卡(全局单例,Shell 驱动计时) ----
    /// 计时/阶段状态机(纯逻辑,单测覆盖)。
    peek: peek::PeekMachine,
    /// 弹出两帧进场:挂载帧为起点(true),16ms 翻牌帧置 false。
    peek_anim_pending: bool,
    peek_prefetch_timer: Option<ComponentTimer>,
    peek_pop_timer: Option<ComponentTimer>,
    peek_grace_timer: Option<ComponentTimer>,
    peek_anim_timer: Option<ComponentTimer>,
    /// bvid → 简介缓存(None 值 = 已拉过但失败/为空,不再重发)。
    peek_desc: HashMap<String, Option<String>>,
    peek_desc_inflight: HashSet<String>,
    /// PeekCard 行内提示(「稍后再看」结果;离开即清)。
    peek_notice: Option<String>,
    /// ✕ 手动关闭的静默范围:仅被关的那张卡,离开它或换卡即恢复(非会话级)。
    peek_dismissal: PeekDismissal,
    /// 不可播稿件标记(bvid → 准确原因):预解析/点击解析命中充电/付费未解锁试看降级时登记,
    /// 首页卡面据此打「充电专稿」角标,后续不再重复请求。
    unplayable: HashMap<String, String>,
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
    /// 综合搜索客户端(WBI 签名,复用共享 nav)。
    search_client: Arc<SearchClient>,
    /// 搜索页展示状态。
    search_state: pages::search::SearchUiState,
    /// 搜索结果索引(open_video 回退查找)。
    search_index: HashMap<String, VideoItem>,
    /// 搜索请求代次(新提交递增,旧结果丢弃)。
    search_seq: u64,
    /// 搜索默认词(U2;`search/default`,启动后台拉取,失败静默)。
    search_default: Option<String>,
    /// 热门页单源状态（真实 popular API，pn/ps 分页）。
    popular: pages::popular::PopularFeedState,
    /// 直播页单源状态(U4;guest 可用 getRoomList)。
    live: LivePageState,
    /// 直播列表/房间流客户端(U4)。
    live_client: Arc<LiveClient>,
    /// 直播封面在飞集合(url 键)。
    live_cover_inflight: std::collections::HashSet<String>,
    /// 历史页单源状态(U5;登录态 history API)。
    user_history: UserListState,
    /// 稍后再看页单源状态(U5;登录态 toview API)。
    watch_later: UserListState,
    /// 收藏页单源状态(U5;v3 夹列表 → 资源列表两级)。
    favorites: FavPageState,
    /// 历史/稍后再看客户端(U5;登录态必需,与共享凭据同源)。
    history_client: Arc<HistoryClient>,
    /// 收藏夹客户端(U5)。
    fav_client: Arc<FavClient>,
    /// 用户列表封面在飞集合(三页共用,url 键)。
    user_cover_inflight: std::collections::HashSet<String>,
    /// 用户列表卡索引(bvid → 展示模型;open_video 数据源链一环)。
    user_index: HashMap<String, VideoItem>,
    /// 热门卡索引（bvid → 展示模型；open_video 数据源链一环）。
    popular_index: HashMap<String, VideoItem>,
    /// 播放页右栏相关推荐状态（真实 archive/related API，跟随播放代次）。
    related: pages::player::RelatedPane,
    /// 相关推荐索引（bvid → 展示模型；open_video 数据源链一环）。
    related_index: HashMap<String, VideoItem>,
    comments_session: CommentsSessionState,
    /// selftest 探针句柄(正常路径恒为 `None`)。
    probe: Option<std::rc::Rc<std::cell::RefCell<ShellProbe>>>,
    comments_snapshot: CommentsSnapshot,
    comments_rev: u64,
    playback_avatar_dirty: bool,
    playback_avatar_timer: Option<ComponentTimer>,
    /// E14:拖框风暴计数器(见 `ResizeStormTrack`;只观测,不改路径)。
    resize_storm: ResizeStormTrack,
    /// §8.2:TitleBar 元素句柄(声明面绑定;档位翻转时补算拖拽区)。
    title_bar_ref: ElementRef<TitleBar>,
    /// §8.2:上次 settled 时的内容结构档位签名(见 `title_bar_tier_signature`)。
    title_bar_tier: u32,
    /// 竖屏模式(实验):贴边停靠态(仅 `settings.portrait_mode` 时有意义)。
    portrait_docked: bool,
    /// 竖屏模式:贴边前外框几何(物理像素,供还原;`None` = 无记录)。
    portrait_dock_restore: Option<(i32, i32, i32, i32)>,
    /// 竖屏模式:系统最大化没收待执行(None = 已同步);窗口未就绪时逐消息重试。
    maximizable_pending: Option<bool>,
    playing: bool,
    position: u32,
    mount_tick: u32,
    volume: u32,
    volume_before_mute: u32,
    /// 引擎回报的媒体总长(秒;None = 尚未加载,用卡片数据兜底)。
    engine_duration: Option<f64>,
    /// 新视频装载闸门:start_playback 置位,直到引擎真正收到新会话
    /// (Load 会清零引擎快照)或装载失败。置位期间 PositionTick 不得把
    /// **旧媒体**的 position/duration/播放态灌进 UI——否则快速切视频时
    /// 新页面会显示旧视频的时长与进度(播放时长残留 bug)。
    media_load_pending: bool,
    /// native 通路本次装载世代(native_load_choice 提交后快照);
    /// 配合 `media_load_pending` 识别「新会话已构建完成」。mpv 通路为 None。
    native_load_epoch: Option<u64>,
    /// mpv 通路本次装载单调世代(`engine.load_with_options` 返回值);
    /// 配合 `media_load_pending` 识别「新会话快照已被引擎受理」。native 通路为 None。
    mpv_load_epoch: Option<u64>,
    /// Bug 2E：native 通路已出声/走时但 `NaturalVideoWidth/Height == 0` 探测起点(≥3s 回退 mpv)。
    native_video_probe_at: Option<Instant>,
    /// 原生引擎持续 Opening/Buffering 无推进的起播/缓冲超时探测起点（≥12s 回退 mpv）。
    native_stall_probe_at: Option<Instant>,
    /// 会话级 CDN 节点健康跟踪表与按需启动的本地并发流式代理。
    cdn_health: Arc<CdnHealthTracker>,
    stream_proxy: Option<Arc<StreamProxy>>,
    proxy_session_id: Option<u64>,
    /// 媒体失败/引擎错误提示(播放页显示一次)。
    player_notice: Option<String>,
    /// 视频加载态:解析/缓冲中为 true(驱动 ProgressRing 环形指示);
    /// 首个 has_media、解析失败或关播熄灭。
    video_loading: bool,
    /// 视频引擎(SwapChainPanel 渲染 + mpv;惰性按需创建，关闭迷你条即释放全部解码/D3D 内存)。
    engine: Option<Arc<VideoEngine>>,
    /// v0.3:M7 系统媒体引擎实例(装载/控制;是否生效见 `native_enabled()`)。
    native_player: Option<Arc<NativePlayer>>,
    /// `QAQ_BX_NATIVE` 强制覆盖(Some = 覆盖设置项;None = 按设置项)。
    /// E11/E12/E13 自动化靠它把引擎钉死在一条通路上,不受 settings.json 影响。
    engine_env_override: Option<PlayerEngine>,
    /// S2 tier1 常驻播放层能力(**产品默认关**):`QAQ_BX_PARK=1` 才启用
    /// 「离开播放页即垫底」(后台播放)。2026-11-19 用户决策:后台播放暂不
    /// 面向用户(部分用户不喜欢),能力与 E17 场景保留,随时可翻默认。
    park_playback: bool,
    /// S4:本会话内 native 解码失败过的视频 codec 前缀(hevc/av01/…)——
    /// 系统扩展缺失时同桶不再重试 native,直接走 mpv;不持久化(装了扩展即恢复)。
    native_fallback_codecs: Vec<String>,
    /// v0.3:native 模式的弹幕透明层(mpv 路径不用——弹幕仍由视频引擎同帧叠画)。
    native_stage: Option<Arc<DanmakuStage>>,
    /// 系统媒体引擎是否正在驱动播放:控制分流(暂停/seek/音量/状态心跳)与弹幕
    /// 投递都以它为准。直播恒走 mpv(FLV,MF 不支持),即使残留 native_player
    /// 也必须把控制与弹幕送回视频引擎。
    native_playback: bool,
    /// 当前播放项的元信息(cid/aid/duration;弹幕拉取参数)。
    current_meta: Option<VideoMeta>,
    /// 弹幕预存链的优先段(播放头所在段;先拉它,其余段升序补齐)。
    danmaku_first_seg: u32,
    /// S3:弹幕投递代次(切引擎即换代;旧链在途分段到货时作废,避免双投)。
    danmaku_epoch: u64,
    /// 当前播放的全量 DASH 分轨(播放页画质菜单数据源;F2)。
    playback_dash: Option<Dash>,
    /// 播放页画质菜单(降序清晰度代号;空 = 未解析或 dash 缺失)。
    quality_qns: Vec<i64>,
    /// 当前生效清晰度代号(0 = 未加载)。
    current_qn: i64,
    /// U1 评论发布:草稿 + 行内提示(登录门控)。
    comment_draft: String,
    composer_notice: Option<String>,
    /// U3 多分P:分P菜单((cid, 标签);单 P 视频为空)。
    pages_menu: Vec<(u64, String)>,
    /// U3 当前生效分P cid。
    current_cid: u64,
    /// F4 探测:当前载入的视频/音频直链(回退重选对照)。
    current_video_url: String,
    current_audio_url: String,
    /// F4 探测起点(每次载入重置;媒体就绪持续 3s 仍缺解码轨 → 自动回退)。
    media_probe_at: Option<Instant>,
    /// F4 已确认失败的视频 codec 桶前缀(解码失败重选排除)。
    failed_video_prefixes: Vec<String>,
    /// F4 每代次只自动回退一次,防循环。
    video_fallback_done: bool,
    audio_fallback_done: bool,
    /// 播放状态节拍定时器：Drop 即取消投递，必须持有。
    timer: Option<ComponentTimer>,
    /// 登录轮询定时器（2s，独立于播放节拍）。
    login_timer: Option<ComponentTimer>,
    /// Windows 控制中心媒体控件（SMTC）控制器。
    smtc: Option<SmtcController>,
    /// v0.3:native 模式下 SMTC 由 MediaPlayer 自持(状态/指令走系统管线)。
    smtc_native: Option<NativeSmtc>,
    smtc_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<SmtcCommand>>>>,
    smtc_bvid: Option<String>,
    smtc_playing: Option<bool>,
    /// Xbox 手柄（XInput）连接与激活状态。
    gamepad_active: bool,
    gamepad_rx: Arc<Mutex<std::sync::mpsc::Receiver<GamepadEvent>>>,
    home_focus_idx: usize,
    /// 首页右侧半窗预览是否展开。
    home_half_pane: bool,
    /// 全屏播放状态、同步代次与全局键盘隧道上下文。
    is_fullscreen: bool,
    /// 全屏 HUD 专用实时窗口宽度（全屏期间冻结底层 `viewport_layout`，避免污染非全屏壳层与虚拟列表）。
    fullscreen_window_width: f64,
    /// 拖拽区重算结构指纹(路由 tag 长度/首字节、全屏、搜索展开、宽度桶):变化才补算。
    last_title_bar_sig: Cell<(u8, u8, bool, bool, u32)>,
    fullscreen_gen: u64,
    fullscreen_sync_in_flight: bool,
    key_route_ctx: Rc<Cell<(bool, bool)>>,
    global_key_handler: RoutedCallback<KeyEventInfo>,
    /// P1 拖拽期间挂起的网格重建(去抖 150ms,静止后一次)。
    pending_grid_rebuild: bool,
    /// 挂起期间是否发生过结构性变化(cols/卡态):settled 时仅结构变化才清
    /// 活动行/锚点(#4 后纯宽度收口复用同批行容器,游标依旧有效)。
    pending_grid_structural: bool,
    grid_rebuild_timer: Option<ComponentTimer>,
    /// 视口布局（壳层形态 / 内容密度 / 卡片度量单源）。
    viewport_layout: ViewportLayout,
    /// Mobile/Medium 下私信会话列表 ↔ 对话详情单页切换开关。
    whisper_detail_open: bool,
    /// Medium 窄桌面下的悬浮搜索栏开关。
    search_overlay_open: bool,
    /// 窄屏底栏“更多”悬浮菜单开关。
    more_menu_open: bool,
    /// MSIX / Store 打包运行时的 Windows App Notifications 激活注册句柄。
    _app_notifications: Option<crate::winrt_ext::AppNotificationGuard>,
    toast_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<()>>>>,
    /// 内置自驱动演示状态（仅 `--demo` / `QAQ_BX_DEMO=1` 时启用）。
    demo: Option<crate::demo::DemoState>,
    demo_timer: Option<ComponentTimer>,
}

/// S3 tier1:播放引擎设置变更的收口动作(纯判定,单测覆盖)。
/// 常驻播放层(park)落地后,切换当次生效:正在播的 VOD 定点换引擎续播。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineSwitchAction {
    RecordOnly,
    LiveKeepMpv,
    ReloadNative,
    ReloadMpv,
}

mod message;
mod util;
mod handlers;

// 跨域共享的状态类型与纯函数(P3 自 impl AppShell/自由函数迁移,调用点零改动):
use self::handlers::{
    apply_feed_page, CachedMedia, CommentsSessionState, missing_media_track,
    missing_native_video_track, MissingTrack, mount_pulse_active, mpv_load_gate_settled,
    mpv_should_show_loading, native_load_gate_settled, native_load_stalled,
    native_should_show_loading, pages_menu_from, ResizeStorm, ResizeStormTrack,
    spawn_feed_batch, spawn_popular_fetch, spawn_resolve_media,
};

pub use self::message::Message;
pub use self::util::*;


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
        out.unplayable_count = self.unplayable.len();
        out.now_playing_bvid = self.now_playing.as_ref().map(|item| item.bvid.clone());
        out.comments_aid = self.comments_session.aid;
        out.comments_sort_latest = self.comments_session.sort == CommentSort::Latest;
        out.hot_loaded = hot.loaded_once;
        out.hot_replies = hot.replies.len();
        out.hot_total = hot.total_count;
        out.latest_loaded = latest.loaded_once;
        out.latest_replies = latest.replies.len();
        out.search_query = self.search_state.query.clone();
        out.search_videos = self.search_state.videos.len();
        out.search_first_bvid = self
            .search_state
            .videos
            .first()
            .map(|item| item.bvid.clone());
        out.popular_items = self.popular.items.len();
        out.popular_first_bvid = self.popular.items.first().map(|item| item.bvid.clone());
        out.related_items = self.related.items.len();
        out.related_bvid = self.related.bvid.clone();
        out.popular_error = self.popular.error.clone();
        out.mount_tick = self.mount_tick;
        out.engine_attached = self.engine.as_ref().is_some_and(|e| e.attached());
        out.video_loading = self.video_loading;
        out.home_half_pane = self.home_half_pane;
        out.is_mobile = self.viewport_layout.is_mobile();
        out.grid_cols = self.home_state.grid_cols;
        // #7:预览卡状态 + 首页选点数据(selftest/E18 驱动用)。
        out.peek_open = self.peek.is_mounted();
        out.peek_bvid = self.peek_mounted_bvid();
        out.home_videos = self.home_state.video_count;
        out.home_first_bvid =
            self.home_slots.iter().find_map(|slot| slot.bvid().map(str::to_string));
        out.whisper_detail_open = self.whisper_detail_open;
        out.search_overlay_open = self.search_overlay_open;
        out.danmaku_enabled = self.settings.danmaku_enabled;
        out.volume = self.volume;
        out.is_fullscreen = self.is_fullscreen;
        out.quality_count = self.quality_qns.len();
        out.current_qn = self.current_qn;
        out.live_rooms = self.live.cards.len();
        out.live_first_roomid = self.live.cards.first().map_or(0, |c| c.room.roomid);
        out.user_history_count = self.user_history.cards.len();
        out.user_watch_later_count = self.watch_later.cards.len();
        out.user_fav_cards = self.favorites.resources.cards.len();
        out.user_fav_folders = self.favorites.folders.len();
        out.user_fav_resources_settled =
            self.favorites.resources.loaded_once || self.favorites.resources.error.is_some();
        out.lists_logged_in = self.lists_logged_in();
        out.player_engine = self.effective_player_engine().tag().to_string();
        out.native_playback = self.native_playback;
        out.portrait_mode = self.settings.portrait_mode;
        out.window_width = self.viewport_layout.window_width;
        out.prefer_official_cdn = self.settings.prefer_official_cdn;
        out.multi_thread_preload = self.settings.multi_thread_preload;
        out.disk_cache_bytes = self.auth_service.image_disk_cache().size_bytes();
        out.cache_notice = self.cache_notice.clone();
        out.popular_loading = self.popular.loading;
    }

    /// #7 根布局覆盖层:全局唯一 PeekCard,根 Grid 末子元素(z 序在液态玻璃宿主
    /// child visual 之后,v0.4 G4/G7 教训);Canvas 无背景,空白区不拦截命中。
    fn peek_overlay_layer(&self, context: &mut ViewContext<Self>) -> Option<KeyedView> {
        if !self.peek_allowed() {
            return None;
        }
        let (bvid, anchor) = self.peek.mounted_content()?;
        // 槽位现值优先(封面回填/驱逐都发生在 home_slots 的 Rc 上,home_index
        // 是加载时副本);越界时回落索引。
        let slot_item = self
            .home_slot_by_bvid
            .get(&bvid)
            .and_then(|idx| self.home_slots.get(*idx))
            .and_then(|slot| match slot {
                HomeSlot::Video { item, .. } => Some(Rc::clone(item)),
                HomeSlot::Skeleton { .. } => None,
            });
        // 预览是低频弹出路径:直接拷贝槽位现值(封面随驱逐/回填的最新状态),
        // 槽位越界时回落加载副本。
        let item = match slot_item
            .map(|item| (*item).clone())
            .or_else(|| self.home_index.get(&bvid).cloned())
        {
            Some(item) => item,
            None => return None,
        };
        let desc = self.peek_desc.get(&bvid).cloned().flatten();
        let notice = self.peek_notice.clone();
        let reduced = self.settings.reduce_animations;
        let scale = if reduced { 1.0 } else { peek::PEEK_POP_SCALE };
        let placement = peek::peek_placement(
            anchor,
            (
                self.viewport_layout.window_width,
                self.viewport_layout.window_height,
            ),
            (peek::PEEK_WIDTH, peek::PEEK_HEIGHT),
            scale,
            peek::PEEK_EDGE_MARGIN,
            peek::PEEK_GAP,
        );
        let render = peek::peek_render(!self.peek.hiding(), !self.peek_anim_pending, reduced);
        let on_enter = context.callback(|_: PointerEventInfo| Message::PeekPointerEntered);
        let on_exit = context.callback(|_: PointerEventInfo| Message::PeekPointerExited);
        let on_play = context.callback(Message::PeekPlay);
        let on_later = context.callback(Message::PeekWatchLater);
        let on_close = context.message(Message::PeekClose);
        let card = peek::peek_card_view(
            &item,
            desc.as_deref(),
            notice.as_deref(),
            placement,
            render,
            &bvid,
            on_enter,
            on_exit,
            &on_play,
            &on_later,
            &on_close,
        );
        Some(keyed(
            "peek-overlay",
            Canvas::new().children([card]),
        ))
    }

    /// 根层编排单源。全屏 / compact / desktop 三条返回路径统一从这里出根 View:
    /// 根 key 与覆盖层顺序集中在一处 —— keyed diff 按 key + kind 匹配,根 key
    /// 变化即整树销毁重建(全屏换源重载的根因)。后续要让全屏沿用播放层同键,
    /// 只需改这一个函数,不必再翻 570 行的 `view()`。
    fn shell_root_view(
        &self,
        root_key: &str,
        root_content: impl Into<View>,
        overlays: Vec<KeyedView>,
    ) -> View {
        let mut layers = Vec::with_capacity(1 + overlays.len());
        layers.push(keyed(root_key, root_content.into()));
        layers.extend(overlays);
        Border::new()
            .on_preview_key_down(self.global_key_handler.clone())
            .content(Grid::new().keyed_children(layers))
            .into()
    }

    /// 播放状态节拍(常驻续拍:now_playing 存在即持续;Drop 取消)。
    fn schedule_tick(&mut self, context: &ComponentContext<Self>) {
        if self.timer.is_none() {
            self.timer = Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
        }
    }

    fn sync_key_route_ctx(&self) {
        self.key_route_ctx
            .set((self.is_player_visible(), self.is_fullscreen));
    }

    /// S2 常驻播放层:播放页视图构造单源(park 模式下路由离开也要渲染同一层,
    /// 组件与 MPE 才不会过渡卸载)。
    fn player_page_content(
        &self,
        author_avatar: &Option<EncodedImage>,
        my_avatar: &Option<EncodedImage>,
        comment_cb: &CommentCallbacks,
        on_open: &Callback<String>,
        layout: ViewportLayout,
        fullscreen: bool,
        pane: Option<(f64, f64, f64)>,
        context: &mut ViewContext<Self>,
    ) -> Option<View> {
            Some(            match (self.now_playing.as_ref(), self.engine.as_ref()) {
                (Some(item), Some(engine)) => {
                    View::component::<pages::player::PlayerPage>(pages::player::PlayerInput {
                        item: item.clone(),
                        author_avatar: author_avatar.clone(),
                        my_avatar: my_avatar.clone(),
                        desc: self.now_playing_desc.clone(),
                        playing: self.playing,
                        position: self.position,
                        volume: self.volume,
                        danmaku_enabled: self.settings.danmaku_enabled,
                        quality_menu: self.quality_menu_view(),
                        current_qn: self.current_qn,
                        pages_menu: self.pages_menu_view(),
                        current_cid: self.current_cid,
                        layout,
                        mount_tick: self.mount_tick,
                        duration_secs: self.display_duration(item),
                        notice: self.player_notice.clone(),
                        loading: self.video_loading,
                        engine: engine.clone(),
                        native: self.native_sink().cloned(),
                        stage: self.stage_sink().cloned(),
                        comments: self.comments_snapshot.clone(),
                        comment_cb: comment_cb.clone(),
                        related: self.related.clone(),
                        on_open_video: on_open.clone(),
                        on_related_retry: context.message(Message::RelatedRetry),
                        on_toggle: context.message(Message::TogglePlay),
                        on_seek: context.callback(|value: f64| Message::Seek(value)),
                        on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                        on_volume_wheel: context.callback(|delta: i32| {
                            Message::PlayerHotkey(pages::fullscreen::PlayerHotkey::VolumeDelta(
                                delta,
                            ))
                        }),
                        on_toggle_mute: context.message(Message::ToggleMute),
                        on_toggle_danmaku: context.message(Message::ToggleDanmaku),
                        on_select_quality: context.callback(Message::SelectQuality),
                        on_select_page: context.callback(Message::SelectPage),
                        on_enter_fullscreen: context.message(Message::EnterFullscreen),
                        demo_override: self.demo.as_ref().and_then(|d| d.player_override),
                        fullscreen,
                        pane_mode: pane.is_some(),
                        pane_width: pane.map(|p| p.0).unwrap_or(0.0),
                        pane_max_width: pane.map(|p| p.1).unwrap_or(0.0),
                        pane_max_height: pane.map(|p| p.2).unwrap_or(0.0),
                        on_pane_expand: context.message(Message::ExpandHalfPane),
                        on_pane_close: context.message(Message::CloseHalfPane),
                        on_pane_resize: context.callback(Message::ResizeHalfPane),
                    })
                }
                _ => self.home_view(context),
            })
    }

    fn half_pane_layer(
        &self,
        author_avatar: Option<EncodedImage>,
        my_avatar: Option<EncodedImage>,
        comment_cb: CommentCallbacks,
        on_open: &Callback<String>,
        context: &mut ViewContext<Self>,
    ) -> Option<KeyedView> {
        let (Some(item), Some(engine)) = (self.now_playing.as_ref(), self.engine.as_ref()) else {
            return None;
        };
        let max_width =
            (self.viewport_layout.content_width() - 48.0).max(pages::player::HALF_PANE_MIN_WIDTH);
        // 与主窗/全屏共用同一 key 与同一组件:surface 的 keyed 路径完全一致,
        // 半窗↔主窗只是 input 属性切换,native MPE 不销毁、不换源重载。
        let pane_geom = (
            self.settings.half_pane_width,
            max_width,
            self.viewport_layout.content_height(),
        );
        let content = self.player_page_content(
            &author_avatar,
            &my_avatar,
            &comment_cb,
            on_open,
            self.viewport_layout,
            false,
            Some(pane_geom),
            context,
        )?;
        Some(keyed(
            format!("player:{}", item.bvid),
            Border::new()
                .width(pane_geom.0)
                .horizontal_alignment(windows_reactor::HorizontalAlignment::Right)
                .vertical_alignment(windows_reactor::VerticalAlignment::Stretch)
                .background(ThemeBrush::SolidBackground)
                .content(content),
        ))
    }

    /// 首页视图（Home 路由与播放页无播放项时的兜底共用）。
    fn home_view(&self, context: &ViewContext<Self>) -> View {
        pages::home::home_page(
            &self.home_source,
            &self.home_state,
            self.viewport_layout,
            &context.message(Message::HomeRefresh),
            &context.message(Message::HomeRetry),
        )
    }

}

impl Component for AppShell {
    type Input = ShellInput;
    type Message = Message;

    /// 最新值语义的消息声明(框架侧 latest-wins 合并,perf-architecture-review
    /// §R1/F1):拖框风暴里入队未消费的中间尺寸没有意义,合并后每次 drain
    /// 只处理一条,整页 `view()` 重跑从每 DIP 一次降到每帧至多一次。
    fn coalesce_key(message: &Self::Message) -> Option<u64> {
        match message {
            Message::WindowResized(_) => Some(1),
            Message::FullscreenProbeResult { .. } => Some(2),
            // #5:无载荷重绘请求,队尾合并——一帧内多次结构翻转只需一次重绘。
            Message::ShellInvalidateLayout => Some(3),
            _ => None,
        }
    }

    fn create(input: &ShellInput, context: &ComponentContext<Self>) -> Self {
        crate::winrt_ext::apply_global_font(crate::theme::DEFAULT_FONT_FAMILY);
        let auth_service = Arc::new(Auth::new());
        let nav = auth_service.nav_client();
        let settings_store = Arc::new(FileSettingsStore::new(
            auth_service.data_dir().join(SETTINGS_FILE),
        ));
        let (settings, settings_notice) = settings_store.load_or_default();

        // WebP 解码能力探测(进程内一次):无解码器时封面/头像回退 JPG,
        // 并提示一次安装「Web 图像扩展」(webp_hint_shown 落盘防重复弹窗)。
        let webp_hint_pending = !crate::codec::webp_supported() && !settings.webp_hint_shown;
        let settings = if webp_hint_pending {
            let mut settings = settings;
            settings.webp_hint_shown = true;
            if let Err(err) = settings_store.save(&settings) {
                eprintln!("webp_hint_shown 落盘失败:{err:?}");
            }
            #[cfg(not(feature = "selftest"))]
            crate::codec::show_webp_hint_dialog();
            settings
        } else {
            settings
        };

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
        let search_client = Arc::new(SearchClient::with_nav(
            auth_service.http(),
            auth_service.credential_provider(),
            nav.clone(),
        ));
        let live_client = Arc::new(LiveClient::new(
            auth_service.http(),
            auth_service.credential_provider(),
        ));
        let history_client = Arc::new(HistoryClient::new(
            auth_service.http(),
            auth_service.credential_provider(),
        ));
        let fav_client = Arc::new(FavClient::new(
            auth_service.http(),
            auth_service.credential_provider(),
        ));

        // 启动即探测登录态（含自动 Cookie 续期 + `bili_ticket` 预热；后台单飞不重跑）。
        context.spawn_background({
            let auth = auth_service.clone();
            move |_| Message::AuthProbed {
                seq: auth_gen,
                report: auth.probe_blocking(),
            }
        });

        // U2:搜索默认词后台拉取(失败静默,占位文案兜底)。
        context.spawn_background({
            let search_client = search_client.clone();
            move |_| {
                let word = block_on(search_client.default_word()).ok();
                Message::SearchDefaultReady(word)
            }
        });

        // 所有需要 nav 的客户端共用同一个 NavClient：用户资料 + WBI 密钥 + bili_ticket 一份缓存。
        let feed_client = Arc::new(FeedClient::with_nav(
            auth_service.http(),
            auth_service.credential_provider(),
            nav.clone(),
        ));
        let feed_session = Arc::new(Mutex::new(FeedSession::new()));
        let initial_layout = ViewportLayout::initial(settings.portrait_mode);
        // 视口自适应首批逻辑页（默认 1280x800 → 48 条，竖屏 859x800 → 24 条）：先分配完整骨架槽位框架再拉数据。
        let initial_target = feed_pager::logical_page_target_for(
            initial_layout.cards,
            initial_layout.content_height(),
        );
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

        // 冒烟辅助：QAQ_BX_SMOKE_LOGIN=1 自动发起扫码流程（generate 只读，不登录）。
        let flow = Arc::new(Mutex::new(None));
        if std::env::var("QAQ_BX_SMOKE_LOGIN").as_deref() == Ok("1") {
            let auth = auth_service.clone();
            let cell = flow.clone();
            context.spawn_background(move |_| match auth.start_login_blocking(&cell) {
                Ok(qr) => Message::LoginQrReady(qr),
                Err(reason) => Message::LoginStartFailed(reason),
            });
        }

        // 冒烟辅助：QAQ_BX_ROUTE=<tag> 指定初始页面。
        let route = std::env::var("QAQ_BX_ROUTE")
            .ok()
            .and_then(|ref tag| Route::from_tag(tag));
        // 冒烟辅助：QAQ_BX_VIDEO=<bvid> 启动即进播放页(与点卡同管线;真实元数据由解析管线回填)。
        let now_playing = std::env::var("QAQ_BX_VIDEO")
            .ok()
            .filter(|bvid| bvid.starts_with("BV"))
            .map(|ref bvid| minimal_video_item(bvid, ""));
        let route = if crate::notifications::launched_from_notification() {
            Route::Login
        } else {
            route
                .or((now_playing.is_some()).then_some(Route::Player))
                .unwrap_or(Route::Home)
        };

        // 冒烟播放与点卡同构:仅在 QAQ_BX_VIDEO 指定时才提前创建 VideoEngine；日常启动 0 播放内存占用。
        let mut timer = None;
        let mut engine = None;
        let mut video_loading = false;
        if let Some(item) = &now_playing {
            engine = Some(VideoEngine::with_config(video_config_from(&settings)));
            spawn_resolve_media(
                &auth_service,
                &settings,
                item.clone(),
                playback_gen,
                context,
            );
            timer = Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
            video_loading = true;
        }
        let demo = crate::demo::DemoConfig::from_env_and_args().map(crate::demo::DemoState::new);
        let demo_timer = if demo.is_some() {
            Some(context.set_local_timeout(crate::demo::DEMO_TICK_INTERVAL, || Message::DemoTick))
        } else {
            None
        };

        let volume = settings.volume;
        let home_on_open = context.callback(Message::OpenVideo);
        let home_on_hover = context.callback(Message::PreResolveVideo);
        let home_on_hover_report = context.callback(Message::CardHover);
        let home_on_row_active =
            context.callback(|(rev, row_idx, active)| Message::HomeRowActive {
                rev,
                row_idx,
                active,
            });
        let initial_cols = initial_layout.cards.cols;
        let initial_rev = video_card::home_row_revision(feed_gen, initial_layout.cards);
        let home_source = video_card::home_virtual_source(
            &home_slots,
            initial_layout.cards,
            initial_rev,
            None,
            &home_on_open,
            &home_on_hover,
            &home_on_hover_report,
            &home_on_row_active,
            &HashMap::new(),
        );

        // 热门页直连（QAQ_BX_ROUTE=popular 等）：真实 popular 首页随启动开拉。
        let image_cache = Arc::new(ImageCache::new());
        if route == Route::Popular {
            spawn_popular_fetch(&feed_client, &image_cache, 1, context);
        }

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

        let key_route_ctx = Rc::new(Cell::new((route == Route::Player, false)));
        let global_key_handler = {
            let key_ctx = Rc::clone(&key_route_ctx);
            let key_sender = context.sender();
            RoutedCallback::new(move |ev: KeyEventInfo| {
                let (player_visible, is_fullscreen) = key_ctx.get();
                let text_focused = crate::winrt_ext::is_text_input_focused();
                if let Some(action) = pages::fullscreen::resolve_player_hotkey(
                    &ev,
                    player_visible,
                    is_fullscreen,
                    text_focused,
                ) {
                    let _ = key_sender.send(Message::PlayerHotkey(action));
                    true
                } else {
                    false
                }
            })
        };

        Self {
            auth_service,
            settings_store,
            settings_notice,
            settings_save_timer: None,
            cache_notice: None,
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
            home_on_hover_report,
            home_on_row_active,
            home_active_rows: BTreeSet::new(),
            home_slot_by_bvid: HashMap::new(),
            home_index: HashMap::new(),
            home_next_slot_seq: initial_target,
            home_pending_skeletons: initial_target,
            home_state: pages::home::HomeState {
                stream_gen: feed_gen,
                grid_cols: initial_cols,
                loading: true,
                ..Default::default()
            },
            home_last_kind: FeedKind::First,
            home_last_target: initial_target,
            home_refresh_pending: false,
            home_anchor_row: 0,
            home_last_evict_row: 0,
            home_viewport_w: initial_layout.content_width(),
            home_viewport_h: initial_layout.content_height(),
            pending_covers: HashMap::new(),
            cover_inflight: HashSet::new(),
            cover_flush_timer: None,
            preresolved: HashMap::new(),
            preresolved_order: VecDeque::new(),
            preresolve_inflight: HashMap::new(),
            pending_open_preresolve: None,
            peek: peek::PeekMachine::new(),
            peek_anim_pending: false,
            peek_prefetch_timer: None,
            peek_pop_timer: None,
            peek_grace_timer: None,
            peek_anim_timer: None,
            peek_desc: HashMap::new(),
            peek_desc_inflight: HashSet::new(),
            peek_notice: None,
            peek_dismissal: PeekDismissal::default(),
            unplayable: HashMap::new(),
            image_cache,
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
            search_client,
            search_state: pages::search::SearchUiState::default(),
            search_index: HashMap::new(),
            search_seq: 0,
            search_default: None,
            live: LivePageState {
                // pn 从 1 起(与 popular 同语义)。
                next_page: 1,
                ..LivePageState::default()
            },
            live_client,
            live_cover_inflight: std::collections::HashSet::new(),
            user_history: UserListState::default(),
            watch_later: UserListState::default(),
            favorites: FavPageState::default(),
            history_client,
            fav_client,
            user_cover_inflight: std::collections::HashSet::new(),
            user_index: HashMap::new(),
            popular: pages::popular::PopularFeedState {
                // pn 从 1 起（实测 pn=0 非法）；直启热门路由时首页已在飞。
                next_pn: 1,
                loading: route == Route::Popular,
                ..pages::popular::PopularFeedState::default()
            },
            popular_index: HashMap::new(),
            related: pages::player::RelatedPane::default(),
            related_index: HashMap::new(),
            comments_session: CommentsSessionState::default(),
            probe: input.probe.clone(),
            comments_snapshot: CommentsSnapshot::default(),
            comments_rev: 0,
            playback_avatar_dirty: false,
            playback_avatar_timer: None,
            resize_storm: ResizeStormTrack::default(),
            title_bar_ref: ElementRef::new(),
            title_bar_tier: 0,
            portrait_docked: false,
            portrait_dock_restore: None,
            maximizable_pending: if settings.portrait_mode { Some(false) } else { None },
            playing: false,
            position: 0,
            mount_tick: 0,
            volume,
            volume_before_mute: if volume == 0 { 80 } else { volume },
            settings,
            engine_duration: None,
            media_load_pending: false,
            native_load_epoch: None,
            mpv_load_epoch: None,
            native_video_probe_at: None,
            native_stall_probe_at: None,
            cdn_health: Arc::new(CdnHealthTracker::new()),
            stream_proxy: None,
            proxy_session_id: None,
            player_notice: None,
            video_loading,
            engine,
            native_player: None,
            engine_env_override: PlayerEngine::env_override(
                std::env::var("QAQ_BX_NATIVE").ok().as_deref(),
            ),
            park_playback: std::env::var("QAQ_BX_PARK").as_deref() == Ok("1"),
            native_fallback_codecs: Vec::new(),
            native_stage: None,
            native_playback: false,
            current_meta: None,
            danmaku_first_seg: 0,
            danmaku_epoch: 0,
            playback_dash: None,
            quality_qns: Vec::new(),
            current_qn: 0,
            current_video_url: String::new(),
            current_audio_url: String::new(),
            media_probe_at: None,
            failed_video_prefixes: Vec::new(),
            video_fallback_done: false,
            audio_fallback_done: false,
            comment_draft: String::new(),
            composer_notice: None,
            pages_menu: Vec::new(),
            current_cid: 0,
            timer,
            login_timer: None,
            smtc: None,
            smtc_native: None,
            smtc_rx: None,
            smtc_bvid: None,
            smtc_playing: None,
            gamepad_active: false,
            gamepad_rx,
            home_focus_idx: 0,
            home_half_pane: false,
            is_fullscreen: false,
            fullscreen_window_width: initial_layout.window_width,
            last_title_bar_sig: Cell::new((0, 0, false, false, 0)),
            fullscreen_gen: 0,
            fullscreen_sync_in_flight: false,
            key_route_ctx,
            global_key_handler,
            pending_grid_rebuild: false,
            pending_grid_structural: false,
            grid_rebuild_timer: None,
            viewport_layout: initial_layout,
            whisper_detail_open: false,
            search_overlay_open: false,
            more_menu_open: false,
            _app_notifications: app_notifications,
            toast_rx,
            demo,
            demo_timer,
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        self.refresh_probe();
        #[cfg(feature = "probe")]
        crate::probe::pump();
        // 竖屏模式:窗口就绪后补上 presenter 级最大化没收(run_window 失败则
        // 留待下一条消息重试;启动期由首条消息驱动)。
        self.sync_portrait_maximizable(context);
        // ── 域分发(P3 拆解,计划见 docs/dev-notes/shell-decomposition-plan.md §3)──
        // handlers/mod.rs 的路由 match 穷尽覆盖 Message 全部变体:新增变体未路由时
        // 在该 match 编译失败,强制补路由(替代原 giant match 的编译期穷尽性);
        // 各域 handle() 内 `_ => {}` 仅兜路由表与域臂不同步的极端情况。
        // 尾后语句(reconcile/peek 兜底/refresh_probe)已并入 dispatch:
        // 臂内顶层 return 跳过尾巴的原语义由 handle 返回值保持。
        handlers::dispatch(self, message, context);
    }

    fn view(&self, _input: &ShellInput, context: &mut ViewContext<Self>) -> View {
        context.window_title(crate::notifications::APP_DISPLAY_NAME);
        let portrait = self.settings.portrait_mode;
        let (client_w, client_h) = if portrait {
            (PORTRAIT_MAX_WIDTH_DIPS, layout::DEFAULT_WINDOW_HEIGHT)
        } else {
            (layout::DEFAULT_WINDOW_WIDTH, layout::DEFAULT_WINDOW_HEIGHT)
        };
        let mut visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .client_size(client_w, client_h)
            // 竖屏模式:宽度钳制在底栏边界带 [390, 860) DIP——presenter 级
            // 硬钳制,拖到上限物理拖不动;高度不设限。
            .constraints(if portrait {
                portrait_window_constraints()
            } else {
                WindowConstraints {
                    min_width: Some(layout::MIN_WINDOW_WIDTH),
                    min_height: Some(layout::MIN_WINDOW_HEIGHT),
                    max_width: None,
                    max_height: None,
                }
            });
        // 竖屏模式下不恢复上次的(可能超带)窗口位置——初始几何交给
        // client_size 声明,贴边态由用户主动点钮进入。
        if !portrait
            && self.demo.is_none()
            && let Some(bounds) = self.settings.window
        {
            let bounds = clamp_window_bounds(bounds);
            visuals = visuals.initial_placement(WindowPlacement {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
                maximized: bounds.maximized,
            });
        }
        context.window_visuals(visuals);
        // #5:尺寸泵改走 silent dispatch——逐帧 resize 只更新静默状态,不再
        // 无条件重跑根 view(7.9k 行整树 IR 重建是拖框卡顿的另一半);结构
        // 翻转由 handler 显式发 ShellInvalidateLayout,静止由 settled 收敛。
        context.on_window_size({
            let pump = context.sender();
            move |size| {
                let _ = pump.send_silent(Message::WindowResized(size));
            }
        });
        self.sync_key_route_ctx();

        let layout = self.viewport_layout;
        let is_mobile = layout.is_mobile();
        // 全屏不再换根树:播放页组件留在 shell-root 原来的 keyed 位置,MPE 不销毁。
        // 这里只构造悬浮 HUD 覆盖层(不挂渲染面);渲染面由同一个 PlayerPage 持有。
        let fullscreen = self.is_fullscreen && self.now_playing.is_some() && self.engine.is_some();
        let fs_hud_layer = if fullscreen {
            if let (Some(item), Some(engine)) =
                (self.now_playing.as_ref(), self.engine.as_ref())
            {
                Some(keyed(
                    "fs-hud",
                    View::component::<pages::fullscreen::FullscreenPlayer>(
                        pages::fullscreen::FullscreenInput {
                            item: item.clone(),
                            playing: self.playing,
                            position: self.position,
                            duration_secs: self.display_duration(item),
                            volume: self.volume,
                            danmaku_enabled: self.settings.danmaku_enabled,
                            quality_menu: self.quality_menu_view(),
                            current_qn: self.current_qn,
                            mount_tick: self.mount_tick,
                            notice: self.player_notice.clone(),
                            loading: self.video_loading,
                            window_width: self.fullscreen_window_width,
                            engine: engine.clone(),
                            native: self.native_sink().cloned(),
                            stage: self.stage_sink().cloned(),
                            on_toggle: context.message(Message::TogglePlay),
                            on_seek: context.callback(|value: f64| Message::Seek(value)),
                            on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                            on_volume_wheel: context.callback(|delta: i32| {
                                Message::PlayerHotkey(
                                    pages::fullscreen::PlayerHotkey::VolumeDelta(delta),
                                )
                            }),
                            on_toggle_mute: context.message(Message::ToggleMute),
                            on_toggle_danmaku: context.message(Message::ToggleDanmaku),
                            on_select_quality: context.callback(Message::SelectQuality),
                            on_exit_fullscreen: context.message(Message::ExitFullscreen),
                            hud_only: true,
                        },
                    ),
                ))
            } else {
                None
            }
        } else {
            None
        };
        let on_open = context.callback(Message::OpenVideo);
        // U5:登录门控页「去登录」按钮(菜单导航路径,含 ensure_account_loaded)。
        let go_login = context.message(Message::NavigateCompact(Route::Login));
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
            on_draft: context.callback(|text: Rc<str>| Message::CommentDraftChanged(text.to_string())),
            on_submit: context.message(Message::SubmitComment),
            composer_notice: self.composer_notice.clone(),
            can_post: matches!(self.auth, AuthState::LoggedIn(_)),
            composer_draft: self.comment_draft.clone(),
        };
        let author_avatar = self
            .now_playing
            .as_ref()
            .and_then(|item| item.author_face_url.as_deref())
            .map(avatar_thumbnail_url)
            .and_then(|url| self.avatar_cache.get(&url));

        let content: View = if fullscreen || self.route == Route::Player {
            match self.player_page_content(
                &author_avatar,
                &my_avatar,
                &comment_cb,
                &on_open,
                layout,
                fullscreen,
                None,
                context,
            ) {
                Some(view) => view,
                None => self.home_view(context),
            }
        } else if self.route == Route::Login {
            let (account_view, account_cb) = match &self.auth {
                AuthState::LoggedIn(user) => (
                    Some(AccountViewData {
                        layout,
                        whisper_detail_open: self.whisper_detail_open,
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
                        on_close_whisper_detail: context.message(Message::CloseWhisperDetail),
                        on_refresh_thread: context.callback(Message::RefreshWhisperThread),
                        on_open_video: context.callback(Message::OpenVideoTarget),
                        on_open_history: context.message(Message::NavigateCompact(Route::History)),
                        on_open_watch_later: context
                            .message(Message::NavigateCompact(Route::WatchLater)),
                        on_open_favorites: context
                            .message(Message::NavigateCompact(Route::Favorites)),
                    }),
                ),
                _ => (None, None),
            };
            pages::login::login_page(
                &self.auth,
                &self.login_flow,
                layout,
                account_view,
                account_cb,
                context.message(Message::AuthStartLogin),
                context.message(Message::AuthCancelLogin),
            )
        } else if self.route == Route::Settings {
            let on_change = context.callback(Message::UpdateSettings);
            let storage = pages::settings::StorageCardState {
                cache_bytes: self.auth_service.image_disk_cache().size_bytes(),
                notice: self.cache_notice.clone(),
            };
            pages::settings::settings_page(
                &self.settings,
                is_vip,
                layout,
                self.settings_notice.as_deref(),
                &on_change,
                &storage,
                &context.message(Message::ClearCaches),
            )
        } else if self.route == Route::Search {
            let on_draft =
                context.callback(|text: Rc<str>| Message::SearchDraftChanged(text.to_string()));
            let on_submit = context.message(Message::SearchSubmitted);
            let on_pick_default = context.message(Message::SearchPickDefault);
            pages::search::search_page(
                &self.search_state,
                &on_draft,
                &on_submit,
                &on_pick_default,
                &on_open,
                layout,
            )
        } else if self.route == Route::Home {
            self.home_view(context)
        } else if self.route == Route::Live {
            let on_open_room = context.callback(|(roomid, title, uname): (u64, String, String)| {
                Message::LiveRoomSelected(roomid, title, uname)
            });
            pages::live::live_page(
                &self.live,
                layout,
                &on_open_room,
                &context.message(Message::LiveMore),
                &context.message(Message::ListAutoMore(crate::paging::AutoPage::Live)),
            )
        } else if self.route == Route::Popular {
            pages::popular::popular_page(
                &self.popular,
                layout,
                &on_open,
                &context.message(Message::PopularMore),
                &context.message(Message::ListAutoMore(crate::paging::AutoPage::Popular)),
            )
        } else if self.route == Route::History {
            // U5:登录门控(未登录不发请求,与 U1 评论发布同款纪律)。
            if !self.lists_logged_in() {
                pages::user_lists::login_gate_page(
                    "历史",
                    "登录后查看观看历史",
                    layout,
                    &go_login,
                )
            } else {
                pages::user_lists::history_page(
                    &self.user_history,
                    layout,
                    &on_open,
                    &context.message(Message::HistoryMore),
                    &context.message(Message::ListAutoMore(crate::paging::AutoPage::History)),
                )
            }
        } else if self.route == Route::WatchLater {
            if !self.lists_logged_in() {
                pages::user_lists::login_gate_page(
                    "稍后再看",
                    "登录后查看稍后再看列表",
                    layout,
                    &go_login,
                )
            } else {
                pages::user_lists::watch_later_page(
                    &self.watch_later,
                    layout,
                    &on_open,
                    &context.message(Message::WatchLaterMore),
                    &context.message(Message::ListAutoMore(crate::paging::AutoPage::WatchLater)),
                )
            }
        } else if self.route == Route::Favorites {
            if !self.lists_logged_in() {
                pages::user_lists::login_gate_page(
                    "收藏",
                    "登录后查看收藏夹",
                    layout,
                    &go_login,
                )
            } else {
                let on_select_folder = context.callback(|folder: u64| Message::FavSelectFolder(folder));
                pages::user_lists::favorites_page(
                    &self.favorites,
                    layout,
                    &on_select_folder,
                    &on_open,
                    &context.message(Message::FavMore),
                    &context.message(Message::ListAutoMore(crate::paging::AutoPage::Favorites)),
                )
            }
        } else {
            router::route(self.route, layout, on_open.clone())
        };

        // 切换目的地时退役原生控件（含播放页换视频的场景）。
        let key = if fullscreen || self.route == Route::Player {
            format!(
                "player:{}",
                self.now_playing
                    .as_ref()
                    .map_or(String::new(), |item| item.bvid.clone())
            )
        } else {
            self.route.tag().to_string()
        };

        // S2 常驻播放层(park):键稳定 ⇒ 组件与 MPE 不过渡卸载(后台播放)。
        // 垫在活动页之下:画面被遮挡但解码/计时/音频继续,回播放页即原样续上。
        let park_layer = (self.park_playback
            && self.now_playing.is_some()
            && self.engine.is_some()
            && !self.is_player_visible())
        .then(|| {
            let park_key = format!(
                "player:{}",
                self.now_playing
                    .as_ref()
                    .map_or(String::new(), |item| item.bvid.clone())
            );
            let parked = self
                .player_page_content(&author_avatar, &my_avatar, &comment_cb, &on_open, layout, false, None, context)
                .unwrap_or_else(|| self.home_view(context));
            keyed(
                park_key,
                Border::new()
                    .background(ThemeBrush::SolidBackground)
                    .content(parked),
            )
        });

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

        let can_back = can_navigate_back(
            layout,
            self.route == Route::Login,
            self.history.len(),
            self.whisper_detail_open,
        );
        let title_bar = chrome::build_title_bar(
            layout,
            &self.title_bar_ref,
            self.settings.portrait_mode,
            context.message(Message::TogglePortraitDock),
            APP_ICON_48_PNG,
            crate::notifications::APP_DISPLAY_NAME,
            route_subtitle,
            can_back,
            matches!(self.auth, AuthState::LoggedIn(_)),
            self.unread_summary.total(),
            &self.search_state.draft,
            self.search_default.as_deref(),
            context.message(Message::Back),
            context.message(Message::TogglePane),
            context.callback(|text: Rc<str>| Message::SearchDraftChanged(text.to_string())),
            context.message(Message::SearchSubmitted),
            context.message(Message::ToggleSearchOverlay),
            context.message(Message::OpenNotifications),
        );
        // B 模式重算口:结构指纹变化(路由/全屏/搜索展开/宽度桶)时按需补算一次
        // 拖拽区;imperative 队列跑在本轮 reconcile 之后,元素必已发布。
        let tag = self.route.tag();
        let sig = (
            tag.len() as u8,
            tag.as_bytes().first().copied().unwrap_or(0),
            self.is_fullscreen,
            self.search_overlay_open,
            (layout.window_width / 64.0).max(0.0) as u32,
        );
        if self.last_title_bar_sig.replace(sig) != sig {
            let _ = self.title_bar_ref.recompute_drag_regions();
        }

        if is_mobile {
            // iOS 26 悬浮底栏:内容跨行铺到 row1+row2,底栏胶囊浮在内容
            // 之上——composition backdrop 采样到的才是滚动内容(玻璃有物可折)。
            // 不透明底色必须有:搜索/我的等页面根容器透明,胶囊带区域若无
            // 不透明重绘,backdrop 会残留上一页(推荐/热门)的旧帧而非实时
            // 刷新——这层底色保证玻璃后面每帧都有当前页面的像素可采。
            let content_host = Grid::new()
                .grid_row(1)
                .grid_row_span(2)
                .background(ThemeBrush::SolidBackground)
                .keyed_children({
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
                } else {
                    vec![keyed(key, content)]
                };
                if let Some(park) = park_layer.clone() {
                    layers.insert(0, park);
                }
                if self.gamepad_active {
                    let focus_title = self.focused_home_video().map(|v| v.title.as_str());
                    layers.push(keyed(
                        "gamepad-hint",
                        gamepad::gamepad_hint_bar(self.route, focus_title, true),
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
                        chrome::compact_more_popup(self.route, |r| {
                            context.message(Message::NavigateCompact(r))
                        }),
                    ));
                }
                if self.search_overlay_open {
                    layers.extend(chrome::floating_search_layers(
                        &self.search_state.draft,
                        context.callback(|text: Rc<str>| {
                            Message::SearchDraftChanged(text.to_string())
                        }),
                        context.message(Message::SearchSubmitted),
                        context.message(Message::CloseSearchOverlay),
                    ));
                }
                layers
            });

            let bottom_bar = chrome::compact_bottom_bar(
                self.route,
                self.more_menu_open,
                |r| context.message(Message::NavigateCompact(r)),
                context.message(Message::ToggleMoreMenu),
            );

            let shell_root = Grid::new()
                .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                .children((
                    title_bar,
                    content_host,
                    Grid::new().grid_row(2).children((bottom_bar,)),
                ));

            let overlays: Vec<KeyedView> = fs_hud_layer.into_iter().collect();
            return self.shell_root_view("shell-root", shell_root, overlays);
        }

        let navigation = NavigationView::new()
            .pane_display_mode(if fullscreen {
                NavigationViewPaneDisplayMode::LeftMinimal
            } else {
                NavigationViewPaneDisplayMode::LeftCompact
            })
            .is_pane_toggle_button_visible(false)
            .is_back_button_visible(NavigationViewBackButtonVisible::Collapsed)
            .is_settings_visible(false)
            .always_show_header(false)
            .pane_title("QAQ-Bx")
            .is_pane_open(if fullscreen { false } else { self.pane_open })
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
                    } else if self.route == Route::Home && self.home_half_pane && !fullscreen {
                    let mut list = vec![keyed(key, content)];
                    if let Some(layer) = self.half_pane_layer(
                        author_avatar.clone(),
                        my_avatar.clone(),
                        comment_cb.clone(),
                        &on_open,
                        context,
                    ) {
                        list.push(layer);
                    }
                    list
                } else {
                    vec![keyed(key, content)]
                };
                if let Some(park) = park_layer.clone() {
                    layers.insert(0, park);
                }
                if self.gamepad_active {
                    let focus_title = self.focused_home_video().map(|v| v.title.as_str());
                    layers.push(keyed(
                        "gamepad-hint",
                        gamepad::gamepad_hint_bar(self.route, focus_title, false),
                    ));
                }
                if self.search_overlay_open {
                    layers.extend(chrome::floating_search_layers(
                        &self.search_state.draft,
                        context.callback(|text: Rc<str>| {
                            Message::SearchDraftChanged(text.to_string())
                        }),
                        context.message(Message::SearchSubmitted),
                        context.message(Message::CloseSearchOverlay),
                    ));
                }
                layers
            }));

        let shell_root = Grid::new()
            .rows([if fullscreen {
                GridLength::Pixel(0.0)
            } else {
                GridLength::Auto
            }, GridLength::Star(1.0)])
            .children((title_bar, navigation));

        // #7:预览卡覆盖层挂在 shell-root 之后(整窗最顶层,玻璃宿主之下无残留)。
        let mut overlays: Vec<KeyedView> = self.peek_overlay_layer(context).into_iter().collect();
        overlays.extend(fs_hud_layer);
        self.shell_root_view("shell-root", shell_root, overlays)
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

#[cfg(test)]
mod tests;

