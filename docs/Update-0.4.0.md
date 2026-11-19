# QAQ-Bx v0.4.0 更新说明:移动底栏「悬浮液态玻璃」+ 竖屏模式(实验)

> 状态:**已实施并验收**(逐项带证据)。分支 `feat/v0.4.0`,
> 主提交:`c290054`(竖屏模式)→ `5e3f7ec`(调研)→ `914c196`(玻璃主体)
> → `12a975c`(打磨:z 序修复 + 主题自适应 tint)→ 本文(版本对齐)。

## G. 移动底栏「悬浮液态玻璃」(iOS 26 风格)✅

移动形态(宽度 < 860 DIP)底部 5-Tab 导航栏脱离边缘,改为悬浮胶囊 +
composition 液态玻璃:效果采样 XAML 背景内容做 blur(saturation(backdrop)),
叠加主题自适应 tint 霜面,Tab 行浮在玻璃之上。**纯 app 侧实现,零 fork 改动、
零新依赖**(windows-numerics 为 fork 自带本地库的 path 引用)。

调研与三路线决策见 `docs/dev-notes/liquid-glass-research.md`(本轮走路线 0+2;
路线 1 自定义 HLSL 折射/色散/菲涅尔高光涉及 Win2D 解冻/fork 增能力,未做)。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| G1 悬浮胶囊 | 左右/底 10 DIP 悬浮 + 全胶囊圆角 32 + CardStroke 1px 描边;内容跨行铺到栏下(Backdrop 有物可采) | `widgets/chrome.rs::compact_bottom_bar`;`widgets/glass.rs::capsule` 常量(单测 `capsule_constants_are_consistent`) |
| G2 手写效果图 | `IGraphicsEffect`/`IGraphicsEffectD2D1Interop`/`IGraphicsEffectSource` 三接口 COM 节点手写(fork core 的 ComCell 3 vtable 槽位),blur(σ=9.0)·saturation(1.15) 经 `Compositor.CreateEffectFactory` 接入;效果图不能做形状填充(E_INVALIDARG,实测),走 SpriteVisual.Brush + CompositionGeometricClip 圆角裁剪 | `widgets/glass_fx.rs`;`widgets/glass.rs::build_effect_brush`;失败回退 tint-only(日志 `[glass] effect graph failed at {step}`) |
| G3 生命周期 | 子组件范式自持 `ElementRef<Grid>` + `use_effect_guard` 注册 composition host 观察(根组件 AppShell 的 use_effect 不触发,实测);Ready 零尺寸先存 compositor,Metrics 有效尺寸补建;resize 仅 SetSize 无重建 | `widgets/glass.rs` `GlassObserver`/`glass_metrics`(单测覆盖非法尺寸) |
| G4 **z 序修复** | 实测 `SetElementChildVisual` 的 child visual 盖在宿主 XAML 内容**之上**——玻璃与 Tab 行同网格混排时 Tab 行整体被毛玻璃化(只透出模糊鬼影,首版误判为 tint 对比度问题)。Tab 行改为玻璃宿主的兄弟节点后绘制(XAML 文档序在后 = z 序在上) | `chrome.rs::compact_bottom_bar`(玻璃宿主空内容 + `tab_row` 兄弟);修复前/后对照截图见验收 |
| G5 主题自适应 tint | `GlassBar` 经 reactor `on_color_scheme` 订阅(订阅即回报初值,变化仅派发);`CompositionColorBrush.SetColor` 原地切换,不重建视觉。浅色白霜 a=0x4D / 深色近黑 a=0x66(暗玻璃衬托白图标) | `glass.rs::tint_color`(单测 `tint_color_adapts_to_scheme`);`GlassMessage::Scheme` |
| G6 对比度 | 未选 Tab 图标/文字 opacity 0.72 → 0.85 | `chrome.rs` `tab_btn` |
| G7 浮层遮挡回归修复 | 竖屏(移动形态)下点「更多」:浮层底锚定在跨行内容(row1+row2)内、底边距 10 DIP,下半截落在玻璃胶囊带(底悬浮 10 + 胶囊高 64)内,被 composition 玻璃 child visual 盖住(同 G4 z 序机制,XAML 文档序在前者被玻璃盖住)。修复:新增 `capsule::FLOATING_CONTENT_BOTTOM_MARGIN`(= HEIGHT + FLOAT_MARGIN×2),「更多」浮层与手柄提示胶囊(compact)底边距统一抬出胶囊带并留一个悬浮留白呼吸间距;单测 `floating_content_bottom_margin_clears_capsule_band` / `compact_more_popup_bottom_margin_clears_glass_capsule_band` 钉住常量不漂移 | `widgets/glass.rs::capsule`;`widgets/chrome.rs::compact_more_popup`;`gamepad.rs::gamepad_hint_bar(compact)` |

### G-fix 验收实测(2026-11-19)

- 自动化点击(390×760 / 845×1000 / 420×680 真机):点击底栏「更多」Tab → 浮层完整浮在玻璃胶囊上方,6 项全部可见可点,无玻璃遮挡;再点击 Tab 收起正常。
- `cargo test --workspace` 全绿;selftest boot / stress(4.8s)/ mobile / e8 PASS。

## F. 合并后回归三连修(2026-11-19,v0.3×v0.4 首次同二进制)✅

> main 合入 feat/v0.3-native-player + feat/v0.4.0 后,两线功能首次同进程运行,
> 真机走查暴露三处回归/缺口,当日修复:

