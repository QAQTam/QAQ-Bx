//! 应用壳：TitleBar + NavigationView + 路由 + 迷你播放条 + 登录状态机
//! （`docs/ui-design.md` §1/§5；登录桥接见 `crate::auth`）。
//! 播放与登录状态在此单源持有，页面组件纯展示 + 回调上报。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bili_core::account::NavClient;
use bili_core::danmaku::{DanmakuClient, DanmakuElem};
use bili_core::feed::{fetch_rcmd_batch, FeedCard, FeedClient, FeedKind, FeedSession};
use bili_core::player::{PlayerClient, StreamChoice, VideoMeta};
use bili_core::settings::{AppSettings, FileSettingsStore, SETTINGS_FILE};
use bili_video::engine::{OverlayDanmaku, VideoConfig, VideoEngine};
use windows_reactor::*;

use crate::auth::{Auth, AuthState, FlowCell, LoginFlowState, PollOutcome, ProbeReport};
use crate::data::{self, VideoItem};
use crate::feed_pager;
use crate::image_cache::ImageCache;
use crate::pages;
use crate::router::{self, Route, NAV_ROUTES};
use crate::runtime::{block_on, cover_semaphore};
use crate::widgets::mini_player;
use crate::widgets::video_card::{self, HomeSlot};

/// 封面下载完成回填 UI 的批处理合并窗口（避免每到 1 张封面重绘整页）。
const COVER_FLUSH_INTERVAL: Duration = Duration::from_millis(80);
/// Hover 预解析缓存有效期（CDN 签名时效内安全复用）。
const PRERESOLVE_TTL: Duration = Duration::from_secs(180);
/// Hover 预解析最多缓存条目数。
const MAX_PRERESOLVE_CACHE: usize = 16;
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
        duration_secs: card.duration.unwrap_or_default().clamp(0, i64::from(u32::MAX)) as u32,
        play_count: card.stat.as_ref().map_or(0, |stat| stat.view.unsigned_abs()),
        danmaku_count: card.stat.as_ref().map_or(0, |stat| stat.danmaku.unsigned_abs()),
        cover: None,
        pic_url: card.pic.as_deref().map(thumbnail_url),
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

pub struct AppShell {
    auth_service: Arc<Auth>,
    /// 设置持久化存储（`data/settings.json`）+ 运行时唯一配置源。
    settings_store: Arc<FileSettingsStore>,
    settings: AppSettings,
    settings_notice: Option<String>,
    settings_save_timer: Option<ComponentTimer>,
    /// 后台任务代次（切视频/换一换/重探登录态时递增，拦截在飞旧结果串台）。
    playback_gen: u64,
    feed_gen: u64,
    feed_epoch: Arc<AtomicU64>,
    auth_gen: u64,
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
    route: Route,
    history: Vec<Route>,
    pane_open: bool,
    now_playing: Option<VideoItem>,
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
}

#[derive(Clone)]
pub enum Message {
    Back,
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
    /// None = 成功；Some = 失败原因。
    AuthLoggedOut(Option<String>),
}

