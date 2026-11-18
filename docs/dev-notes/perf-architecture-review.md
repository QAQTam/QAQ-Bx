# v0.4.0 前瞻:架构性能问题研究(窗口拖动/缩放卡顿专章)

> 记录时间:2026-11-18。研究基线:`feat/v0.4.0`(= `feat/v0.3-native-player` HEAD `76f5f50`)。
> 方法:windows-reactor 框架源码(43k 行)+ 应用三 crate(33k 行)全静态走查,
> 所有结论附 `file:line`;量级判断分「实证」「推断」「未验证」三档,未复测前不得当作实测数据。
> 姊妹文档:性能修复史 `long-run.md` §2-P1/§6,黑屏排查 `resize-black-screen.md`,
> 崩溃相邻问题 `crash-0x359595-xamlroot.md`。

## 0. 结论速览(TL;DR)

拖框卡顿不是单点 bug,是**三层成本在高频事件上叠加**的结果:

| # | 根因 | 层 | 每事件成本量级 | 可修性 |
|---|---|---|---|---|
| R1 | 框架对每条 Message **无条件全量重跑 `view()`**(无 dirty-check、无同型消息合并) | reactor | 高(推断 3–15ms@debug) | 框架侧可加 coalesce,应用侧可降频 |
| R2 | 窗口尺寸事件按 XAML 布局节拍全量入队(每 ~2DIP 一条 `WindowResized`),应用照单全收 | reactor→app | 高频放大器 | app 侧 16ms 采样或框架 latest-wins |
| R3 | 播控页评论/关联视频**未虚拟化**,却随每次重渲染全量建行 | app | 中-高(正比内容数) | 接 VirtualSource |
| R4 | 跨布局档位触发组件树 DROP/CREATE → native 路径**换源重建**(1.5s 节流) + 数百 ms 黑屏 | app→video | 尖峰(实测过同类:E11) | 结构统一(0.3 已知待办) |
| R5 | WinUI `TitleBar` 控件每次尺寸变化重算拖拽区(`UpdateDragRegion`),且该路径有**已知无判空 XamlRoot 崩溃点** | 框架 DLL | 低-中 + 崩溃风险 | 换自绘标题栏可绕 |
| R6 | 日常在跑的包是 **debug(opt-level=0)**,workspace 无任何 `[profile]` 调优 | 构建 | 全链路 ×2–×20 | 白捡的,先修 |
| R7 | 拖框期间 Mica 背景合成、全屏 probe 往返、250ms 心跳与 resize 消息交错 | 框架/合成 | 低(未验证) | 实测后决定 |

**已排除**(本轮走查确认干净,别再查):引擎/弹幕层 ResizeBuffers 已有 120ms 合并
(`engine.rs:47`、`stage.rs:44`);UI→引擎全非阻塞 channel(`engine.rs:531-555`);
网络/设置落盘/媒体装载均不在 UI 线程(`shell.rs:5168`、`native.rs` M1 装载线程);
封面字节 `Arc<[u8]>` 免重拷(decoder 在 XAML 内部);gamepad 空闲 500ms 节拍、
事件驱动(`gamepad.rs:26-27`);`NativePlayer::status()` 仅 2 短锁 + 3 进程内属性读
(`native.rs:600-633`)。

## 1. 线程与表面拓扑(现状)

```
[UI 线程] WinUI3 DispatcherQueue(框架独占)
  ├─ XAML 布局/渲染(TitleBar 拖拽区、Mica、VirtualSource 行 realize)
  ├─ reactor drain(每次唤醒 ≤64 条消息,每条:dispatch→render_view→plan→apply)
  ├─ SwapChainPanel ×2(视频/mpv 通道、弹幕透明层)+ MediaPlayerElement(native 通道)
  └─ 应用 timer:PositionTick 250ms / 去抖器 150ms、300ms、80ms
[qaq-bx-async ×2]  tokio 共享 runtime(网络:feed/弹幕分段/playurl)
[mpv render 线程]  D3D11 device + Present(1) vsync 节拍 + D2D 弹幕叠加(mpv 模式)
[弹幕 stage 线程]  独立 D3D11 device + 预乘 alpha composition swapchain,vsync 满帧
[native 装载线程]  COM MTA,MPD→AdaptiveMediaSource 建源(用完即走)
[gamepad 线程]     XInput 轮询,16ms(活动)/500ms(空闲)
[层监听] 每 spawn_background 一根短命 OS 线程(阻塞 recv 转发事件)
```