| 项 | 现象 | 根因 | 修复 | 验证 |
|---|---|---|---|---|
| F-1 玻璃 backdrop 残留 | 切到搜索/我的,玻璃仍透出推荐/热门底部的旧卡片颜色,不实时刷新 | content_host 跨行铺底(row1+row2)后,搜索/我的等页面根容器透明,胶囊带区域无不透明重绘 → composition backdrop 停在旧帧 | mobile `content_host` 补 `SolidBackground` 不透明底色,玻璃后面每帧都有当前页面像素可采(滚动内容仍浮在其上,透色语义不变) | 真机点击 推荐→搜索→我的 截图对照:玻璃区域均匀暗底无残影 |
| F-2 竖屏音量控制丢失 | 竖屏播放页只剩静音/取消静音,无音量滑条 | 单列 transport 从未放音量滑条( longstanding 缺口);半窗预览滑条又有 440 DIP 阈值整体隐藏 | 单列 transport 插入 48 DIP 紧凑滑条(带 M9 滚轮调节);`half_pane_shows_volume_slider` 改为 `half_pane_volume_slider_width`(44/68 自适应,任何宽度都出滑条) | 竖屏播放页截图:滑条+静音钮并排;单测改钉新语义 |
| F-3 切视频播放时长残留 | 快速切视频后,新页面显示旧视频的进度/时长,新视频不从 0 播 | 解析在途(start_playback→loadfile 之间,网络可达数秒)PositionTick 仍把**旧媒体**引擎快照灌进 UI 且提前解除 loading 遮罩 | 新增 `media_load_pending` 装载闸门:start_playback 置位,引擎 Load 清零快照(mpv `!has_media` / native `status.loading`)后放行;解析失败/引擎错误解除;置位期间旧位置/时长/播放态/弹幕时基全冻结 | 切换视频实测新片 `0:00:00 / 23:55` 从零起播,无旧片(1:04:35)残留 |

- 回归证据:`cargo test --workspace` 全绿(187);selftest boot/mobile/e8/stress PASS;
  自动化走查截图 `target/ui-demo/frames-fix3/`(未入库)。

### 验收实测(2026-11-19,96 DPI 真机)

- `cargo test --workspace` 全绿(core 61 / ui 78 / video 25 / core-cli 7 / 6)。
- selftest:boot PASS(0.0s)/ stress PASS(4.1s,基线 4.4s)/ mobile PASS(0.2s)/ e8 通过。
- E14(桌面播放页 resize storm,玻璃不在其路径上)干净跑完:CPU phase A=23.1% / B=14.8%(8 逻辑核),storm 合并 consumed=47 / produced≈54。
- 真机截图(D:/tmp/glass-e2e/,未入库):浅色(`fix-light-zoom.png`)白霜 + 深色图标锐利;深色(`fix-dark-zoom.png`)暗霜 + 白图标清晰;玻璃上 Tab 点击导航正常(搜索页 ⇄ 首页实测);打包应用 0.4.0.1 竖屏启动(`packaged-portrait.png`)玻璃正常。
- 运行时主题翻转(注册表 AppsUseLightTheme + ImmersiveColorSet 广播):日志先后出现 `[glass] tint adapted to Light` / `[glass] tint adapted to Dark`,玻璃跟随系统主题实时切换,无需重启。

### 已知限制(诚实清单)

- 折射/色散/菲涅尔高光(iOS 26 完整观感)未做——属路线 1(自定义 HLSL,涉 Win2D 解冻 / fork 增能力),动之前需向用户报备。
- composition backdrop 对 **SwapChainPanel**(mpv 视频 / 弹幕)的采样:当前移动布局下播放页不与底栏重叠,该子问题未触发、未验证。
- 玻璃参数(模糊 σ、饱和度、tint α)烘焙在 `glass.rs::material`/常量,无用户设置项。
- 玻璃仅移动形态(chrome: Mobile)底栏;桌面形态无底栏不受影响。
- 移动形态 resize storm 的开/关量化对照未脚本化(理论开销极小:resize 路径仅 composition SetSize;交互目测无卡顿)。

## P. 竖屏模式(实验)✅

`settings.portrait_mode`(默认关,设置页「窗口」卡 ToggleSwitch):
窗口宽度钳制在底栏边界带 [390, 859] DIP(presenter 级硬钳制)、没收系统最大化
(WS_MAXIMIZEBOX CLEAR)、右缘贴合最大化(工作区全高)+ 还原;全屏播放正交保留。
设计文档 `docs/dev-notes/portrait-mode.md`(双轴隔离模型)。真机验证证据见提交 `c290054`。

| 项 | 现象 / 隐患 | 根因 | 修复 | 验证 |
|---|---|---|---|---|
| P-1 竖屏退出全屏闪回 Desktop 导致卡顿 | 竖屏模式下进入全屏再退出时,界面先闪现桌面双栏+侧栏 UI 再跳回竖屏底栏,伴随明显卡顿 | ① 全屏期间系统派发 `WindowResized(1920, 1080)` 污染了 `self.viewport_layout`(切为 `Desktop/Expanded` 8 列并在后台重建 8 列虚拟源);② `exit_fullscreen` 同步置 `is_fullscreen = false` 而 `SetPresenterByKind(Default)` 异步执行,退出首帧 `view()` 按 `1920×1080` 渲染了整棵 Desktop `NavigationView` 视觉树,下一帧窗口缩回 `859×800` 又全量销毁重建为 Mobile 视觉树 | ① `ViewportLayout::for_window(w, h, portrait_mode)` 布局层硬钳制:`portrait_mode` 开启时有效宽度恒锁 `[390, 859]`、`chrome` 恒 `Mobile`、`cols` 恒 2;② 全屏期间冻结底层 `viewport_layout`,新增 `fullscreen_window_width` 专供全屏 HUD 紧凑断点;③ `AppShell::create` 按 `ViewportLayout::initial(portrait_mode)` 初始化首帧 2 列骨架;④ `winrt_ext::enforce_portrait_window_policy` 在退出全屏回 `Default` presenter 及运行时开启开关时重申 `IOverlappedPresenter3` 最大宽度硬钳制并主动收缩超宽窗口 | 单测 `portrait_mode_strictly_clamps_to_mobile_band_across_all_widths`;`selftest fullscreen` 阶段 6;真机 `ui-edge.ps1 -Scenario E21` PASS(进退全屏 0 Desktop 闪回,强拉 1400px 仍锁 859 DIP) |
| P-2 竖屏视频页去除“画质”解释性文本 | 竖屏/紧凑播控条清晰度下拉框上方出现“画质”二字，与 `1080P 高清` 按钮文本语义重复，且撑高单行播控条导致按钮纵向错位 | `quality_selector` 对 `compact = true`（竖屏/单列/紧凑 HUD）无条件设置 `.header("画质")` | `crates/qaq-bx-ui/src/pages/player.rs`：`quality_selector` 仅在 `!compact` 时保留 `.header("画质")`，并设 `.automation_id("player-quality-selector")`；抽出 `single_column_transport` 纯函数承接竖屏单行播控 | 单测 `quality_selector_requires_two_tiers_and_maps_selection` 与 `portrait_single_column_transport_omits_quality_header`（`qaq-bx-ui` 90 passed, workspace 202 passed）；`just selftest all` 全绿（`fullscreen` 阶段 6 含竖屏 `SelectQuality`）；真机 `ui-edge.ps1 -Scenario E21` PASS（UIA 断言无“画质”节点、展开选择 `360P 流畅` 成功，`e21-0/1/2` 截图视觉对齐正常） |