impl AppShell {
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
        self.home_source = video_card::home_virtual_source(
            &self.home_slots,
            self.home_state.grid_cols.max(1),
            rev,
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
        self.history.push(self.route);
        self.route = Route::Player;
        self.playback_gen = self.playback_gen.wrapping_add(1);
        self.pending_open_preresolve = None;
        self.current_meta = None;
        self.ensure_engine().set_danmaku(Vec::new());
        self.now_playing = Some(item.clone());
        self.playing = self.settings.autoplay;
        self.position = 0;
        self.engine_duration = None;
        self.player_notice = None;
        self.schedule_tick(context);

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

    fn apply_resolved_media(
        &mut self,
        video_url: String,
        audio_url: String,
        meta: VideoMeta,
        seq: u64,
        context: &ComponentContext<Self>,
    ) {
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

    /// 首页视图（Home 路由与播放页无播放项时的兜底共用）。
    fn home_view(&self, context: &ViewContext<Self>) -> View {
        pages::home::home_page(
            &self.home_source,
            &self.home_state,
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
    type Input = ();
    type Message = Message;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let auth_service = Arc::new(Auth::new());
        let nav = auth_service.nav_client();
        let settings_store = Arc::new(FileSettingsStore::new(
            auth_service.data_dir().join(SETTINGS_FILE),
        ));
        let (settings, settings_notice) = settings_store.load_or_default();

        let playback_gen = 1u64;
        let feed_gen = 1u64;
        let feed_epoch = Arc::new(AtomicU64::new(feed_gen));
        let auth_gen = 1u64;

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
        let route = route
            .or((now_playing.is_some()).then_some(Route::Player))
            .unwrap_or(Route::Home);

        // 冒烟播放与点卡同构:仅在 BILIPLAY_VIDEO 指定时才提前创建 VideoEngine；日常启动 0 播放内存占用。
        let mut timer = None;
        let mut engine = None;
        if let Some(item) = &now_playing {
            engine = Some(VideoEngine::with_config(video_config_from(&settings)));
            spawn_resolve_media(&auth_service, &settings, item.clone(), playback_gen, context);
            timer = Some(context.set_local_timeout(PLAYBACK_TICK, || Message::PositionTick));
        }

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
            &home_on_open,
            &home_on_hover,
            &home_on_row_active,
        );

        Self {
            auth_service,
            settings_store,
            settings_notice,
            settings_save_timer: None,
            playback_gen,
            feed_gen,
            feed_epoch,
            auth_gen,
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
            route,
            history: Vec::new(),
            pane_open: true,
            now_playing,
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
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
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
                }
            }
            Message::PaneOpenChanged(open) => self.pane_open = open,
            Message::TogglePane => self.pane_open = !self.pane_open,
            Message::OpenVideo(bvid) => self.open_video(&bvid, context),
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
                self.playback_gen = self.playback_gen.wrapping_add(1);
                self.pending_open_preresolve = None;
                self.now_playing = None;
                self.playing = false;
                self.position = 0;
                self.engine_duration = None;
                self.player_notice = None;
                self.current_meta = None;
                self.timer = None;
                if let Some(engine) = self.engine.take() {
                    engine.stop();
                    drop(engine);
                }
                if self.route == Route::Player {
                    self.route = self.history.pop().unwrap_or(Route::Home);
                }
            }

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
            Message::WindowResized(size) => {
                let vw = (size.width - 56.0).max(320.0);
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
            Message::AuthLoggedOut(error) => match error {
                Some(reason) => self.auth_notice = Some(format!("登出失败：{reason}")),
                None => {
                    self.auth_gen = self.auth_gen.wrapping_add(1);
                    self.auth_notice = None;
                    self.auth = AuthState::Guest;
                }
            },
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("biliplay");
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

        let on_open = context.callback(Message::OpenVideo);
        let is_vip = matches!(&self.auth, AuthState::LoggedIn(user) if user.vip.is_vip());

        let content: View = if self.route == Route::Player {
            match (self.now_playing.as_ref(), self.engine.as_ref()) {
                (Some(item), Some(engine)) => {
                    View::component::<pages::player::PlayerPage>(pages::player::PlayerInput {
                        item: item.clone(),
                        playing: self.playing,
                        position: self.position,
                        volume: self.volume,
                        mount_tick: self.mount_tick,
                        duration_secs: self.display_duration(item),
                        notice: self.player_notice.clone(),
                        engine: engine.clone(),
                        on_toggle: context.message(Message::TogglePlay),
                        on_seek: context.callback(|value: f64| Message::Seek(value)),
                        on_volume: context.callback(|value: f64| Message::SetVolume(value)),
                    })
                }
                _ => self.home_view(context),
            }
        } else if self.route == Route::Login {
            pages::login::login_page(
                &self.auth,
                &self.login_flow,
                self.logout_confirm,
                self.auth_notice.clone(),
                context.message(Message::AuthStartLogin),
                context.message(Message::AuthCancelLogin),
                context.message(Message::AuthLogout),
            )
        } else if self.route == Route::Settings {
            let settings_path = self.settings_store.path().display().to_string();
            let on_change = context.callback(Message::UpdateSettings);
            pages::settings::settings_page(
                &self.settings,
                is_vip,
                &settings_path,
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
                .content("检查登录态…"),
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
            .content(Grid::new().keyed_children([keyed(key, content)]));

        let title_bar = TitleBar::new()
            .preferred_height(WindowTitleBarHeight::Tall)
            .title("biliplay")
            .subtitle(self.route.label())
            .is_back_button_visible(true)
            .is_back_button_enabled(!self.history.is_empty())
            .is_pane_toggle_button_visible(true)
            .on_back_requested(context.message(Message::Back))
            .on_pane_toggle_requested(context.message(Message::TogglePane))
            .grid_row(0)
            .content(TextBox::new().placeholder_text("搜索视频、UP主、直播间……（待接入）"));

        // 播放页自带全量传输控制 → 隐藏迷你条。
        let mini = if self.route != Route::Player
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
}
