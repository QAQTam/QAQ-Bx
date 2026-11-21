# shell.rs 拆解计划(基于 codegraph 3256 节点/11087 边索引)

> 状态:**计划待实施**(v0.7.0 分支)。所有行号基于当前 HEAD(`0435740`)的
> `crates/qaq-bx-ui/src/shell.rs`,codegraph 索引可随时 `codegraph sync .` 刷新复核。

## 0. 现状证据(codegraph node / callers / cycles)

| 维度 | 数值 | 说明 |
|---|---|---|
| 文件规模 | 9,049 行 / 371KB | 全仓最大 rs(第二名 player.rs 3,591 行) |
| 符号总数 | 384 | `codegraph node shell.rs --symbols-only` |
| `impl AppShell` 方法 | 172 | 含 25 个单测在内 8,351–9,049 为 `mod tests` |
| `Message` 枚举 | 163 变体 / 约 353 行(L945–1298) | `coalesce_key` 同文件 |
| `update()` | L5386–7740,**2,354 行** | 单个 match 分发全部消息 |
| `view()` | L7740–8338,598 行 | 根视图装配 |
| `create()` | L4963–5386,约 420 行 | 状态初始化 + 监听装配 |
| 外部依赖面 | **仅 4 处** | selftest.rs:20 `use crate::shell::{AppShell, Message, ResolveProfileKey, ShellInput, ShellProbe}`;chrome.rs:83/204 与 search.rs:53 调 `crate::shell::search_enter_submits` |
| 既有循环依赖(拆解无关,记录) | 2 组 | pages/player.rs ↔ pages/fullscreen.rs;core account/feed/httpx/ticket/wbi。拆解后 `codegraph cycles` 不得新增 |

外部依赖面极小是本次拆解的最大利好:**冻结 5+1 个符号名即可保证全仓编译**。

## 1. update() 的域分布(163 变体 → 13 个域,按 match 臂行号实测)

| 域 | match 臂(行号) | 支撑方法(行号) | 规模估计 |
|---|---|---|---|
| 播放器控制/引擎/媒体 | OpenVideo、TogglePlay、Seek、SetVolume、ToggleMute、ToggleDanmaku、*Fullscreen、PlayerHotkey、PositionTick(5704–5912 单臂 210 行)、MediaReady/Failed、SelectQuality、SelectPage、PageStreamReady、*HalfPane、Related* | ensure_engine、native_*/mpv_* 全家(1662–2147)、start_playback(2492–2623)、start_live_playback(3249)、apply_resolved_media/fallback(3439–3609)、absorb_video_meta、kickoff_video_extras、SMTC 同步(3846–4053) | **最大,~1,100 行** |
| 首页 feed + 封面管线 | HomeRefresh/RowActive/Retry/Loaded/Failed(6499–6877)、CoverReady、FlushCovers | rebuild_home_source、allocate/truncate_skeletons、request_feed_batch、spawn_cover_fetch、sync_offscreen_covers、maybe_prefetch_next_page、apply_feed_page、mount_pulse_active、video_item 系映射(234–298、1298–1408、4657–4945) | ~700 |
| 账号/通知/私信/关系 | OpenNotifications、AccountRefresh、*NotifyTab、Replies/Ats/Likes/Systems/SessionsLoaded、Whisper*(7437–7703)、AvatarLoaded、FlushPlaybackAvatars | fetch_account_summary、fetch_relations、fetch_notify_tab、fetch_whisper_thread、spawn_avatars 系(4374–4655) | ~500 |
| 列表(热门/直播/历史/稍后再看/收藏) | PopularMore/Loaded、ListAutoMore、Live*(6049–6086)、History*、WatchLater*、Fav*、UserCoverReady | ensure/fetch/absorb 全家(2850–3248、3314–3360)、lists_logged_in、spawn_live_cover、spawn_user_cover | ~450 |
| 评论 | CommentDraft、SubmitComment、*CommentsLoaded、Toggle/LoadMoreSubReplies(6134–6371) | CommentsSessionState(302–414)、fetch_comments、fetch_sub_replies、refresh_comments_snapshot | ~400 |
| 窗口/布局/竖屏 | WindowResized(6634–6734,含全屏探测+去抖)、ShellInvalidateLayout、WindowResizeSettled、TogglePortraitDock、PortraitSynced | ResizeStormTrack(103–206)、sync_portrait_maximizable、track/close_resize_storm、resize_render_signature | ~350 |
| Peek/悬停预取 | CardHover、Peek*(7034–7113,12 臂)、PreResolveVideo、MediaPreResolved(6942–7033) | peek_allowed/hide_now、handle_card_hover/peek_pop、fetch_peek_desc、peek_overlay_layer(2174–2368)、preresolve 缓存 | ~350 |
| 认证/登录 | AuthProbed、AuthStartLogin、LoginQrReady/PollTick/Polled、AuthLogout/CancelLogout/LoggedOut(7310–7436) | clear_login_session、schedule_login_tick | ~250 |
| 设置/存储 | ClearCaches、CachesCleared、UpdateSettings(7172–7296)、FlushSettingsSave、SettingsSaved | schedule_settings_save、video_config_from、danmaku_filter_from、resolve_params_for/profile_key(428–477) | ~230 |
| 弹幕 | DanmakuPart、DanmakuFailed(6372–6419) | ensure_stage、danmaku_replace/append/config/filter(1849–1928)、fetch_danmaku*(3610–3660)、segment 数学(481–502) | ~200 |
| 手柄 | GamepadInput | listen_gamepad、handle_gamepad_action(3686–3845)、focused_home_video、cycle_nav_route | ~180 |
| SMTC | SmtcAction | listen_smtc、ensure_window_smtc、sync_smtc、apply_playback_want、reset_smtc_writer | ~170 |
| 导航/搜索/杂项 | NavTagSelected、Search*(5495–5548)、ToggleMoreMenu、ToggleSearchOverlay、NavigateCompact、ToastActivated、PaneOpenChanged/TogglePane、SelfTestSync、DemoTick | submit_search(1480)、search_enter_submits、pick_demo_bvid | ~250 |

