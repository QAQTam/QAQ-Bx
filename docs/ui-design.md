# biliplay UI 设计（WinUI3 / Fluent 对照 Windows 媒体播放器）

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
Grid rows: [TitleBar(Auto)] [NavigationView(Star)] [MiniPlayer(Auto)]
```

| 层 | 控件 | 设计要点 |
|---|---|---|
| 标题栏 | `TitleBar` | 左：应用图标 + "biliplay"；中：搜索框（占位，路由到搜索页）；右：主题无开关（跟随系统）。高度 Tall。 |
| 导航 | `NavigationView` | `PaneDisplayMode::LeftCompact`（窄图标栏，悬停展开，对照媒体播放器左侧栏）。 |
| 迷你播放条 | 自组（StackPanel+Slider） | 见 §5。播放页打开时隐藏（播放页自带全量传输控制）。 |

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
- 账号（登录态动态化：`AuthState::LoggedIn` 显示真实头像（nav face → EncodedImage）+ 昵称；
  Guest 显示"登录"；Unknown 显示"检查登录态…"。点击 → 登录页）
- 设置（`Setting`）

### 窗口
- `WindowVisuals::new().backdrop(WindowBackdrop::Mica).client_size(1280.0, 800.0)`，theme 跟随 System。
- 最小客户端尺寸建议 ≥ 720×560（卡片网格 2 列底线）。

### 登录（已接入，2026-11-15）

- 桥接层 `crates/bili-ui/src/auth.rs`：`Auth` 服务装配 FileCredentialStore + QrAuthPlugin
  （同 CLI 组装），每个阻塞入口内建 current_thread tokio runtime `block_on`；
  `LoginFlow`（`trait LoginFlow: Send`）放 `Arc<Mutex<Option<Box<dyn LoginFlow>>>>`
  与 shell 的 2s 轮询 tick 共享。
- 状态机：`AuthState{Unknown/Guest/LoggedIn(UserInfo)}` + `LoginFlowState{Idle/Starting/
  AwaitingScan/Scanned/Expired/Failed}`；Message：AuthProbed/AuthStartLogin/LoginQrReady/
  LoginPollTick/LoginPolled/AuthCancelLogin/AuthLogout(二次确认)/AuthLoggedOut。
- 数据目录：`BILIPLAY_DATA_DIR` 或 `<cwd>/data/credentials.json`（与 CLI 一致）。
- QR 展示直接用插件 `qr_png()` → `EncodedImage` → `Image::source_data`（UI 无 qrcode 依赖）。
- 冒烟辅助环境变量：`BILIPLAY_ROUTE`（初始页）、`BILIPLAY_VIDEO`（初始播放项）、
  `BILIPLAY_SMOKE_LOGIN=1`（自动发起扫码流程，generate 只读）。
- 登出按钮双击确认（服务端作废不可逆）；失败以 InfoBar(Error) 显示。

### 个人中心 / 消息中心空位（骨架）

- 页脚账号入口（`Route::Login`）登录态下即**个人中心**：账号卡片（头像 / 昵称 /
  mid·Lv / 登出）+ **消息中心空位**，二者在同一页内自上而下渲染。
- **消息中心不设独立导航项**（§1 导航项表格与 `Route` 枚举都不加）：消息中心是个人中心
  内部的一个区块，不是顶层页面。Guest 态（二维码登录流程）不渲染该区。
- 空位内容：`回复我的 / @我的 / 收到的赞 / 系统通知 / 私信` 五行占位 +
  InfoBar「数据源待接入」，纯展示、无回调；落地在 `pages/login.rs` 的
  `message_center_stub()`。
- 接入落点：`bili-core` 新增 `MessageClient`（Cookie，注入 `CredentialProvider`），
  端点线索见 `docs/bilibili-api-inventory.md` §8（`dynamic/entrance` 红点、
  `api.vc.bilibili.com` 私信未读）；消息中心 API 尚未逆向，骨架期不实现。
- 与账号资料的关系：本页即 `UserProfile` 契约的展示落点（见下节）。

### 账号资料契约 / nav 统一拉取（已接入）

- `bili-core/src/account.rs`：`UserProfile`（`is_login/mid/uname/face/level/vip/vip_due_date`）、
  `VipStatus`（`None/Monthly/Annual/Other`，由 nav 的 `vipStatus`+`vipType` 推导）、
  `space_url()`（mid 派生，无需 API）、`NavClient`（nav 24h 缓存 + 失败降级复用旧快照）。
- **nav 只拉一次**：改造前 `bili-core/wbi.rs`（取 `wbi_img`）与 `bili-ui/auth.rs`
  （取昵称头像）各拉一次、各自定义私有 struct；现在 WBI 签名器、feed/播放/弹幕客户端
  与 UI 账号展示共用同一个 `Arc<NavClient>`（`with_nav` 构造，`Auth::nav_client()` 装配）。
  登录成功 / 登出后 `invalidate()` 作废缓存，下次探测用新凭据重拉。
- 个人中心账号卡片展示：昵称、`mid`·等级、**会员状态**（`VipStatus::label()`）、
  **空间主页**（`HyperlinkButton` 的 `NavigateUri`）；头像位图仍走 `Auth::fetch_image`。
- **大会员影响清晰度**（`inventory §2`：未登录/非大会员时 `dash.video` 不含高档）：
  `shell.rs::max_qn_for()` 读共享 nav 缓存 → 大会员 116（1080P60）/ 其余 80（1080P），
  取代原硬编码 `MAX_QN=116`。缓存未就绪时保守取 80（服务端本就不下发高档）。
- 消息中心空位保持纯占位，未引入任何插件；所有能力仍是 `bili-core` 契约 + 具体客户端。

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
- 封面加载：`Image::source_data(EncodedImage)` —— bili-core 后续提供 `image_loader`（reqwest 下载
  + 磁盘缓存 + Referer 头），骨架期先用占位底色（`Rectangle` 圆角 + 主题色变体）。
- 卡片宽度 240px 基准（8GB 机器一屏 4~5 列不卡）。

## 4. 播放页（PlayerPage，未来视频通道落点）

```
Grid columns: [视频区(Star, MaxWidth 960)] [信息区(380px)]
视频区 Grid rows: [视频面(Star)] [传输控制(Auto)]
```

| 区 | 内容 |
|---|---|
| 视频面 | **骨架期**：黑底 `Border` + 居中 `ProgressRing`/提示文字"视频通道待接入(mpv↔SwapChainPanel)"。正式期：`SwapChainPanel`（mpv render API D3D11 interop）+ 弹幕覆盖层（windows-canvas）。 |
| 传输控制 | 对照 MediaTransportControls：左（音量 Slider+按钮）；中（上一部/播放暂停/下一部 + 进度 Slider + 时间 TextBlock ×2）；右（倍速/全屏/弹幕开关）。骨架期控件齐全但只绑定占位状态。 |
| 信息区 | StackPanel: 标题(20px Bold) → UP 行（PersonPicture + 名字 + 粉丝数）→ 数据行（播放/弹幕/发布时间）→ 简介（可折叠 Expander）→ 分 P 列表 → 评论区（占位：老接口 `/x/v2/reply/main` 数据后续接入）。 |

返回：TitleBar 返回键（`is_back_button_visible(true)` + history pop），或面包屑。

## 5. 迷你播放条（MiniPlayer，对照底部 MediaTransportControls）

```
Border(顶部 1px 分隔线, 高 64)
└─ Grid columns: [Auto 封面+标题] [Star 控制] [Auto 右侧工具]
   ├─ 左: 80×45 圆角封面 + (标题 12px / UP 11px opacity0.6)
   ├─ 中: StackPanel: Slider(进度, theme accent) + 时间行  |  按钮行: Previous/PlayPause/Next(Symbol)
   └─ 右: 音量(Slider 80px) / 队列(Video) / 弹幕开关 / 全屏
