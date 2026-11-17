# QAQ-Bx UI 设计（WinUI3 / Fluent 对照 Windows 媒体播放器）

> 依据：`docs/bilibili-login-research.md` 同轮 Fluent 考古（Windows 媒体播放器截图分析 +
> ZuneMusic 11.2609 解包 + winui-controls.pri dump `.qaqh/research/winui-controls.xml`）。
> 布局母版 = Windows 媒体播放器（ZuneMusic `AppChrome` / `PageHeader` / `ResponsiveItemsRepeaterList`
> / `MediaTransportControls`）。

## 0. 硬约束（不变）

- 纯 Rust，禁止一切 web 技术栈；渲染走 windows-reactor（WinUI3 声明式，Elm 架构）。
- 用户机器 8GB RAM / 4 核 Xeon E3-1230 V2（无 AVX2）/ RX560 4GB：**内存与 CPU 紧缺是第一约束**。
  - 不做常驻图片缓存池（用 WIC 解码缓存即可）；列表虚拟化必须开（`keyed_children` + ScrollViewer）。
  - Mica 背景代替自绘模糊；动画只用系统默认（reactor transitions），禁自定义逐帧动画。
- 组件直接引用 WinUI 系统主题资源（`ThemeBrush::*`），不抄死颜色 → 自动跟随深浅色与强调色。

## 1. 应用壳（AppShell，对照 AppChrome）

```
Grid rows: [TitleBar(Auto)] [NavigationView(Star)]
```

| 层 | 控件 | 设计要点 |
|---|---|---|
| 标题栏 | `TitleBar` | 左：应用图标 + "QAQ-Bx"；中：搜索框（随窗口宽度在 `200..360` DIP 自适应，路由到搜索页）；右：通知按钮（登录态）。高度 Tall。 |
| 导航 | `NavigationView` | `PaneDisplayMode::LeftCompact`（窄图标栏，悬停展开，对照媒体播放器左侧栏）。 |
| 全局播放条 | **已移除** | 不再保留全局底部 `MiniPlayer`；播放控制与生命周期完全收口至每个视频容器自身的独立控件（全屏播放页 `PlayerPage` 与首页右侧半窗 `HalfPanePreview`），离开播放页或关闭半窗即停止播放并释放引擎资源。 |

### 导航项（顶部 menu_items）

| 页 | tag | 图标(Symbol) | 数据源（后续接入） |
|---|---|---|---|
| 推荐 | `home` | `Home` | 综合 feed（需 WBI 签名） |
| 热门 | `popular` | `Like`(无则 `Favorite`) | popular API |
| 关注 | `following` | `People` | 动态/关注 feed（需登录） |
| 直播 | `live` | `Camera` | live feed |
| 收藏 | `favorites` | `Favorite` | 收藏夹列表（需登录） |
| 历史 | `history` | `Clock` | 历史记录（需登录） |
| 稍后再看 | `watch-later` | `TwoPage`/`Video` | 稍后再看（需登录） |

### footer_menu_items
- 账号（登录态动态化：`AuthState::LoggedIn` 显示真实头像 + 昵称；
  Guest 显示"登录"；Unknown 显示"账号"。点击 → 登录页 / 个人中心）
- 设置（`Setting`）

### 标题栏右侧（`TitleBar::right_header`）
- 登录态下展示通知入口按钮（`Mail` 图标 + 未读数 `InfoBadge`，无未读时不显示角标），点击直达个人中心的通知区（有未读时自动定位到首个未读分类）。

### 窗口与响应式形态（v0.1.0 Mobile + 半屏双维断点）