## 2. 跨域共享热点(callers 证据,不能归属单一域)

- `spawn_cover_fetch`:update×2 + absorb_popular + sync_offscreen_covers + absorb_related(feed 与播放器相关推荐共用)→ **cover_pipeline**
- `spawn_avatars` 系:账号页 + 播放页评论/播放条头像 → **avatar 管线**
- `open_video` / `open_video_target`:peek 播放、相关推荐、列表、直播、搜索、手柄都汇入 → **导航域**
- `advance_playback_gen`:媒体代次守卫,player_media 专用但被 hotkey 域触发
- `schedule_tick` / `schedule_playback_avatar_flush` / `schedule_settings_save`:定时器基建
- `runtime::{block_on, cover_semaphore}`:外部依赖,保持现状

## 3. 目标结构(shell.rs → shell/ 目录,全部 `pub(crate)` 内聚)

```
crates/qaq-bx-ui/src/shell/
├── mod.rs            AppShell 结构体 + Component impl(update=薄分发器) + 冻结面 re-export   ≤1,200
├── message.rs        Message 枚举(163 变体先平铺不动) + coalesce_key                        ~400
├── probe.rs          ShellProbe + refresh_probe(selftest 契约单源)                          ~150
├── util.rs           纯函数:thumbnail_url 系、video_item 系4个、merge_popular_items、
│                     search_enter_submits、engine_switch_plan、resolve_params 系、
│                     video_config_from、danmaku_filter_from、danmaku segment 数学、
│                     resize_render_signature、title_bar_tier_signature、portrait 约束      ~600
├── handlers/         update 域分发(见 §4 顺序),每文件 ≤600 行
│   ├── mod.rs            dispatch 顺序表 + Handled
│   ├── settings.rs  auth.rs  account_notify.rs  search.rs  window.rs
│   ├── gamepad.rs   smtc.rs   comments.rs  danmaku.rs  lists.rs
│   ├── peek.rs      home_feed.rs  cover_pipeline.rs  nav.rs
│   └── player_ctl.rs player_media.rs   ← 最后拆,最大
├── view/             view() 区域构建(阶段 5)
│   ├── mod.rs(根装配) player_views.rs(全屏 HUD/half-pane/park 层) chrome_nav.rs(标题栏/NavigationView) mobile.rs
└── tests.rs          现有 25 个单测原样搬迁
```

**分发器形态**(Message 保持平铺,零枚举重构):