## V. 视频全屏黑屏与「有声无画」根因修复(2026-11-19)✅

> **前后端归因结论**：两类故障均为 **100% 前端 / 媒体引擎桥接与选流层问题**，非 B 站后端 API 问题（playurl、DASH 分轨、MPD 生成、Referer/UA 请求、`AdaptiveMediaSource` 建源与音轨解码均正常完成）。

| 编号 | 现象与归属 | 根因锁定 | 修复落点 |
|---|---|---|---|
| V-1A | Windows 原生引擎全屏必现黑屏 | WinUI 3 全屏层 `FullscreenPlayer` 直接铺满无非零圆角裁剪容器时，`MediaPlayerPresenter` 的 `HWCompMediaNode` 在全屏窗口中被 DComp 合成树遮挡/丢弃（而 `PlayerPage`/`HalfPanePreview` 因包裹 `.corner_radius(8.0)` 的 `Border` 建立了圆角裁剪子树，始终正常出画；实测去掉圆角亮度由 120.6 跌至 7.1，加 `.corner_radius(1.0)` 全屏 1920×1080 恢复至 130.5） | `crates/qaq-bx-ui/src/pages/fullscreen.rs` 全屏 surface 外层包裹 `Border::new().corner_radius(1.0)` |
| V-1B | 原生引擎进退全屏/缩放重建丢画或卡死 | ① `remount_pending()` 在 1500ms 节流期返回 `false` 后，`NativeSurface::try_attach()` 未在后续 250ms 节拍重试；② 显式进退全屏与快速缩放共用 1500ms 节流导致快速进退全屏卡顿；③ `note_surface_detached()` 未暂停旧 `MediaPlayer` 且二次 detach 会用 `0.0` 覆盖有效续播进度；④ `attach_player` 在 `MediaOpened` 前 seek/Play 可能被吞；⑤ reactor `ImperativeRequest::complete_unavailable()` 漏处理 `SetMediaSourceGetPlayer` | `crates/qaq-bx-video/src/native.rs`（`allow_immediate_remount`、`MediaOpened` 内补 seek + `Play()`、双 detach 进度保护、`stop()`）、`crates/qaq-bx-ui/src/widgets/native_surface.rs`、fork `reactor/src/reference.rs` |
| V-1C | mpv 全屏偶发黑屏 | `VideoEngine::detach_panel()` 与 `DanmakuStage::detach_panel()` 未同步清零 `attached` 且无挂载代次（`attach_gen`），旧面板迟到的 `SetSwapChain` completion 会把未挂载的新面板误标为 `attached = true` 停止重试；同时面板瞬态 `IntegrationError::Unavailable` 未放回待挂载指针 | `crates/qaq-bx-video/src/engine.rs` & `stage.rs`（共用 `SurfaceMountSlot` 单调代次槽）、`crates/qaq-bx-ui/src/widgets/video_surface.rs` & `danmaku_surface.rs` |
| V-2A | 原生解码失败回退 mpv 后「有声无画」 | `fallback_native_to_mpv` 在 `switch_to_mpv_now` 之后写入 `self.player_notice = Some(...)`，而 `PlayerPage`/`FullscreenPlayer`/`HalfPanePreview` 一旦发现 `notice.is_some()` 会直接用错误文本替换 `VideoSurface`，导致 mpv 后台放音但视频面被卸载 | `crates/qaq-bx-ui/src/shell.rs::fallback_native_to_mpv`（mpv 接管成功时保持 `player_notice = None`） |
| V-2B | 双引擎串音与状态泄漏 | 原生与 mpv 切换/回退/换视频时未互斥调用 `player.stop()` / `engine.stop()`；`native_load_choice` 同步报错时未回退 mpv；`media_load_pending` 仅靠 `!status.loading` / `!status.has_media()` 易提前放行旧会话快照 | `crates/qaq-bx-ui/src/shell.rs`（`mpv_load_streams`、`native_load_gate_settled`、`mpv_load_gate_settled`） |
| V-2C | 首页半屏抽屉（HalfPane）原生引擎「有声无画」 | `HalfPaneInput` 与 `HalfPanePreview` 仅接入 mpv `VideoSurface`，未传入 `native` 与 `stage`，在默认原生引擎下只渲染空置的 mpv 面板而原生引擎后台出声 | `crates/qaq-bx-ui/src/pages/player.rs`（抽出共用 `player_surface_view`）、`crates/qaq-bx-ui/src/shell.rs::half_pane_layer` |
| V-2D | 编码桶回退选流失败与无备选音轨杀画面 | `select_stream_excluding` 在 `max_qn == 0`（不限清晰度）时因 `v.id > max_qn` 恒真返回 `None`；所有视频桶均失败时未 `engine.stop()`；`apply_audio_fallback` 无备选音轨时设 `player_notice` 卸载正常视频面 | `crates/qaq-bx-core/src/player.rs::select_stream_excluding`、`crates/qaq-bx-ui/src/shell.rs` |
| V-2E | 原生引擎视频轨静默解码缺失（0×0）不回退 | Windows `MediaPlayer` 在缺失 HEVC/AV1 扩展时可能不触发 `MediaFailed`，仅解出音频且 `NaturalVideoWidth/Height == 0` | `crates/qaq-bx-video/src/native.rs` 上报固有宽高，`crates/qaq-bx-ui/src/shell.rs::missing_native_video_track` 活跃播放满 3s 自动回退 mpv |
| V-2F | DASH `segment_base` 蛇形键兼容与 mpv 残留音轨 | B 站部分稿件仅下发 snake_case `segment_base`（或与 `SegmentBase` 同对象双键共存）导致 MPD 缺 `<SegmentBase>`；`MpvPlayer::loadfile` 在 `audio_url == None` 时未清空上一条 `audio-files` | `crates/qaq-bx-core/src/player.rs::resolved_segment_base`、`crates/qaq-bx-core/src/mpd.rs`、`crates/qaq-bx-video/src/player.rs` |