表面合成栈(native 播放 + 弹幕开):XAML 内容 + MPE 内部视频表面 + 弹幕 flip-swapchain
——三层同窗合成,由 DWM 负责;各层重绘互相独立,**这层没有共享锁**(实证)。

## 2. 拖一次窗框,一帧内到底发生了什么(全链路)

以下逐跳标注证据。`reactor` = `D:/Projects/qaq-winui-reactor/crates/libs/reactor/src`。

1. **Win32**:拖框进入 `WM_ENTERSIZEMOVE` 模态循环,`WM_SIZING` 按鼠标包速率
   (60–125Hz)驱动 `SetWindowPos` → `WM_SIZE`。模态循环仍泵消息,
   所以 DispatcherQueue 上的活**全部插进拖框节奏里抢线程**。
2. **XAML**:根元素每 DIP 变化跑一遍 measure/arrange;
   `TitleBar` 属性/尺寸变更触发 `UpdateDragRegion`(上游 `TitleBar.cpp:647`
   `GetBounds` 读 XamlRoot **无判空**,崩溃文档 §4.5)——这段在框架 DLL 内,
   应用层去抖完全罩不住。
3. **reactor→app**:窗口尺寸经 root `SizeChanged` 观察者转成消息
   (`reactor/native/winui/window/mod.rs:182-199`,无节流;每事件附带
   `ClientSize`+`GetDpiForWindow` 查询)。
4. **入队**:`LocalSender::send` 每条消息一次全局锁 + 至多一次 wake
   (`reactor/component/messaging.rs:114-142`)。**队列没有「同型消息取最新」
   语义**——拖 400px 就是几十条 `WindowResized` 排队,每条都会被完整消费。
5. **消费**:drain 每轮预算 64 条(`reactor/native/app/mod.rs:20`),逐条:
   `dispatch` = `update(msg)` + **无条件 `render_view()`**
   (`reactor/component/scope.rs:120-128`)→ 整棵 AppShell IR 重建。
6. **AppShell::view**(`shell.rs:5596` 起):Mica 声明、chrome TitleBar 重建、
   路由页输入重建——其中 `PlayerInput` 构造深拷 `item.clone()`、
   `related: Vec<VideoItem>` 全表 clone(`shell.rs:5687-5710`;
   `comments` 是 `Arc` 语义,便宜,`pages/player.rs:126-133`)。
7. **子组件展开**:父每次渲染对每个子节点重建输入并**深比较**旧新
   (`scope.rs:88-99 apply_input`);`layout`/`position` 变了 → `PlayerPage::view()`
   重跑 → 评论 `for` 循环逐行建 View(`pages/player.rs:1279-1285`)+
   关联视频逐行建 View(`pages/player.rs:1837`),**无虚拟化**;
   pip/非 pip 结构分支还会引起组件 DROP/CREATE(黑屏文档 §3 已知残留)。
8. **reconcile→XAML**:diff 出 mutation 批应用到可视树 → 又触发一轮局部布局;
   SwapChainPanel `Metrics` 事件转发到引擎/弹幕层(已合并,干净)。
9. **尖峰项**:若本帧跨了布局档位:`GRID_REBUILD_DEBOUNCE` 150ms 后首页
   VirtualSource 全量重建;native 播放页则组件树重建 → `surface detached` →
   新面板 `remount_pending` 换源续播(≥1.5s 节流,`native.rs:54`),
   画面冻结/黑数百 ms + 装载线程起线程。