```

状态由 AppShell 顶层持有（`NowPlaying { item, position, playing }`），播放页与迷你条共享同一 Message 源。

## 6. 播放队列 / 弹幕开关 / 设置页

- 队列：Flyout 或右侧滑出 SplitView（骨架期不做，仅留 Message 槽位）。
- 设置页：`RadioButtons`（清晰度偏好：360P/480P/720P/1080P/1080P+/4K）、`ToggleSwitch`（自动播放、弹幕、硬解）、`NumberBox`（弹幕密度）——数据落 `data/settings.json`。
- 搜索页：页头 AutoSuggestBox + 结果卡片网格（复用视频卡片），建议词 API 后续接入。

## 7. 组件骨架结构（本轮落地）

```
crates/bili-ui/src/
  main.rs        App 入口（run AppShell）
  shell.rs       AppShell：TitleBar + NavigationView + 路由 + MiniPlayer + 窗口 visuals
  router.rs      Route enum（home/popular/…/player/…）+ route() 分发
  theme.rs       设计 token 常量（间距/圆角/字号）+ 通用小件
  data.rs        占位数据 VideoItem / ChannelItem（后续由 bili-core feed 替换）
  widgets/
    mod.rs
    video_card.rs    视频卡片 + 卡片网格
    page_header.rs   页头
    mini_player.rs   迷你播放条
  pages/
    mod.rs
    home.rs       推荐页（卡片网格）
    popular.rs    热门页（复用网格，占位数据）
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