### V 项验收实测(2026-11-19)

- `windows-reactor`: `cargo test --manifest-path D:\Projects\qaq-winui-reactor\Cargo.toml -p windows-reactor` 全绿（351 unit + 3 + 75 doc tests，0 失败）。
- `d:\QAQ-Bx`: `cargo test --workspace` 全绿（`qaq-bx-core` 66 + `feed_fixtures` 6 / `qaq-bx-plugin-login` 7 / `qaq-bx-ui` 88 / `qaq-bx-video` 33，合计 **200 passed, 0 failed, 7 ignored**）。
- `just selftest all`: 14 项集成自测全部 `[PASS]`（`boot`, `nav`, `comments`, `rapid-switch`, `stress`, `search`, `codec`, `popular`, `live`, `unplayable`, `half-pane`, `mobile`, `fullscreen`, `engine`），0 panic。
- 真机自动化点击与 `PrintWindow(..., 2)` 像素亮度/方差验收（`scripts/ui-edge.ps1 -Scenario E10,E11,E12,E20`）：
  - **E10-native（Windows 原生全屏进退与自隐）**：`PASS`（全屏前 `mean=106.1/sd=74.8` → 全屏 HUD `mean=68.4/sd=33.9` → 退出恢复 `mean=126.2/sd=69.9` → 二次全屏 HUD 自隐 `mean=131.8/sd=36.5`）。
  - **E10-mpv（mpv 全屏进退与自隐）**：`PASS`（全屏前 `mean=109.7/sd=73.0` → 全屏 HUD `mean=67.9/sd=34.4` → 退出恢复 `mean=102.9/sd=60.5` → 二次全屏 HUD 自隐 `mean=140.2/sd=37.1`）。
  - **E11（mpv 30 次快速伸缩）**：`PASS`（800ms `luma=137.1` / 3300ms `luma=132.5` / 6300ms `luma=128.0`）。
  - **E12（Windows 原生 30 次快速伸缩恢复）**：`PASS`（起播 `mean=122.3/sd=71.8` → 3300ms `mean=106.1/sd=74.4` → 6300ms `mean=64.7/sd=36.7`，stderr 装载/attach/开画/透明层全绿）。
  - **E20（HalfPane 半窗预览 Windows 原生引擎出画）**：`PASS`（UIA 点击首页卡片打开半窗，`mean=35.6/sd=30.7`，stderr `native load` + `native attach` + `native media opened` 全绿）。

## L. 播放加载动画修复 × 多 CDN 择优 × 有界并发 Range 代理(2026-11-19)✅

针对「起播/缓冲卡加载中但无加载动画」及默认命中 `mcdn`/PCDN 易卡顿问题，完成全链治理与双引擎加固（严格适配 4c8T / 8G 约束）：