- `WindowVisuals::new().backdrop(WindowBackdrop::Mica).client_size(1280.0, 800.0).constraints(WindowConstraints { min_width: Some(390.0), min_height: Some(640.0), .. })`。
- **正交两维布局单源（`crates/qaq-bx-ui/src/layout.rs`）**：
  - `ShellChromeMode::{Mobile, Desktop}`（断点 `860.0` DIP —— 恰好对应桌面 `36px` 边距下 3 列下限 `56+72+236×3+24=860`）：
    - `Mobile`（`< 860.0`，涵盖 `390..859` 全部 2 列窗口与 1280/1080p@125% 半屏贴靠）：隐藏左侧 `NavigationView` 改用底部 5-Tab 栏（推荐/热门/搜索/我的/更多 `2×3` 菜单含关注·直播·收藏·历史·稍后再看·设置）；禁用右侧 `HalfPanePreview`；`MiniPlayer` 切为 `56px` 单行浮条；推荐/热门/搜索严格 **2 列等宽铺满**（`160..414px` 弹性卡宽，播放/弹幕移至封面左下角半透明胶囊，关闭悬停缩放动画）；播放页与个人中心走单列自适应模式。
    - `Desktop`（`>= 860.0`，3+ 列桌面全态）：保留左侧 `NavigationView`、双栏播放页、可选半窗抽屉与桌面 236×240 卡片网格。
  - `ContentLayoutTier::{Compact (<600), Medium (600..860), Expanded (>=860)}`：
    - `Compact`（`< 600`）：`TitleBar` 右侧仅留登录态通知铃铛（防挤爆系统三键），播放页视频高上限 `260` DIP；
    - `Medium`（`600..860`）：保持 Mobile 外框与 2 列等宽卡片，同时 `TitleBar` 右上额外提供搜索按钮触发**顶部悬浮搜索浮窗**，播放页视频高上限 `360` DIP；
    - `Expanded`（`>= 860`）：全量桌面双栏与 `TitleBar` 内嵌 `360px` 搜索框。

### 登录（已接入，2026-11-15）

- 桥接层 `crates/qaq-bx-ui/src/auth.rs`：`Auth` 服务装配 FileCredentialStore + QrAuthPlugin
  （同 CLI 组装），共享 `crate::runtime::shared_runtime()` `block_on`；
  `LoginFlow`（`trait LoginFlow: Send`）放 `Arc<Mutex<Option<Box<dyn LoginFlow>>>>`
  与 shell 的 2s 轮询 tick 共享。
- 状态机：`AuthState{Unknown/Guest/LoggedIn(UserInfo)}` + `LoginFlowState{Idle/Starting/
  AwaitingScan/Scanned/Expired/Failed}`；Message：AuthProbed/AuthStartLogin/LoginQrReady/
  LoginPollTick/LoginPolled/AuthCancelLogin/AuthLogout(二次确认)/AuthCancelLogout/AuthLoggedOut。
- 数据目录：`QAQ_BX_DATA_DIR` 或 `<cwd>/data/credentials.json`（与 CLI 一致）。
- QR 展示直接用插件 `qr_png()` → `EncodedImage` → `Image::source_data`（UI 无 qrcode 依赖）。
- 冒烟辅助环境变量：`QAQ_BX_ROUTE`（初始页）、`QAQ_BX_VIDEO`（初始播放项）、
  `QAQ_BX_SMOKE_LOGIN=1`（自动发起扫码流程，generate 只读）。
- 登出按钮二次确认（确认退出 / 取消）；失败以 InfoBar(Warning) 显示。

### 个人中心与通知中心（已接入，2026-11-16）

- 页脚账号入口（`Route::Login`）登录态下即**个人中心**（`pages/account.rs`）：
  顶部资料卡（头像 / 昵称 / 等级与经验进度条 / 大会员状态与到期日 / UID / 关注·粉丝·动态·硬币计数 / 主页·刷新·退出，其中`关注`与`粉丝`挂载按钮下拉 `Flyout` 分页列表而非外部跳转）+
  下方**通知中心**（`回复` / `@我` / `赞` / `系统` / `私信` 五个分段标签，带未读 `InfoBadge`）。
- **通知中心不设独立导航项**（§1 导航项表格与 `Route` 枚举都不加）：标题栏右侧通知入口与页脚账号入口共用该工作区；Guest 态仅渲染简洁扫码登录。
- 数据来源：`qaq-bx-core/src/account.rs`（`NavClient` + `/x/web-interface/nav/stat` + `/x/relation/{followings,followers}`）与
  `qaq-bx-core/src/notify.rs`（`NotifyClient`：`msgfeed/unread`、`single_unread`、`link_setting/get`、
  `msgfeed/{reply,at,like}`、`sys-msg/{query_unified_notify,query_user_notify,query_notify_list}`、
  `session_svr/get_sessions` + `user/cards` + WBI 签名 `fetch_session_msgs`）。
- 交互联动：通知与私信中的视频链接（含 `BV...`）直接拉起应用内播放；私信采用左侧会话列表 + 右侧对话历史分栏。

### 账号资料契约 / nav 统一拉取（已接入）

- `qaq-bx-core/src/account.rs`：`UserProfile`（`is_login/mid/uname/face/level/current_exp/next_exp/coins/vip/vip_label/vip_due_date/official_title`）、
  `VipStatus`（`None/Monthly/Annual/Other`，由 nav 的 `vipStatus`+`vipType` 推导）、
  `AccountStat`（`following/follower/dynamic_count`）、`space_url()`（mid 派生）、
  `NavClient`（nav 24h 缓存 + 失败降级复用旧快照）。
