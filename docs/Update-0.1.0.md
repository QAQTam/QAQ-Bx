# Update 0.1.0 — Mobile 窄屏形态与半屏响应式全链路落地

> 记录时间：2026-11-17。Cargo workspace 版本升级至 `0.1.0`，`just dev` 打包末尾修订号自增为 `0.1.0.x`。
> 依据：Mobile 探索设计方案与 `windows-reactor` 接口核验；严格遵循 `AGENTS.md` #1（设置页零解释性文本）、#2（单测）、#3（低耦合模块拆分）、#4（集成 + 点击走查）、#5（文档同步）。

## 新增

- **纯值响应式布局引擎**（`crates/qaq-bx-ui/src/layout.rs`，零 WinUI 依赖）：
  - 施加窗口客户区最小约束 `390.0 × 640.0` DIP（`WindowVisuals::constraints` 走 `IOverlappedPresenter3` 原生钳制 + 历史 `WindowBounds` 夹紧）。
  - 正交两维断点：
    - `ShellChromeMode::{Mobile (< 860.0 DIP), Desktop (>= 860.0 DIP)}`：以桌面 36px 边距可排下 3 列的精确下限 `860` DIP（`56+72+236×3+24`）为界，只要退回 2 列即直接切换至 Mobile 竖屏形态（640px/768px 等常见 Windows 半屏贴靠消灭右侧大留白与左侧栏挤压）。
    - `ContentLayoutTier::{Compact (< 600), Medium (600..860), Expanded (>= 860)}`：`< 600` 极窄屏隐去顶栏搜索图标仅留通知，`600..859` 半屏 Mobile 在底栏之外额外保留右上悬浮搜索按钮与更高 16:9 视频槽（上限 `360`）。
  - `CardMetrics::for_viewport()`：Mobile 严格 **2 列等宽铺满**（`margin_x=12, gap=8`，卡宽自适应 `160..414px`，390px 下 `179×178`、640px 下 `304×248`、859px 下 `413×309`）；网格防抖重建（列数变更、卡态切换或单卡宽变化 `≥ 2 DIP` 时更新 `VirtualSource`）。
- **壳层外框解耦模块**（`crates/qaq-bx-ui/src/widgets/chrome.rs`）：
  - 从 `shell.rs` 拆出响应式 `TitleBar`、悬浮搜索栏、`compact_bottom_bar` 与 `compact_more_popup`；
  - Mobile `TitleBar` 隐藏内嵌搜索框与副标题，右侧仅保留登录态通知铃铛；
  - `Medium` 半屏态（`600..860`）右上提供搜索按钮，呼出**顶部悬浮搜索浮窗**（提交自动收起并跳 `Route::Search`）；
  - Mobile 底栏「更多」扩展为 `2×3` 网格（补全 `Route::WatchLater` 稍后再看，共 6 项）。
- **Mobile 双列紧凑视频卡与 56px 迷你浮条**：
  - `video_card.rs`：Mobile 紧凑卡将播放量/弹幕数移至封面左下角半透明暗底胶囊、右下角保留时长；Mobile 卡体与相关推荐卡改用原生 `Border` 点击释放（避开 `windows-reactor` `ResourceOverrides` 装箱 `Thickness` 引发的 WinUI `0x80000011` 崩溃）修复 16:9 封面内缩与间隙放大；关闭 Mobile 悬停缩放定时器；`video_grid` 改用精确行 `StackPanel` 避免 `VariableSizedWrapGrid` 末列间距早折行；补齐 `TextTrimming::CharacterEllipsis`。
  - `mini_player.rs`：Mobile 态切换为顶部 `2.5px` `ProgressBar` + 单行 `48×27` 封面/单行标题 UP/`40×40` 播停关闭按钮（总高 ~56px），点击封面标题直达 `Route::Player`。
- **个人中心单页化 & 私信单页钻取**（`pages/account.rs`、`pages/login.rs`）：
  - 单列态合并外层为整页单一 `ScrollViewer`，取消嵌套列表滚动；资料卡改为上段身份+自适应经验条、下段分隔线 + 4 等分「关注/粉丝/动态/硬币」网格；关系 `Flyout` 自适应收窄至 `308px`；通知 Tab 条支持隐藏滚动条横滑；互动通知卡在 Mobile 下调整按钮位置防止长昵称挤压。
  - 私信（`NotifyTab::Whisper`）在单列态取消左右分栏，改走会话列表 ↔ 对话详情页内钻取（`whisper_detail_open`），并联动 `TitleBar::is_back_button_enabled` 与 `Back` 优先收起详情栈。