| 项 | 现象 / 根因 | 修复落点 |
|---|---|---|
| L-1 Native 加载动画早熄与卡死无兜底 | ① `NativeStatus.loading` 仅覆盖 MPD 建源，attach 后 `media_load_pending` 立即解除，`shell.rs` 按 `!status.loading && !status.playing` 在 `Opening`/`Buffering` 阶段立刻清掉 `video_loading` 并把按钮变 `▷`；② `missing_native_video_track` 依赖 `playing \|\| position > 0`，导致 `0s` 卡死不回退 mpv；③ 切画质/分 P/回退未置 `video_loading = true` | `qaq-bx-video/src/native.rs`（`NativeStatus` 暴露 `opening`/`buffering`/`paused`/`wants_play`/`media_opened`/`buffering_progress`）；`qaq-bx-ui/src/shell.rs`（`native_should_show_loading`、`native_load_stalled` 12s 超时回退 mpv、`SelectQuality`/`SelectPage`/回退全补 `video_loading = true`） |
| L-2 Native 建源无超时、重挂竞态与 `_keep` 累积 | `CreateFromStreamWithDownloaderAsync(...).join()` 无超时取消；建源期间 detach 留下的 `awaiting_rebuild` 误阻新构建成品认领；`_keep` 在每次装载只增不减 | `qaq-bx-video/src/native.rs`（12s `BUILD_TIMEOUT` + `op.Cancel()`、`LoadLedger::finish` 对未绑定新成品清 `awaiting_rebuild`、`PendingSource.keep` 转移至 `active_keep` 并在 `stop()` 清理） |
| L-3 mpv 缓冲感知、吞错与请求头逗号截断 | `observe_status` 漏订 `paused-for-cache`；`EndFile` 丢弃 `reason`/`error`；`http-header-fields` 按逗号拼接切断含逗号的 `BROWSER_UA` | `qaq-bx-video/src/{player.rs, engine.rs}`（独立设 `user-agent`/`referrer`、`network-timeout=10`、`stream-lavf-o` 重连、订阅 `paused-for-cache` + `FileLoaded`/`PlaybackRestart`、`EndFile` 错误上报；`shell.rs::mpv_should_show_loading`） |
| L-4 多 CDN 择优与防重复节点降级 | B 站 `playurl` 默认将 `*.mcdn.bilivideo.cn` / PCDN 放 `base_url`，`upos-*` 官方节点放 `backup_url`/`backupUrl`，原代码仅取 `base_url` | `qaq-bx-core/src/{cdn.rs, player.rs}`（识别 `mcdn`/PCDN、`OfficialUpos` 优先排序、同 host 多签名后置、`CdnHealthTracker` 冷却机制、兼容 `baseUrl`/`backupUrl` 双键名） |
| L-5 本地有界并发 HTTP Range 代理 | 单连接循序拉片易受单节点限速/403 中断 | `qaq-bx-core/src/stream_proxy.rs`（`127.0.0.1:0` 环回 Range 代理：512KiB 分块 + single-flight + 最多 6 块超前并发预取 + 每轨上限 20 块/10MiB 淘汰 + 403/5xx/超时透明切换 `backup_url`，同时兼容 Native 与 mpv） |
| L-6 设置开关与设置页多行网格修复 | 暴露 `prefer_official_cdn` / `multi_thread_preload`（默认开，无解释性文本）；同时修复设置页双栏 `Grid` 缺 `RowDefinitions` 导致 `window_card` 压在 `playback_card` 上的历史叠层 bug | `qaq-bx-core/src/settings.rs`、`qaq-bx-ui/src/pages/settings.rs`、`qaq-bx-ui/src/shell.rs` |

### L 项验收实测(2026-11-19)

- `cargo test --workspace` 全绿：**214 passed, 0 failed, 7 ignored**（`qaq-bx-core` 72 + `feed_fixtures` 6 / `qaq-bx-plugin-login` 7 / `qaq-bx-ui` 93 / `qaq-bx-video` 36）。
- `just selftest all` 全绿：**15 项 `[PASS]`**（含新增 `loading-state` 场景：起播置位加载动画 → 落定熄灭 → `SelectQuality` 再次亮起与落定 → 设置切换 0 panic）。
- 真机自动化验收（`scripts/ui-edge.ps1 -Scenario E12,E20,E21,E22`）：
  - **E12 / E20 / E21**：全部 `PASS`（Native 经 `StreamProxy` 起播、`HalfPane` 半窗出画 `mean=26.1/sd=43.2`、竖屏全屏切换 + `360P 流畅` 换流零闪回）。
  - **E22（新）**：UIA 自动点击 `settings-prefer-official-cdn` / `settings-multi-thread-preload` 开关往返 + 默认 Native 走 `[stream-proxy]` (`prefer_upos=true`) 实播 `mean=106.6/sd=74.2` + UIA 切清晰度 `360P 流畅` 成功 + `nat=1920x1080` → `640x360` 绿灯，0 崩溃。
- 说明：当前会话环境为锁屏/非交互桌面，依赖 `CopyFromScreen` / 鼠标坐标的既有场景 `E17` 报 `The handle is invalid`，`E10` 在非交互退避等待 HUD 时超出 3s 自动隐藏窗口；`Fullscreen` 核心功能已由 `selftest fullscreen` 与 `E21` UIA 路径实测通过。

## C. 评论发布器竖堆三行布局回归修复(2026-11-19)✅

| 项 | 现象 | 根因 | 修复 | 验证 |
|---|---|---|---|---|
| C-1 评论区 composer 竖堆 | 播放页评论区头像独占一行居中、输入框独占一行拉满、「发布」钮掉到第三行左侧,本应单行三列 `[头像 \| 输入框 \| 发布]` | v0.2.0「U1 评论发布接线」(提交 `18c755e`)为支持条件性行内提示把 composer 子元素改成动态 `Vec<View>`,容器从 `Grid [Auto, Star(1), Auto]` 顺手换成垂直 `StackPanel`,却保留子元素的 `Grid.Column` 附加属性——该属性映射 WinUI `Grid.Column`(fork `native/generated.rs` → `Grid::ColumnProperty`),**只有 Grid 布局会读**,StackPanel 静默忽略 → 三子元素各排一行;notice 的 `.grid_column(1)` 同失效 | `crates/qaq-bx-ui/src/pages/player.rs`:抽出 `comment_composer` 纯函数,容器恢复 `Grid` 三列 `[Auto, Star(1), Auto]` + 两行 `[Auto, Auto]`(`Vec<View>` 本就实现 `IntoViews`,Grid 可直接收动态子元素,换容器本不必要);notice 补 `.grid_row(1)` 落第 2 行输入框列,无 notice 时第 2 行 Auto 收零高 | 新增单测 `composer_is_three_column_grid_with_notice_on_second_row`(根节点必须 Grid、禁 StackPanel、TextBox/Button 段落各钉 Grid.Column 1/2、notice 钉 Grid.Row=1+Column=1、登录/未登录双态文案与置灰);`qaq-bx-ui` 94 passed 0 failed;`just selftest comments` PASS(真实评论首页 + 排序双桶 + Back,0 panic) |

## Z. 后端零拷贝三件套(refs #1/#2/#3,2026-11-19)✅