- **nav 只拉一次**：WBI 签名器、feed/播放/弹幕/通知客户端与 UI 账号展示共用同一个
  `Arc<NavClient>`（`with_nav` 构造，`Auth::nav_client()` 装配）。
  登录成功 / 登出后 `invalidate()` 作废缓存，下次探测用新凭据重拉。
- **大会员影响清晰度**（`inventory §2`：未登录/非大会员时 `dash.video` 不含高档）：
  `shell.rs::resolve_params_for()` 读共享 nav 缓存与用户画质偏好，未登录/非大会员保守上限 80（1080P）。

## 2. 通用页模式（对照 PageHeader / LibraryPageHeader）

所有浏览页 = `ScrollViewer > Border(padding 36,24,36,36) > StackPanel(spacing 24)`：

1. **页头**：大标题（FontSize 28 Bold）+ 副标题（opacity 0.6，计数/说明）。
2. **内容**：卡片网格或列表。刷新用页头右侧 `AppBarButton(Sync)`。

## 3. 视频卡片（核心复用件，对照 ResponsiveItemsRepeaterList 卡片）

```
Border(corner_radius 8, background ThemeBrush::CardBackground, border ThemeBrush::CardStroke)
└─ StackPanel(vertical)
   ├─ 封面区 Grid(高 = 宽*9/16, clip 圆角 8 上侧)
   │   ├─ Image(Stretch::UniformToFill, source=封面)
   │   └─ 右下角时长徽标: Border(半透明黑底, corner_radius 4, TextBlock 白色 12px "12:34")
   ├─ 文本区(padding 12, spacing 4)
   │   ├─ 标题 TextBlock(14px SemiBold, TextWrapping::Wrap, MaxLines=2)
   │   ├─ UP主 TextBlock(12px, opacity 0.6)
   │   └─ 数据行 StackPanel(horizontal): ▶ 播放量  💬 弹幕  (12px, opacity 0.6)
```

- 网格：`VariableSizedWrapGrid(orientation Horizontal, item_width ≈ 240+12, item_height ≈ 卡高+12)`
  外包 `ScrollViewer`。卡片点击 → 打开播放页。
- 封面加载：`Image::source_data(EncodedImage)` —— qaq-bx-core 后续提供 `image_loader`（reqwest 下载
  + 磁盘缓存 + Referer 头），骨架期先用占位底色（`Rectangle` 圆角 + 主题色变体）。
- 卡片宽度 240px 基准（8GB 机器一屏 4~5 列不卡）。

## 4. 播放页（PlayerPage，未来视频通道落点）

```
Grid columns: [左滚动列(Star)] [右栏(Pixel 360)]
左滚动列 StackPanel: 视频槽(高 420, margin 16) → 标题(20px Bold) → UP 行(36px 圆形头像 + 名字 + 播放/弹幕/时长) → 数据行(action pills) → 简介(可折叠 Expander) → 评论区
视频面为浮动挂载(margin 16/16/372/0):顶部主位 ↔ 右下角小窗(PiP)通过尺寸切换过渡,SwapChainPanel 不销毁
```

| 区 | 内容 |
|---|---|
| 视频面 | **已接入**：`SwapChainPanel`（mpv render API D3D11 interop，见 `docs/video-channel.md`）+ 弹幕覆盖层；桌面双栏下滚动超 300px 过渡为右下角小窗（PiP）；Mobile / Medium 单列下顶部「`16:9` 视频槽 + 单行紧凑播控 + `简介 / 评论` 双页签」吸顶固定，下方独立滚动（「简介」内含 UP/折叠简介/4 等分互动/相关推荐；「评论」直达评论区）。 |
| 传输控制 | `PlayerPage` 桌面双栏：进度 Slider + 播放/暂停 + 时间 + 静音按钮/音量 Slider + **弹幕开关**（与设置页 `danmaku_enabled` 单源同步）+ 小窗切换；窄桌面（`< 540` DIP 左栏）自动切换紧凑档位防裁切。Mobile / Medium 单列：播放/暂停 + 进度 Slider + 时间 + 静音切换 + **弹幕开关**。首页右侧半窗 `HalfPanePreview`：同样内置独立进度 Slider + 播放/暂停 + 时间 + 静音/音量（宽于 `440` DIP 展开滑块）+ **弹幕开关**，宽度按内容区边界钳制在 `[340, 820]` DIP 且拖拽起止保持 `VideoSurface` 稳定挂载。 |
| 信息区/评论区 | **评论区已接入真实数据**（`/x/v2/reply/main` 主楼 + `/x/v2/reply/reply` 楼中楼）：总评论数、按热度/按时间排序切换、置顶/UP主/LV/IP属地/UP主觉得很赞徽标、楼中楼预览与展开/加载更多/收起、空态/错误重试；写接口（发评/点赞）未接入。右栏 360px「接下来播放 · 相关推荐」**已接入真实数据**（`/x/web-interface/archive/related`，随播放代次刷新，封面复用 ImageCache，点击走同一 open_video 管线）。 |