- **播放页吸顶视频与「简介 / 评论」双页签精修**（`pages/player.rs`）：
  - 单列（Mobile / Medium）下顶部「`16:9` 视频槽 + 单行紧凑播控条（`32×32` 播停 + 进度条 + 时间音量）+ `简介 / 评论 <N>` 页签栏」**吸顶固定**；
  - 「简介」页签展示 UP、可展开/收起标题与简介、4 等分图文字胶囊（点赞/投币/收藏/分享）与相关推荐（`116×65` 等高卡）；「评论」页签直达评论区；窗口拉宽至 `Expanded (>= 860)` 时恢复完整桌面双栏。
- **浏览页/空态/手柄全面对齐**：`home.rs`、`popular.rs`（修复竖屏窄屏下「加载更多」脚标外层固定 `40px` 边框裁切与浮层遮挡视频标题异常，改为滚动流底部自适应居中独立按钮）、`search.rs`（移除写死 `420px` 输入框改 `Grid [Star(1), Auto]`）、`settings.rs`（零解释文案）、`stub.rs`、`gamepad.rs`（Mobile 精简 `A/B` 提示胶囊）。
- **内置自驱动演示模式**（`crates/qaq-bx-ui/src/demo.rs`、`shell.rs`、`pages/player.rs`）：支持 `--demo` / `--demo-exit`（及 `QAQ_BX_DEMO=1`）零鼠标侵入内部时间线，自动沿 `1280×800 → 640×760 → 390×760 → 1280×800` 居中舞台缩放并自动演示「热门」「更多 2×3」「640 浮窗搜索」「吸顶播放页简介/评论切换」全流程；`scripts/ui-demo.ps1 -Record` 通过 `ffmpeg gdigrab` 无鼠标直录固定中心舞台供宣传片复用。
- **20s 商业级宣传工程**（`promo-video/`）：采用 `Bun + TypeScript + GSAP + Canvas + OfflineAudioContext 纯音乐合成 + Chromium CDP + FFmpeg` 管线制作《Overcoder！胡闹编程：竖屏 WinUI 客户端》，默认输出 `9:16`（兼容 `16:9`），支持自动合成桌面实录帧（`assets/raw-demo.mp4`）与程序化高保真 WinUI 降级帧。

## 变更

- **移除全局播放浮条，收敛为视频容器独立播控与弹幕开关**（`crates/qaq-bx-ui/src/shell.rs`、`crates/qaq-bx-ui/src/pages/player.rs`、`crates/qaq-bx-ui/src/widgets/mod.rs`、`docs/ui-design.md`）：
  - 删除 `crates/qaq-bx-ui/src/widgets/mini_player.rs` 与 `widgets/mod.rs` 中的 `pub mod mini_player;`，移除 `shell.rs` 的全局底栏渲染及 `Message::MiniNext`、`Message::OpenPlayerRoute`、`Message::ClosePlayer`。
  - `shell.rs` 新增 `is_player_visible`、`stop_playback`（同时清理导航 `history` 中的 `Route::Player`）与 `reconcile_playback_visibility`（于 `update()` 末尾统一对账），并在 `Message::CloseHalfPane` 调用 `self.stop_playback(context)`；离开播放页或关闭半窗侧栏后立即停播并释放 `VideoEngine`。
  - `shell.rs` 新增 `volume_before_mute: u32`、`Message::ToggleMute`、`Message::ToggleDanmaku` 与 `ShellProbe::{danmaku_enabled, volume}`，并与设置页 `AppSettings::danmaku_enabled` 双向同步；`pages/player.rs` 新增共用控件 `play_pause_button`、`mute_toggle_button`、`danmaku_toggle_button`、`seek_slider`、`time_label` 与 `half_pane_transport`。
  - `PlayerInput` 补充 `danmaku_enabled`、`on_toggle_mute`、`on_toggle_danmaku`，`PlayerPage` 桌面播控条新增 `desktop_transport_compact`（`< 540.0`）窄桌面紧凑分级，`860` DIP 与 Mobile `390` DIP 均完整展示无裁切。
