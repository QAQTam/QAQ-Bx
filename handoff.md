# Handoff — 0.2.0 体验补完与未接线功能全面接线

> 记录时间:2026-11-18(夜)。
> **新队友第一入口:[docs/dev-notes/long-run.md](dev-notes/long-run.md)**(任务目标/审计/核验表格/代码地图/坑/实测数据,自足可续)。
> 基线:`0.2.0`,`just dev` 自增末尾修订号,本轮已出 `package/debug/QAQ-Bx-Dev_0.2.0.5_x64.appx`(自签)。

## 1. 本轮完成(全部有测试证据,详见 long-run.md §3 核验表)

- **用户反馈五项**:全屏 HUD 半透明黑+紧凑+白前景(F1);播放页画质菜单+定点换流(F2);弹幕字体/字号设置(F3);首播音画缺失根因=捆绑 libmpv 缺解码器/音轨拉取失败,自动探测+编码桶回退(F4);桌面卡片连续大小曲线+16:9 取整(F5)。
- **窗口拖拽卡顿**:引擎 resize 合并 120ms(拖拽 0 重挂)+网格重建去抖 150ms(P1)。
- **未接线接线**:评论发布(U1,登录门控)、搜索默认词(U2)、多分P选择器(U3)、直播页真实数据+mpv 直播播放(U4,guest 可用 getRoomList;新版 xlive 端点 -352 风控不可用)。
- **性能基线(P2,判定不可达)**:实测 debug 播放 290MB / 空转 130MB / release 播放 289.5MB(私有 526MB,hwdec=d3d11va 正常);内存由映射 dll 与解码池主导,非 Rust 堆可调。已裁 mpv 缓存 32+16→8+4MiB(−54MB)。探针脚本 `scripts/perf-probe.ps1`。
- **验证**:`cargo test --workspace` 全绿(core 53/fix 6/login 7/ui 57/video 5);`just selftest all` **12/12 PASS**(新增第 12 项 `live` 实网场景);`ui-edge E10` exit 0 且截图视觉验收(F1/F2 上屏);打包清洁(无凭据/selftest-data,打包脚本已加排除)。

## 2. 待决与继承(新队友必读)