```rust
// handlers/mod.rs
pub(super) fn dispatch(m: &mut AppShell, msg: &Message, ctx: &mut ComponentContext<AppShell>) -> bool {
    settings::handle(m, msg, ctx)
        || auth::handle(m, msg, ctx)
        || account_notify::handle(m, msg, ctx)
        // … 13 域顺序链
        || player_media::handle(m, msg, ctx)
}
```

每个域文件 `pub(super) fn handle(...) -> bool` 内部 `match msg { 本域臂 => {…; true}, _ => false }`。
所有状态仍留在 AppShell 上(阶段 4 之前),handlers 只做**物理切分**,借词/语义零变化。

## 4. 实施阶段与验收门

| 阶段 | 内容 | 门禁(每步全过才提交) |
|---|---|---|
| P0 基线 | `codegraph sync` 记录节点/边数;`cargo test --workspace`(273)+ selftest 18/18 | 全绿留档 |
| P1 目录化 | shell.rs → shell/mod.rs;mod tests → shell/tests.rs;Message → message.rs;mod.rs `pub use` 冻结面保持原名 | cargo test + selftest;`codegraph node shell.rs` 外部依赖文件仍为 3 个;`codegraph cycles` 不新增 |
| P2 纯函数迁移 | §3 util.rs 清单逐个搬(连同其单测),search_enter_submits 保持 `crate::shell` 路径可见 | 同上;ui-edge E28(卡片几何)/E14(resize)不受影响 |
| P3 update 域拆分 | 按耦合从低到高 11 步:**settings → auth → account_notify → search → window → gamepad → smtc → comments → danmaku → lists → peek → home_feed/cover_pipeline → player_ctl+player_media**;每步一个 commit | cargo test + selftest;对应 E 场景:settings→E27、window→E14/E25、peek→E23、lists→E27/selftest user-lists、comments→selftest、player→E10/E20/E24/E25/E26 全跑 |
| P4(可选)状态分组 | AppShell 字段并入 §3 state 子结构;仅读取型域先行(probe/window),写交织域(PositionTick)最后 | 同上;出现借用冲突即回退该域,允许只完成部分 |
| P5 view 拆分 | 根装配留 mod.rs(≤250),player_views/chrome_nav/mobile 各自成文件;home_view 本就薄(4247–4257) | E20/E24/E25/E26/E28 + 截图对比 |
| P6(可选)Message 子枚举 | 163 变体 → 13 子枚举;需同步 coalesce_key 与 selftest 全量改写,**收益/风险比最差,默认不做** | — |

## 5. 风险与对策

1. **update 臂跨域交织**(PositionTick 同时驱动弹幕段预取/SMTC 进度/封面 flush;WindowResized 触发全屏探测+竖屏+去抖):阶段 3 全程保持状态在 AppShell、handlers 只拿 `&mut AppShell`,纯物理切分;阶段 4 才谈状态归属,且允许半途收工。
2. **分发器丢失编译期穷尽性**(原 giant match 编译器强制全覆盖):对策——handlers 链尾 `player_media` 之后兜底臂在 debug 构建打 stderr 日志;每个域的 handle 用 `match` 显式列臂,漏臂在 codegraph `node` 对比 + selftest 全场景回归中暴露。
3. **selftest 契约**:probe.rs 单源化后 refresh_probe 调用点(1412)与 `SelfTestSync` 臂语义不变;selftest.rs 的 `use crate::shell::{…}` 由 mod.rs re-export 兜住,不改外部一行。
4. **create() 的监听装配**与各域 handler 强耦合(436 处 context.spawn/timeout):P1–P3 不动 create,视拆分后 mod.rs 体量再决定是否抽 `shell/bootstrap.rs`。
5. **既有双循环依赖**(player↔fullscreen;core 五文件)不在本次范围,但拆解完成后 `codegraph cycles` 结果不得比基线多一组——作为回归指标。

## 6. 量化目标

- `update()` 2,354 行 → mod.rs 分发器 ≤ 60 行 + handlers 各 ≤ 600
- `view()` 598 行 → 根装配 ≤ 250 + view/ 3 文件
- shell.rs 单文件 9,049 → mod.rs ≤ 1,200;全目录任一文件 ≤ 600
- 273 测试 + selftest 18/18 + ui-edge E10/E14/E20/E23–E28 全绿,每阶段复测
