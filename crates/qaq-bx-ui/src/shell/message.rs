//! `Message` 枚举 —— AppShell 的全部输入消息(163 变体,按域分组)。
//!
//! 拆解约定:每个域的 match 臂与配套方法位于 `shell/handlers/<域>.rs`,
//! 本文件只做定义;分组注释与 handlers 文件一一对应。
//! 变体保持平铺(阶段 P6 子枚举化默认不做,见 docs/dev-notes/shell-decomposition-plan.md)。

use super::*;

#[derive(Clone)]
pub enum Message {
    Back,
    /// selftest 空转消息:驱动探针在队列尾部刷新状态(生产路径零开销)。
    #[cfg(feature = "selftest")]
    SelfTestSync,
    /// 全局搜索框草稿变更(标题栏内敛搜索框 TextChanged)。
    // ===== ── 搜索 ──
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
    // ===== ── 导航与全局浮层 ──
    NavTagSelected(Option<String>),
    PaneOpenChanged(bool),
    TogglePane,
    // ===== ── 播放器控制(开关/进度/音量/画质/全屏/半窗) ──
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
    /// 竖屏模式(实验):窗口策略 presenter 手术回执(贴边态对账)。
    PortraitSynced {
        docked: bool,
        restore: Option<(i32, i32, i32, i32)>,
    },
    /// 竖屏模式:贴边最大化钮 → 切换停靠态。
    TogglePortraitDock,
    PlayerHotkey(pages::fullscreen::PlayerHotkey),
    /// 引擎状态节拍(250ms;读引擎快照刷新 UI 单源)。
    // ===== ── 播放器媒体/引擎协商 ──
    PositionTick,
    /// 后台解析完成(分轨直链)→ 引擎 loadfile + 弹幕拉取（带代次校验防切视频串台）。
    MediaReady {
        seq: u64,
        media: ResolvedMedia,
    },
    /// 播放页画质切换(F2):同视频换流,记忆进度与播放态。
    SelectQuality(i64),
    /// U1:评论草稿变更。
    // ===== ── 评论与楼中楼 ──
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
    /// U5:历史页就绪(page 失配即丢弃)。
    HistoryLoaded {
        page: u32,
        res: Result<Vec<UserVideoCard>, String>,
    },
    /// U5:历史页加载更多。
    HistoryMore,
    /// U5:稍后再看页就绪(count + 卡;page 失配即丢弃)。
    WatchLaterLoaded {
        page: u32,
        res: Result<(u64, Vec<UserVideoCard>), String>,
    },
    /// U5:稍后再看页加载更多。
    WatchLaterMore,
    /// U5:收藏夹列表就绪。
    FavFoldersLoaded { res: Result<Vec<FavFolderChip>, String> },
    /// U5:选中收藏夹(复位资源列表并拉首页)。
    FavSelectFolder(u64),
    /// U5:收藏夹资源页就绪(folder+page 失配即丢弃;载 (夹总数, 卡, has_more))。
    FavResourcesLoaded {
        folder: u64,
        page: u32,
        res: Result<(u64, Vec<UserVideoCard>, bool), String>,
    },
    /// U5:收藏页加载更多。
    FavMore,
    /// U5:用户列表封面就绪(url 键回填)。
    UserCoverReady { url: String, image: Option<EncodedImage> },
    /// 弹幕分段到货(预存链;即到即画,引擎侧归并排序)。
    // ===== ── 弹幕分段加载 ──
    DanmakuPart {
        seq: u64,
        /// S3:弹幕投递代次(切引擎即换代;旧链在途分段到货时作废,避免双投)。
        epoch: u64,
        index: u32,
        segment: DanmakuSegment,
    },
    /// 弹幕分段拉取失败(静默;预存链跳过该段继续,播放不受影响)。
    DanmakuFailed {
        seq: u64,
        /// S3:同上,过期链的续链请求一并作废。
        epoch: u64,
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
    // ===== ── SMTC 系统媒体控制 ──
    SmtcAction(Option<SmtcCommand>),
    /// P1:窗口尺寸去抖到期(150ms 内无新 resize)→ 应用挂起的网格重建。
    WindowResizeSettled,
    /// #5:结构档位(tier/cols/卡态)翻转的显式重绘请求。尺寸泵现走 silent
    /// dispatch(只 update 不 render),需要重绘时由本消息走普通队列——其处理
    /// 为空操作,dispatch 附带的根 view 重跑就是目的。
    ShellInvalidateLayout,
    /// Xbox 手柄事件回调（None 表示通道已关闭）。
    // ===== ── 手柄 ──
    GamepadInput(Option<GamepadEvent>),
    /// Windows Toast 通知激活回调（false 表示通道关闭）。
    ToastActivated(bool),

    // ---- 首页推荐流与 Hover 预解析 ----
    /// 换一换（Refresh 语义：清流 + 预分配首批骨架 + 重发）。
    // ===== ── 首页 feed ──
    HomeRefresh,
    /// 首页网格某一行被虚拟化容器实际挂载或回收（无感续页 + 离屏封面窗口锚点）。
    HomeRowActive {
        rev: u64,
        row_idx: usize,
        active: bool,
    },
    /// 窗口尺寸变化：同步自适应网格列数与每行视口缓冲。
    // ===== ── 窗口/布局/竖屏 ──
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
    // ===== ── 封面管线(首页/列表/相关推荐共用) ──
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
    // ---- #7 悬停预览卡(全局单例 PeekCard) ----
    /// 卡片进入/离开上报(含窗口坐标锚点;焦点触发 rect 为 None,弹出时解析)。
    // ===== ── 悬停预览卡与媒体预解析 ──
    CardHover(HoverReport),
    /// 300ms 预取简介 tick(带悬停代次,旧代次作废)。
    PeekPrefetchTick(u64),
    /// 1s 弹出 tick。
    PeekPopTick(u64),
    /// 200ms 宽限到期收起 tick。
    PeekGraceTick(u64),
    /// 挂载帧 → 翻牌帧(Composition 两帧进场)。
    PeekAnimTick(u64),
    /// 指针移入/移出 PeekCard 本体(移入撤销收起)。
    PeekPointerEntered,
    PeekPointerExited,
    /// 简介预取到货(None = 拉取失败/无简介,不再重发)。
    PeekDescReady { bvid: String, desc: Option<String> },
    /// PeekCard「播放」→ 进播放页(等同点卡)。
    PeekPlay(String),
    /// PeekCard「稍后再看」→ 登录后走 toview/add,未登录行内提示。
    PeekWatchLater(String),
    /// 「稍后再看」回执(None = 成功;仅当前预览仍为该稿件时展示)。
    PeekWatchLaterDone { bvid: String, error: Option<String> },
    /// PeekCard「关闭」:立即收起,仅静默当前卡直到指针离开/换卡。
    PeekClose,

    // ---- 热门页（真实 popular API，无签名 pn/ps 分页）----
    /// 加载下一页/首屏失败重试（`next_pn` 由单源状态推进；失败不推进=自动重取同页）。
    // ===== ── 列表页(热门/直播/历史/稍后再看/收藏) ──
    PopularMore,
    PopularLoaded(Result<Vec<VideoItem>, String>),

    // ---- 列表页滚动自动续页(五页共用;经 should_auto_page 门控) ----
    ListAutoMore(crate::paging::AutoPage),

    // ---- 设置页存储卡:一键清空缓存(白名单:图片磁盘缓存+票据;凭据/设置不动) ----
    // ===== ── 设置与存储 ──
    ClearCaches,
    /// 后台清理完成,携带实际释放字节数。
    CachesCleared(u64),

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
    // ===== ── 认证/登录 ──
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
    // ===== ── 账号/通知/私信/关系 ──
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