单帧预算粗账(推断,debug 包,E3-1230V2 级 CPU):5+6+7 ≈ 3–15ms,
叠加 2+8 的框架侧成本,**每事件 8ms 预算大概率击穿** → 窗口内容跟手性崩坏、
掉帧;鼠标越甩越卡。这解释了 0.2.0「合并了引擎 ResizeBuffers 仍有卡顿」——
当年修的是 GPU 表面重分配,**没修消息风暴 × 全量重渲染**。

## 3. 根因详述与建议(R=根因,F=修复方向)

### R1+F1 框架每条消息全量 `view()`,无合并语义 —— **核心**
- 证据:`scope.rs:120-128`(dispatch 无条件 render_view);`messaging.rs` 全文
  无任何按 key 合并;`expand.rs:154` 每轮把整棵声明树重新展开。
- 建议(框架侧,reactor 是自家 fork,可改):
  - **latest-wins 合并**:对同一组件的同一「可合并消息类型」(建议加
    `Message: coalescable()` 或按 TypeId 白名单),drain 时先扫队列只留最后一条。
    对 `WindowResized` 立竿见影:60 条 → 1–2 条。
  - 备选:dispatch 后先 diff 新 view 与 `cached_view`,相等则跳 plan(治不了
    「每帧都重建 IR」本身,但省 diff 下游;优先级低)。
- 风险:合并语义要保守(默认关,消息类型显式声明可合并),否则事件语义丢失。

### R2+F2 尺寸事件高频直灌,应用照单全收
- 证据:`window/mod.rs:182-199`(root SizeChanged 无节流;每事件另做
  `ClientSize` + `GetDpiForWindow` 两次查询,`IWindow2/AppWindow` 解析在
  装配期一次性完成)→ `shell.rs:5620`(`on_window_size` 每事件一条
  Message)→ `shell.rs:4780`(处理本身很轻,重的是它触发 R1/R3)。
- 建议(应用侧,小改动):把 `Message::WindowResized` 改为「存最新值 +
  60Hz(16ms)合并投递」——用一个 `pending_size: Option<WindowSize>` +
  单发 16ms timer 实现;布局档位判断、引擎 `set_metrics` 全部只经这条采样路径。
  F1 落地后此项可降级为保险丝。
- 顺带:拖拽期间挂起 `PositionTick` 的整页重渲染(进度条本来 250ms 才动一格,
  拖框时没人看它),静止 150ms 后恢复——一条布尔就能砍掉 4Hz 的整页重建底噪。

### R3+F3 评论/关联列表未虚拟化 × 每消息重建
- 证据:`pages/player.rs:1279/1285/1837` 纯 `for` 建 View;一屏评论+关联
  数十行 × 每行多节点,乘上 R1 的重建频率。
- 建议:评论与关联视频接 `VirtualSource`(框架已有,首页在用;
  注意 `reconcile/virtualization.rs` 行 realize 也走 drain 预算);
  或至少给行组件加 `input_changed` 短路(行输入用 `Arc<ReplyItem>`)。
  首页卡片曲线已做过整数对齐,不重复施工。

### R4+F4 跨档位组件树重建 → native 换源黑屏尖峰
- 证据:`resize-black-screen.md` §3「pip/非 pip 分支结构不同仍会触发组件
  重建」;`media-engine-comparison.md` §7.1(同一份 MediaSource 不能复用);
  `native.rs:523-547`(remount 1.5s 节流,拖拽风暴期间旧面断新面续)。
- 建议(0.4 的正解,文档已埋雷):**单实例常驻 MediaPlayerElement**
  挂在窗口根视觉层、位置/尺寸用布局属性跟随,路由/档位切换只改宿主
  Border 的显隐与几何——组件永远不 DROP。这是 0.3 Update 文档「已知限制」
  钦定的后续解法,顺带消灭「切页黑数百 ms」。弹幕层同理可常驻。
- 判据:E11/E12 拉伸风暴中 stderr 不再出现成对 `native surface detached/remount`
  与 `VideoSurface CREATE/DROP`。

