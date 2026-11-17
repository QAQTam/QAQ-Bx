# QAQ-Bx v0.1.x Mobile 形态设计方案

> 目标：围绕 WinUI 3 (`windows-reactor`) 打造类似 Android 原生客户端的窄屏单手体验。
> Worktree：`D:\biliplay-rs-v0.1.0`（分支 `feat/v0.1.0`，基线 `e59035c`）。
> 约束遵循：`AGENTS.md`（#1 禁止解释性文本设置、#2 单测、#3 4c8T/8G 低耦合、#4 集成+自动化点击测试、#5 `Update-0.1.0.md`、#7 测后清令牌）。

---

## 1. 定案规格（已拍板）

1. **最窄窗口客户区下限**：**`390.0 × 640.0` DIP**
   - 通过 `WindowVisuals::constraints(WindowConstraints { min_width: Some(390.0), min_height: Some(640.0), .. })` 交由 `windows-reactor` 原生 `IOverlappedPresenter3::SetPreferredMinimumWidth/Height` 强制钳制。
   - 恢复历史 `settings.window` 时同步夹紧启动计算宽度，防止旧配置文件绕过或导致首帧闪烁。
2. **「真 Mobile 形态」单一判定标准**：**`window_width < 600.0` DIP**
   - 对照 Android Material 3 `Compact (< 600dp)` 断点；仅依据客户区宽度判定，避免宽而扁的桌面窗口误触发 Mobile。
   - 1280 屏幕 Windows 半屏贴靠（640px）保留在 `Desktop` 紧凑态；手动拖窄至 `< 600px` 才进入 `Mobile`。
   - 废除此前仅凭 `COMPACT_BREAKPOINT = 760.0` 切底栏的半吊子做法，引入统一枚举 `LayoutMode::{Desktop, Mobile}` 驱动全链路。
3. **「真 Mobile 形态」6 维联动契约**
   - **顶栏 TopAppBar**：隐藏内嵌 `360px` 搜索框，仅保留返回键 + 应用图标/页标题 + 右侧轻动作图标（搜索图标按钮 + 登录态通知铃铛角标）。
   - **底栏导航**：隐藏左侧 `NavigationView`，使用底部 5-Tab 栏（推荐 / 热门 / 搜索 / 我的 / 更多上拉菜单）。
   - **推荐/热门/搜索卡片**：严格 **2 列等宽自适应铺满** + 紧凑信息排版（播放量与弹幕量移入封面左下角半透明胶囊，底部仅留 2 行标题 + 1 行 UP 主）。
   - **播放交互**：Mobile 下**禁用右侧抽屉式半窗 (`HalfPanePreview`)**，点卡直达全页竖屏播放页。
   - **迷你播放条**：从三栏 `~80px` 桌面条切换为 **`56px` Android 紧凑单行浮条**。
   - **个人中心与全页面单页化**：「我的」改为整页统一滚动 + 4 格横铺关系栏 + 私信「会话列表 ↔ 对话详情」页内钻取（接入 `TitleBar` 返回键栈）。

---

## 2. 模块设计与解耦边界

### 2.1 `LayoutMode` 与 `CardMetrics`（纯值层，零 WinUI 依赖）
建议落在 `crates/qaq-bx-ui/src/theme.rs`（或独立轻量 `layout.rs`）：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    Desktop,
    Mobile,
}

