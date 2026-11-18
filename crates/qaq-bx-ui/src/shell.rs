//! 应用壳：TitleBar + NavigationView + 路由 + 迷你播放条 + 登录状态机
//! （`docs/ui-design.md` §1/§5；登录桥接见 `crate::auth`）。
//! 播放与登录状态在此单源持有，页面组件纯展示 + 回调上报。

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use qaq_bx_core::account::{AccountStat, NavClient, RelationKind, RelationPage, RelationUser};
use qaq_bx_core::danmaku::{DanmakuClient, DanmakuElem, SEGMENT_MS};
use qaq_bx_core::feed::{
    ArchiveCard, FeedCard, FeedClient, FeedKind, FeedSession, fetch_rcmd_batch,
};
use qaq_bx_core::live::LiveClient;
use crate::pages::live::{LiveCard, LivePageState};
use qaq_bx_core::notify::{
    AtNotice, FeedCursor, LikeNotice, ReplyNotice, SystemNotice, UnreadSummary, WhisperMessage,
    WhisperSession,
};
use qaq_bx_core::player::{
    Dash, PlayerClient, ResolvedMedia, VideoMeta, VideoPage, is_trial_downgrade_reason,
    quality_label, quality_menu, select_audio_excluding, select_stream_by_qn,
    select_stream_excluding,
};
use qaq_bx_core::reply::{ReplyClient, ReplyCursor, ReplyItem, ReplyPage, ReplySort, SubReplyPage};
use qaq_bx_core::search::{SearchClient, SearchVideo};
use qaq_bx_core::settings::{AppSettings, FileSettingsStore, SETTINGS_FILE};
use qaq_bx_video::engine::{DanmakuFilter, OverlayDanmaku, VideoConfig, VideoEngine};
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
/// P1 网格重建去抖:拖拽期间 VirtualSource 重建延迟到窗口静止后 150ms 一次。
const GRID_REBUILD_DEBOUNCE: Duration = Duration::from_millis(150);
/// 设置滑块（音量/透明度）去抖落盘窗口。
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// 升级 https 并对 B 站图床追加 `@480w_270h_1c.{webp|jpg}` 缩略图后缀,
/// 把单张封面流量与 WinUI 解码像素面积压缩到原图的 ~1/16–1/46。
/// 格式由系统 WebP 解码能力探测决定(见 `codec` 模块)。
pub fn thumbnail_url(raw: &str) -> String {
    thumbnail_url_with(raw, crate::codec::raster_suffix())
}

fn thumbnail_url_with(raw: &str, suffix: &str) -> String {
    let url = raw.replacen("http://", "https://", 1);
    if url.contains(".hdslb.com/") && !url.contains('@') && !url.contains('?') {
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".png")
            || lower.ends_with(".webp")
        {
            return format!("{url}@480w_270h_1c.{suffix}");
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
        author: card
            .owner
            .as_ref()
            .map_or_else(String::new, |owner| owner.name.clone()),
        author_mid: card.owner.as_ref().map_or(0, |owner| owner.mid),
        author_face_url: card
            .owner
            .as_ref()
            .and_then(|owner| owner.face.as_deref())
            .and_then(normalize_face_url),
        duration_secs: card
            .duration
            .unwrap_or_default()
            .clamp(0, i64::from(u32::MAX)) as u32,
        play_count: card
            .stat
            .as_ref()
            .map_or(0, |stat| stat.view.unsigned_abs()),
        danmaku_count: card
            .stat
            .as_ref()
            .map_or(0, |stat| stat.danmaku.unsigned_abs()),
        like_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.like)
            .map(|v| v.unsigned_abs()),
        coin_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.coin)
            .map(|v| v.unsigned_abs()),
        favorite_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.favorite)
            .map(|v| v.unsigned_abs()),
        share_count: card
            .stat
            .as_ref()
            .and_then(|stat| stat.share)
            .map(|v| v.unsigned_abs()),
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
                && sub.items.iter().any(|item| {
                    !item.avatar.is_empty() && avatar_thumbnail_url(&item.avatar) == thumb_url
                })
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
        let loading_more =
            bucket.loading && (!bucket.top_replies.is_empty() || !bucket.replies.is_empty());
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
    media: ResolvedMedia,
    fetched_at: Instant,
}

/// 结合设置与当前大会员缓存推导请求清晰度上限和编码优先级（不发 IO）。
fn resolve_params_for(settings: &AppSettings, nav: &NavClient) -> (i64, &'static [&'static str]) {
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
        danmaku_font_family: settings.danmaku_font_family.clone(),
        danmaku_font_scale: settings.danmaku_font_scale,
        danmaku_filter: danmaku_filter_from(settings),
    }
}

/// 弹幕过滤规则(设置模型 → 引擎规则;每行一条,空行忽略)。
fn danmaku_filter_from(settings: &AppSettings) -> DanmakuFilter {
    DanmakuFilter {
        block_scroll: settings.danmaku_block_scroll,
        block_bottom: settings.danmaku_block_bottom,
        block_top: settings.danmaku_block_top,
        block_color: settings.danmaku_block_color,
        block_repeat: settings.danmaku_block_repeat,
        weight_min: settings.danmaku_weight_min,
        rules: settings
            .danmaku_filter_rules
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    }
}

/// 弹幕分段时长(秒;与 core `danmaku::SEGMENT_MS` 锚,B 站 6 分钟/段)。
const DANMAKU_SEGMENT_SECS: u64 = SEGMENT_MS / 1000;

/// 弹幕分段数(向上取整,至少 1;duration 缺失按单段兜底)。
fn danmaku_segment_count(duration_secs: f64) -> u32 {
    (duration_secs.max(0.0) / DANMAKU_SEGMENT_SECS as f64)
        .ceil()
        .max(1.0) as u32
}

/// 播放头所在分段(index 从 1 起)。
fn danmaku_segment_at(secs: f64) -> u32 {
    (secs.max(0.0) / DANMAKU_SEGMENT_SECS as f64).floor() as u32 + 1
}