- **横屏半窗侧栏补全独立播控与多维边界防护**（`crates/qaq-bx-ui/src/pages/player.rs`、`crates/qaq-bx-ui/src/shell.rs`、`crates/qaq-bx-ui/src/widgets/chrome.rs`）：
  - `HalfPaneInput` 补全 `playing`、`position`、`duration_secs`、`volume`、`danmaku_enabled`、`max_height` 与 `on_toggle` / `on_seek` / `on_volume` / `on_toggle_mute` / `on_toggle_danmaku` 回调；`HalfPanePreview` 升级为三行 `main_pane` `[Auto (video), Auto (half_pane_transport), Star(1.0) (ScrollViewer)]`，窄抽屉宽度（`< 440.0`）由 `half_pane_shows_volume_slider` 自动切换折叠音量滑块。
  - 抽出 `HALF_PANE_MIN_WIDTH`（`340.0`）、`HALF_PANE_MAX_WIDTH`（`820.0`）、`clamp_half_pane_width` 与 `compute_half_pane_video_height`；`shell.rs` `half_pane_layer` 传入 `max_width = (content_width() - 48.0).max(HALF_PANE_MIN_WIDTH)` 与 `max_height = content_height()`，并在 `DragMove` / `live_width` 夹紧抽屉宽度，防止超出可视区或压扁详情页。
  - 拖拽时保持 `keyed("half-pane-drawer", drawer)` 稳定不重建 `VideoSurface`，并为 `half-pane-drag-scrim` 补全 `on_pointer_canceled` 与 `on_pointer_capture_lost` 释放指针状态。
  - `chrome.rs` 新增 `title_search_box_width(window_width)`（`(window_width - 620.0).clamp(200.0, 360.0)`），修复 `860` DIP 窄桌面下顶栏搜索放大镜按钮裁切问题。
- **修复高 DPI 缩放（>100%）下视频区域与容器框架偏移/裁切**（`crates/qaq-bx-video/src/engine.rs`、`crates/qaq-bx-ui/src/widgets/video_surface.rs`）：
  - `VideoSurface` 完整透传 `SwapChainPanelEvent::Metrics` 的 `scale_x` / `scale_y`；`VideoEngine` 抽出纯函数 `compute_surface_metrics` 并以 `SurfaceMetrics { width, height, scale_x, scale_y }` 驱动引擎线程。
  - `Resize` 与仅缩放因子变化（`scale_changed`）均通过 `configure_swap_chain` 调用 `SwapChain::set_dpi(96 * scale_x, 96 * scale_y)` 与 `SwapChain::set_composition_scale(scale_x, scale_y)`（设置 `IDXGISwapChain2::SetMatrixTransform` 逆缩放矩阵 `1/scale_x, 1/scale_y`），避免 WinUI `SwapChainPanel` 对物理像素尺寸的 swapchain 二次叠加显示缩放导致画面越界裁切与视口偏移；D2D 弹幕叠加层同步切至 DIP 坐标对齐容器尺寸。
  - `SendRaw` 补齐 `Drop` 释放，消除 `pending` 槽位覆写时的 COM 引用计数泄漏。
- 版本号：`Cargo.toml` `[workspace.package].version` 从 `0.0.1` 升至 `0.1.0`；`shell.rs` 移除 `COMPACT_BREAKPOINT = 760.0` 与旧底栏内联代码；Mobile 下忽略 `HomeOpenMode::HalfPane` 并自动收起已展开的半窗抽屉；`main.rs` 修复 `install_panic_hook` 为仅首错落盘+级联追加，避免 FFI 边界 `panic_cannot_unwind` 覆盖真实异常栈。
- 项目标识与 Crate 统一：全部 5 个 crate 重命名为 `crates/qaq-bx-{core,plugin-login,video,cli,ui}`（CLI 二进制输出 `qaq-bx.exe`，桌面端可执行文件输出 `qaq-bx-ui.exe`）；环境变量全量切换为 `QAQ_BX_*`，移除旧版环境变量与 `%LOCALAPPDATA%` 旧包名兜底，所有活跃与历史文档及打包清单同步迁至 `QAQ-Bx` / `qaq-bx`。