pub const MOBILE_BREAKPOINT: f64 = 600.0;
pub const MIN_WINDOW_WIDTH: f64 = 390.0;
pub const MIN_WINDOW_HEIGHT: f64 = 640.0;
```

- `LayoutMode::from_width(width: f64) -> Self`：`< 600.0` 返回 `Mobile`，否则 `Desktop`。
- `CardMetrics::for_viewport(mode: LayoutMode, viewport_width: f64) -> CardMetrics`：
  - **Mobile**（`viewport_width` 在 `390.0..600.0`）：
    - `cols = 2`（严格 2 列，消灭半边留白）
    - `margin_x = 12.0`、`gap = 8.0`
    - `card_width = ((viewport_width - 24.0 - 8.0) / 2.0).floor().clamp(160.0, 278.0)`
      - 基准 `390px` 窗：`card_width = 179.0`
    - `cover_height = (card_width * 9.0 / 16.0).round()`（`390px` 下 `101.0`）
    - `card_height = cover_height + 77.0`（`390px` 下 `178.0`，对比桌面 `240.0` 缩减 26%，一屏可露 3 行共 6 卡）
    - `compact_card = true`
  - **Desktop**（`viewport_width >= 600.0`）：
    - `card_width = 236.0`、`cover_height = 133.0`、`card_height = 240.0`、`gap = 12.0`、`margin_x = 36.0`
    - `cols = floor((inner + 12.0) / 248.0).clamp(2, 12)`
    - `compact_card = false`

### 2.2 视频卡片与虚拟化分页（`widgets/video_card.rs` + `feed_pager.rs`）
- `feed_pager.rs`：
  - `columns_for(mode, viewport_width)` / `logical_page_target_for(mode, viewport_width, viewport_height)` / `visible_rows_for(card_height, gap, viewport_height)` 全部改由 `CardMetrics` 驱动（保留原有桌面行为与测试等价）。
- `video_card.rs`：
  - `FeedRowInput`、`VideoCardInput`、`video_grid` 增加 `metrics: CardMetrics`。
  - `metrics.compact_card == true` 时的紧凑卡视觉：
    - **封面区（16:9）**：左下角叠加半透明深色胶囊 `▶ 1.2万 · 💬 340`（`10px`），右下角保留时长 `12:34`（`10.5px`）；封面加载占位字号 `11px`。
    - **文本区**（padding `8, 6, 8, 8`、spacing `4`）：
      - 标题：`12.5px SemiBold`，`max_lines(2)`，`TextWrapping::Wrap`。
      - UP 主：`11px`，`opacity(0.6)`，`max_lines(1)`。
    - **交互与性能**：关闭 Mobile 下两段式鼠标悬停微缩-扩张（零定时器开销，避免窄窗边缘裁切）。
    - **骨架槽位**：`skeleton_card_view(metrics)` 严格匹配 `metrics.card_width / cover_height / card_height`，防重排抖动。
- `shell.rs::WindowResized` 防抖重建：
  - 记录 `home_metrics: CardMetrics`；当 `cols` 改变或 `card_width` 变化 `>= 2.0` 时更新并重建 `home_source`，既保证双列铺满，又不在拖动边框时高频重建 `VirtualSource`。

### 2.3 应用壳层（`shell.rs` + `widgets/mini_player.rs`）
- **`TitleBar`（Android TopAppBar 形态）**：
  - `Desktop`：保留 `content` 内的 `360px` 搜索框 + 搜索按钮 + `right_header` 通知按钮。
  - `Mobile`：`TitleBar::content` 不放搜索框（防挤爆系统三键），改为 `right_header` 水平排列：
    - 搜索图标按钮（点击触发 `NavigateCompact(Route::Search)`）；
    - 登录态通知铃铛按钮（带未读 `InfoBadge`）。
  - 返回键 `on_back_requested`：若当前在 `Route::Login` 且 `Mobile` 模式正处于私信对话详情（`whisper_detail_open == true`），优先收起对话详情回到会话列表，再退回历史路由。
- **`MiniPlayer`（Android 底部浮条）**：
  - `Desktop`：维持三栏完整控制（`80×45` 封面 + `230px` 标题 + 进度条/上下首/播停 + 关闭）。
  - `Mobile`（总高 `~56px`，置于 `bottom_bar` 上方）：
    - 顶部/底部 `ProgressBar`（`2.5px` 高，仅展示进度，避免窄屏滑块拇指误触）；
    - 单行 `Grid [Auto, Star(1.0), Auto, Auto]`：`48×27` 封面 + 弹性标题/UP（`max_lines(1)`，点击可进入 `Route::Player`）+ 播停按钮 + 关闭按钮。
- **半窗策略**：
  - `open_video` / `open_video_target` 在 `LayoutMode::Mobile` 下忽略 `HomeOpenMode::HalfPane`，一律直达 `Route::Player`；
  - `WindowResized` 从 `Desktop` 拖入 `Mobile` 且当前 `home_half_pane == true` 时，自动收起半窗并视需要转全屏播放或保持播放状态，确保窄屏不出现 `340px` 挤死主视图的抽屉。

### 2.4 个人中心单页化（`pages/account.rs` + `pages/login.rs`）
针对 `AGENTS.md` #1（零解释性文字）与 Android「我的」单页习惯进行改造：

1. **已登录态（`account.rs`）**：
   - **默认列表/常规通知态（单页整屏滚动）**：
     - 取消外层死锁顶部高度的 `Grid [Auto, Star(1.0)]`，改为整页单一 `ScrollViewer`（内容内边距 `12, 12, 12, 24`）。
     - **顶部精简页头**：`22px Bold`「个人中心」+ 右侧紧凑操作按钮（主页 / 刷新 / 退出二次确认）。
     - **资料卡（垂直上下两段）**：
       - 上段：`Grid [Auto, Star(1.0)]` 左 `52px` 圆角头像 + 右「昵称 / Lv / VIP 徽章 / UID / 自适应宽度经验条」；
       - 下段：分隔线 + 4 等分 `Grid [Star(1), Star(1), Star(1), Star(1)]` 横铺「关注 / 粉丝 / 动态 / 硬币」。
       - 关注/粉丝下拉 `Flyout` 在 Mobile 下宽度收窄至 `308.0`（内部列表行同步自适应 `Star(1.0)`，删去写死 `308px` 固定宽）。
     - **通知区**：
       - 分段 Tab 条（`回复 / @我 / 赞 / 系统 / 私信` + 未读 `InfoBadge`）外包横向 `ScrollViewer`（隐藏横向滚动条或自动按需滚动），保证 5 个标签在 `390px` 宽下绝不折行溢出。
       - 下方直接渲染当前选中 Tab 的卡片列表，随页面一体滚动（内部不再套第二个 `ScrollViewer`）。
   - **私信（`NotifyTab::Whisper`）主从单页钻取**：
     - `Mobile` 下彻底取消 `[280px, Star(1.0)]` 左右分栏；
     - `AppShell` 维护 `whisper_detail_open: bool`（切换 Tab、退出登录、返回、拖回 Desktop 时按需重置/兼容）：
       - `whisper_detail_open == false`：通栏显示会话列表，点击会话设置 `selected_whisper` 并置 `whisper_detail_open = true`；
       - `whisper_detail_open == true`：整页切至对话详情视图，顶部固定「`← 会话列表` + 对方昵称 + 主页/刷新按钮」，下方对话历史可独立滚动到底部。
2. **未登录态（`login.rs`）**：
   - `Mobile` 下外边距 `12px`，页头 `22px`，登录卡片去除桌面 `420px` 左对齐限制改为水平 `Stretch`，二维码尺寸 `196×196`。

### 2.5 其他页面响应式补全
- **`pages/home.rs`**：Mobile 下 `header_x = 12.0`、标题 `22px`、换一换按钮紧凑化。
- **`pages/popular.rs`**：接入 `LayoutMode`，Mobile 下 `margin_x = 12.0`、标题 `22px`、卡片网格传递 Mobile `CardMetrics`。
- **`pages/search.rs`**：去掉写死的 `TextBox::width(420.0)`，改为 `Grid [Star(1.0), Auto]` 撑满剩余宽度；结果网格接入 Mobile `CardMetrics`。
- **`pages/player.rs`**：
  - Mobile 视频槽高度按 `((window_width - 24.0) * 9.0 / 16.0).clamp(180.0, 260.0)` 自适应计算（不再写死 `220.0`）；
  - `transport` 在 `compact` 下隐藏 `110px` 桌面音量滑块与「右下角小窗」按钮，仅保留「播停 + 进度时间」紧凑行；
  - `info_and_actions` 在 `compact` 下拆为上下两行：第 1 行 UP 主头像+名字+数据，第 2 行 4 等分 `Grid` 均分「点赞 / 投币 / 收藏 / 分享」胶囊；
  - `related_card` 在 `compact` 下缩略图由 `132×74` 收窄为 `116×65`，给标题留足折行空间。
- **`pages/settings.rs` & `pages/stub.rs`**：统一消费 `LayoutMode`，`12px` 横向边距与 `22px` 页头；`settings.rs` 严守 `AGENTS.md` #1 不添加任何解释性文案。

---

## 3. 分阶段实施与验证计划

### 实施阶段
1. **Phase 1（窗口约束 + 布局度量 + 卡片重做）**
   - `theme.rs` / `feed_pager.rs`：`LayoutMode`、`CardMetrics`、分页函数对齐与单元测试。
   - `video_card.rs`：Mobile 双列紧凑卡（封面左下角数据角标、去悬停动画、骨架同步）。
   - `shell.rs`：`WindowConstraints(390×640)`、启动宽夹紧、`WindowResized` 驱动 `LayoutMode` 与 `CardMetrics`。
2. **Phase 2（壳层与浏览页全链路 Mobile 化）**
   - `shell.rs`：`TitleBar` Mobile 形态（右置搜索图标 + 通知角标）、禁用 Mobile 半窗、Back 返回栈整合。
   - `mini_player.rs`：`56px` 单行紧凑浮条与点击回播放页入口。
   - `home.rs` / `popular.rs` / `search.rs` / `stub.rs` / `settings.rs` 全部接入 `LayoutMode`。
3. **Phase 3（个人中心单页化 + 播放页 Mobile 精修）**
   - `account.rs` / `login.rs`：整页单滚动、4 格横铺、Flyout 窄屏适配、私信主从单页切换。
   - `player.rs`：16:9 动态视频槽高度、紧凑传输栏、两行互动胶囊、`related_card` 收窄。
4. **Phase 4（全量测试与版本文档收口）**
   - 单元测试：`cargo test --workspace` 全绿。
   - 集成自测：`just selftest all` 新增 `mobile` 场景（覆盖 `390×760` resize、双列卡指标、TitleBar 搜索切换、禁用半窗、个人中心私信钻取+Back、播放页 compact）。
   - 自动化点击走查：`scripts/ui-edge.ps1` 增加窄屏拖拽/Resize 实机验证（验证窗口最小宽高钳制、底栏 5 Tab、双列卡点击开播、个人中心单页浏览；若测登录态则结束即清理令牌）。
   - 文档：`docs/ui-design.md` 同步章节 + `docs/Update-0.1.0.md`。