返回：TitleBar 返回键（`is_back_button_visible(true)` + history pop），或面包屑。

## 5. 独立视频控件与生命周期（替代原全局迷你条）

- 不再渲染全局底部 `MiniPlayer`，由 `PlayerPage` 与 `HalfPanePreview` 各自承担播放/暂停、进度跳转、静音/音量调节及实时弹幕开关。
- 仅当视频处于可见宿主（`Route::Player` 或 `Route::Home && home_half_pane`）时保持播放与引擎心跳；返回首页、切换导航或关闭半窗时自动停止播放并释放 `VideoEngine`。
- 半窗展开进入完整播放页或窄屏自动升格至 `Route::Player` 时保持播放与状态无缝衔接。

## 6. 播放队列 / 弹幕开关 / 设置页

- 队列：Flyout 或右侧滑出 SplitView（骨架期不做，仅留 Message 槽位）。
- 设置页：`RadioButtons`（清晰度偏好：360P/480P/720P/1080P/1080P+/4K）、`ToggleSwitch`（自动播放、弹幕、硬解）、`NumberBox`（弹幕密度）——数据落 `data/settings.json`。
- 搜索页（**已接入**）：标题栏/页内 `TextBox` + 搜索按钮 → `/x/web-interface/wbi/search/all/v2`（WBI 签名）→ 结果卡片网格（复用视频卡片，封面经 ImageCache 异步填充）；建议词 API（`search/default`）后续接入。

## 7. 组件骨架结构（本轮落地）

```
crates/qaq-bx-ui/src/
  main.rs        App 入口（run AppShell）
  layout.rs      纯值布局单源：ShellChromeMode / ContentLayoutTier / ViewportLayout / CardMetrics
  shell.rs       AppShell：路由 + 登录/播放/推荐流状态机 + 窗口约束
  router.rs      Route enum（home/popular/…/player/…）+ route() 分发
  theme.rs       设计 token 常量（间距/圆角/字号）+ 通用小件
  data.rs        展示模型 VideoItem（内容全部来自 qaq-bx-core 卡片族转换，无占位数据）
  widgets/
    mod.rs
    chrome.rs        响应式 TitleBar / 悬浮搜索浮窗 / Mobile 底部 5-Tab 与 2×3 更多菜单
    video_card.rs    视频卡片（桌面标准卡 + Mobile 2 列紧凑卡）+ 虚拟/行网格
    video_surface.rs SwapChainPanel ↔ VideoEngine 高 DPI 适配挂载桥
  pages/
    mod.rs
    home.rs       推荐页（卡片网格）
    popular.rs    热门页（复用网格，真实 popular API 分页）
    favorites.rs  收藏/历史/稍后（空态页 InfoBar 提示"待接入"）
    search.rs     搜索页（AutoSuggestBox + 空态）
    settings.rs   设置页（控件齐全，状态暂存内存）
    player.rs     播放页（占位视频面 + 传输控制 + 信息区）
```

设计 token（theme.rs）：
- 圆角：卡片 8 / 徽标 4 / 视频面 8（Fluent overlay corner）
- 间距：页面 padding 36/24/36/36，卡片 gap 12，文本 spacing 4~8
- 字号：页标题 28 / 卡片标题 14 / 辅助 12 / 迷你条标题 12
- 时长/数据格式化：`fmt_duration(secs)->"12:34"`、`fmt_count(n)->"1.2万"`

## 8. 视频嵌入通道（已定案，详见 `docs/video-channel.md`）

**调研已完成（2026-11-15）**：libmpv 原生 D3D11 render API（2024 master 引入，`render_d3d11.h`）
+ reactor `request_set_swap_chain`（内部 `ISwapChainPanelNative::SetSwapChain`）——GPU 零拷贝
闭环成立。发布版最低 mpv 版本待实测（候选 0.40+）；保底顺序 SW 渲染 → OpenGL → --wid。
实施清单见 `docs/video-channel.md` §6。