### R5 框架 TitleBar 拖拽区重算(性能 + 稳定双嫌疑)
- 证据:上游 `TitleBar.cpp:647` 无判空 `GetBounds`,`UpdateDragRegion`
  6 处事件处理器都会触发(崩溃文档 §4.5);本 app 用了 `TitleBar` 控件
  (`widgets/chrome.rs:49-51`,`shell.rs:5599` Mica)。
- 影响:每次 root 尺寸变化在框架 DLL 内做一轮拖拽区几何重算(推断亚 ms 级,
  未实证);更重要的是它和 0x359595 崩溃(「卡顿数秒后闪退」)共用同一
  高频路径——拖框风暴正是「半拆除态被读 XamlRoot」的温床。
- 建议:首选 §8.2 的「保留控件、关 AutoRefresh、低频手动 Recompute」
  (半天级,性能+崩溃面同治);自绘标题栏降级为备胎。

### R6+F6 debug 包在扛日常负载 —— **白捡的 2–20×**
- 证据:workspace `Cargo.toml` 无 `[profile]`(0 处匹配);`just dev` =
  `package-appx.ps1 -Profile debug`;宪法第 5 条使日常分发全是 dev 包。
  R1/R3 那种「每消息几百次小分配 + 深比较」的路径对 opt-level 最敏感。
- 建议:短期给 dev profile 加 `opt-level = 1`(`[profile.dev]`,一行,
  增量编译几乎不伤);或 `just dev` 改 `-Profile release` +
  `debug = "line-tables-only"` 保堆栈可读。P2 内存基线当年实测 debug≈release,
  **但那是内存,不是帧时间**;CPU 帧时间收益需 E14 前后对比坐实。

### R7 次要嫌疑(记录,先测后动)
- Mica 背板随移动重合成(`shell.rs:5599`):低配 GPU 上可试
  `WindowBackdrop::None` A/B。
- 全屏态每个 resize 事件一次 `run_window` 往返 probe(`shell.rs:4804-4811`):
  全屏时本就几乎不能拖框,优先级最低。
- `spawn_background` 每次一根短命 OS 线程(reactor services 实现):
  列表页每次 avatar 批下载都会 spawn;改线程池收益存在但非拖框路径。
- drain 预算 64 条/轮(`native/app/mod.rs:20`):拖框积压时一轮 drain 可占满
  一个 dispatcher 时间片,与模态循环互相挤——F1/F2 落地后自然缓解。

## 4. 0.4.0 落地顺序建议(按 ROI)

> **实施进度(2026-11-18,`feat/v0.4.0`)**:
> - ✅ **F6 profile**:workspace 加 `[profile.dev] opt-level=1` +
>   `[profile.dev.package."*"] opt-level=2` + `[profile.release] lto="thin"`。
>   实测 `just dev` 产物标签变为 `optimized + debuginfo`;`cargo test --workspace`
>   core 60+6 / login 7 / ui 67 / video 25 全绿,selftest 全场景 PASS
>   (half-pane/mobile/fullscreen 含 mpv+d3d11va 实开画)。
> - ✅ **F1 latest-wins 合并(框架正解)**:reactor 新增
>   `Component::coalesce_key`(挂载时绑进 `LocalSender::coalescer`,
>   合并发生在 `component/messaging.rs` 入队点),
>   `LocalSender::send` 命中同 key 未消费消息时**原位换负载**(保留槽位/序)。
>   默认返回 `None`=对其它使用方零行为变化(opt-in)。应用侧 `AppShell` 只对
>   `WindowResized`/`FullscreenProbeResult` 声明可合并(单测
>   `latest_value_messages_are_coalescable_and_the_rest_are_not` 钉死语义边界:
>   带副作用的 `PositionTick`/`Back`/`WindowResizeSettled` 一律 `None`)。
>   reactor 测试 `coalesced_messages_deliver_only_the_latest_queued_payload`
>   (3→1)+ `coalescing_keeps_distinct_keys_and_ordinary_messages_apart`
>   (5 槽只并同键的 2)双绿。
> - ✅ **§8.2 TitleBar 开关**:reactor 声明层加 `TitleBar::auto_refresh_drag_regions`
>   (默认 true,行为不变),`WindowTitleBar{height,auto_refresh_drag_regions}`
>   贯通到 winui 装配处的 `SetAutoRefreshDragRegions`。应用侧 chrome 关自动刷新,
>   避开拖框期间逐布局 pass 的全树重算(兼避开 0x359595 崩溃路径)。
>   测试 `title_bar_auto_refresh_opt_out_survives_height_override` 验证与
>   `preferred_height` 互不冲顶 + 默认值不受影响。
> - ⏳ **F3 评论/关联虚拟化**:待做(与 native 弹幕迁移无关,可独立推进)。
> - ⏳ **F4 MPE/弹幕层常驻单实例**:待做(跨 v0.3 迁移线,需协调窗口)。