1. **U1 真机发帖验证**:需用户登录后人工发一条评论(AGENTS.md #7,测试后移除令牌);客户端参数/门控已有单测。
2. **U3 多P实机**:本轮测试视频均单P,多P视频切换需实机复核。
3. **P1 体感复核**:resize 合并已实现,拖拽体感需人工确认;若换流后画面异常,检查 `attached=false` 重挂路径(engine Resize 合并应用处)。
4. **登录态四页**(关注/收藏/历史/稍后)维持空态(上游既定决策),端点见 `docs/bilibili-api-inventory.md` §8。
5. 评论点赞写接口、播放心跳/续播、player/wbi/v2 字幕:记录为后续(需登录)。

## 3. 关键改动文件索引

- core:`player.rs`(ResolvedMedia/quality_menu/select_stream_by_qn/excluding/audio_excluding/quality_label)、`settings.rs`(danmaku_font_family/scale)、`reply.rs`(add)、`search.rs`(default_word)、`live.rs`(新)、`httpx.rs`(post_form_with_ticket)。
- video:`engine.rs`(start_secs 定点换流/hwdec+轨道观察/resize 合并/DanmakuFonts.with)、`player.rs`(mpv 缓存 8+4MiB)。
- ui:`shell.rs`(SelectQuality/SelectPage/Live*/Comment*/SearchDefault*/WindowResizeSettled/missing_media_track/pages_menu_from)、`pages/{player,fullscreen,live(新),settings,search}.rs`、`widgets/chrome.rs`(占位词)、`layout.rs`(F5 曲线)、`feed_pager.rs`(曲线启发式)、`selftest.rs`(live 场景+断言同步)。
- scripts:`perf-probe.ps1`(新)、`package-appx.ps1`(+selftest-data 排除)、`ui-edge.ps1`(未改)。

## 4. 崩溃分析(2026-11-18,新任务,完整文档 `docs/dev-notes/crash-0x359595-xamlroot.md`)

- 事件:无征兆卡顿数秒后闪退;WER 0.2.0.2 与 0.2.0.6 两版本**同一偏移**
  `Microsoft.UI.Xaml.Controls.dll +0x359595`,0xC0000005(WinAppSDK 2.6 自包含打包)。
- 已定位:故障指令为框架内部 `consume_IUIElement<D>::XamlRoot`(ICF 折叠,全部控件共用)
  中 **QI 结果未查 HRESULT 直接解引用 NULL**;触发者未知(无 dump、无栈)。
- 归因:非我们代码直接越界(故障在框架 DLL);reactor 未见跨线程 XAML 访问;
  嫌疑最大是 WinAppSDK 2.6 框架在控件半拆除态被读 XamlRoot 的竞态——**需 dump 闭环**。
- 已布置:① WER LocalDumps(HKCU,full dump→`data/crash-dumps`,10 个);
  ② `ui/src/trace.rs` UI 轨迹日志(`data/ui-trace.log`,app start + 11 处 route 切换);
  ③ `scripts/crash-sym/symbolize.py`(事件 1000 偏移→符号/反汇编,已验证;用法见其 README)。
- 下一步:等下次崩溃拿 dump → `!analyze -v` 定调用方 → 按结果修 reactor 用法或
  上报 microsoft-ui-xaml(模板见文档 §6)。UI 测试 58 绿(新增 trace 单测),workspace 全绿。
- 追加(同日):上游 DLL **开源**(microsoft/microsoft-ui-xaml,MIT,dxaml/+controls/;
  本地 sparse clone 在 `D:/Projects/microsoft-ui-xaml`);来源=WinAppSDK NuGet 2.6.5
  (非 reactor 自带);框架内部 XamlRoot 调用点已定位到源码级
  (ScrollPresenter 布局路径:1259 无判空 / TitleBar 拖拽区域:647 无判空 / ScrollView 键盘导航:2068),
  嫌疑收窄为布局拆除竞态;详见文档 §4.5。
- 追加 2(同日):**探针已建成并实测**(§4.6):fork 增 `DebugRaw` 诊断 imperative;
  `--probe-xamlroot`(feature `probe`)分阶段实测——「移出可视树」与「断开态属性设置」
  **均不触发**(排除);跨线程 QI=RPC_E_WRONG_THREAD(context station 签名确认)。
  剩余候选:peer 销毁型 UAF / 框架内部跨线程续体——**只能靠真实 dump 区分**。
  顺带:E10 全屏二次进入断言失败(用户在途 engine.rs 重构相关,另行排查);
  有并行编辑(用户 IDE)曾致 player.rs/engine.rs 编译错误,已由用户自行修复。
  workspace 测试全绿(core 56/ui 60/video 9)。
- 追加 3(22:39,第三次崩溃):0.2.0.12(11:47Z 构建)再次同偏移 0x359595
  (WER `AppCrash_QAQ-Bx-Dev_0.2.0_5c939a...6ac20c1e`,BucketId 8b0d44f2…)。
  **HKCU LocalDumps 对打包应用无效**(EtwNonCollectReason=1,无 dump 落袋)——
  需 HKLM 管理员项或改测打包外运行;打包应用的 ui-trace.log 写到包内 LocalState
  (%LOCALAPPDATA%\Packages\QAQ-Bx-Dev_ptzv47e0cg1m0\…),22:39 崩溃前的面包屑在
  那里,先翻它。详见 docs/dev-notes/crash-0x359595-xamlroot.md §4.5/§4.6。
- 追加 4(23:0x,**dump 捕获链路已实测打通**):HKCU 确被打包应用忽略(官方文档
  确认 LocalDumps 只认 HKLM)。已经用户 UAC 授权写入 HKLM LocalDumps 三值
  (脚本 `scripts/crash-sym/setup-hklm-localdumps.ps1`),转储目录补
  ALL APPLICATION PACKAGES 写 ACL(打包进程 AppContainer 落盘必需,零成本预防)。
  新增 `capture_gate.rs`(常开;数据目录哨兵文件 `selftest-av.flag` 触发,绕开
  打包应用不带参的限制;单测 2 项覆盖触发判定)→ just dev 出 0.2.0.13 安装 →
  放 flag → 拉起 → **0xC0000005(写空指针)崩溃,88MB full dump 落
  `data/crash-dumps/qaq-bx-ui.exe.14744.dmp`**;用新工具
  `scripts/crash-sym/minidump-info.py` 纯 Python 解析(exception 流字段序为
  EXCEPTION_RECORD 序:addr@+16,非 MS 头声明顺序),故障 RVA 0x19054c 用本地
  PDB 符号化 = `capture_gate::fire+0xDC`,证据链完全自洽。flag 已删,回归启动
  无误触发。**现在唯一等的是下一次自然崩溃的真实 dump。**
  现场面包屑另证:22:39 崩溃会话仅存活 169 秒、零 route 事件(崩在默认路由,
  没进播放器)——§4.7 已修正嫌疑面;19 点档的 `AppCrash_qaq_bx_ui-f3580a` 群
  为 cargo 测试 harness 崩溃,与本案无关。
- 追加 5(23:1x,**根因闭环 §8**):用户给出确定性复现(全屏退出→立刻拖窗→卡死
  →崩)并触发真崩溃,pid 10476 落 1.0GB full dump。栈走查
  (`scripts/crash-sym/minidump-stackscan.py` 新工具)实锤调用链 =
  `TitleBar::OnWindowRectChanged→UpdateIconRegion→GetInputNonClientPointerSource
  →GetAppWindowId→XamlRoot()@RVA 0x359595`(同三次历史崩溃同指令)。
  **主因=上游 TitleBar::UpdateIcon 用裸 this 订阅窗口级
  InputNonClientPointerSource.WindowRectChanged 且 ~TitleBar() 不撤销**
  (main@11-17 仍未修;撤销唯一门=IconSource 置 null)→ TitleBar 死后
  拖框事件回调幽灵对象。**共因=reactor 硬编码
  `SetAutoRefreshDragRegions(true)`(fork window/mod.rs:636)布局风暴**(同事
  0.4.0 前瞻分析证实,对应「卡死」;RecomputeDragRegions 逃生舱 2.6.5 二进制
  实测存在,bindings.txt 需补方法绑定——put_ 半边已在:157)。
  app 触发面:chrome.rs:52 设 icon + shell.rs 双组合结构切换 retire TitleBar。
  探针未测出的原因:探针持强引用,对象从未死亡。
  修复三件套(§8.4,待落地):A[fork 治本]退役前 SetIconSource(null)+
  SetTitleBar(null);B[fork+app]翻 AutoRefresh=false + 手动 Recompute
  (路由/档位/resize settle);C[上游 issue 要素已齐]+ ui-edge E12 回归
  (全屏→退出→连移窗口 20 次)。E10 与本缺陷同窗,预期修好一并转绿。
- 追加 6(23:5x,**A+B 已落地,待复现裁决**):fork 分支
  `fix/titlebar-ghost-rectchanged` 两个 commit(1ac775d: clear 前
  SetIconSource(null) 走官方撤销门 + bindings 补 ITitleBar2::RecomputeDragRegions
  + ImperativeRequest/ElementRef 贯通,347 绿;68fb482: schema reference=true
  使 TitleBar 声明可挂 element_ref)。app:chrome 声明 false、shell view() 结构
  指纹变化按需重算。线程取证:崩溃线程=user32 帧最多的 UI 线程(跨线程候选排除)。
  已装 0.2.0.15(工作树含用户 v0.3 在途;bin 绿,cargo test 的 10 个编译错误全部
  来自用户 shell.rs test block 未完成的 EngineSwitchAction,与本修复无关)。
  **待用户跑复现步骤裁决;E16(ui-edge)已入库;C(upstream issue)待写。**