依据 `docs/dev-notes/zero-copy-audit.md`,领单实施 GitHub issue #1(P0)/#2(P1)/
#3(P2)。播放热路径 CPU、瞬时峰值与首帧延迟收益;P2 总内存不变(本就由 mpv/解码
池主导)。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| Z1 #1 stream_proxy `Bytes` 贯通 + 边收边转发 | 缓存/watch/`FetchOutcome` 全链路 `bytes::Bytes`(去 `to_vec` 二次拷贝;单片终态 0 拷贝直转);分块 leader 改 spawn 任务,`bytes_stream` 逐片到达即广播连续前缀快照,播放器不必等整块下完;候选中途截断从已发布缺口续拉(已发字节绝对位置不变,failover 语义保留);请求头解析保持复用 | `qaq-bx-core/src/stream_proxy.rs`;新增测试 `reader_receives_bytes_before_chunk_completes`(首 8 KiB 到达 <200ms,整块需 ~300ms 滴流)、`midstream_truncation_resumes_from_gap_on_backup`(备线自 16384 缺口续拉);既有 Range/failover/416/有界缓存测试全绿。⚠ perf-probe 真机首帧/内存曲线未在本会话实测(需登录实播),留待回放对照后补数 |
| Z2 #2 弹幕解析零分配 | `get_bytes_with_ticket` 返回 `Bytes`;`DanmakuSegment{data:Bytes, elems}` 元素仅存 `(content_off,content_len)`,`mid_hash` 转 `Option<u32>`(8 位 hex 直解);video 侧 `OverlayDanmaku.content: ContentRef`(Bytes+offset/len),解码收拢到过滤命中/上屏绘制点,被密度筛掉的弹幕全程零分配;`u:` 屏蔽规则编译期直解 u32(core/video 仍零跨 crate 类型耦合,经 shell 映射) | `qaq-bx-core/src/{danmaku.rs,httpx.rs}`、`qaq-bx-video/src/{engine.rs,stage.rs}`、`qaq-bx-ui/src/shell.rs`;新增单测:非法 UTF-8 延迟解码/越界 content 丢弃/空 content 保留/mid_hash 非 hex 回退/大小写 hex 解析;filter 语义测试迁移 u32 比对 |
| Z3 #3 JSON 二次解析清理 | `loose_str`/`seg_base_loose` 手写 Visitor 直读(零 `Value` 中转,重复内键 last-wins 与旧语义一致);dash `segment_base` snake 键形改独立强类型字段(不再 `extra`+`from_value(v.clone())` 二次解析);live 房间列表 `Vec<LooseRoom>` derive 直读;native MPD 写入 `mpd.as_bytes()` 去 `bytes().collect()` 拷贝 | `qaq-bx-core/src/{player.rs,live.rs}`、`qaq-bx-video/src/native.rs`;新增 `seg_base_visitor_handles_all_wire_shapes`、`snake_segment_base_lands_in_typed_field_not_extra`;既有 Pascal/camel/snake/null/双键契约测试保持全绿 |

行为差异(有意收紧):#3 中 `initialization`/`indexRange` 为 null/容器形态时由旧
「to_string 保留垃圾串」改为置空缺失,避免向 MPD 写非法字节范围;#1 中「无
Content-Range 的短 206」由整块接受改为收敛该块尾(等价旧上层短块判定)。

### Z 项验收实测(2026-11-19)

- `cargo test --workspace` 全绿:**222 passed, 0 failed, 7 ignored**(较 v0.4.0
  基线净增 8 项:core +6 新单测/回归、stream_proxy +2 时序测试;2 项旧 dash 契约
  测试迁移到新签名)。
- 新增依赖:`bytes = "1"`(reqwest 同树传递依赖)、reqwest `stream` feature
  (仅启用已解析的 futures-util/tokio-util,依赖树零新增)。

## 版本

- workspace 版本 0.2.0 → **0.4.0**(`Cargo.toml [workspace.package]`,成员
  `version.workspace` 继承;Cargo.lock 同步)。打包(`just dev`/`release-sign`)
  的版本号自此取 0.4.0.x。

## R. 首页网格拖拽整片重挂修复(refs #4,P0,2026-11-19)✅(真机体感待录)

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| R-1 行 key 去卡宽 | `row_revision` 原编码卡宽低 11 位,F5 连续卡宽曲线下收口卡宽几乎必变 → 全部已实现行 key 翻转 → ItemsRepeater 一次性拆挂 ~60 卡 × ~10 元素(「松手卡一下」主因)。改 `home_row_revision(stream_gen, cards)` 仅编码结构因子;宽度收口同 key 复用行容器,经 `FeedRowInput.metrics`→`input_changed` 原地 diff | `widgets/video_card.rs`(helper)、`shell.rs`(`current_row_revision`/初值共用 helper);单测 `row_revision_ignores_card_width_but_tracks_structure` |
| R-2 卡片自适应宽度 | 行容器 `StackPanel`→`Grid` N 等分 star 列 + `column_spacing`,卡片经 `VideoCardInput.grid_col` 把 `Grid.Column` 附加在既有外层 Border(零新增元素)且不再定宽——拖拽 150ms 去抖窗内列宽由 XAML 连续跟随;静态页(热门/搜索)保留显式宽 | `widgets/video_card.rs` `FeedRow::view`/`VideoCard::view`/`skeleton_card_view` |
| R-3 重建判定收窄 | `should_rebuild_grid` 只认 cols/compact;新增 `metrics_refresh_needed`(卡宽 ≥2 DIP)走同一去抖的轻量刷新;settled 仅结构性变化才清活动行/锚点/驱逐游标(`pending_grid_structural`),竖屏切换路径同型改造 | `layout.rs`(单测 `grid_rebuild_debounce_and_back_navigation_predicates` 更新:宽度差 → 非结构、需轻量刷新)、`shell.rs` resize/portrait 两处调用点 |