1. **F6 profile**(半天内见效,零风险)→ E14 坐实收益。
2. **F2 应用侧 16ms 采样 + 拖拽期停心跳整页重建**(应用层单文件改动,可回归)。
3. **F3 评论/关联虚拟化**(纯应用层,配合 E14 度量帧时间)。
4. **F1 框架 latest-wins 合并**(reactor 改动要带上它自己的测试面,
   与上游同步策略另议——此为本研究认定的**架构级正解**,应进 0.4 主线)。
5. **F4 MPE/弹幕层常驻单实例**(0.3 已知限制钦定项,顺带修 R4 尖峰与切页黑)。
6. R5/R7 视 E14/E15 数据决定。

## 5. 量化与验收方案(实施轮配套,AGENTS.md #2/#4)

- **E14 新场景**(`scripts/ui-edge.ps1`):拖框风暴仿真(复用 E11 的
  SetWindowPos 55ms×30 双向,另加 ±40px 慢速往返段模拟人手拖框),
  期间以 stderr 锚点打时间戳计算节拍抖动:
  - 现有锚:`native heartbeat`(30s 一条,太稀,**需临时把门控降到每拍或用
    `--probe` 特征**)、`STAGE-A/B`、`VideoSurface CREATE/DROP`、detach/remount 对。
  - 新建议锚:在 `WindowResized` 处理入口加排空深度计数
    (队列长度可从 drain report 取),输出 `RESIZE-STORM n=… span_ms=…`。
- **帧时间直测**:框架 test 面已有 `measure_work`/`views` 计数
  (`reactor/lib.rs:100`,feature `test`)→ 写一个 host 级 bench:
  连发 100 条 WindowResized,断言合并前后 `views` 次数与耗时;
  该测试可同时守住 F1 的回归。
- **验收判据(拖框专项)**:
  1. E14 全程 detach/remount 对数=0(F4);
  2. 风暴期 AppShell `render_view` 次数 ≤ 风暴时长/16ms ×1.5(F1/F2);
  3. 主观:1080P 播放 + 弹幕开,慢速拖框 2s,窗口跟手无撕裂,
     弹幕/视频表面不出现 >300ms 冻结(F6 前后各录一段对比);
  4. `cargo test --workspace` + `just selftest all` 全绿,E11/E12/E13 不回退;
  5. 任务管理器基线:拖框期间本进程 CPU 峰值记录进本文 §6 实测区。

## 6. 实测记录区(实施轮回填)

- **2026-11-18 F1+F6+§8.2 落地回归**(`feat/v0.4.0`,debug@opt-level 1/2 构建):
  - `cargo test --workspace`:core 60+6 / fix 6 / login 7 / ui 67 / video 25 全绿;
    reactor `cargo test --lib` 346 全绿(新增 3)。`selftest all` 全 PASS。
  - **E12(native 播放,30×快速伸缩 55%↔100%)**:PASS。
    `native media failed`=0,`native media opened`=2,STAGE-A/B 齐;
    但 detach=**12** / remount=**4** —— R4 组件树重建风暴仍在(预期内,F4 未做),
    伸缩后 800ms 帧亮度 244(MF 管线仍在补帧)。
  - **E11(mpv 播放,同风暴)**:三轮采样亮度 119.5/121.3/107.2,全程无黑屏,
    合并改动未伤挂载协议。
  - 拖框主观体感:待用户真机复核(本 harness 无法量帧)。
