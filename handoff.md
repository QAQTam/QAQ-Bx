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

