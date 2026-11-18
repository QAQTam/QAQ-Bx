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
- 建议:低成本试验 = 实测确认耗时占比;根治 = 标题栏改 `Border` 自绘 +
  窗口集成走 `AppWindow.TitleBar.ExtendsContentIntoTitleBar` 手动设
  drag rect(reactor 已有 WinUI 集成层,可评估)。

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

## 6. 实测记录区(留空,实施轮回填)

- (待填)debug@opt0 vs opt1:拖框 CPU 峰值 / 帧时间
- (待填)F1+F2 前后 render_view 次数
- (待填)TitleBar 换自绘前后 E14 对比(若做)

## 7. 本次未验证项(诚实清单)

- 全链路所有**毫秒量级**均为推断,无一帧计时实测(探针方案见 §5)。
- XAML 模态循环内 DispatcherQueue 的实际优先级行为、DWM 对 flip 表面的
  节流策略:未做 ETW/wpr 追踪。
- MPE 内部管线对宿主尺寸变化的响应成本(黑屏文档修复后未单独测):未验证。
- `spawn_background` 的 services 具体实现(线程 vs 线程池)未读到实现体
  (interface 在 `reactor/component/tasks.rs:8`,实现在 `native/app` 侧,
  本轮未展开)。
- 3 个并行研究子代理因运行时故障未产出,本文全部结论为主会话直读所得。