### R 项验收实测(2026-11-19)

- `cargo test --workspace`:**223 passed, 0 failed**;`just selftest all`:**15/15 PASS**(含 mobile/half-pane/popular/search,覆盖虚拟化与静态两条网格通路;0 panic)。
- E14 前后对照**不构成本项证据**:E14 直开播放页路由不经过首页行网格,风暴计数器被网络解码波动主导(pre 两样本 max_gap {167,197}ms vs post {97,178}ms,同噪声水平)。本项收益为结构性(收口 remount→diff),定量需真机首页拖框体感或将 E14 改为首页路由探针;新增 `scripts/home-storm-probe.ps1`(首页路由风暴采样,修复后 30×55ms 伸缩仅消费 3 条消息)。

## S. WindowResized 不再每帧重跑根 view(refs #5,P1,2026-11-20)✅

依赖 fork 能力:windows-reactor `b215f14`「silent dispatch」——`LocalSender::send_silent`/`ComponentSender::send_silent` 投递只跑 `update` 不排渲染的消息(同 coalesce 槽遇普通消息保守提升;fork 侧测试 `silent_message_updates_state_and_defers_render` 钉住「不重挂 effect、状态延后随首次普通消息可见」)。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| S-1 尺寸泵静默化 | `on_window_size` 回调改 `send_silent(Message::WindowResized)`:拖框每帧只更新 `viewport_layout`/探针/风暴计数等**非渲染状态**,7.9k 行根组件 view 不再每帧重建 | `shell.rs` view 注册点 + `Message::ShellInvalidateLayout`(空操作,dispatch 的重绘即目的;`coalesce_key=3` 队尾合并) |
| S-2 结构签名门控 | `resize_render_signature`(tier/cols/compact/移动端/内联搜索/私信钻取)翻转才显式请求重绘;档内像素变化交给 XAML 自动布局 + 静止 150ms settled 普通消息统一收敛 | 单测 `same_tier_resize_keeps_render_signature_and_cross_tier_flips_it` |
| S-3 全屏 HUD 按需读取 | 全屏冻结语义不变;`fullscreen_window_width` 更新走静默路径,去抖到期经 settled 重绘收敛(不再每帧) | `WindowResized` 全屏分支 |

不变量验证:fullscreen/mobile/half-pane selftest 全 PASS(全屏冻结、竖屏锁 2 列不受影响)。

### S 项验收实测(2026-11-20)

- `cargo test --workspace`:**224 passed, 0 failed, 7 ignored**;`just selftest all` 15/15 PASS;fork `cargo test -p windows-reactor` 352+3+75 全绿。
- E14 风暴 `max_gap_ms`(UI 线程最大节拍间隔):修复前样本 {233(文档基线), 167, 197} → 修复后 {96, 103},**约砍半**;消费/产出比与 CPU 占比变化在噪声范围内(播放页 CPU 由解码主导)。

## T. UI 小项三连(refs #6,P1/P2×2,2026-11-20)✅

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| T-1 滚动驱逐局部化 | `HomeSlot::Video.item: VideoItem → Rc<VideoItem>`;驱逐/封面回填改 `Rc::make_mut` copy-on-write,仅触及单条发生真实拷贝;`rebuild_home_source` 的 `chunk.to_vec()` 从此只 bump 引用计数;`HomeSlot` 手写 `PartialEq`(`Rc::ptr_eq` O(1) 短路 + 深比兜底),未变行在 reactor `apply_input` 判等即停,不重挂不重绘 | `widgets/video_card.rs`、`shell.rs`(sync_offscreen_covers/FlushCovers/HomeLoaded);单测 `cover_eviction_only_copies_touched_items_and_keeps_row_diff_pointer_equal` |
| T-2 玻璃风暴降级 | composition host Metrics 转投 `GlassMessage::Pulse` 进组件消息域;`StormTracker` 判定(脉冲间隔 ≤300ms 且连续 ≥3)进风暴 → 模糊效果层 `SetSize(0,0)`(生成的绑定无 `IsVisible`,零面积即零 backdrop 重采样),tint 霜面层照常显示;每拍重挂 400ms 去抖,静默到期恢复;风暴中途补建亦以零尺寸入场。参数与 shell 去抖解耦,组件自治 | `widgets/glass.rs`;单测 `glass_storm_tracker_enters_on_burst_and_exits_on_quiet`(稀疏不降级/第 3 拍进入/风暴内保持/未静默不误恢复) |
| T-3 封面双拷贝 | `auth.rs` 头像/封面下载 `bytes.to_vec()` 再转 `Arc<[u8]>` → `Arc::<[u8]>::from(&bytes[..])` 单次拷贝 | `crates/qaq-bx-ui/src/auth.rs` |

### T 项验收实测(2026-11-20)

- `cargo test --workspace`:**226 passed, 0 failed, 7 ignored**(UI 96→98);`just selftest all` **15/15 PASS**(mobile/half-pane 走玻璃与驱逐路径,0 panic;stress 4.1s 无回归)。
- 长列表滚动帧耗时未单独定量(需真机 + E14 类脚本对首页路由采样);结构性证据:驱逐路径深拷贝次数从 O(全部槽位×String 字段) 降为 O(触及条数)。