- **与主干 `main` 领先修复合并对齐**（`crates/qaq-bx-ui/src/pages/player.rs`、`crates/qaq-bx-ui/src/widgets/chrome.rs`、`crates/qaq-bx-ui/src/pages/search.rs`、`crates/qaq-bx-ui/src/shell.rs`、`scripts/ui-edge.ps1`、`docs/Update-0.0.2.md`）：
  - **相关推荐等高/等宽回归**：合并 `RELATED_COVER_HEIGHT`/`RELATED_CARD_WIDTH`/`RELATED_TITLE_HEIGHT`/`RELATED_META_HEIGHT`/`RELATED_TEXT_SPACING` 定高定宽槽、`ScrollBarVisibility::Disabled`（桌面右栏 + Mobile 滚动宿主）与单测 `related_card_slots_are_content_independent`，同时在 `related_card` 保留 `Border::on_pointer_released` 避开 `ResourceOverrides` 崩溃。
  - **全局内嵌 Enter 搜索框**：合并一体化 `Border` 壳 + 无边框透明 `TextBox` + 内嵌放大镜 `Button` + `PreviewKeyDown` 隧道提交（`search_enter_submits` 纯函数与单测 `search_enter_gate_only_handles_press`，同步扩展至 `Medium` 悬浮搜索浮窗与窄屏页内搜索栏）；桌面全态（`>= 860` DIP）搜索页去除重复输入框，`title_search_box_width(window_width)` 自适应缩放 `200..360` DIP 兼容窄桌面。
  - **脚本与文档同步**：`scripts/ui-edge.ps1` 合并全局搜索框 Enter 走查（E9，适配 DPI 缩放与 `qaq-bx-ui` 标识）与 Mobile 走查（E8）；`Cargo.toml` / `Cargo.lock` 保持 `0.1.0` 工作区版本。

## 测试与验证

- **单元测试**（`cargo test --workspace`）：全量零告警通过（`qaq-bx-core` 44 passed / 5 ignored，`feed_fixtures` 6 passed，`qaq-bx-plugin-login` 7 passed，`qaq-bx-ui` 46 passed 含 `related_card_slots_are_content_independent`、`search_enter_gate_only_handles_press`、`video_surface::tests::apply_surface_metrics_forwards_high_dpi_without_panicking`、`half_pane_width_and_video_height_respect_viewport_boundaries`、`title_search_box_width_clamps_on_narrow_desktop`，`qaq-bx-video` 4 passed / 1 ignored 含 `compute_surface_metrics_scales_dips_and_handles_anisotropic_dpi`、`configure_swap_chain_sets_inverse_matrix_transform` 与 `video_engine_set_metrics_handles_valid_and_invalid_inputs_without_panic`）。
- **集成自测**（`just selftest all`）：10 项全通过（`boot` / `nav` / `comments` 含返回停播断言 / `rapid-switch` / `stress` / `search` / `codec` / `popular` / `half-pane` 含 `ToggleDanmaku`、`ToggleMute` 与 `CloseHalfPane` 停播断言 / `mobile`）。
- **内置 `--demo` 实机走查**（`powershell.exe -NoLogo -ExecutionPolicy Bypass -File .\scripts\ui-demo.ps1 -AutoExit`）：150% DPI（144 DPI）下无鼠标内部时序全程跑通，`ExitCode=0`；150% DPI 真机 `PrintWindow`（`PerMonitorV2`）实测 `860×640` DIP（`1290×960` 客户区 / `1312×971` 窗口）下合并后一体化顶栏搜索框、`PlayerPage` 紧凑播控栏及视频区域对齐完整无裁切，前序同步验收 `1280` 桌面、`390×640` Mobile 与 `480` 半窗侧栏边界。
- **开发包构建**（`just dev`）：打包并自签名输出 `package/debug/QAQ-Bx-Dev_0.1.0.10_x64.appx`（及公钥证书 `package/debug/QAQ-Bx-Dev_0.1.0.10_x64.cer`）；检查无残留登录凭据（`data/credentials.json`、`target/debug/data/credentials.json` 与 `target/appx-staging/debug/data/credentials.json` 均不存在）。
- **宣传片渲染校验**（`promo-video`）：`bun run typecheck` 通过，`9:16`/`16:9` 封面图与关键帧冒烟图已视觉验收，20 秒 9:16 MP4（`promo-video/out/overcoder-winui-9x16.mp4`）已渲染导出。