- (待填)F1+F2 前后 render_view 次数(E14 计数器方案 §5)
- (待填)TitleBar 自绘备胎对比(仅当开关方案体感不足时)

## 7. 本次未验证项(诚实清单)

- 全链路所有**毫秒量级**均为推断,无一帧计时实测(探针方案见 §5)。
- XAML 模态循环内 DispatcherQueue 的实际优先级行为、DWM 对 flip 表面的
  节流策略:未做 ETW/wpr 追踪。
- MPE 内部管线对宿主尺寸变化的响应成本(黑屏文档修复后未单独测):未验证。
- `spawn_background` 的 services 具体实现(线程 vs 线程池)未读到实现体
  (interface 在 `reactor/component/tasks.rs:8`,实现在 `native/app` 侧,
  本轮未展开)。
- 3 个并行研究子代理因运行时故障未产出,本文全部结论为主会话直读所得。

## 8. 载体选型盘点(2026-11-18 追补:「有没有更好的控件承载现有设施?」)

> 证据源:本机 WinAppSDK **2.6.5** winmd 元数据(strings 全量扫
> `~/.nuget/packages/microsoft.windowsappsdk.winui/2.6.5/metadata/Microsoft.UI.Xaml.winmd`)、
> 上游源码 `D:/Projects/microsoft-ui-xaml`(controls/dev + dxaml/xcp)、
> reactor `bindings.rs` 已绑全表(`pub struct` 提取 258 类)。

### 8.1 结论矩阵

| 现有设施 | 更优载体? | 判定 |
|---|---|---|
| `TitleBar` 控件 | **同一控件,换用法**(关 AutoRefresh + 手动 `RecomputeDragRegions`) | ✅ 首推,半天级改动,连性能带崩溃面一起治 |
| 评论/关联裸 for 循环 | **已绑的 `VirtualSource`+`ItemsRepeater`**(首页在用) | ✅ 零新绑定,R3 就地解决 |
| `ItemsView`(2.6.5 有,reactor 未绑) | 可选 | ⏸ 与 VirtualSource 收益重叠,不阻塞本轮 |
| 弹幕层 SwapChainPanel+自建 D3D | 无更好的(XAML winmd 无更新载体;ContainerVisualFactory 不在 WinAppSDK 面) | ❌ 维持现状,Win2D 引入否决 |
| 视频层 MPE | 系统面唯一正解;结构性修法=常驻单实例(§4-F4) | ❌ 不换控件,换挂载策略 |
| `ScrollViewer`(全应用) | `ScrollView`(1.6+,已绑)是官方后继,**但**其 ScrollPresenter 布局路径正是崩溃双嫌疑之一 | ⚠ 崩溃闭环前**不要**迁移 |
| Mica backdrop | 无替代需要;拖拽 A/B 试 `None` 即可 | ⏸ 观测项 |

### 8.2 TitleBar:真正的病灶与新用法(本轮最有价值发现)

上游 `TitleBar` 控件(MUX_PUBLIC_V11,2.6.5 内含)源码实锤:

1. **reactor 硬编码 `SetAutoRefreshDragRegions(true)`**
   (`reactor/native/winui/window/mod.rs:631-632`)⇒ 控件对 `Content` 挂
   `LayoutUpdated` 常驻订阅(上游 `TitleBar.cpp:601-604` 注释:关开关则是
   one-shot「首帧算完即退订,避免重复树遍历」)——**拖框/动画期间每次布局
   pass 都会**:`UpdateInteractableElementsList()` 全子树遍历
   + 每个可交互元素 `GetBounds()`(`= TransformToVisual + XamlRoot()
   .RasterizationScale`,上游 `TitleBar.cpp:636`,**XamlRoot 无判空**,
   即崩溃文档 §4.5 锁定的 `TitleBar.cpp:647`(2.6.5 行号)同源指令)
   + 脏比较后 `InputNonClientPointerSource.SetRegionRects`。
   我们 TitleBar 子树有搜索框+铃铛+头像+back+pane-toggle ≈ 5-7 个交互元素,
   每个每布局 pass 一次跨接口调用。