## U. 卡片悬停 1s 展开预览卡(refs #7,feature,2026-11-20)✅(真机滚动掉帧与 DPI 缩放体感待录)

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| U-1 卡片只上报,Shell 驱动单例 | `VideoCard` 悬停/焦点事件不再各自计时展开:`PointerEntered` 携带 `PointerEventInfo`,由 `card_rect`(origin = window_x-x / window_y-y 精确,尺寸取布局度量)换算窗口坐标锚点后经 `Callback<HoverReport>` 上报;`on_got_focus/on_lost_focus` 同样上报(rect 缺省,弹出时解析)。预解析微动画链路(#卡片 700ms 蓄力)保持不变 | `widgets/video_card.rs`、`widgets/peek.rs::HoverReport` |
| U-2 全局单例 PeekCard + 覆盖层 | 根布局 `Grid` 末子元素新增全屏覆盖 `Canvas`(无背景,空白区不拦截命中;z 序在液态玻璃宿主 child visual 之后,v0.4 G4/G7 教训),内挂**全局唯一** PeekCard(`automation_id="peek-card"`):封面/标题 2 行/简介 3 行/播放·弹幕·UP 主/「播放」「稍后再看」「✕」。不用 Popup/Flyout/TeachingTip;不原地展开(避免 ItemsRepeater 重排,#4) | `shell.rs::peek_overlay_layer`、`widgets/peek.rs::peek_card_view` |
| U-3 计时状态机 | `PeekMachine`(纯逻辑):进入 300ms 预取简介(`video_meta.desc`,缓存去重)→ 停留 1s 弹出 → 离开宽限 200ms;宽限内回卡/移入 PeekCard 撤销收起;快速掠过(Pending 期离开)只撤定时器绝不弹出;全部 tick 带悬停代次,旧代次作废。路由切换/换一选/整源重建/行回收/resize settled/浮层弹出统一 `peek_hide_now()` 作废 | `widgets/peek.rs`;单测 `hover_pop_and_grace_lifecycle`、`fast_sweep_never_pops`、`moving_into_peek_card_keeps_it_open`、`switching_card_resets_to_pending`、`cancel_reports_visibility_and_voids_ticks`、`focus_pop_without_rect_stays_unmounted` |
| U-4 边缘翻转定位 | `peek_placement` 纯函数:优先卡片上方水平居中(视觉中线=卡片中线);视觉盒(布局盒×1.15 中心缩放四周外扩)越顶翻卡片下方;水平/垂直钳进窗口安全边距 12DIP。坐标口径:pointer 事件窗口相对 DIP 与根 Canvas 同为客户区空间,DPI 无需换算(事件坐标已是 DIP) | `widgets/peek.rs`;单测 `placement_flips_at_top_and_clamps_at_side_edges`(居上/翻下/左右钳位/矮窗钳位五断言) |
| U-5 Composition 进场动画 | 两帧进场:挂载帧发布 Scale 1.0/Opacity 0 + `scale_transition/opacity_transition`(reactor DSL,底层 `Visual.SetScaleTransition/SetOpacityTransition` 隐式过渡,GPU 合成线程,UI 线程零成本),16ms 翻牌帧改 Scale 1.15/Opacity 1 触发过渡;宽限收起帧淡出。未用 Storyboard、不动 Width/Height | `widgets/peek.rs::peek_render`;单测 `peek_render_frames_and_reduced_mode` |
| U-6 手柄/键盘焦点停留触发 | 卡片 Button 焦点进入/离开上报 → 同款 1s 计时;弹出 tick 时无 pointer 锚点则经 `FocusManager.GetFocusedElement → UIElement.TransformToVisual(root).TransformBounds` 解析焦点卡片矩形(build.rs 新增 2 条 bindgen filter;解析失败放弃弹出,不猜坐标) | `winrt_ext::focused_element_rect`、`shell.rs::handle_peek_pop` |
| U-7 降级与开关 | 设置页新开关「悬停预览卡」(可关闭功能,默认开)与「减少动画」(默认关):`hover_preview=false` 完全不弹;`reduce_animations=true` 跳过两帧过渡直接终态显示;✕ 或播放消费后 dismissed 静默位控制(点卡/重开设置解除)。已知偏差:系统 `UISettings.AnimationsEnabled` **不在 fork curated winmd**,无法读取,由「减少动画」设置项替代;ThemeShadow 无 reactor 暴露面,以描边+不透明卡面替代 | `crates/qaq-bx-core/src/settings.rs`(2 新字段,旧配置默认补齐)、`pages/settings.rs` |
| U-8 稍后再看 | 预览卡按钮接 `x/v2/history/toview/add` 写接口:`post_form_expect_ok`(成功只认 code==0,data 可缺省)+ `PlayerClient::add_watch_later`;未登录不发请求行内提示,结果回执仅当预览仍为该稿件时展示 | `crates/qaq-bx-core/src/httpx.rs`、`player.rs`;单测 `watch_later_params_pins_form_surface`;**实网写路径未真验(无登录令牌),待真机登录后确认** |

### U 项验收实测(2026-11-20)

- `cargo test --workspace`:**236 passed, 0 failed, 7 ignored**(新增 peek 8 项、settings 1 项、core watch_later 表单 1 项);`just selftest all` **16/16 PASS**(新场景 `peek`:真实首页流 → 悬停 1s 弹出 → 移入 PeekCard 不收起 → 宽限收起 → 掠过 10 卡全程不弹 → 点「播放」进播放页,全走真实定时器)。
- 自动化点击(E23,`scripts/ui-edge.ps1 -Scenario E23`):真实 `mouse_event(MOVE|ABSOLUTE)` 悬停(SetCursorPos 不产生 WM_MOUSEMOVE,不触发 PointerEntered——踩坑修正)+ UIA 断言 `peek-card` 0.5s 不出现/1.4s 出现 → Invoke「播放」→ `player-quality-selector` 出现且预览收起 → 快速掠过 10 卡中心无弹出无 panic。**修复内容溢出(PEEK_HEIGHT 420→440,按钮行曾越出边框)与封面取旧副本(改读槽位 Rc 现值)后的复验因测试环境桌面会话中断未跑完,留真机确认。**
- 真机验收格:4c8G 弹出期滚动无掉帧未录;300ms 简介预取命中真实 `view` API(selftest `peek` 场景走真实网络,PASS)。