/// 预存链的下一分段:优先段(播放头所在段)先到,其余按 1..=total 升序补齐。
fn next_danmaku_segment(current: u32, first: u32, total: u32) -> Option<u32> {
    if current == first {
        (1..=total).find(|&i| i != first)
    } else {
        (current + 1..=total).find(|&i| i != first)
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
    playing: bool,
    position: u32,
    mount_tick: u32,
    volume: u32,
    volume_before_mute: u32,
    /// 引擎回报的媒体总长(秒;None = 尚未加载,用卡片数据兜底)。
    engine_duration: Option<f64>,
    /// 媒体失败/引擎错误提示(播放页显示一次)。
    player_notice: Option<String>,
    /// 视频加载态:解析/缓冲中为 true(驱动 ProgressRing 环形指示);
    /// 首个 has_media、解析失败或关播熄灭。
    video_loading: bool,
    /// 视频引擎(SwapChainPanel 渲染 + mpv;惰性按需创建，关闭迷你条即释放全部解码/D3D 内存)。
    engine: Option<Arc<VideoEngine>>,
    /// v0.3 spike:系统媒体引擎(QAQ_BX_NATIVE=1 启用;与 mpv 引擎互斥生效,
    /// 解析/控制路径按 native_active 分流)。
    native_player: Option<Arc<NativePlayer>>,
    native_enabled: bool,
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
    fullscreen_gen: u64,
    fullscreen_sync_in_flight: bool,
    key_route_ctx: Rc<Cell<(bool, bool)>>,
    global_key_handler: RoutedCallback<KeyEventInfo>,
    /// P1 拖拽期间挂起的网格重建(去抖 150ms,静止后一次)。
    pending_grid_rebuild: bool,
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

/// 全局搜索框壳的 Enter 判定:PreviewKeyDown 隧道命中主键盘/小键盘 Enter
/// (`original_key` 对 NumpadEnter 上报 0x0D)才提交;释放与其余键放行不拦截。
/// 纯函数便于单测锁行为,视图侧 `on_preview_key_down` 只做「判定→投递→handled」。
pub(crate) fn search_enter_submits(ev: &KeyEventInfo) -> bool {
    !ev.status.is_released && (ev.key == VirtualKey::ENTER || ev.original_key == VirtualKey::ENTER)
}

#[derive(Clone)]
pub enum Message {
    Back,
    /// selftest 空转消息:驱动探针在队列尾部刷新状态(生产路径零开销)。
    #[cfg(feature = "selftest")]
    SelfTestSync,
    /// 全局搜索框草稿变更(标题栏内敛搜索框 TextChanged)。
    SearchDraftChanged(String),
    /// 提交当前草稿发起搜索(新提交递增 seq,旧在飞结果丢弃)。
    SearchSubmitted,
    /// 搜索结果就绪(后台线程派发;封面缓存命中已挂,未命中的由 UI 线程补拉)。
    SearchResults {
        seq: u64,
        query: String,
        items: Vec<VideoItem>,
        total: Option<u64>,
    },
    /// 搜索失败(seq 失配即丢弃)。
    SearchFailed {
        seq: u64,
        error: String,
    },
    /// U2:搜索默认词就绪(None = 拉取失败,保持通用占位)。
    SearchDefaultReady(Option<String>),
    /// U2:点击默认词 → 填充草稿并提交。
    SearchPickDefault,
    NavTagSelected(Option<String>),
    PaneOpenChanged(bool),
    TogglePane,
    OpenVideo(String),
    TogglePlay,
    Seek(f64),
    SetVolume(f64),
    ToggleMute,
    ToggleDanmaku,
    EnterFullscreen,
    ExitFullscreen,
    ToggleFullscreen,
    FullscreenSynced {
        gen_id: u64,
        active: bool,
    },
    FullscreenProbeResult {
        gen_id: u64,
        active: bool,
    },
    PlayerHotkey(pages::fullscreen::PlayerHotkey),
    /// 引擎状态节拍(250ms;读引擎快照刷新 UI 单源)。
    PositionTick,
    /// 后台解析完成(分轨直链)→ 引擎 loadfile + 弹幕拉取（带代次校验防切视频串台）。
    MediaReady {
        seq: u64,
        media: ResolvedMedia,
    },
    /// 播放页画质切换(F2):同视频换流,记忆进度与播放态。
    SelectQuality(i64),
    /// U1:评论草稿变更。
    CommentDraftChanged(String),
    /// U1:提交发布评论(未登录在 shell 侧拦截提示)。
    SubmitComment,
    /// U1:发布完成(带代次防串台;Ok=新 rpid;Err=行内提示)。
    CommentSubmitted { gen_id: u64, result: Result<u64, String> },
    /// U3:切换分P(cid;后台重取 playurl → 定点换流)。
    SelectPage(u64),
    /// U3:分P 流就绪(带代次防串台)。
    PageStreamReady { gen_id: u64, cid: u64, result: Result<ResolvedMedia, String> },
    /// U4:直播房间列表就绪(page 失配即丢弃)。
    LiveRoomsLoaded {
        page: u32,
        res: Result<Vec<LiveCard>, String>,
    },
    /// U4:直播封面就绪(url 键回填)。
    LiveCoverReady { url: String, image: Option<EncodedImage> },
    /// U4:点击直播间卡(roomid, title, uname)→ 房间流直链 mpv 播放。
    LiveRoomSelected(u64, String, String),
    /// U4:房间流就绪。
    LiveReady { gen_id: u64, result: Result<String, String> },
    /// U4:直播页加载更多。
    LiveMore,
    /// 弹幕分段到货(预存链;即到即画,引擎侧归并排序)。
    DanmakuPart {
        seq: u64,
        index: u32,
        elems: Vec<DanmakuElem>,
    },
    /// 弹幕分段拉取失败(静默;预存链跳过该段继续,播放不受影响)。
    DanmakuFailed {
        seq: u64,
        index: u32,
        reason: String,
    },
    /// 解析/播放准备失败(播放页提示)。
    MediaFailed {
        seq: u64,
        reason: String,
    },
    /// 首页右侧半窗预览：展开进入完整播放页 / 关闭半窗 / 记忆宽度。
    ExpandHalfPane,
    CloseHalfPane,
    ResizeHalfPane(f64),
    ToggleMoreMenu,
    CloseMoreMenu,
    NavigateCompact(Route),
    ToggleSearchOverlay,
    CloseSearchOverlay,
    /// 自驱动演示节拍（内部状态推进，无鼠标驱动）。
    DemoTick,
    /// Windows 控制中心媒体按钮回调（None 表示通道已关闭）。
    SmtcAction(Option<SmtcCommand>),
    /// P1:窗口尺寸去抖到期(150ms 内无新 resize)→ 应用挂起的网格重建。
    WindowResizeSettled,
    /// Xbox 手柄事件回调（None 表示通道已关闭）。
    GamepadInput(Option<GamepadEvent>),
    /// Windows Toast 通知激活回调（false 表示通道关闭）。
    ToastActivated(bool),

    // ---- 首页推荐流与 Hover 预解析 ----
    /// 换一换（Refresh 语义：清流 + 预分配首批骨架 + 重发）。
    HomeRefresh,
    /// 首页网格某一行被虚拟化容器实际挂载或回收（无感续页 + 离屏封面窗口锚点）。
    HomeRowActive {
        rev: u64,
        row_idx: usize,
        active: bool,
    },
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
    HomeFailed {
        seq: u64,
        reason: String,
    },
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
        result: Result<ResolvedMedia, String>,
    },

    // ---- 热门页（真实 popular API，无签名 pn/ps 分页）----
    /// 加载下一页/首屏失败重试（`next_pn` 由单源状态推进；失败不推进=自动重取同页）。
    PopularMore,
    PopularLoaded(Result<Vec<VideoItem>, String>),

    // ---- 播放页相关推荐（真实 archive/related API）----
    RelatedLoaded {
        seq: u64,
        bvid: String,
        res: Result<Vec<VideoItem>, String>,
    },
    RelatedRetry,

    // ---- 设置持久化 ----
    UpdateSettings(AppSettings),
    FlushSettingsSave,
    SettingsSaved(Option<String>),

    // ---- 登录（桥接 crate::auth）----
    /// 启动/登录成功后的 nav 探测结果（含自动续期状态）。
    AuthProbed {
        seq: u64,
        report: ProbeReport,
    },
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
        res: Result<qaq_bx_core::notify::FeedPage<ReplyNotice>, String>,
    },
    AtsLoaded {
        seq: u64,
        append: bool,
        res: Result<qaq_bx_core::notify::FeedPage<AtNotice>, String>,
    },
    LikesLoaded {
        seq: u64,
        append: bool,
        res: Result<qaq_bx_core::notify::FeedPage<LikeNotice>, String>,
    },
    SystemsLoaded {
        seq: u64,
        append: bool,
        res: Result<qaq_bx_core::notify::SystemPage, String>,
    },
    SessionsLoaded {
        seq: u64,
        append: bool,
        res: Result<qaq_bx_core::notify::SessionPage, String>,
    },
    SelectWhisperSession((u64, i32)),
    CloseWhisperDetail,
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

/// 搜索结果卡 → 展示模型(封面占位,由 ImageCache/CoverReady 异步填充)。
fn search_video_item(video: &SearchVideo) -> VideoItem {
    VideoItem {
        bvid: video.bvid.clone(),
        aid: video.aid,
        cid: video.cid,
        title: video.title.clone(),
        author: video.author.clone(),
        author_mid: video.author_mid,
        author_face_url: None,
        duration_secs: video.duration_secs,
        play_count: video.play_count,
        danmaku_count: video.danmaku_count,
        // 搜索卡不下发互动计数，保持 None（进播放页后由 video_meta 回填）。
        like_count: None,
        coin_count: None,
        favorite_count: None,
        share_count: None,
        cover: None,
        pic_url: video.pic_url.clone(),
    }
}

/// 归档卡（popular/related 真实接口）→ 展示模型(封面占位,由 ImageCache/CoverReady 异步填充)。
fn archive_video_item(card: &ArchiveCard) -> VideoItem {
    VideoItem {
        bvid: card.bvid.clone(),
        aid: card.aid,
        cid: card.cid,
        title: card.title.clone(),
        author: card.owner.name.clone(),
        author_mid: card.owner.mid,
        author_face_url: card.owner.face.as_deref().and_then(normalize_face_url),
        duration_secs: card.duration.clamp(0, i64::from(u32::MAX)) as u32,
        play_count: card.stat.view.unsigned_abs(),
        danmaku_count: card.stat.danmaku.unsigned_abs(),
        like_count: card.stat.like.map(|v| v.unsigned_abs()),
        coin_count: card.stat.coin.map(|v| v.unsigned_abs()),
        favorite_count: card.stat.favorite.map(|v| v.unsigned_abs()),
        share_count: card.stat.share.map(|v| v.unsigned_abs()),
        cover: None,
        pic_url: Some(thumbnail_url(&card.pic)),
    }
}

/// 未知元数据稿件的最小展示模型（QAQ_BX_VIDEO 冒烟路由 / open_video 兜底）：
/// 标题暂以 bvid 占位，真实标题/UP 主/封面由 video_meta 解析管线回填。
fn minimal_video_item(bvid: &str, title: &str) -> VideoItem {
    VideoItem {
        bvid: bvid.to_string(),
        aid: 0,
        cid: 0,
        title: if title.trim().is_empty() {
            bvid.to_string()
        } else {
            title.to_string()
        },
        author: String::new(),
        author_mid: 0,
        author_face_url: None,
        duration_secs: 0,
        play_count: 0,
        danmaku_count: 0,
        like_count: None,
        coin_count: None,
        favorite_count: None,
        share_count: None,
        cover: None,
        pic_url: None,
    }
}

/// 热门分页合并：按 bvid 去重（跨页重叠防御），返回实际新增项。
fn merge_popular_items(existing: &mut Vec<VideoItem>, incoming: Vec<VideoItem>) -> Vec<VideoItem> {
    let mut seen: HashSet<String> = existing.iter().map(|item| item.bvid.clone()).collect();
    let mut fresh = Vec::new();
    for item in incoming {
        if seen.insert(item.bvid.clone()) {
            fresh.push(item);
        }
    }
    existing.extend(fresh.iter().cloned());
    fresh
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
        out.whisper_detail_open = self.whisper_detail_open;
        out.search_overlay_open = self.search_overlay_open;
        out.danmaku_enabled = self.settings.danmaku_enabled;
        out.volume = self.volume;
        out.is_fullscreen = self.is_fullscreen;
        out.quality_count = self.quality_qns.len();
        out.current_qn = self.current_qn;
        out.live_rooms = self.live.cards.len();
        out.live_first_roomid = self.live.cards.first().map_or(0, |c| c.room.roomid);
    }

    fn submit_search(&mut self, context: &ComponentContext<Self>) {
        let query = self.search_state.draft.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.exit_fullscreen(context);
        self.search_overlay_open = false;
        self.more_menu_open = false;
        self.search_seq += 1;
        let seq = self.search_seq;
        if self.route != Route::Search {
            self.history.push(self.route);
            self.route = Route::Search;
            crate::trace::note("route->search (搜索提交)");
        }
        self.search_state.query = query.clone();
        self.search_state.loading = true;
        self.search_state.error = None;
        let client = self.search_client.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| match block_on(client.all_v2(&query, 1, 20)) {
            Ok(page) => {
                let items: Vec<VideoItem> = page
                    .videos
                    .iter()
                    .map(|video| {
                        let mut item = search_video_item(video);
                        if item.cover.is_none()
                            && let Some(url) = &item.pic_url
                        {
                            item.cover = cache.get(url);
                        }
                        item
                    })
                    .collect();
                Message::SearchResults {
                    seq,
                    query: page.query,
                    items,
                    total: page.total,
                }
            }
            Err(err) => Message::SearchFailed {
                seq,
                error: err.to_string(),
            },
        });
    }

    fn pick_demo_bvid(&self) -> String {
        self.home_slots
            .iter()
            .find_map(|slot| match slot {
                HomeSlot::Video { item, .. } => Some(item.bvid.clone()),
                _ => None,
            })
            .or_else(|| self.popular.items.first().map(|item| item.bvid.clone()))
            .or_else(|| {
                self.search_state
                    .videos
                    .first()
                    .map(|item| item.bvid.clone())
            })
            .unwrap_or_else(|| "BV1GJ411x7h7".to_string())
    }

    fn handle_demo_tick(&mut self, context: &ComponentContext<Self>) {
        let actions = {
            let Some(demo) = self.demo.as_mut() else {
                self.demo_timer = None;
                return;
            };
            demo.step()
        };

        for action in actions {
            match action {
                crate::demo::DemoAction::OpenPopular => {
                    self.logout_confirm = false;
                    self.more_menu_open = false;
                    self.search_overlay_open = false;
                    self.home_half_pane = false;
                    if self.route != Route::Popular {
                        self.whisper_detail_open = false;
                        self.history.push(self.route);
                        self.route = Route::Popular;
                        crate::trace::note("route->popular (半屏详情关闭)");
                        self.ensure_popular(context);
                    }
                }
                crate::demo::DemoAction::ToggleMoreMenu => {
                    self.search_overlay_open = false;
                    self.more_menu_open = true;
                }
                crate::demo::DemoAction::CloseMoreMenu => {
                    self.more_menu_open = false;
                }
                crate::demo::DemoAction::OpenSearchOverlay => {
                    self.more_menu_open = false;
                    self.search_overlay_open = true;
                }
                crate::demo::DemoAction::SetSearchDraft(draft) => {
                    self.search_state.draft = draft.to_string();
                }
                crate::demo::DemoAction::SubmitSearch => {
                    self.submit_search(context);
                }
                crate::demo::DemoAction::OpenDemoVideo => {
                    let bvid = self.pick_demo_bvid();
                    self.more_menu_open = false;
                    self.search_overlay_open = false;
                    self.home_half_pane = false;
                    if let Some(demo) = self.demo.as_mut() {
                        demo.player_override = None;
                    }
                    self.open_video(&bvid, context);
                    self.sync_smtc(context);
                }
                crate::demo::DemoAction::PlayerExpandDesc => {
                    if let Some(demo) = self.demo.as_mut() {
                        demo.player_override = Some((1, pages::player::PlayerTab::Intro, true));
                    }
                }
                crate::demo::DemoAction::PlayerShowComments => {
                    if let Some(demo) = self.demo.as_mut() {
                        demo.player_override = Some((2, pages::player::PlayerTab::Comments, false));
                    }
                }
                crate::demo::DemoAction::Complete => {
                    if let Some(demo) = self.demo.as_ref() {
                        demo.request_exit_if_configured();
                    }
                }
            }
        }

        if self.demo.as_ref().is_some_and(|d| !d.is_finished()) {
            self.demo_timer = Some(
                context.set_local_timeout(crate::demo::DEMO_TICK_INTERVAL, || Message::DemoTick),
            );
        } else {
            self.demo_timer = None;
        }
    }

    fn open_video(&mut self, bvid: &str, context: &ComponentContext<Self>) {
        // 数据源：首页已加载卡 → 搜索/热门/相关推荐索引 → 最小模型（真实标题由 video_meta 回填）。
        let mut item = match self
            .home_index
            .get(bvid)
            .cloned()
            .or_else(|| self.search_index.get(bvid).cloned())
            .or_else(|| self.popular_index.get(bvid).cloned())
            .or_else(|| self.related_index.get(bvid).cloned())
        {
            Some(item) => item,
            None => minimal_video_item(bvid, ""),
        };
        if item.cover.is_none()
            && let Some(url) = &item.pic_url
        {
            item.cover = self.image_cache.get(url);
        }
        self.start_playback(item, context);
    }

    fn open_video_target(&mut self, target: OpenTarget, context: &ComponentContext<Self>) {
        if self.home_index.contains_key(&target.bvid)
            || self.popular_index.contains_key(&target.bvid)
            || self.related_index.contains_key(&target.bvid)
        {
            self.open_video(&target.bvid, context);
            return;
        }
        let item = minimal_video_item(&target.bvid, &target.title);
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

    /// v0.3 spike:native 引擎是否接管播放控制(QAQ_BX_NATIVE=1)。
    fn native_active(&self) -> bool {
        self.native_enabled
    }

    /// v0.3:native 引擎接管装载(系统 DASH 管线;UI 线程只提交请求)。
    /// 返回 false = native 未启用,调用方走 mpv 路径。MPD→MediaSource 的网络+
    /// 解析在引擎装载线程,产物由 `NativeSurface` 的挂载节拍注入;失败经
    /// `status().error` 在 PositionTick 落到 `player_notice`,不在此阻塞。
    fn native_load_choice(
        &mut self,
        choice: &qaq_bx_core::player::StreamChoice,
        autoplay: bool,
        start_secs: f64,
    ) -> bool {
        if !self.native_enabled {
            return false;
        }
        let player = match &self.native_player {
            Some(player) => player.clone(),
            None => {
                let player = Arc::new(NativePlayer::new());
                self.native_player = Some(player.clone());
                player
            }
        };
        let mpd = qaq_bx_core::mpd::build_mpd(&choice.video, Some(&choice.audio));
        // 诊断锚:系统媒体管线只吃 MF 支持的编码,这里把选中分轨的 codec/mime 留痕。
        eprintln!(
            "[qaq-bx-ui] native load qn={} codec={} mime={} w={} h={}",
            choice.video.id, choice.video.codecs, choice.video.mime_type, choice.video.width,
            choice.video.height
        );
        if let Err(e) = player.load(
            &mpd,
            &choice.video.base_url,
            Some(&choice.audio.base_url),
            autoplay,
            start_secs,
        ) {
            eprintln!("[qaq-bx-ui] native engine load failed: {e}");
            self.player_notice = Some(format!("系统媒体引擎装载失败:{e}"));
            return true;
        }
        // 弹幕改由透明层承接(视频面归 MPE,mpv 引擎保持 idle)。
        self.ensure_stage();
        self.native_playback = true;
        // 音量先记账(未 attach 时 SetVolume 无对象,attach 时由引擎统一施加)。
        let _ = player.set_volume(f64::from(self.volume));
        self.playing = autoplay;
        true
    }

    /// v0.3:native 引擎当前进度(换画质/换分P 的定点续播起点)。
    fn native_position(&self) -> f64 {
        self.native_sink()
            .map(|player| player.status().position_secs)
            .unwrap_or(0.0)
    }

    /// v0.3:native 弹幕透明层懒创建(native 未启用返回 None)。创建时按当前
    /// 设置下发一次样式/规则,之后一切变更走 `danmaku_*` 分流口,杜绝两侧漂移。
    fn ensure_stage(&mut self) -> Option<Arc<DanmakuStage>> {
        if !self.native_enabled {
            return None;
        }
        // 诊断用(临时):QAQ_BX_NATIVE_STAGE=0 → 不建透明层,用于隔离
        // 「多一个 D3D device 是否让 MF 拒绝 MediaSource」。
        if std::env::var("QAQ_BX_NATIVE_STAGE").as_deref() == Ok("0") {
            eprintln!("[qaq-bx-ui] native danmaku stage: 诊断模式跳过");
            return None;
        }
        if self.native_stage.is_none() {
            let stage = DanmakuStage::with_config(video_config_from(&self.settings));
            eprintln!("[qaq-bx-ui] native danmaku stage: 透明层已创建");
            self.native_stage = Some(stage.clone());
        }
        self.native_stage.clone()
    }

    // ---- 弹幕投递单源(native_playback → 透明层;否则 → 视频引擎同帧叠画) ----

    /// 全量替换(新视频/换分P 的清层语义)。
    fn danmaku_replace(&self, items: Vec<OverlayDanmaku>) {
        if let Some(stage) = self.stage_sink() {
            stage.set_danmaku(items);
        } else if let Some(engine) = &self.engine {
            engine.set_danmaku(items);
        }
    }

    /// 增量追加(分段预存链逐段到货)。
    fn danmaku_append(&self, items: Vec<OverlayDanmaku>) {
        if let Some(stage) = self.stage_sink() {
            stage.append_danmaku(items);
        } else if let Some(engine) = &self.engine {
            engine.append_danmaku(items);
        }
    }

    fn danmaku_config(
        &self,
        enabled: bool,
        opacity: f32,
        max_lanes: usize,
        family: &str,
        scale: f32,
    ) {
        if let Some(stage) = self.stage_sink() {
            stage.set_danmaku_config(enabled, opacity, max_lanes, family, scale);
        } else if let Some(engine) = &self.engine {
            engine.set_danmaku_config(enabled, opacity, max_lanes, family, scale);
        }
    }

    fn danmaku_filter(&self, filter: DanmakuFilter) {
        if let Some(stage) = self.stage_sink() {
            stage.set_danmaku_filter(filter);
        } else if let Some(engine) = &self.engine {
            engine.set_danmaku_filter(filter);
        }
    }

    /// 透明层投递闸:仅在系统媒体引擎真正驱动播放时返回 Some。
    fn stage_sink(&self) -> Option<&Arc<DanmakuStage>> {
        if self.native_playback {
            self.native_stage.as_ref()
        } else {
            None
        }
    }

    /// 播放控制分流闸(native 驱动时返回引擎;直播/mpv 路径返回 None,调用方
    /// 走视频引擎)。残留的 native_player 不得劫持直播控制,否则直播的进度/音量
    /// 会打到已卸载的引擎上。
    fn native_sink(&self) -> Option<&Arc<NativePlayer>> {
        if self.native_playback {
            self.native_player.as_ref()
        } else {
            None
        }
    }

    fn current_row_revision(&self) -> u64 {
        let cards = self.viewport_layout.cards;
        let cols = cards.cols.max(1) as u64;
        let compact_bit = u64::from(cards.compact_card);
        let width_bits = (cards.card_width.round().max(0.0) as u64) & 0x7FF;
        (self.home_state.stream_gen << 16) | (cols << 12) | (compact_bit << 11) | width_bits
    }

    fn rebuild_home_source(&mut self) {
        let rev = self.current_row_revision();
        let focused = self.gamepad_active.then_some(self.home_focus_idx);
        self.home_source = video_card::home_virtual_source(
            &self.home_slots,
            self.viewport_layout.cards,
            rev,
            focused,
            &self.home_on_open,
            &self.home_on_hover,
            &self.home_on_row_active,
            &self.unplayable,
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
            self.viewport_layout.cards,
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
        let batch_size = feed_pager::aligned_next_batch_for(
            self.home_slots.len(),
            self.viewport_layout.cards,
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
        self.more_menu_open = false;
        self.search_overlay_open = false;
        if self.route == Route::Home
            && !self.viewport_layout.is_mobile()
            && self.settings.home_open_mode == qaq_bx_core::settings::HomeOpenMode::HalfPane
        {
            self.home_half_pane = true;
        } else {
            self.home_half_pane = false;
            if self.route != Route::Player {
                self.history.push(self.route);
            }
            self.route = Route::Player;
            crate::trace::note("route->player (打开视频)");
        }
        self.advance_playback_gen();
        self.pending_open_preresolve = None;
        self.current_meta = None;
        self.playback_dash = None;
        self.quality_qns = Vec::new();
        self.current_qn = 0;
        self.current_video_url.clear();
        self.current_audio_url.clear();
        self.media_probe_at = None;
        self.failed_video_prefixes.clear();
        self.video_fallback_done = false;
        self.audio_fallback_done = false;
        self.now_playing_desc = None;
        self.meta_fetch_inflight_seq = None;
        self.playback_avatar_dirty = false;
        self.playback_avatar_timer = None;
        // 视频引擎仍是播放页的渲染前提(native 模式下它保持 idle)。
        self.ensure_engine();
        // 清弹幕层:投递口按当前模式分流(native → 透明层;mpv → 引擎同帧)。
        self.danmaku_replace(Vec::new());
        // U1:新视频重置评论输入态。
        self.comment_draft.clear();
        self.composer_notice = None;
        // U3:新视频重置分P态。
        self.pages_menu.clear();
        self.current_cid = 0;
        self.now_playing = Some(item.clone());
        self.playing = self.settings.autoplay;
        self.position = 0;
        self.engine_duration = None;
        self.player_notice = None;
        self.video_loading = true;
        self.comments_session.reset(item.aid, item.author_mid);
        self.refresh_comments_snapshot();
        self.schedule_tick(context);
        self.kickoff_video_extras(self.playback_gen, context);
        // 右栏相关推荐：真实 archive/related 随播放代次拉取（快速切视频由代次校验作废旧响应）。
        self.fetch_related(self.playback_gen, item.bvid.clone(), context);

        let nav = self.auth_service.nav_client();
        let current_key = resolve_profile_key(&self.settings, &nav);

        // 1) 命中新鲜 Hover 预解析缓存：直接送入 mpv，0ms 等待解析。
        if let Some(cached) = self.take_fresh_preresolved(&item.bvid, current_key) {
            let seq = self.playback_gen;
            self.apply_resolved_media(cached.media, seq, context);
            return;
        }

        // 2) 若同名 Hover 预解析正在飞且配置匹配：直接接管在飞请求结果，不重复发 playurl。
        if self.preresolve_inflight.get(&item.bvid).copied() == Some(current_key) {
            self.pending_open_preresolve =
                Some((item.bvid.clone(), self.playback_gen, current_key));
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
        self.playback_epoch
            .store(self.playback_gen, Ordering::SeqCst);
        self.playback_gen
    }

    /// 播放页画质菜单视图((qn, 标签) 降序;F2)。
    fn quality_menu_view(&self) -> Vec<(i64, String)> {
        self.quality_qns
            .iter()
            .map(|qn| (*qn, quality_label(*qn)))
            .collect()
    }

    /// U3 分P菜单视图。
    fn pages_menu_view(&self) -> Vec<(u64, String)> {
        self.pages_menu.clone()
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
            // 互动计数回填（搜索卡/最小模型缺 stat 时经 view 补齐；已有真实值不覆盖）。
            if let Some(stat) = &meta.stat {
                if cur.play_count == 0 {
                    cur.play_count = stat.view.unsigned_abs();
                }
                if cur.danmaku_count == 0 {
                    cur.danmaku_count = stat.danmaku.unsigned_abs();
                }
                if cur.like_count.is_none() {
                    cur.like_count = stat.like.map(|v| v.unsigned_abs());
                }
                if cur.coin_count.is_none() {
                    cur.coin_count = stat.coin.map(|v| v.unsigned_abs());
                }
                if cur.favorite_count.is_none() {
                    cur.favorite_count = stat.favorite.map(|v| v.unsigned_abs());
                }
                if cur.share_count.is_none() {
                    cur.share_count = stat.share.map(|v| v.unsigned_abs());
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

    fn fetch_comments(
        &mut self,
        sort: CommentSort,
        append: bool,
        context: &ComponentContext<Self>,
    ) {
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

    fn fetch_sub_replies(
        &mut self,
        root_rpid: u64,
        append: bool,
        context: &ComponentContext<Self>,
    ) {
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

    /// 热门页首拉守卫：导航进入/重试时仅在从未成功加载且不在飞时开拉。
    fn ensure_popular(&mut self, context: &ComponentContext<Self>) {
        if self.popular.loaded_once || self.popular.loading {
            return;
        }
        self.fetch_popular(context);
    }

    /// U4:直播页首屏/加载更多(页号推进与热门同语义)。
    fn ensure_live(&mut self, context: &ComponentContext<Self>) {
        if self.live.loaded_once || self.live.loading {
            return;
        }
        self.fetch_live_rooms(context);
    }

    fn fetch_live_rooms(&mut self, context: &ComponentContext<Self>) {
        if self.live.loading {
            return;
        }
        self.live.loading = true;
        self.live.error = None;
        let client = self.live_client.clone();
        let cache = self.image_cache.clone();
        let page = self.live.next_page.max(1);
        context.spawn_background(move |_| {
            let res = block_on(client.rooms(page)).map_err(|err| err.to_string());
            Message::LiveRoomsLoaded {
                page,
                res: res.map(|rooms| {
                    rooms
                        .into_iter()
                        .map(|room| LiveCard {
                            cover: cache.get(&room.cover),
                            room,
                        })
                        .collect()
                }),
            }
        });
    }

    fn absorb_live_rooms(
        &mut self,
        page: u32,
        res: Result<Vec<LiveCard>, String>,
        context: &ComponentContext<Self>,
    ) {
        if page != self.live.next_page {
            return;
        }
        self.live.loading = false;
        let cards = match res {
            Ok(cards) => cards,
            Err(err) => {
                self.live.error = Some(err);
                return;
            }
        };
        self.live.next_page += 1;
        self.live.reached_end = cards.is_empty();
        let existing: std::collections::HashSet<u64> =
            self.live.cards.iter().map(|c| c.room.roomid).collect();
        for card in cards {
            if existing.contains(&card.room.roomid) {
                continue;
            }
            if card.cover.is_none() && !card.room.cover.is_empty() {
                self.spawn_live_cover(card.room.cover.clone(), context);
            }
            self.live.cards.push(card);
        }
        self.live.loaded_once = true;
    }

    /// U4:直播间封面补拉(url 键 LRU;与视频封面同一信号量节流)。
    fn spawn_live_cover(&mut self, url: String, context: &ComponentContext<Self>) {
        if !self.live_cover_inflight.insert(url.clone()) {
            return;
        }
        let auth = self.auth_service.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| {
            let image = block_on(async {
                let _permit = cover_semaphore().acquire().await.ok()?;
                auth.fetch_image_async(&url).await
            });
            if image.is_some() {
                cache.put(url.clone(), image.clone().unwrap());
            }
            Message::LiveCoverReady { url, image }
        });
    }

    /// U4:直播间 → Player 页(mpv 直播 FLV/HLS;评论区/画质/分P/弹幕不适用)。
    fn start_live_playback(
        &mut self,
        roomid: u64,
        title: String,
        uname: String,
        context: &ComponentContext<Self>,
    ) {
        self.more_menu_open = false;
        self.search_overlay_open = false;
        self.home_half_pane = false;
        if self.route != Route::Player {
            self.history.push(self.route);
        }
        self.route = Route::Player;
        crate::trace::note("route->player (换流/重开)");
        self.advance_playback_gen();
        self.pending_open_preresolve = None;
        self.current_meta = None;
        self.playback_dash = None;
        self.quality_qns = Vec::new();
        self.current_qn = 0;
        self.current_video_url.clear();
        self.current_audio_url.clear();
        self.media_probe_at = None;
        self.failed_video_prefixes.clear();
        self.video_fallback_done = false;
        self.audio_fallback_done = false;
        self.pages_menu.clear();
        self.current_cid = 0;
        self.now_playing_desc = None;
        self.meta_fetch_inflight_seq = None;
        self.playback_avatar_dirty = false;
        self.playback_avatar_timer = None;
        // 直播恒走 mpv(FLV,MF 不支持):弹幕必须送回视频引擎,透明层不接管。
        self.native_playback = false;
        self.ensure_engine().set_danmaku(Vec::new());
        self.comment_draft.clear();
        self.composer_notice = None;
        let mut item = minimal_video_item(&format!("live:{roomid}"), &title);
        item.author = uname;
        self.now_playing = Some(item);
        self.playing = self.settings.autoplay;
        self.position = 0;
        self.engine_duration = None;
        self.player_notice = None;
        self.video_loading = true;
        self.comments_session.reset(0, 0);
        self.refresh_comments_snapshot();
        self.schedule_tick(context);
        let client = self.live_client.clone();
        let gen_id = self.playback_gen;
        context.spawn_background(move |_| {
            let result = block_on(client.play_url(roomid)).map_err(|err| err.to_string());
            Message::LiveReady { gen_id, result }
        });
    }

    fn fetch_popular(&mut self, context: &ComponentContext<Self>) {
        if self.popular.loading {
            return;
        }
        self.popular.loading = true;
        self.popular.error = None;
        spawn_popular_fetch(
            &self.feed_client,
            &self.image_cache,
            self.popular.next_pn,
            context,
        );
    }

    fn absorb_popular(
        &mut self,
        res: Result<Vec<VideoItem>, String>,
        context: &ComponentContext<Self>,
    ) {
        self.popular.loading = false;
        let items = match res {
            Ok(items) => items,
            Err(err) => {
                // next_pn 不推进：「加载更多」即自动重取同页（兼作重试）。
                self.popular.error = Some(err);
                return;
            }
        };
        self.popular.next_pn += 1;
        self.popular.reached_end = items.is_empty();
        let fresh = merge_popular_items(&mut self.popular.items, items);
        for mut item in fresh {
            if item.cover.is_none()
                && let Some(url) = &item.pic_url
            {
                if let Some(cached) = self.image_cache.get(url) {
                    item.cover = Some(cached);
                } else {
                    self.spawn_cover_fetch(item.bvid.clone(), url.clone(), self.feed_gen, context);
                }
            }
            self.popular_index.insert(item.bvid.clone(), item);
        }
        self.popular.loaded_once = true;
    }

    /// 相关推荐拉取：随播放代次发起，旧 bvid 的迟到响应在吸收端双校验丢弃。
    fn fetch_related(&mut self, seq: u64, bvid: String, context: &ComponentContext<Self>) {
        self.related = pages::player::RelatedPane {
            bvid: bvid.clone(),
            loading: true,
            ..pages::player::RelatedPane::default()
        };
        let client = self.feed_client.clone();
        let cache = self.image_cache.clone();
        context.spawn_background(move |_| {
            let res = block_on(client.related(&bvid)).map_err(|e| e.to_string());
            let items = match &res {
                Ok(cards) => cards
                    .iter()
                    .map(|card| {
                        let mut item = archive_video_item(card);
                        if let Some(url) = &item.pic_url {
                            item.cover = cache.get(url);
                        }
                        item
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
            Message::RelatedLoaded {
                seq,
                bvid,
                res: match res {
                    Ok(_) => Ok(items),
                    Err(err) => Err(err),
                },
            }
        });
    }

    fn absorb_related(
        &mut self,
        seq: u64,
        bvid: String,
        res: Result<Vec<VideoItem>, String>,
        context: &ComponentContext<Self>,
    ) {
        // 代次 + 目标双校验：快速切视频时迟到的旧响应一律丢弃。
        if seq != self.playback_gen || self.related.bvid != bvid {
            return;
        }
        self.related.loading = false;
        let items = match res {
            Ok(items) => items,
            Err(err) => {
                self.related.error = Some(err);
                return;
            }
        };
        for mut item in items {
            // 接口不保证剔除当前稿件，防御性过滤自身。
            if item.bvid == self.related.bvid {
                continue;
            }
            if item.cover.is_none()
                && let Some(url) = &item.pic_url
            {
                if let Some(cached) = self.image_cache.get(url) {
                    item.cover = Some(cached);
                } else {
                    self.spawn_cover_fetch(item.bvid.clone(), url.clone(), self.feed_gen, context);
                }
            }
            self.related_index.insert(item.bvid.clone(), item.clone());
            self.related.items.push(item);
        }
        self.related.loaded_once = true;
    }

    fn apply_resolved_media(
        &mut self,
        media: ResolvedMedia,
        seq: u64,
        context: &ComponentContext<Self>,
    ) {
        let ResolvedMedia { meta, choice, dash } = media;
        self.absorb_video_meta(&meta, seq, context);
        self.current_qn = choice.video.id;
        self.current_cid = meta.cid;
        self.pages_menu = pages_menu_from(&meta.pages);
        self.playback_dash = Some(dash.clone());
        self.quality_qns = quality_menu(&dash.video, 0);
        self.current_video_url = choice.video.base_url.clone();
        self.current_audio_url = choice.audio.base_url.clone();
        self.media_probe_at = Some(Instant::now());
        // v0.3 spike:native 引擎接管时,mpv 引擎仍需创建(播放页以 engine 存在
        // 为渲染前提),但不 loadfile —— 保持 idle。
        let engine = self.ensure_engine();
        if self.native_load_choice(&choice, self.settings.autoplay, 0.0) {
            self.playing = self.settings.autoplay;
            self.current_meta = Some(meta);
            self.fetch_danmaku(seq, 0.0, context);
            return;
        }
        engine.set_volume(f64::from(self.volume));
        engine.load_with_options(
            &choice.video.base_url,
            Some(&choice.audio.base_url),
            0.0,
            self.settings.autoplay,
        );
        self.playing = self.settings.autoplay;
        self.current_meta = Some(meta);
        self.fetch_danmaku(seq, 0.0, context);
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

    /// F4:视频轨解码失败自动回退——按失败桶重选编码,记忆进度无缝换流。
    fn apply_video_fallback(&mut self) {
        let Some(dash) = self.playback_dash.clone() else {
            return;
        };
        let failed_prefix = dash
            .video
            .iter()
            .find(|v| v.base_url == self.current_video_url)
            .and_then(|v| v.codecs.split('.').next())
            .unwrap_or_default()
            .to_string();
        if !failed_prefix.is_empty() && !self.failed_video_prefixes.contains(&failed_prefix) {
            self.failed_video_prefixes.push(failed_prefix);
        }
        let failed: Vec<&str> = self
            .failed_video_prefixes
            .iter()
            .map(String::as_str)
            .collect();
        let Some(choice) =
            select_stream_excluding(&dash, 0, self.settings.codec.priority(), &failed)
        else {
            self.player_notice = Some(
                "视频轨无法解码(所有编码桶均失败)。可尝试切换清晰度/编码或更新系统解码组件。"
                    .to_string(),
            );
            self.playing = false;
            return;
        };
        let start = self
            .engine
            .as_ref()
            .map(|engine| engine.status().time_pos)
            .unwrap_or(0.0);
        if let Some(engine) = &self.engine {
            engine.load_with_options(
                &choice.video.base_url,
                Some(&choice.audio.base_url),
                start,
                self.playing,
            );
        }
        eprintln!(
            "[qaq-bx-ui] video-track fallback: {} -> {}",
            failed.join("+"),
            choice.video.codecs
        );
        self.current_video_url = choice.video.base_url;
        self.current_audio_url = choice.audio.base_url;
        self.current_qn = choice.video.id;
        self.media_probe_at = Some(Instant::now());
        self.player_notice = None;
    }

    /// F4:音频轨缺失自动回退——换次高带宽音轨,原画质不动。
    fn apply_audio_fallback(&mut self) {
        let Some(dash) = self.playback_dash.clone() else {
            return;
        };
        let Some(audio) = select_audio_excluding(&dash, &self.current_audio_url) else {
            self.player_notice = Some("音频轨不可用(无备选音轨)。".to_string());
            return;
        };
        let start = self
            .engine
            .as_ref()
            .map(|engine| engine.status().time_pos)
            .unwrap_or(0.0);
        if let Some(engine) = &self.engine {
            engine.load_with_options(
                &self.current_video_url,
                Some(&audio.base_url),
                start,
                self.playing,
            );
        }
        eprintln!(
            "[qaq-bx-ui] audio-track fallback: {} -> id={}",
            self.current_audio_url, audio.id
        );
        self.current_audio_url = audio.base_url;
        self.media_probe_at = Some(Instant::now());
        self.player_notice = None;
    }

    fn cache_preresolved(&mut self, bvid: String, key: ResolveProfileKey, media: ResolvedMedia) {
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
                media,
                fetched_at: Instant::now(),
            },
        );
    }

    /// 预存弹幕:按 6 分钟段链式拉取,播放头所在段最先到货即画,其余段
    /// 后台陆续补齐(每段 DanmakuPart → 引擎 AppendDanmaku → 链式取下一段)。
    /// 不再等全片拉完才上屏;分P 切换须先 set_danmaku(Vec::new()) 清层。
    fn fetch_danmaku(&mut self, seq: u64, start_secs: f64, context: &ComponentContext<Self>) {
        if self.current_meta.is_none() {
            return;
        }
        let first = danmaku_segment_at(start_secs);
        self.danmaku_first_seg = first;
        self.fetch_danmaku_segment(seq, first, context);
    }

    /// 拉取单个弹幕分段(预存链的一环;结果经 DanmakuPart/DanmakuFailed 驱动下一段)。
    fn fetch_danmaku_segment(&mut self, seq: u64, index: u32, context: &ComponentContext<Self>) {
        let Some(meta) = self.current_meta.clone() else {
            return;
        };
        let auth = self.auth_service.clone();
        let nav = auth.nav_client();
        context.spawn_background(move |_| {
            let client = DanmakuClient::with_nav(auth.http(), auth.credential_provider(), nav);
            match block_on(client.segment(meta.cid, meta.aid, index)) {
                Ok(elems) => Message::DanmakuPart { seq, index, elems },
                Err(error) => Message::DanmakuFailed {
                    seq,
                    index,
                    reason: error.to_string(),
                },
            }
        });
    }

    /// 预存链的下一段(播放头段 → 其余段升序;全部段到货后返回 None 收束)。
    fn next_danmaku_index(&self, current: u32) -> Option<u32> {
        let total = self
            .current_meta
            .as_ref()
            .map_or(0, |meta| danmaku_segment_count(meta.duration as f64));
        next_danmaku_segment(current, self.danmaku_first_seg, total)
    }

    /// 去抖调度配置落盘（滑块拖动不频繁写盘，稳定后 300ms 原子写一次）。
    fn schedule_settings_save(&mut self, context: &ComponentContext<Self>) {
        self.settings_save_timer =
            Some(context.set_local_timeout(SETTINGS_SAVE_DEBOUNCE, || Message::FlushSettingsSave));
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

    fn listen_gamepad(
        rx: &Arc<Mutex<std::sync::mpsc::Receiver<GamepadEvent>>>,
        context: &ComponentContext<Self>,
    ) {
        let rx = Arc::clone(rx);
        context.spawn_background(move |_| {
            let ev = rx.lock().ok().and_then(|guard| guard.recv().ok());
            Message::GamepadInput(ev)
        });
    }

    fn listen_toast(
        rx: &Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        context: &ComponentContext<Self>,
    ) {
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
        let current_idx = NAV_ROUTES
            .iter()
            .position(|&r| r == self.route)
            .unwrap_or(0) as isize;
        let len = NAV_ROUTES.len() as isize;
        let next_idx = ((current_idx + delta).rem_euclid(len)) as usize;
        let next_route = NAV_ROUTES[next_idx];
        if next_route != self.route {
            self.history.push(self.route);
            self.route = next_route;
            crate::trace::note(format!("route->{}", next_route.tag()));
        }
    }

    fn handle_gamepad_action(&mut self, action: GamepadAction, context: &ComponentContext<Self>) {
        match action {
            GamepadAction::PrevTab => self.cycle_nav_route(-1),
            GamepadAction::NextTab => self.cycle_nav_route(1),
            GamepadAction::Back => {
                if self.is_fullscreen {
                    self.exit_fullscreen(context);
                } else {
                    self.navigate_back();
                }
            }
            GamepadAction::TogglePlay => {
                self.update(Message::TogglePlay, context);
            }
            GamepadAction::Refresh => {
                if self.is_player_visible() {
                    self.update(Message::ToggleFullscreen, context);
                } else if self.route == Route::Home {
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
                        let step = self.viewport_layout.cards.cell_height();
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
                            let step = self.viewport_layout.cards.cell_height();
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

    /// 播放意愿落地的唯一入口(UI 按钮/快捷键、SMTC 指令、系统媒体键同一条路径):
    /// native → 系统媒体引擎,mpv → 视频引擎;两条路径共享「末帧重播回零」语义。
    fn apply_playback_want(&mut self, want_play: bool, context: &ComponentContext<Self>) {
        // 先克隆句柄:下面要写 self.playing/self.position,不能一路借用 self。
        if let Some(player) = self.native_sink().cloned() {
            let status = player.status();
            if want_play {
                // 播放结束(锁屏点播放)→ 回零重播。
                if status.finished {
                    let _ = player.seek(0.0);
                    self.position = 0;
                }
                let _ = player.play();
                self.playing = true;
            } else {
                let _ = player.pause();
                self.playing = false;
            }
            self.sync_smtc(context);
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let status = engine.status();
        if !status.has_media() {
            return; // 媒体未加载(解析中/失败):按钮无操作对象。
        }
        if want_play {
            if status.eof {
                // 末帧重播:回零续播。
                engine.seek(0.0);
            }
            engine.play();
            self.playing = true;
        } else {
            engine.pause();
            self.playing = false;
        }
        self.sync_smtc(context);
    }

    /// 窗口级 SMTC 桥懒建(按钮输入通道)。native 模式下也建它:系统媒体会话按
    /// 窗口绑定,媒体键/控制栏按钮的 ButtonPressed 只经这条路进来,而展示状态与
    /// 元数据交给 MediaPlayer 自持控件写 —— 一个管输入、一个管展示,不互相覆盖。
    fn ensure_window_smtc(&mut self, context: &ComponentContext<Self>) {
        if self.smtc.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        match SmtcController::try_init(tx) {
            Some(ctrl) => {
                self.smtc = Some(ctrl);
                self.smtc_rx = Some(Arc::new(Mutex::new(rx)));
                self.listen_smtc(context);
            }
            // 诊断锚:窗口级桥建不起来 = 媒体键/控制栏按钮的输入通道缺失。
            None => eprintln!("[qaq-bx-ui] smtc window bridge 不可用(按钮输入通道缺失)"),
        }
    }

    fn sync_smtc(&mut self, context: &ComponentContext<Self>) {
        // item 先取快照:后面要改 self 的多个字段,不能一路借用 now_playing。
        let item = self.now_playing.clone();
        if let Some(item) = item.as_ref() {
            // v0.3 native:展示状态/时间轴交给 MediaPlayer 自持 SMTC(系统管线驱动),
            // 我们只补元数据;输入通道(媒体键/控制栏按钮)始终走窗口级桥。
            let player = self
                .native_sink()
                .and_then(|p| p.current_player());
            if let Some(player) = player.as_ref() {
                self.ensure_window_smtc(context);
                if self
                    .smtc_native
                    .as_ref()
                    .is_none_or(|bound| !bound.is_bound_to(player))
                {
                    self.smtc_native = NativeSmtc::from_player(player);
                    // 面板重建后 player 变更:元数据与状态锚都得重来一次。
                    self.smtc_bvid = None;
                    self.smtc_playing = None;
                    if let Some(bound) = &self.smtc_native {
                        eprintln!(
                            "[qaq-bx-ui] smtc native: 控件已绑定(status={:?})",
                            bound.playback_status()
                        );
                    }
                }
                if self.smtc_bvid.as_deref() != Some(item.bvid.as_str()) {
                    if let Some(bound) = &self.smtc_native {
                        let cover_bytes = item.cover.as_ref().map(|img| img.as_bytes());
                        bound.update_metadata(&item.title, &item.author, cover_bytes);
                    }
                    self.smtc_bvid = Some(item.bvid.clone());
                }
                // 只读不写:锁屏/控制栏显示的就是系统侧状态,变化时留诊断锚。
                let system_playing = self
                    .smtc_native
                    .as_ref()
                    .and_then(|bound| bound.playback_status())
                    == Some(MediaPlaybackStatus::Playing);
                if self.smtc_playing != Some(system_playing) {
                    self.smtc_playing = Some(system_playing);
                    eprintln!(
                        "[qaq-bx-ui] smtc native: status={}",
                        if system_playing { "Playing" } else { "NotPlaying" }
                    );
                }
                return;
            }
            // 系统媒体引擎在驱动但尚未 attach(装载在途/面板重建中):不写状态,
            // 但仍要把输入通道备好(否则重建期间的媒体键会丢)。
            if self.native_playback {
                self.ensure_window_smtc(context);
                return;
            }
            self.ensure_window_smtc(context);
            if self.smtc_bvid.as_deref() != Some(item.bvid.as_str()) {
                if let Some(smtc) = &self.smtc {
                    let cover_bytes = item.cover.as_ref().map(|img| img.as_bytes());
                    smtc.update_metadata(&item.title, &item.author, cover_bytes);
                }
                self.smtc_bvid = Some(item.bvid.clone());
            }
            if self.smtc_playing != Some(self.playing) {
                if let Some(smtc) = &self.smtc {
                    smtc.set_playing(self.playing);
                }
                self.smtc_playing = Some(self.playing);
            }
        } else if self.smtc_bvid.is_some() || self.smtc_playing.is_some() {
            if let Some(smtc) = &self.smtc {
                smtc.set_stopped();
            }
            if let Some(bound) = &self.smtc_native {
                bound.set_stopped();
            }
            self.smtc_native = None;
            self.smtc_bvid = None;
            self.smtc_playing = None;
        }
    }

    fn is_player_visible(&self) -> bool {
        self.route == Route::Player
            || (self.route == Route::Home && self.home_half_pane)
            || (self.is_fullscreen && self.now_playing.is_some())
    }

    fn can_enter_fullscreen(&self) -> bool {
        self.now_playing.is_some()
            && self.engine.is_some()
            && (self.route == Route::Player || (self.route == Route::Home && self.home_half_pane))
    }

    fn sync_key_route_ctx(&self) {
        self.key_route_ctx
            .set((self.is_player_visible(), self.is_fullscreen));
    }

    fn sync_window_fullscreen(&mut self, fullscreen: bool, context: &ComponentContext<Self>) {
        self.fullscreen_gen = self.fullscreen_gen.wrapping_add(1);
        let gen_id = self.fullscreen_gen;
        let started = context.run_window(move |window| {
            crate::winrt_ext::set_window_fullscreen(window.as_raw(), fullscreen);
            Message::FullscreenSynced {
                gen_id,
                active: fullscreen,
            }
        });
        self.fullscreen_sync_in_flight = started;
    }

    fn enter_fullscreen(&mut self, context: &ComponentContext<Self>) {
        if self.is_fullscreen || !self.can_enter_fullscreen() {
            return;
        }
        self.more_menu_open = false;
        self.search_overlay_open = false;
        self.is_fullscreen = true;
        self.sync_window_fullscreen(true, context);
        self.sync_key_route_ctx();
    }

    fn exit_fullscreen(&mut self, context: &ComponentContext<Self>) {
        if !self.is_fullscreen {
            return;
        }
        self.is_fullscreen = false;
        self.sync_window_fullscreen(false, context);
        self.sync_key_route_ctx();
    }

    fn stop_playback(&mut self, context: &ComponentContext<Self>) {
        self.exit_fullscreen(context);
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
        self.video_loading = false;
        self.current_meta = None;
        self.timer = None;
        self.home_half_pane = false;
        self.comments_session.reset(0, 0);
        self.refresh_comments_snapshot();
        self.history.retain(|r| *r != Route::Player);
        if let Some(engine) = self.engine.take() {
            engine.stop();
            drop(engine);
        }
        self.sync_smtc(context);
    }

    fn reconcile_playback_visibility(&mut self, context: &ComponentContext<Self>) {
        if self.is_fullscreen
            && (self.now_playing.is_none() || self.engine.is_none() || !self.is_player_visible())
        {
            self.exit_fullscreen(context);
        }
        if self.now_playing.is_some() && !self.is_player_visible() {
            self.stop_playback(context);
        }
        self.sync_key_route_ctx();
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
        let max_width =
            (self.viewport_layout.content_width() - 48.0).max(pages::player::HALF_PANE_MIN_WIDTH);
        Some(keyed(
            format!("half-pane:{}", item.bvid),
            View::component::<pages::player::HalfPanePreview>(pages::player::HalfPaneInput {
                item: item.clone(),
                author_avatar,
                my_avatar,
                desc: self.now_playing_desc.clone(),
                playing: self.playing,
                position: self.position,
                duration_secs: self.display_duration(item),
                volume: self.volume,
                danmaku_enabled: self.settings.danmaku_enabled,
                mount_tick: self.mount_tick,
                width: self.settings.half_pane_width,
                max_width,
                max_height: self.viewport_layout.content_height(),
                notice: self.player_notice.clone(),
                loading: self.video_loading,
                engine: engine.clone(),
                comments: self.comments_snapshot.clone(),
                comment_cb,
                on_toggle: context.message(Message::TogglePlay),
                on_seek: context.callback(|value: f64| Message::Seek(value)),
                on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                on_toggle_mute: context.message(Message::ToggleMute),
                on_toggle_danmaku: context.message(Message::ToggleDanmaku),
                on_fullscreen: context.message(Message::EnterFullscreen),
                on_expand: context.message(Message::ExpandHalfPane),
                on_close: context.message(Message::CloseHalfPane),
                on_resize: context.callback(Message::ResizeHalfPane),
            }),
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

    fn navigate_back(&mut self) {
        self.logout_confirm = false;
        self.more_menu_open = false;
        self.search_overlay_open = false;
        if self.viewport_layout.uses_whisper_drilldown()
            && self.route == Route::Login
            && self.whisper_detail_open
        {
            self.whisper_detail_open = false;
            return;
        }
        if let Some(route) = self.history.pop() {
            self.route = route;
            crate::trace::note(format!("route->{} (返回)", route.tag()));
        } else if self.route == Route::Player {
            self.route = Route::Home;
            crate::trace::note("route->home (返回,空历史)");
        }
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
        self.whisper_detail_open = false;
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
                let cursor = if append {
                    self.notify_replies_cursor
                } else {
                    None
                };
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
                let cursor = if append {
                    self.notify_likes_cursor
                } else {
                    None
                };
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
                let cursor = if append {
                    self.notify_systems_cursor
                } else {
                    None
                };
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
                let end_ts = if append {
                    self.notify_sessions_end_ts
                } else {
                    None
                };
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
    res: Result<qaq_bx_core::notify::FeedPage<T>, String>,
    avatar_of: impl Fn(&T) -> &str,
) -> Vec<String> {
    slice.loading = false;
    slice.loaded_once = true;
    match res {
        Ok(page) => {
            let urls: Vec<String> = page
                .items
                .iter()
                .map(|i| avatar_of(i).to_string())
                .collect();
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

/// 挂载脉冲门控:全播放页 **或** 首页半屏抽屉打开时,swapchain 未挂载成功
/// 就要持续重试(250ms 一拍经 `mount_tick` 驱动 `VideoSurface::try_mount`)。
/// 半屏抽屉 route 停留在 Home——旧门控只认 `route == Player`,冷启动时面板
/// metrics 抢在引擎产出 swapchain 之前触发一次就再无第二次机会,首个视频
/// 永久黑屏;`ClosePlayer` 丢弃引擎后重开又新建引擎,同样复发。
fn mount_pulse_active(
    route: Route,
    home_half_pane: bool,
    is_fullscreen: bool,
    engine_attached: bool,
) -> bool {
    (route == Route::Player || home_half_pane || is_fullscreen) && !engine_attached
}

/// F4 首播音画缺失探测:媒体已就绪(时长>0)但解码轨道缺失持续 ≥3s → 报缺哪条轨。
/// 「无画面有声音」= video-format 空;「有画面无声音」= audio-codec 空。
pub(crate) fn missing_media_track(
    video_format: &str,
    audio_codec: &str,
    has_media: bool,
    probing_for: Option<Duration>,
) -> Option<MissingTrack> {
    const PROBE_GRACE: Duration = Duration::from_secs(3);
    if !has_media {
        return None;
    }
    if probing_for.is_none_or(|elapsed| elapsed < PROBE_GRACE) {
        return None;
    }
    let video_missing = video_format.trim().is_empty();
    let audio_missing = audio_codec.trim().is_empty();
    match (video_missing, audio_missing) {
        (true, _) => Some(MissingTrack::Video),
        (false, true) => Some(MissingTrack::Audio),
        (false, false) => None,
    }
}

/// F4 探测结论(驱动自动回退方向)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingTrack {
    /// 视频轨未解出(典型:编码器缺失,如 AV1-only 流)。
    Video,
    /// 音频轨未解出(典型:外挂音轨拉取失败或杜比解码缺失)。
    Audio,
}

/// U3 分P菜单:(cid, "P{page} {part}");少于 2 个分P返回空(不渲染选择器)。
pub(crate) fn pages_menu_from(pages: &[VideoPage]) -> Vec<(u64, String)> {
    let mut menu: Vec<(u64, String)> = pages
        .iter()
        .filter(|page| page.cid > 0)
        .map(|page| {
            let label = if page.part.trim().is_empty() {
                format!("P{}", page.page.max(1))
            } else {
                format!("P{} {}", page.page.max(1), page.part.trim())
            };
            (page.cid, label)
        })
        .collect();
    if menu.len() < 2 {
        menu.clear();
    }
    menu
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
        let res = block_on(client.resolve_full_with_cid(
            &item.bvid,
            item.aid,
            item.cid,
            &item.title,
            i64::from(item.duration_secs),
            max_qn,
            codecs,
        ));
        match res {
            Ok(media) => Message::MediaReady { seq, media },
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

/// 单次 popular 分页抓取（后台线程转展示模型 + LRU 缓存命中预填封面）。
fn spawn_popular_fetch(
    feed_client: &Arc<FeedClient>,
    image_cache: &Arc<ImageCache>,
    pn: u32,
    context: &ComponentContext<AppShell>,
) {
    let client = feed_client.clone();
    let cache = image_cache.clone();
    context.spawn_background(move |_| {
        let res = block_on(client.popular(pn, POPULAR_PAGE_SIZE)).map_err(|e| e.to_string());
        let items = match &res {
            Ok(cards) => cards
                .iter()
                .map(|card| {
                    let mut item = archive_video_item(card);
                    if let Some(url) = &item.pic_url {
                        item.cover = cache.get(url);
                    }
                    item
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        Message::PopularLoaded(match res {
            Ok(_) => Ok(items),
            Err(err) => Err(err),
        })
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
        let initial_layout = ViewportLayout::default();
        // 视口自适应首批逻辑页（默认 1280x800 → 48 条）：先分配完整骨架槽位框架再拉数据。
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
        let home_on_row_active =
            context.callback(|(rev, row_idx, active)| Message::HomeRowActive {
                rev,
                row_idx,
                active,
            });
        let initial_cols = initial_layout.cards.cols;
        let initial_rev = (feed_gen << 16)
            | ((initial_cols as u64) << 12)
            | ((u64::from(initial_layout.cards.compact_card)) << 11)
            | ((initial_layout.cards.card_width.round() as u64) & 0x7FF);
        let home_source = video_card::home_virtual_source(
            &home_slots,
            initial_layout.cards,
            initial_rev,
            None,
            &home_on_open,
            &home_on_hover,
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
            playing: false,
            position: 0,
            mount_tick: 0,
            volume,
            volume_before_mute: if volume == 0 { 80 } else { volume },
            settings,
            engine_duration: None,
            player_notice: None,
            video_loading,
            engine,
            native_player: None,
            native_enabled: std::env::var("QAQ_BX_NATIVE").as_deref() == Ok("1"),
            native_stage: None,
            native_playback: false,
            current_meta: None,
            danmaku_first_seg: 0,
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
            fullscreen_gen: 0,
            fullscreen_sync_in_flight: false,
            key_route_ctx,
            global_key_handler,
            pending_grid_rebuild: false,
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
        match message {
            Message::Back => {
                if self.is_fullscreen {
                    self.exit_fullscreen(context);
                } else {
                    self.navigate_back();
                }
            }
            Message::NavTagSelected(tag) => {
                self.exit_fullscreen(context);
                self.logout_confirm = false;
                self.more_menu_open = false;
                self.search_overlay_open = false;
                if let Some(route) = tag.as_deref().and_then(Route::from_tag)
                    && route != self.route
                {
                    self.whisper_detail_open = false;
                    self.history.push(self.route);
                    self.route = route;
                    crate::trace::note(format!("route->{} (页签点击)", route.tag()));
                    if route == Route::Login {
                        self.ensure_account_loaded(context);
                    }
                    if route == Route::Popular {
                        self.ensure_popular(context);
                    }
                    if route == Route::Live {
                        self.ensure_live(context);
                    }
                }
            }
            Message::PopularMore => {
                self.fetch_popular(context);
            }
            Message::PopularLoaded(res) => {
                self.absorb_popular(res, context);
            }
            Message::RelatedLoaded { seq, bvid, res } => {
                self.absorb_related(seq, bvid, res, context);
            }
            Message::RelatedRetry => {
                if let Some(item) = self.now_playing.clone() {
                    self.fetch_related(self.playback_gen, item.bvid, context);
                }
            }
            Message::PaneOpenChanged(open) => self.pane_open = open,
            Message::TogglePane => self.pane_open = !self.pane_open,
            #[cfg(feature = "selftest")]
            Message::SelfTestSync => {}
            Message::DemoTick => {
                self.handle_demo_tick(context);
            }
            Message::SearchDraftChanged(text) => {
                self.search_state.draft = text;
            }
            Message::SearchSubmitted => {
                self.submit_search(context);
            }
            Message::SearchResults {
                seq,
                query,
                items,
                total,
            } => {
                if seq != self.search_seq {
                    return;
                }
                let mut owned = Vec::with_capacity(items.len());
                for item in items {
                    if item.cover.is_none()
                        && let Some(url) = &item.pic_url
                    {
                        // 复用首页封面补拉(缓存命中由后台线程已挂;feed_epoch 未变即生效)。
                        self.spawn_cover_fetch(
                            item.bvid.clone(),
                            url.clone(),
                            self.feed_gen,
                            context,
                        );
                    }
                    self.search_index.insert(item.bvid.clone(), item.clone());
                    owned.push(item);
                }
                self.search_state.videos = owned;
                self.search_state.total = total;
                self.search_state.query = query;
                self.search_state.loading = false;
            }
            Message::SearchFailed { seq, error } => {
                if seq != self.search_seq {
                    return;
                }
                self.search_state.loading = false;
                self.search_state.error = Some(error);
            }
            Message::SearchDefaultReady(word) => {
                self.search_default = word.clone();
                self.search_state.default_word = word;
            }
            Message::SearchPickDefault => {
                // U2:默认词填充草稿并立即提交(草稿先入单源,提交复用同一路径)。
                if let Some(word) = self.search_default.clone() {
                    self.search_state.draft = word;
                    self.update(Message::SearchSubmitted, context);
                }
            }
            Message::OpenVideo(bvid) => {
                self.open_video(&bvid, context);
                self.sync_smtc(context);
            }
            Message::OpenVideoTarget(target) => {
                self.open_video_target(target, context);
                self.sync_smtc(context);
            }
            Message::TogglePlay => {
                let want_play = !self.playing;
                self.apply_playback_want(want_play, context);
                self.sync_smtc(context);
            }
            Message::Seek(value) => {
                self.position = value.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                if let Some(player) = self.native_sink() {
                    let _ = player.seek(value.max(0.0));
                    return;
                }
                if let Some(engine) = &self.engine {
                    engine.seek(value.max(0.0));
                }
            }
            Message::SetVolume(value) => {
                let vol = value.round().clamp(0.0, 100.0) as u32;
                if vol > 0 {
                    self.volume_before_mute = vol;
                }
                if vol != self.volume {
                    self.volume = vol;
                    if let Some(player) = self.native_sink() {
                        let _ = player.set_volume(f64::from(self.volume));
                    } else if let Some(engine) = &self.engine {
                        engine.set_volume(f64::from(self.volume));
                    }
                }
                if self.settings.volume != vol {
                    self.settings.volume = vol;
                    self.schedule_settings_save(context);
                }
            }
            Message::ToggleMute => {
                let target = if self.volume == 0 {
                    self.volume_before_mute.max(10)
                } else {
                    self.volume_before_mute = self.volume;
                    0
                };
                self.volume = target;
                if let Some(player) = self.native_sink() {
                    let _ = player.set_volume(f64::from(target));
                } else if let Some(engine) = &self.engine {
                    engine.set_volume(f64::from(target));
                }
                if self.settings.volume != target {
                    self.settings.volume = target;
                    self.schedule_settings_save(context);
                }
            }
            Message::ToggleDanmaku => {
                self.settings.danmaku_enabled = !self.settings.danmaku_enabled;
                self.danmaku_config(
                    self.settings.danmaku_enabled,
                    self.settings.danmaku_opacity,
                    self.settings.danmaku_max_lanes,
                    &self.settings.danmaku_font_family,
                    self.settings.danmaku_font_scale,
                );
                self.schedule_settings_save(context);
            }
            Message::EnterFullscreen => {
                self.enter_fullscreen(context);
            }
            Message::ExitFullscreen => {
                self.exit_fullscreen(context);
            }
            Message::ToggleFullscreen => {
                if self.is_fullscreen {
                    self.exit_fullscreen(context);
                } else {
                    self.enter_fullscreen(context);
                }
            }
            Message::FullscreenSynced { gen_id, active } => {
                if gen_id == self.fullscreen_gen {
                    self.fullscreen_sync_in_flight = false;
                    if !active {
                        self.is_fullscreen = false;
                    }
                }
            }
            Message::FullscreenProbeResult { gen_id, active } => {
                if gen_id == self.fullscreen_gen && self.is_fullscreen && !active {
                    self.is_fullscreen = false;
                }
            }
            Message::PlayerHotkey(hotkey) => match hotkey {
                pages::fullscreen::PlayerHotkey::ToggleFullscreen => {
                    if self.is_fullscreen {
                        self.exit_fullscreen(context);
                    } else {
                        self.enter_fullscreen(context);
                    }
                }
                pages::fullscreen::PlayerHotkey::ExitFullscreen => {
                    self.exit_fullscreen(context);
                }
                pages::fullscreen::PlayerHotkey::TogglePlay => {
                    self.update(Message::TogglePlay, context);
                }
                pages::fullscreen::PlayerHotkey::SeekDelta(delta) => {
                    if self.is_player_visible() {
                        let max_secs = self
                            .now_playing
                            .as_ref()
                            .map(|item| self.display_duration(item))
                            .unwrap_or(0);
                        let next = if delta < 0 {
                            self.position.saturating_sub(delta.unsigned_abs())
                        } else {
                            self.position.saturating_add(delta as u32)
                        };
                        let clamped = if max_secs > 0 {
                            next.min(max_secs)
                        } else {
                            next
                        };
                        self.update(Message::Seek(f64::from(clamped)), context);
                    }
                }
                pages::fullscreen::PlayerHotkey::VolumeDelta(delta) => {
                    if self.is_player_visible() {
                        let next = if delta < 0 {
                            self.volume.saturating_sub(delta.unsigned_abs())
                        } else {
                            (self.volume + delta as u32).min(100)
                        };
                        self.update(Message::SetVolume(f64::from(next)), context);
                    }
                }
                pages::fullscreen::PlayerHotkey::ToggleMute => {
                    if self.is_player_visible() {
                        self.update(Message::ToggleMute, context);
                    }
                }
                pages::fullscreen::PlayerHotkey::ToggleDanmaku => {
                    if self.is_player_visible() {
                        self.update(Message::ToggleDanmaku, context);
                    }
                }
            },
            Message::PositionTick => {
                // v0.3:native 引擎状态单源(无 has_media/missing_track 探测)。
                if let Some(player) = self.native_sink() {
                    let status = player.status();
                    // 心跳诊断锚(F4 风格):播放态每 30s 一行,验证 pos 推进/时长有效。
                    if status.playing
                        && (status.position_secs as u64).is_multiple_of(30)
                        && status.position_secs % 1.0 < 0.5
                    {
                        eprintln!(
                            "[qaq-bx-ui] native heartbeat pos={:.1} dur={:.1}",
                            status.position_secs, status.duration_secs
                        );
                    }
                    // 装载在途(工作线程建源)保持遮罩,别用旧会话的状态提前解除。
                    if !status.loading && (!status.playing || status.position_secs > 0.0) {
                        self.video_loading = false;
                    }
                    self.position = status
                        .position_secs
                        .round()
                        .clamp(0.0, f64::from(u32::MAX)) as u32;
                    self.playing = status.playing && !status.loading;
                    if status.duration_secs > 0.0 && !status.loading {
                        self.engine_duration = Some(status.duration_secs);
                    }
                    // 弹幕透明层时基:节拍喂真实播放位置与播放态(暂停即冻结);
                    // 层内用墙钟外推到满刷新率,与 mpv 通道的弹幕手感一致。
                    if let Some(stage) = self.stage_sink() {
                        stage.set_time(status.position_secs);
                        stage.set_playing(status.playing);
                    }
                    if self.player_notice.is_none()
                        && let Some(reason) = &status.error
                    {
                        self.player_notice = Some(reason.clone());
                        self.video_loading = false;
                    }
                    // 挂载节拍:engine.attached 恒 false(mpv idle),持续驱动
                    // NativeSurface 重试注入(500ms 节流一次足够)。
                    if mount_pulse_active(
                        self.route,
                        self.home_half_pane,
                        self.is_fullscreen,
                        false,
                    ) {
                        self.mount_tick = self.mount_tick.wrapping_add(1);
                    }
                    self.sync_smtc(context);
                    if self.now_playing.is_some() {
                        self.timer =
                            Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
                    } else {
                        self.timer = None;
                    }
                    return;
                }
                // 引擎快照 → UI 单源(乐观更新的 TogglePlay/Seek 在此被真实值校正)。
                // Arc 克隆出借用(F4 回退需要 &mut self)。
                let engine = self.engine.clone();
                if let Some(engine) = engine {
                    let status = engine.status();
                    if status.has_media() {
                        self.video_loading = false;
                        self.position =
                            status.time_pos.round().clamp(0.0, f64::from(u32::MAX)) as u32;
                        self.playing = !status.pause && !status.eof;
                        if status.duration > 0.0 {
                            self.engine_duration = Some(status.duration);
                        }
                    }
                    // F4 首播音画缺失探测:媒体就绪后计时,持续缺轨 3s 自动回退。
                    if status.has_media() && self.media_probe_at.is_none() {
                        self.media_probe_at = Some(Instant::now());
                    }
                    if let Some(track) = missing_media_track(
                        &status.video_format,
                        &status.audio_codec,
                        status.has_media(),
                        self.media_probe_at.map(|at| at.elapsed()),
                    ) {
                        // 触发一次即清探测点,回退重载会重置。
                        self.media_probe_at = None;
                        match track {
                            MissingTrack::Video => {
                                if !self.video_fallback_done {
                                    self.video_fallback_done = true;
                                    self.apply_video_fallback();
                                }
                            }
                            MissingTrack::Audio => {
                                if !self.audio_fallback_done {
                                    self.audio_fallback_done = true;
                                    self.apply_audio_fallback();
                                }
                            }
                        }
                    }
                    // 仅当 SwapChain 尚未挂载成功时按 250ms 递增驱动 VideoSurface::try_mount()。
                    if mount_pulse_active(
                        self.route,
                        self.home_half_pane,
                        self.is_fullscreen,
                        engine.attached(),
                    ) {
                        self.mount_tick = self.mount_tick.wrapping_add(1);
                    }
                    // 引擎错误(初始化/挂载/设备级)一次性上屏。
                    if self.player_notice.is_none()
                        && let Some(reason) = engine.error()
                    {
                        self.player_notice = Some(reason);
                        self.video_loading = false;
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
            Message::MediaReady { seq, media } => {
                if seq != self.playback_gen {
                    return; // 过期解析结果直接丢弃，防止切视频后串台覆盖
                }
                self.pending_open_preresolve = None;
                self.apply_resolved_media(media, seq, context);
            }
            Message::SelectQuality(qn) => {
                if qn == self.current_qn {
                    return;
                }
                let Some(dash) = self.playback_dash.clone() else {
                    return;
                };
                let Some(choice) = select_stream_by_qn(&dash, qn, self.settings.codec.priority())
                else {
                    return;
                };
                // 定点换流:记住当前进度与播放态,mpv `start` 直达,弹幕/相关推荐不动。
                // v0.3 spike:native 走重载 + SetPosition。
                if self.native_active() {
                    let start = self.native_position();
                    if self.native_load_choice(&choice, self.playing, start) {
                        self.current_qn = qn;
                        self.current_video_url = choice.video.base_url.clone();
                        self.current_audio_url = choice.audio.base_url.clone();
                        self.media_probe_at = Some(Instant::now());
                        self.player_notice = None;
                        return;
                    }
                }
                let start = self
                    .engine
                    .as_ref()
                    .map(|engine| engine.status().time_pos)
                    .unwrap_or(0.0);
                if let Some(engine) = &self.engine {
                    engine.load_with_options(
                        &choice.video.base_url,
                        Some(&choice.audio.base_url),
                        start,
                        self.playing,
                    );
                }
                self.current_qn = qn;
                self.current_video_url = choice.video.base_url.clone();
                self.current_audio_url = choice.audio.base_url.clone();
                self.media_probe_at = Some(Instant::now());
                self.player_notice = None;
            }
            Message::SelectPage(cid) => {
                // U3 分P切换:重取 playurl(cid) → 定点换流 + 弹幕/进度联动。
                if cid == 0 || cid == self.current_cid {
                    return;
                }
                let Some(item) = self.now_playing.clone() else {
                    return;
                };
                let nav = self.auth_service.nav_client();
                let (max_qn, codecs) = resolve_params_for(&self.settings, &nav);
                let auth = self.auth_service.clone();
                let gen_id = self.playback_gen;
                context.spawn_background(move |_| {
                    let client = PlayerClient::with_nav(auth.http(), auth.credential_provider(), nav);
                    let result = block_on(client.resolve_full_with_cid(
                        &item.bvid,
                        item.aid,
                        cid,
                        &item.title,
                        i64::from(item.duration_secs),
                        max_qn,
                        codecs,
                    ))
                    .map_err(|err| err.to_string());
                    Message::PageStreamReady { gen_id, cid, result }
                });
            }
            Message::PageStreamReady { gen_id, cid, result } => {
                if gen_id != self.playback_gen {
                    return;
                }
                match result {
                    Ok(media) => {
                        let ResolvedMedia { choice, dash, .. } = media;
                        // v0.3 spike:native 分支(重载 + SetPosition;native_position
                        // 保持的是旧分P末进度,分P语义本就回零,可接受)。
                        if self.native_active() {
                            if self.native_load_choice(&choice, self.playing, 0.0) {
                                self.current_cid = cid;
                                self.current_qn = choice.video.id;
                                self.current_video_url = choice.video.base_url;
                                self.current_audio_url = choice.audio.base_url;
                                self.playback_dash = Some(dash.clone());
                                self.quality_qns = quality_menu(&dash.video, 0);
                                self.media_probe_at = Some(Instant::now());
                                self.player_notice = None;
                                if let Some(meta) = &mut self.current_meta {
                                    meta.cid = cid;
                                }
                                // 换分P:旧 P 的弹幕先清层(与 mpv 分支同语义)。
                                self.danmaku_replace(Vec::new());
                                self.fetch_danmaku(gen_id, 0.0, context);
                                return;
                            }
                        }
                        let start = self
                            .engine
                            .as_ref()
                            .map(|engine| engine.status().time_pos)
                            .unwrap_or(0.0);
                        if let Some(engine) = &self.engine {
                            engine.load_with_options(
                                &choice.video.base_url,
                                Some(&choice.audio.base_url),
                                start,
                                self.playing,
                            );
                        }
                        self.current_cid = cid;
                        self.current_qn = choice.video.id;
                        self.current_video_url = choice.video.base_url;
                        self.current_audio_url = choice.audio.base_url;
                        self.playback_dash = Some(dash.clone());
                        self.quality_qns = quality_menu(&dash.video, 0);
                        self.media_probe_at = Some(Instant::now());
                        self.player_notice = None;
                        // 弹幕跟随分P cid 重拉(评论区仍按 aid 不动);预存为
                        // 增量追加,换流前先清空旧 P 的弹幕层。
                        self.danmaku_replace(Vec::new());
                        if let Some(meta) = &mut self.current_meta {
                            meta.cid = cid;
                        }
                        self.fetch_danmaku(gen_id, start, context);
                    }
                    Err(reason) => {
                        self.player_notice = Some(format!("切换分P失败:{reason}"));
                    }
                }
            }
            Message::LiveRoomsLoaded { page, res } => {
                self.absorb_live_rooms(page, res, context);
            }
            Message::LiveCoverReady { url, image } => {
                self.live_cover_inflight.remove(&url);
                if let Some(img) = image {
                    for card in &mut self.live.cards {
                        if card.room.cover == url && card.cover.is_none() {
                            card.cover = Some(img.clone());
                        }
                    }
                }
            }
            Message::LiveRoomSelected(roomid, title, uname) => {
                self.start_live_playback(roomid, title, uname, context);
                self.sync_smtc(context);
            }
            Message::LiveReady { gen_id, result } => {
                if gen_id != self.playback_gen {
                    return;
                }
                match result {
                    Ok(url) => {
                        if let Some(engine) = &self.engine {
                            engine.set_volume(f64::from(self.volume));
                            engine.load_with_options(&url, None, 0.0, self.settings.autoplay);
                        }
                        self.playing = self.settings.autoplay;
                        self.current_video_url = url;
                        self.media_probe_at = Some(Instant::now());
                    }
                    Err(reason) => {
                        self.player_notice = Some(format!("直播间打开失败:{reason}"));
                        self.video_loading = false;
                    }
                }
            }
            Message::LiveMore => {
                self.fetch_live_rooms(context);
            }
            Message::CommentDraftChanged(text) => {
                self.comment_draft = text;
                if self.composer_notice.is_some() {
                    self.composer_notice = None;
                }
            }
            Message::SubmitComment => {
                // U1 登录门控:未登录直接行内提示,不发请求。
                if !matches!(self.auth, AuthState::LoggedIn(_)) {
                    self.composer_notice = Some("请先登录后再发布评论".to_string());
                    return;
                }
                let draft = self.comment_draft.trim().to_string();
                if draft.is_empty() {
                    self.composer_notice = Some("评论内容不能为空".to_string());
                    return;
                }
                let Some(meta) = self.current_meta.clone() else {
                    self.composer_notice = Some("视频信息尚未就绪,稍后再试".to_string());
                    return;
                };
                self.composer_notice = None;
                let reply_client = self.reply_client.clone();
                let gen_id = self.playback_gen;
                context.spawn_background(move |_| {
                    let result = block_on(reply_client.add(meta.aid, &draft))
                        .map_err(|err| err.to_string());
                    Message::CommentSubmitted { gen_id, result }
                });
            }
            Message::CommentSubmitted { gen_id, result } => {
                if gen_id != self.playback_gen {
                    return;
                }
                match result {
                    Ok(_) => {
                        // 发布成功:清草稿 + 回到首页重拉最新评论。
                        self.comment_draft.clear();
                        self.composer_notice = None;
                        let sort = self.comments_session.sort;
                        self.comments_session.bucket_mut(sort).cursor = None;
                        self.comments_session.bucket_mut(sort).has_more = true;
                        self.fetch_comments(sort, false, context);
                    }
                    Err(reason) => {
                        self.composer_notice = Some(format!("发布失败:{reason}"));
                    }
                }
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
                let need_fetch =
                    if let Some(entry) = self.comments_session.sub_replies.get_mut(&rpid) {
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
            Message::DanmakuPart { seq, index, elems } => {
                if seq != self.playback_gen {
                    return;
                }
                // 转引擎镜像类型(避免 qaq-bx-video 依赖 qaq-bx-core),增量追加;
                // 引擎线程归并回 progress 升序后在视频帧上叠画(暂停即冻结)。
                let items = elems
                    .into_iter()
                    .map(|e| OverlayDanmaku {
                        progress_ms: e.progress_ms,
                        mode: e.mode,
                        fontsize: e.fontsize,
                        color: e.color,
                        content: e.content,
                        mid_hash: e.mid_hash,
                        weight: e.weight,
                    })
                    .collect();
                self.danmaku_append(items);
                // 预存链:驱动下一段(全部段到货后自然收束)。
                if let Some(next) = self.next_danmaku_index(index) {
                    self.fetch_danmaku_segment(seq, next, context);
                }
            }
            Message::DanmakuFailed { seq, index, reason } => {
                // 弹幕拉取失败不影响播放;静默跳过该段,预存链继续补齐后续段。
                if seq == self.playback_gen {
                    eprintln!("[qaq-bx-ui] 弹幕分段 {index} 拉取失败(不影响播放): {reason}");
                    if let Some(next) = self.next_danmaku_index(index) {
                        self.fetch_danmaku_segment(seq, next, context);
                    }
                }
            }
            Message::MediaFailed { seq, reason } => {
                if seq != self.playback_gen {
                    return;
                }
                // 直点路径命中试看降级:登记不可播(回到首页时卡面带角标)。
                if is_trial_downgrade_reason(&reason)
                    && let Some(bvid) = self.now_playing.as_ref().map(|i| i.bvid.clone())
                {
                    self.unplayable.entry(bvid).or_insert(reason.clone());
                    self.rebuild_home_source();
                }
                self.player_notice = Some(format!("视频解析失败:{reason}"));
                self.playing = false;
                self.video_loading = false;
            }
            Message::ExpandHalfPane => {
                self.home_half_pane = false;
                if self.route != Route::Player {
                    self.history.push(self.route);
                    self.route = Route::Player;
                    crate::trace::note("route->player (半屏展开)");
                }
            }
            Message::CloseHalfPane => {
                self.stop_playback(context);
            }
            Message::ResizeHalfPane(width) => {
                let clamped = width.clamp(
                    pages::player::HALF_PANE_MIN_WIDTH,
                    pages::player::HALF_PANE_MAX_WIDTH,
                );
                if (self.settings.half_pane_width - clamped).abs() >= 1.0 {
                    self.settings.half_pane_width = clamped;
                    self.schedule_settings_save(context);
                }
            }
            Message::SmtcAction(Some(cmd)) => {
                // 诊断锚:系统媒体键 / 控制栏按钮确实打到了本进程。
                eprintln!("[qaq-bx-ui] smtc action: {cmd:?}");
                // 系统媒体控件/媒体键的播放暂停请求统一走同一入口:
                // native → MediaPlayer,mpv → 视频引擎(两条输入路径不两套语义)。
                let want_play = matches!(cmd, SmtcCommand::Play);
                self.apply_playback_want(want_play, context);
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
                self.home_state.grid_cols = self.viewport_layout.cards.cols;
                self.home_state.page_count = 0;
                self.home_state.video_count = 0;
                self.home_state.reached_end = false;
                self.home_state.error = None;

                let target = feed_pager::logical_page_target_for(
                    self.viewport_layout.cards,
                    self.home_viewport_h,
                );
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

                let vis_rows =
                    feed_pager::visible_rows_for(self.viewport_layout.cards, self.home_viewport_h);
                let evict_stride = vis_rows.saturating_mul(2).max(4);
                if self.home_anchor_row.abs_diff(self.home_last_evict_row) >= evict_stride {
                    self.sync_offscreen_covers(context);
                }

                if active {
                    if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                        eprintln!(
                            "[qaq-bx-ui] row active: row={}/{} anchor={} active_cnt={} slots={} loading={}",
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
                self.search_overlay_open = false;
                self.more_menu_open = !self.more_menu_open;
            }
            Message::CloseMoreMenu => {
                self.more_menu_open = false;
            }
            Message::ToggleSearchOverlay => {
                self.more_menu_open = false;
                self.search_overlay_open = !self.search_overlay_open;
            }
            Message::CloseSearchOverlay => {
                self.search_overlay_open = false;
            }
            Message::NavigateCompact(target) => {
                self.exit_fullscreen(context);
                self.more_menu_open = false;
                self.search_overlay_open = false;
                self.home_half_pane = false;
                self.logout_confirm = false;
                if self.route != target {
                    self.whisper_detail_open = false;
                    self.history.push(self.route);
                    self.route = target;
                    crate::trace::note(format!("route->{} (菜单导航)", target.tag()));
                    if target == Route::Login && self.auth_gen > 0 {
                        self.ensure_account_loaded(context);
                    }
                    if target == Route::Popular {
                        self.ensure_popular(context);
                    }
                    if target == Route::Live {
                        self.ensure_live(context);
                    }
                }
            }
            Message::WindowResized(size) => {
                let prev_cards = self.viewport_layout.cards;
                let next_layout = ViewportLayout::from_window_size(size.width, size.height);
                self.viewport_layout = next_layout;
                self.home_viewport_w = next_layout.content_width();
                self.home_viewport_h = next_layout.content_height();

                if !next_layout.is_mobile() {
                    self.more_menu_open = false;
                }
                if next_layout.shows_inline_title_search() {
                    self.search_overlay_open = false;
                }
                if !next_layout.uses_whisper_drilldown() {
                    self.whisper_detail_open = false;
                }
                if !self.is_fullscreen && next_layout.is_mobile() && self.home_half_pane {
                    self.home_half_pane = false;
                    if self.now_playing.is_some() && self.route == Route::Home {
                        self.history.push(Route::Home);
                        self.route = Route::Player;
                        crate::trace::note("route->player (移动布局半屏收起)");
                    }
                }
                if self.is_fullscreen && !self.fullscreen_sync_in_flight {
                    let gen_id = self.fullscreen_gen;
                    let _ = context.run_window(move |window| {
                        let active =
                            crate::winrt_ext::is_window_fullscreen(window.as_raw()).unwrap_or(true);
                        Message::FullscreenProbeResult { gen_id, active }
                    });
                }
                if next_layout.should_rebuild_grid(prev_cards) {
                    // 轻量字段即时更新(探针/分页目标依赖);重量级 VirtualSource
                    // 重建去抖 150ms(P1:拖拽期间每 2 DIP 一重建是卡顿主因)。
                    self.home_state.grid_cols = next_layout.cards.cols;
                    self.pending_grid_rebuild = true;
                    self.grid_rebuild_timer =
                        Some(context.set_local_timeout(GRID_REBUILD_DEBOUNCE, || {
                            Message::WindowResizeSettled
                        }));
                }
            }
            Message::WindowResizeSettled => {
                self.grid_rebuild_timer = None;
                if self.pending_grid_rebuild {
                    self.pending_grid_rebuild = false;
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
                    feed_pager::logical_page_target_for(
                        self.viewport_layout.cards,
                        self.home_viewport_h,
                    )
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
                    let target =
                        self.home_pending_skeletons
                            .max(feed_pager::logical_page_target_for(
                                self.viewport_layout.cards,
                                self.home_viewport_h,
                            ));
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
                if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                    eprintln!(
                        "[qaq-bx-ui] batch loaded: total_videos={}, page_count={}, exhausted={}",
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
                    let target =
                        self.home_pending_skeletons
                            .max(feed_pager::logical_page_target_for(
                                self.viewport_layout.cards,
                                self.home_viewport_h,
                            ));
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
                    self.viewport_layout.cards,
                    self.home_viewport_h,
                    self.home_slots.len(),
                    COVER_WINDOW_BUFFER_SCREENS,
                );
                let batch = std::mem::take(&mut self.pending_covers);
                let mut changed = false;
                for (bvid, image) in batch {
                    let mut applied = false;
                    if let Some(&idx) = self.home_slot_by_bvid.get(&bvid)
                        && keep.contains(&idx)
                        && let Some(HomeSlot::Video { item, .. }) = self.home_slots.get_mut(idx)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image.clone());
                        changed = true;
                        applied = true;
                    }
                    // 搜索结果封面兜底回填(首页未消费的封面落到搜索卡)。
                    if !applied
                        && let Some(item) =
                            self.search_state.videos.iter_mut().find(|v| v.bvid == bvid)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image.clone());
                        applied = true;
                    }
                    // 热门/相关推荐封面兜底回填（同一有界 LRU，8GB 内存约束内共享字节）。
                    if !applied
                        && let Some(item) = self.popular.items.iter_mut().find(|v| v.bvid == bvid)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image.clone());
                        applied = true;
                    }
                    if !applied
                        && let Some(item) = self.related.items.iter_mut().find(|v| v.bvid == bvid)
                        && item.cover.is_none()
                    {
                        item.cover = Some(image);
                    }
                }
                if changed {
                    self.rebuild_home_source();
                }
            }
            Message::PreResolveVideo(bvid) => {
                // 已判定不可播(充电/付费未解锁):不再重复发 playurl。
                if self.unplayable.contains_key(&bvid) {
                    return;
                }
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
                    let res = block_on(client.resolve_full_with_cid(
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
                let matched_open = self.pending_open_preresolve.as_ref().is_some_and(
                    |(pending_bvid, gen_id, pending_key)| {
                        pending_bvid == &bvid && *gen_id == self.playback_gen && *pending_key == key
                    },
                );
                if matched_open {
                    self.pending_open_preresolve = None;
                }
                match result {
                    Ok(media) => {
                        if std::env::var_os("QAQ_BX_TRACE_HOME").is_some() {
                            eprintln!(
                                "[qaq-bx-ui] preresolved cached (no autoplay): bvid={}, qn={}, route={:?}",
                                bvid, media.choice.video.id, self.route
                            );
                        }
                        if matched_open {
                            self.apply_resolved_media(media.clone(), self.playback_gen, context);
                        }
                        self.cache_preresolved(bvid, key, media);
                    }
                    Err(reason) => {
                        // 试看降级(充电/付费未解锁):登记不可播并刷新卡面角标。
                        if is_trial_downgrade_reason(&reason) {
                            self.unplayable.entry(bvid.clone()).or_insert(reason.clone());
                            self.rebuild_home_source();
                        }
                        if matched_open {
                            self.player_notice = Some(format!("视频解析失败:{reason}"));
                            self.playing = false;
                            self.video_loading = false;
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
                    if let Some(player) = self.native_sink() {
                        let _ = player.set_volume(f64::from(next.volume));
                    } else if let Some(engine) = &self.engine {
                        engine.set_volume(f64::from(next.volume));
                    }
                }
                if next.hardware_decode != self.settings.hardware_decode
                    && let Some(engine) = &self.engine
                {
                    engine.set_hardware_decode(next.hardware_decode);
                }
                let danmaku_style_changed = next.danmaku_enabled != self.settings.danmaku_enabled
                    || (next.danmaku_opacity - self.settings.danmaku_opacity).abs() > f32::EPSILON
                    || next.danmaku_max_lanes != self.settings.danmaku_max_lanes
                    || next.danmaku_font_family != self.settings.danmaku_font_family
                    || (next.danmaku_font_scale - self.settings.danmaku_font_scale).abs()
                        > f32::EPSILON;
                if danmaku_style_changed {
                    // 样式下发按当前承接方分流(native 透明层 / mpv 视频引擎)。
                    self.danmaku_config(
                        next.danmaku_enabled,
                        next.danmaku_opacity,
                        next.danmaku_max_lanes,
                        &next.danmaku_font_family,
                        next.danmaku_font_scale,
                    );
                }
                if danmaku_filter_from(&next) != danmaku_filter_from(&self.settings) {
                    // 规则变化 → 承接线程编译并对已缓存弹幕全量重滤(热更新)。
                    self.danmaku_filter(danmaku_filter_from(&next));
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
                        | LoginFlowState::Expired { qr } => {
                            LoginFlowState::Expired { qr: qr.clone() }
                        }
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
                self.exit_fullscreen(context);
                self.logout_confirm = false;
                self.more_menu_open = false;
                self.search_overlay_open = false;
                self.whisper_detail_open = false;
                if self.route != Route::Login {
                    self.history.push(self.route);
                    self.route = Route::Login;
                    crate::trace::note("route->login (账号入口)");
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
                self.whisper_detail_open = false;
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
                self.whisper_detail_open = true;
                if !self
                    .whisper_threads
                    .contains_key(&(talker_id, session_type))
                {
                    self.fetch_whisper_thread(talker_id, session_type, context);
                }
            }
            Message::CloseWhisperDetail => {
                self.whisper_detail_open = false;
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
        self.reconcile_playback_visibility(context);
        self.refresh_probe();
    }

    fn view(&self, _input: &ShellInput, context: &mut ViewContext<Self>) -> View {
        context.window_title(crate::notifications::APP_DISPLAY_NAME);
        let mut visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .client_size(layout::DEFAULT_WINDOW_WIDTH, layout::DEFAULT_WINDOW_HEIGHT)
            .constraints(WindowConstraints {
                min_width: Some(layout::MIN_WINDOW_WIDTH),
                min_height: Some(layout::MIN_WINDOW_HEIGHT),
                max_width: None,
                max_height: None,
            });
        if self.demo.is_none()
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
        context.on_window_size(context.callback(Message::WindowResized));
        self.sync_key_route_ctx();

        let layout = self.viewport_layout;
        let is_mobile = layout.is_mobile();
        if self.is_fullscreen
            && let (Some(item), Some(engine)) = (self.now_playing.as_ref(), self.engine.as_ref())
        {
            let fs_view = View::component::<pages::fullscreen::FullscreenPlayer>(
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
                    window_width: layout.window_width,
                    engine: engine.clone(),
                    native: self.native_sink().cloned(),
                    stage: self.stage_sink().cloned(),
                    on_toggle: context.message(Message::TogglePlay),
                    on_seek: context.callback(|value: f64| Message::Seek(value)),
                    on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                    on_toggle_mute: context.message(Message::ToggleMute),
                    on_toggle_danmaku: context.message(Message::ToggleDanmaku),
                    on_select_quality: context.callback(Message::SelectQuality),
                    on_exit_fullscreen: context.message(Message::ExitFullscreen),
                },
            );
            return Border::new()
                .on_preview_key_down(self.global_key_handler.clone())
                .content(Grid::new().keyed_children([keyed("fs-root", fs_view)]))
                .into();
        }
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
                        on_toggle_mute: context.message(Message::ToggleMute),
                        on_toggle_danmaku: context.message(Message::ToggleDanmaku),
                        on_select_quality: context.callback(Message::SelectQuality),
                        on_select_page: context.callback(Message::SelectPage),
                        on_enter_fullscreen: context.message(Message::EnterFullscreen),
                        demo_override: self.demo.as_ref().and_then(|d| d.player_override),
                    })
                }
                _ => self.home_view(context),
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
            pages::settings::settings_page(
                &self.settings,
                is_vip,
                layout,
                self.settings_notice.as_deref(),
                &on_change,
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
            )
        } else if self.route == Route::Popular {
            pages::popular::popular_page(
                &self.popular,
                layout,
                &on_open,
                &context.message(Message::PopularMore),
            )
        } else {
            router::route(self.route, layout, on_open.clone())
        };

        // 切换目的地时退役原生控件（含播放页换视频的场景）。
        let key = if self.route == Route::Player {
            format!(
                "player:{}",
                self.now_playing
                    .as_ref()
                    .map_or(String::new(), |item| item.bvid.clone())
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

        let can_back = can_navigate_back(
            layout,
            self.route == Route::Login,
            self.history.len(),
            self.whisper_detail_open,
        );
        let title_bar = chrome::build_title_bar(
            layout,
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

        if is_mobile {
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
                } else {
                    vec![keyed(key, content)]
                };
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
                .children((title_bar, content_host, bottom_bar.grid_row(2)));

            return Border::new()
                .on_preview_key_down(self.global_key_handler.clone())
                .content(Grid::new().keyed_children([keyed("shell-root", shell_root)]))
                .into();
        }

        let navigation = NavigationView::new()
            .pane_display_mode(NavigationViewPaneDisplayMode::LeftCompact)
            .is_pane_toggle_button_visible(false)
            .is_back_button_visible(NavigationViewBackButtonVisible::Collapsed)
            .is_settings_visible(false)
            .always_show_header(false)
            .pane_title("QAQ-Bx")
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
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .children((title_bar, navigation));

        Border::new()
            .on_preview_key_down(self.global_key_handler.clone())
            .content(Grid::new().keyed_children([keyed("shell-root", shell_root)]))
            .into()
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
mod tests {
    use super::*;
    use qaq_bx_core::settings::{CodecPref, QualityPref};

    #[test]
    fn danmaku_segment_math_anchors_six_minute_boundaries() {
        assert_eq!(danmaku_segment_count(0.0), 1);
        assert_eq!(danmaku_segment_count(359.9), 1);
        assert_eq!(danmaku_segment_count(360.0), 1);
        assert_eq!(danmaku_segment_count(360.1), 2);
        assert_eq!(danmaku_segment_count(1234.0), 4);
        // 负值/缺失兜底单段。
        assert_eq!(danmaku_segment_count(-5.0), 1);
        assert_eq!(danmaku_segment_at(0.0), 1);
        assert_eq!(danmaku_segment_at(359.9), 1);
        assert_eq!(danmaku_segment_at(360.0), 2);
        assert_eq!(danmaku_segment_at(800.0), 3);
    }

    #[test]
    fn danmaku_preload_order_playhead_first_then_ascending() {
        // 播放头在第 1 段:1 → 2 → 3 → 4 → 收束。
        let chain: Vec<u32> = std::iter::successors(Some(1u32), |&c| next_danmaku_segment(c, 1, 4))
            .take(10)
            .collect();
        assert_eq!(chain, vec![1, 2, 3, 4]);
        // 播放头在第 3 段:3 → 1 → 2 → 4 → 收束(优先段只到一次)。
        let chain: Vec<u32> = std::iter::successors(Some(3u32), |&c| next_danmaku_segment(c, 3, 4))
            .take(10)
            .collect();
        assert_eq!(chain, vec![3, 1, 2, 4]);
        // 优先段越界(总段数缩水):先试 9(该段为空/失败),再 1 → 2 收束。
        let chain: Vec<u32> = std::iter::successors(Some(9u32), |&c| next_danmaku_segment(c, 9, 2))
            .take(10)
            .collect();
        assert_eq!(chain, vec![9, 1, 2]);
        // 单段视频:首段即收束。
        let chain: Vec<u32> = std::iter::successors(Some(1u32), |&c| next_danmaku_segment(c, 1, 1))
            .take(10)
            .collect();
        assert_eq!(chain, vec![1]);
    }

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

    /// F4:首播音画缺失探测判定(媒体就绪 + 3s 宽限 + 轨道格式为空)。
    #[test]
    fn missing_media_track_requires_media_grace_and_empty_format() {
        use MissingTrack::*;
        // 媒体未就绪不探测。
        assert_eq!(
            missing_media_track("", "", false, Some(Duration::from_secs(10))),
            None
        );
        // 宽限期内不探测。
        assert_eq!(
            missing_media_track("", "aac", true, Some(Duration::from_millis(2500))),
            None
        );
        // 无画面有声音 → Video;有画面无声音 → Audio;齐备 → None。
        assert_eq!(
            missing_media_track("", "aac", true, Some(Duration::from_secs(4))),
            Some(Video)
        );
        assert_eq!(
            missing_media_track("h264", "", true, Some(Duration::from_secs(4))),
            Some(Audio)
        );
        assert_eq!(
            missing_media_track("h264", "aac", true, Some(Duration::from_secs(4))),
            None
        );
        // 探测点缺失(未载入)不探测。
        assert_eq!(missing_media_track("", "aac", true, None), None);
    }

    #[test]
    fn mount_pulse_covers_half_pane_fullscreen_and_stops_when_attached() {
        // 全播放页未挂载 → 脉冲。
        assert!(mount_pulse_active(Route::Player, false, false, false));
        // 半屏抽屉(route 停在 Home)未挂载 → 必须脉冲(修复冷启动/关闭播放条后首个视频黑屏)。
        assert!(mount_pulse_active(Route::Home, true, false, false));
        // 全屏态未挂载 → 必须脉冲(切全屏重挂新 SwapChainPanel)。
        assert!(mount_pulse_active(Route::Player, false, true, false));
        assert!(mount_pulse_active(Route::Home, true, true, false));
        // 已挂载成功 → 任何路由都停脉冲。
        assert!(!mount_pulse_active(Route::Player, false, false, true));
        assert!(!mount_pulse_active(Route::Home, true, false, true));
        assert!(!mount_pulse_active(Route::Player, false, true, true));
        // 首页无半屏抽屉、无播放画面 → 不脉冲(省电:无 surface 需要挂载)。
        assert!(!mount_pulse_active(Route::Home, false, false, false));
        // 其它空态页(收藏/历史等)同理不脉冲。
        assert!(!mount_pulse_active(Route::Favorites, false, false, false));
    }

    /// 全局搜索框 Enter 提交门控:按下(主/小键盘)提交并拦截;释放与其他键放行。
    #[test]
    fn search_enter_gate_only_handles_press() {
        let press = |key: VirtualKey, original: VirtualKey, released: bool| KeyEventInfo {
            key,
            original_key: original,
            status: PhysicalKeyStatus {
                repeat_count: 1,
                scan_code: 0x1C,
                is_extended: false,
                is_menu_down: false,
                was_down: false,
                is_released: released,
            },
            modifiers: InputModifiers::NONE,
        };
        assert!(
            search_enter_submits(&press(VirtualKey::ENTER, VirtualKey::ENTER, false)),
            "主键盘 Enter 按下应提交"
        );
        // 小键盘 Enter:VKEY 为 NumpadEnter(0x68),original_key 上报 0x0D。
        assert!(
            search_enter_submits(&press(VirtualKey(0x68), VirtualKey::ENTER, false)),
            "小键盘 Enter 也应提交"
        );
        assert!(
            !search_enter_submits(&press(VirtualKey::ENTER, VirtualKey::ENTER, true)),
            "释放事件不重复提交"
        );
        assert!(
            !search_enter_submits(&press(VirtualKey(0x41), VirtualKey(0x41), false)),
            "普通字符键放行不拦截"
        );
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
        // 双格式分支由参数化版本锁定(webp/jpg 由运行时探测决定)。
        assert_eq!(
            thumbnail_url_with("http://i2.hdslb.com/bfs/archive/abc.jpg", "webp"),
            "https://i2.hdslb.com/bfs/archive/abc.jpg@480w_270h_1c.webp"
        );
        assert_eq!(
            thumbnail_url_with("http://i2.hdslb.com/bfs/archive/abc.jpg", "jpg"),
            "https://i2.hdslb.com/bfs/archive/abc.jpg@480w_270h_1c.jpg"
        );
        // 运行时版本与参数化版本一致(探测结果只影响后缀)。
        assert_eq!(
            thumbnail_url("https://i0.hdslb.com/bfs/archive/abc.jpg"),
            thumbnail_url_with(
                "https://i0.hdslb.com/bfs/archive/abc.jpg",
                crate::codec::raster_suffix()
            )
        );
        // 已有参数/非图床保持不二次拼接。
        assert_eq!(
            thumbnail_url("https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp"),
            "https://i0.hdslb.com/bfs/archive/abc.jpg@100w.webp"
        );
        assert_eq!(
            thumbnail_url("https://example.com/pic.jpg"),
            "https://example.com/pic.jpg"
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
            owner: Some(qaq_bx_core::feed::Owner {
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

    /// rcmd 卡 stat 的可选互动计数透传（缺失保持 None，UI 不编造数字）。
    #[test]
    fn video_item_maps_optional_interaction_counts() {
        let with_like = FeedCard {
            id: 1,
            goto: Some("av".into()),
            bvid: Some("BV1x".into()),
            stat: Some(qaq_bx_core::feed::CardStat {
                view: 10,
                danmaku: 2,
                like: Some(88),
                ..Default::default()
            }),
            ..FeedCard::default()
        };
        let item = video_item(&with_like);
        assert_eq!(item.like_count, Some(88));
        assert_eq!(item.coin_count, None);
        assert_eq!(item.favorite_count, None);
        assert_eq!(item.share_count, None);

        let bare = FeedCard {
            id: 2,
            goto: Some("av".into()),
            bvid: Some("BV2".into()),
            ..FeedCard::default()
        };
        let item = video_item(&bare);
        assert_eq!(item.like_count, None);
        assert_eq!(item.play_count, 0);
    }

    #[test]
    fn archive_video_item_maps_owner_stat_and_https_thumbnail() {
        let card = qaq_bx_core::feed::ArchiveCard {
            aid: 80433022,
            bvid: "BV1GJ411x7h7".into(),
            cid: 137649199,
            title: "测试稿件".into(),
            pic: "http://i0.hdslb.com/bfs/archive/x.jpg".into(),
            duration: 210,
            pubdate: 1501000000,
            owner: qaq_bx_core::feed::Owner {
                mid: 486906719,
                name: "索尼音乐中国".into(),
                face: Some("//i0.hdslb.com/bfs/face/up.jpg".into()),
            },
            stat: qaq_bx_core::feed::CardStat {
                view: 1_234_567,
                danmaku: 4_567,
                like: Some(89_000),
                coin: Some(3_420),
                favorite: Some(5_890),
                share: Some(123),
                reply: None,
            },
            desc: None,
            tname: None,
            tid: None,
            score: None,
            videos: None,
        };
        let item = archive_video_item(&card);
        assert_eq!(item.aid, 80433022);
        assert_eq!(item.cid, 137649199);
        assert_eq!(item.author_mid, 486906719);
        assert_eq!(item.duration_secs, 210);
        assert_eq!(item.play_count, 1_234_567);
        assert_eq!(item.danmaku_count, 4_567);
        assert_eq!(item.like_count, Some(89_000));
        assert_eq!(item.coin_count, Some(3_420));
        assert_eq!(item.favorite_count, Some(5_890));
        assert_eq!(item.share_count, Some(123));
        assert_eq!(
            item.author_face_url.as_deref(),
            Some("https://i0.hdslb.com/bfs/face/up.jpg")
        );
        // 缩略图后缀随 WebP 探测结果变化，仅锚定 https 升级与图源。
        assert!(
            item.pic_url
                .as_deref()
                .is_some_and(|url| url.starts_with("https://i0.hdslb.com/bfs/archive/x.jpg"))
        );
    }

    #[test]
    fn merge_popular_items_dedups_by_bvid() {
        let mut existing = vec![
            minimal_video_item("BV1", "一"),
            minimal_video_item("BV2", "二"),
        ];
        let incoming = vec![
            minimal_video_item("BV2", "二重叠"),
            minimal_video_item("BV3", "三"),
            minimal_video_item("BV3", "三重叠"),
        ];
        let fresh = merge_popular_items(&mut existing, incoming);
        assert_eq!(
            fresh
                .iter()
                .map(|item| item.bvid.as_str())
                .collect::<Vec<_>>(),
            ["BV3"]
        );
        assert_eq!(existing.len(), 3);
        assert_eq!(existing[1].title, "二");
    }

    #[test]
    fn minimal_video_item_falls_back_to_bvid_title() {
        let item = minimal_video_item("BV1xx", "   ");
        assert_eq!(item.title, "BV1xx");
        assert_eq!(item.aid, 0);
        assert_eq!(item.cid, 0);
        assert!(item.cover.is_none() && item.pic_url.is_none());
        let named = minimal_video_item("BV1xx", "真实标题");
        assert_eq!(named.title, "真实标题");
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
        assert!(
            !session.references_avatar("https://i0.hdslb.com/bfs/face/other.jpg@96w_96h_1c.jpg")
        );

        let cache = ImageCache::with_capacity(8);
        cache.put(thumb.clone(), EncodedImage::new(vec![1, 2, 3]));
        let snap = session.to_snapshot(7, &cache);
        assert_eq!(snap.rev, 7);
        assert_eq!(snap.data.replies.len(), 1);
        assert!(snap.data.avatars.contains_key(&thumb));
    }
}