2. **官方逃生舱已公开**:`AutoRefreshDragRegions`(get/set)与
   `RecomputeDragRegions()`(内含同步 `UpdateLayout`,只宜低频调用,
   上游 `TitleBar.cpp:823-834`)。
3. 修法(绑定成本=bindings.txt 一行 + `cargo run -p tool-reactor` 再生,
   `ITitleBar2::put_AutoRefreshDragRegions` 已在清单第 157 行,补
   `RecomputeDragRegions`/`get_AutoRefreshDragRegions` 即可):
   - reactor `apply_title_bar_to_window` 改 `SetAutoRefreshDragRegions(false)`
     (或暴露开关给 app);
   - app 在「内容真正变化」的节点手动 `RecomputeDragRegions()`:路由切换、
     布局档位翻转(搜索框进/出 TitleBar)、`WindowResizeSettled`(150ms
     去抖已有)。**窗口移动不改相对矩形,无需重算**——正好掐掉拖框风暴。
   - 注意 OnSizeChanged(上游 191 行)仍会按尺寸变化跑
     `UpdateDragRegion`(有 rect 脏比较,遍历不在该路径)——量级可接受。

### 8.3 顺带的选型红利(已绑未用,零绑定成本)

- `VirtualSource`+`ItemsRepeater`(声明层 `declaration.rs:1622`,首页
  `video_card.rs:67-98` 实战)→ 评论/关联列表直接套同款配方。
- `SelectorBar`/`SelectorBarItem`(WinAppSDK 1.6+ 底部选项卡)→ Mobile
  布局底栏可从手搓 chip 升级,系统视觉与 RTL/键盘全免。
- `Expander` → 评论楼中楼「展开 N 条回复」可换系统控件(现手搓),
  顺带让拆除路径进框架自维护的视觉态。**注意**与 8.1 的 ScrollView 同理,
  涉及框架布局拆除竞态的迁移,等 dump 闭环后再批量做。
- `SvgImageSource` 已绑 → 图标可去 Symbol 字体限制(外观项,不急)。
- `DesktopWindowXamlSource`+`DesktopChildSiteBridge` 已绑 → 未来若需
  独立「迷你悬浮窗」可用 XAML 岛,不必再拿 SwapChainPanel 硬凑。

### 8.4 否决清单(调研过,不采用,防止后人再翻)

- **WebView2 承载播放/弹幕**:全 web 栈内存 ×2-3,4c8T/8G 约束下否决。
- **Win2D(Microsoft.Graphics.Win2D)**:CanvasSwapChain 与现有自建
  D3D+D2D 层等价,白增一层 WinRT 依赖与设备争用面。
- **CompositionDrawingSurface/ContainerVisualFactory 类「新合成载体」**:
  2.6.5 的 Microsoft.UI.Xaml.winmd 中不存在(全量 strings 核对),
  属 Windows.UI.Composition/Win11 系统面,自包含打包不可依赖。
- **ItemsView 替换 VirtualSource**:能力重叠,迁移面(键/回收/测量协议)
  全新,无本季度收益;留作 reactor 上游对齐项。
- **自绘 Border 标题栏 + InputNonClientPointerSource 手动 pass-through**:
  是 8.2 方案不可行时的备胎(全手工,回归面大);先用开关方案。
- **ScrollViewer→ScrollView 迁移**:崩溃文档 §4.5 的两个无判空点
  (ScrollPresenter.cpp:1259 / TitleBar.cpp:647)都在 Controls.dll;
  在拿到 dump 闭环前迁移=从一条嫌疑路径搬进另一条。**冻结**。
