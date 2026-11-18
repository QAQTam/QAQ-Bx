# 崩溃根因分析:Microsoft.UI.Xaml.Controls.dll RVA 0x359595(0xC0000005)

> 记录时间:2026-11-18。分析人:QAQ(自动化会话)。
> 结论状态:**根因已闭环(§8)**——用户给出确定性复现步骤(全屏退出后立刻拖窗),
> 真实 dump 栈实锤调用链 `TitleBar::OnWindowRectChanged→UpdateIconRegion→
> GetInputNonClientPointerSource→GetAppWindowId→XamlRoot()@0x359595`;
> 主因=上游 TitleBar `WindowRectChanged` 订阅泄漏(析构不撤销→幽灵回调),
> 共因=reactor 硬编码 `AutoRefreshDragRegions(true)` 的布局风暴。
> 修复与上报方案见 §8.4。

## 1. 现象与事件

- 用户报告:**毫无征兆 → 卡顿数秒 → 闪退**,复现难度高。
- WER 报告 `3612370f-ce20-4b2d-af3f-5b74154426ff`(0.2.0.6)与更早的
  `e11119c1`(0.2.0.2,**两个不同 app 版本**):`Microsoft.UI.Xaml.Controls.dll`
  版本 3.3.0.0,异常 `0xC0000005`,故障偏移**完全相同** `0x359595`。
  同一指令反复命中 ⇒ 不是随机堆损坏,而是确定性的故障点。
- OS 10.0.26300(Insider);打包的运行时:Windows App SDK **2.6.0.0**,
  Microsoft.UI.Xaml.dll 3.3.0.0(自包含打包,无框架依赖)。
- WER 目录里只有 Report.wer,**没有 dump**(`EtwNonCollectReason=1`),
  所以最初只能靠"偏移符号化"硬啃。

## 2. 定位过程(可复现,工具已入库 `scripts/crash-sym/`)

1. 从包内 DLL 解析 CodeView → PDB GUID `08e7203c...`,age 2。
2. 从 msdl 下载对应 PDB(346MB;注意 curl 需断点续传,曾 3 次截断)。
3. 纯 Python 解析 MSF 7.00:块映射在 superblock **0x34**(0x30 为保留 0),
   目录块表**无前导计数**;DBI 在 stream 3;S_PUB32 记录实际位于
   stream 10(而非 DBI 头部声明的 stream 9;9 号流只有哈希桶)。
   S_PUB32 payload 为 `flags(4)+off(4)+sect(2)+name`(无 imod 字段)。
4. RVA 0x359595 → `.text` +0x358595,最近 public(共 89,386 条):

```
?XamlRoot@?$consume_Microsoft_UI_Xaml_IUIElement@VTitleBar@@@impl@winrt@@QEBA@XZ
?XamlRoot@?$consume_Microsoft_UI_Xaml_IUIElement@VScrollView@@...(同址)
?XamlRoot@?$consume_Microsoft_UI_Xaml_IUIElement@VSwipeControl@@...(同址)
下一个符号: ?ZoomBy@consume_IScrollPresenter<ScrollPresenter>@+0x3585f8
```

## 3. 故障点反汇编(决定性证据)

```
0x35955a: mov rcx, [rcx+0x38]        ; 取成员接口指针
0x35955e: test rcx, rcx
0x359561: jne 0x359567               ; 非空 → QI
0x359563: xor edi, edi
0x359565: jmp 0x359590               ; 空成员 → 仍跳进解引用(见下)
0x359567: mov [rsp+0x30], 0          ; QI out = null
0x359570: mov rax, [rcx]             ; vtable
0x35957f: mov rax, [rax]             ; QueryInterface 槽
0x359582: call [rip+0x1a01a0]        ; QI(GUID c3c01020-320c-5cf6-9d24-d396bbfa4d8b)——返回值未检查!
0x359588: mov rcx, [rsp+0x30]        ; rcx = QI 结果(QI 失败时为 null)
0x359590: mov [rsp+0x20], rcx
0x359595: mov rax, [rcx]  <<<< 故障:rcx == NULL
0x35959d: mov rax, [rax+0x368]       ; get_XamlRoot 槽
```

即:**框架在自己 DLL 内部对某控件读取 `XamlRoot()` 时,QI 结果(或成员
接口指针)为 NULL,未检查 HRESULT 直接解引用** → AV。注意该 consume 模板
对 D 无依赖,ICF 把全部 UIElement 派生控件(TitleBar/ScrollView/SwipeControl/
PipsPager/…)折叠到同一函数体,因此**不能**凭 public 名锁定是哪个控件。

## 4. 归因判断(当前证据下的诚实结论)

| 假设 | 证据 | 判定 |
|---|---|---|
| 我们(qaq-bx-ui)代码直接越界 | 故障模块是框架 DLL,不是我们的 exe;Rust 侧 `XamlRoot()` 调用(reactor `mod.rs:489/501`、`transient_menu.rs:346`)的 consume 代码在 qaq-bx-ui.exe 内 | **排除(直接原因)** |
| windows-reactor 设计缺陷 | reactor 全部变更在 UI 线程应用;`Local<T>` 线程亲和哨兵;retirement 计时器在 Drop 全部 Stop;未发现跨线程 XAML 访问 | **未见直接证据,无法完全排除间接触发** |
| Windows/WinAppSDK 2.6 框架缺陷 | 故障指令在框架 DLL 内、未检查 QI HRESULT 即解引用;同一偏移跨两个 app 版本复现 | **嫌疑最大,且已定位到源码级调用面(见 §4.5)** |

最可能机制(按概率排序):
1. **控件对象半拆除状态被框架内部 XamlRoot 读取**(teardown 竞态;
   QI 因对象已 closed/断开而失败 → null → AV)。app 高频创建/销毁控件
   (弹幕、虚拟列表、页面切换、半屏/全屏切换)+ 延迟 fade-out retirement
   是此类竞态的典型温床。
2. 框架内部不变量破坏(成员指针本不该为 null,却走到空分支)。
3. 堆损坏恰好把成员清零(概率低:同一指令精确复现两次)。

「卡顿 → 闪退」的时序支持 1:UI 线程先被长任务/风暴阻塞(卡顿),
其间延迟回调(计时器、动画完成、visibility 变化)排到队列,恢复执行时
对象已处于半拆除状态 → AV。

## 4.5 上游源码对齐(2026-11-18 新增)

- 崩溃 DLL `Microsoft.UI.Xaml.Controls.dll` **完全开源**:
  [microsoft/microsoft-ui-xaml](https://github.com/microsoft/microsoft-ui-xaml)(MIT),
  产品源码在 `dxaml/`(引擎)+ `controls/`(现代控件);本地 sparse clone:
  `D:/Projects/microsoft-ui-xaml`(main 分支,网络不稳时用 `--depth 1 --sparse` 多重试,
  blob:none + 懒取会被网络破坏)。
- 来源链确认:不是 windows-reactor 自带(reactor 是纯 Rust 绑定);DLL 来自
  Windows App SDK NuGet **2.6.5**(`~/.nuget/packages/microsoft.windowsappsdk/2.6.5`),
  打包脚本自包含拷入 appx。
- 反汇编里"先 QI(GUID c3c01020-…)再解引用"的形状来自微软自家 codegen
  (`src/projection` 只是薄壳,真正的 consume 模板构建期生成,仓库里没有);
  WinMD(2.6.5 `microsoft.windowsappsdk.winui/metadata/Microsoft.UI.Xaml.winmd`)
  可用于后续 GUID→接口名映射(未做,非必需)。
- 框架内部 `.XamlRoot()` 调用点(controls 源码):
  - `ScrollPresenter.cpp:1259` —— **Arrange/布局路径,无判空**,每次布局都会执行;
  - `ScrollPresenter.cpp:2935` `GetLayoutRoundFactor`(布局取整,有判空);
  - `TitleBar.cpp:647` `GetBounds` —— **无判空**,拖拽区域计算,任何 TitleBar 属性/
    布局/尺寸变化都会触发(`UpdateDragRegion` 调用点遍布 6 处事件处理器);
  - `TitleBar.cpp:916` `GetAppWindowId`(窗口集成,有判空);
  - `ScrollView.cpp:2068` —— **键盘/焦点导航处理器**(方向键、翻页键),无判空;
  - `SwipeControl.cpp:713`、`WebView2.cpp` 多处(本 app 未使用,排除)。
- 我们 app 的使用面:TitleBar(窗口集成)+ ScrollView/ScrollPresenter(每页/卡片,
  高频创建销毁)→ 嫌疑面收窄为 **ScrollPresenter 布局路径** 与 **TitleBar 属性/布局
  风暴** 两条。反汇编里 ICF 把全部控件折叠到同一函数体,故不能凭符号名锁定单一控件;
  上述调用点共同点是"控件处于拆除/断开态时布局或输入事件仍触发一次 XamlRoot 读取"。
- 自修上游的可能性:打包是自包含(拷 DLL 而非依赖框架包),理论上可用开源源码
  自建带修复的 WinUI runtime 替换进 appx(重路径,仅在确认 bug 且等不到上游修复时考虑)。

## 4.6 探针实测结果(2026-11-18,`--probe-xamlroot` 已建成并运行)

探针实现:
- fork 增加诊断 imperative `ImperativeRequest::DebugRaw`(ElementRef→裸 IUnknown,
  adapter 经 `ui_element(object)` 解析;test/诊断专用,全部匹配点已覆盖);
- app 侧 `probe.rs`(feature `probe`):0 尺寸锚点 Button 挂 TitleBar left_header,
  `debug_raw` 拿原生指针 → cast IUIElement → 向上找 TitleBar 祖先 → 分阶段实测。
  日志:`data/xamlroot-probe.log`。复跑:`cargo run -p qaq-bx-ui --features probe -- --probe-xamlroot`。

实测梯子结果(2.6.5 runtime,真实 shell):
| 状态 | XamlRoot() 结果 | 结论 |
|---|---|---|
| 连接态(在树内) | Ok 非空 | 基线正常 |
| **已从可视树移除**(对象仍存活,持强引用) | **Ok 非空** | 「断开」不构成触发条件 |
| 断开态设置 Title(框架内部 UpdateDragRegion 全路径) | 不崩 | 框架内部无检查 consume 在该状态正常工作 |
| 跨线程调用 | **Err(RPC_E_WRONG_THREAD 0x8001010E)** | context station 拦截跨线程,QI 失败签名确认 |

结论修正:
1. 「从树里移除/属性churn」**排除**——比预想温和;
2. 剩余两个候选:**(a) peer 已销毁的 use-after-free**(引用计数竞态,
   框架内部事件在对象释放后仍触发——与「无征兆、难复现」吻合);
   **(b) 框架内部异步续体在非 UI 线程触发**(QI 必失败 → NULL 解引用)。
   两者只能靠真实 dump 的「崩溃线程 ID + this 指针堆状态」区分。
3. 顺带发现:E10 全屏二次进入断言失败(窗口未回全屏)——与用户在途的
   engine.rs 重构相关,另有会话在编辑 player.rs/engine.rs(曾致编译错误,
   已由其自行修复,未代劳)。

## 4.7 崩溃账本与 22:39 现场面包屑(2026-11-18 夜,包内数据目录取证)

打包应用 `resolve_data_dir()` 实际落在
`%LOCALAPPDATA%\Packages\QAQ-Bx-Dev_ptzv47e0cg1m0\LocalCache\Local\QAQ-Bx\data\`,
仓库 `D:/QAQ-Bx/data` 对它无效。该目录里的 `ui-trace.log` 即崩溃现场记录。

**完整崩溃账本**(WER ReportArchive,时间均为本地 UTC+8):

| 报告 | 版本 | 时间 | 故障模块/偏移 | dump |
|---|---|---|---|---|
| e11119c1 | 0.2.0.2 | 11-17 22:36:34 | Microsoft.UI.Xaml.Controls.dll 3.3.0.0 + 0x359595 (c0000005) | 无(EtwNonCollectReason=1) |
| d7158897 | 0.2.0.6 | 11-18 05:44:53 | 同上,同偏移(PE 时间戳 6afc9e23) | 无 |
| 6ac20c1e | 0.2.0.12 | 11-18 22:39:08 | 同上,同偏移(PE 时间戳 6afd9061) | 无 |

三份报告 `LoadedModule` 无 dump 附件;`data/crash-dumps\` 与 WER 全目录零 `.dmp`。

**22:39 崩溃现场(ui-trace.log + 文件 mtime 配对)**:

- 22:36:19.981 app start pid=9240(0.2.0.12)→ 22:39:08 崩溃,**存活仅 ~169 秒**;
- 该会话 **零条 route 事件**——用户没有进入播放页,崩在启动后的默认路由上;
  上一条 route(打开视频)属于当天上午的 0.2.0.7 会话,与本次崩溃无关;
- 22:37 settings.json 被重写一次(会话内某设置落盘或自动保存),随后崩溃;
- 同日 0.2.0.9 会话(17:46 启动)也只有 app start 一条、无 route,但正常退出无报告。

**推论修正**:此前 §4 把「播放页高频创建销毁 + 半屏/全屏切换」当主嫌疑场景;
本次数据显示崩溃可以在**完全不进播放器**的首页/窗口集成场景下发生。嫌疑面应
维持 §4.5 的两条(TitleBar 属性/布局风暴、首页 ScrollPresenter 布局路径)且不再
偏向播放器侧。候选 (a) UAF / (b) 框架内部跨线程续体 的区分仍然只能靠 dump。

**排除噪声**(防误判):

- 11-17 15:24 的 0.0.1 报告(`AppCrash_QAQ-Bx-Dev_0.0.1_*`)+包内 `crash.log`:
  Rust「panic in a function that cannot unwind」abort,栈含
  WinUICreateDispatcherQueueController——另一条独立旧事故,与 0x359595 无关;
- 11-18 19:01–19:06 的 6+ 份 `AppCrash_qaq_bx_ui-f3580a_*`:崩溃模块是 cargo
  **测试 harness exe**(`target\debug\build\qaq-bx-ui\f3580a93b0123191\out\*.exe`,
  即 `cargo test -p qaq-bx-ui` 的运行体,本次跑测试复核确认同一路径),
  故障偏移发散(0x34c36/0x3b9fc/0x3411f4/0x4ebda…),非同一故障点,与打包应用
  AV 无关(疑为并行编辑会话期间的失败测试运行)。

## 5. 已布置的捕获与工具(本仓库)

1. **WER LocalDumps(HKLM,2026-11-18 22:54 已写入并回读确认)**:`HKCU\...\LocalDumps\qaq-bx-ui.exe`
   已配置但**对打包(MoAppCrash)应用被忽略**——三次崩溃的报告全部带
   `EtwNonCollectReason=1`。官方文档(Collecting User-Mode Dumps)确认该特性
   仅读 `HKLM\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps`,
   需管理员权限。HKLM 三值已由提权脚本写入(脚本入库
   `scripts/crash-sym/setup-hklm-localdumps.ps1`):`DumpFolder`
   (REG_EXPAND_SZ, `D:\QAQ-Bx\data\crash-dumps`)、`DumpType=2`、`DumpCount=10`。
   转储目录 ACL 已补:`icacls D:\QAQ-Bx\data\crash-dumps /grant *S-1-15-2-2:(OI)(CI)M`
   (打包进程在 AppContainer 内写目录所需,文档明确提示)。
   无需管理员的兜底方案:procdump `-ma -e -w qaq-bx-ui.exe` 常驻监视(Dev Mode
   已开,同用户 attach 可行),可挂计划任务;若 HKLM 方案对 MoAppCrash 仍不生效则启用。
   **HKLM 方案已实测对 MoAppCrash 生效(见 §5.4 自检),procdump 兜底不再需要。**
2. **UI 轨迹日志**:`crates/qaq-bx-ui/src/trace.rs` → `data/ui-trace.log`,
   记录 app start + 全部 11 处 route 切换(带来源标注),512KiB 上限截断;
   与 dump 时间戳配对还原现场。
3. **符号化工具**:`scripts/crash-sym/symbolize.py`(用法见该目录 README),
   已用本案例验证:输入模块路径 + 偏移即可输出故障符号与反汇编。
   配套新增 `scripts/crash-sym/minidump-info.py`:纯 Python 粗读 full dump
   (异常码/ExceptionAddress/故障模块+RVA/崩溃线程栈范围),自检实测可用。
4. **捕获链路自检门**:`crates/qaq-bx-ui/src/capture_gate.rs`(常开,无特性门控;
   触发条件是数据目录哨兵文件 `selftest-av.flag`,非命令行参数,绕开打包应用
   激活不带参的限制)。命中后 UI 启动前 `write_volatile` 空指针写 → 0xC0000005,
   与线上崩溃同异常码、同 WER 路径。单元测试覆盖触发判定(2 项)。
   用法:`touch '<包LocalCache>/QAQ-Bx/data/selftest-av.flag'` → 启动应用 →
   检查 `D:\QAQ-Bx\data\crash-dumps\*.dmp` → **删除哨兵文件**。
   **2026-11-18 23:00 实测通过(0.2.0.13)**:flag → 崩溃 → WER 落
   `qaq-bx-ui.exe.14744.dmp`(full,88MB);exception 流 code=0xc0000005、
   info[0]=1(写)、info[1]=0(空指针),故障 RVA=0x19054c,本地 PDB 符号化
   `capture_gate::fire+0xDC`——全链路自洽。删 flag 后回归启动正常(无误触发)。
   dump 命名规律:`<exe名>.<pid>.dmp`,DumpCount=10 滚动覆盖。
   本次自检 dump 已改名留证:
   `data/crash-dumps/SELFTEST-PROOF-2026-11-18-qaq-bx-ui.exe.14744.dmp`
   (真实崩溃 dump 到达时不会与此混淆)。

## 6. 下一步(拿到 dump 后)

0. 先用本机无依赖工具粗读:`python scripts/crash-sym/minidump-info.py <dmp>`
   ——打印异常码/ExceptionAddress/故障模块+RVA/崩溃线程。
   **坑(自检实测)**:线程 CONTEXT 里的 Rip 可能不是故障点——WerFault 落盘时
   该线程已停在 WER 交接路径(实测 Rip 指向 ntdll);**以 ExceptionStream 的
   ExceptionAddress 为准**。
1. `data/crash-dumps/*.dmp` → WinDbg `!analyze -v`(符号路径
   `srv*D:\qaqh-sym*https://msdl.microsoft.com/download/symbols`)。
2. 读故障线程栈:确认调用方(哪个框架子系统:Tooltip/Popup/Focus/
   ScrollPresenter bring-into-view/TitleBar 窗口集成…)+ 哪个控件实例。
3. 若调用方是 TitleBar 窗口集成路径 → 复现脚本:反复开关窗口/切主题;
   若是 ScrollPresenter → 滚动/缩放风暴。确认后:
   - 框架缺陷 → 按模板向 microsoft-ui-xaml / WindowsAppSDK 提 issue
     (附 RVA 0x359595 + 本文档反汇编 + UI 轨迹);
   - 我们的用法触发 → 在 reactor 侧加防护(如拆除前断开相应集成)。
4. 若 dump 复现困难:在 dump 里同时看 `!handle` 与 RCW 断开情况。

## 7. 本次未验证项

- 未能确认崩溃时用户正在哪个页面(无轨迹日志的历史数据)。
  【已由 §4.7 部分解决:0.2.0.12 崩溃会话零 route 事件;§8 起有 100% 复现步骤】
- 未尝试主动复现(用户明确说复现难;优先布好捕获而非盲试)。
  【已失效:2026-11-18 23:08 用户提供确定性复现步骤,见 §8】
- GUID `c3c01020-320c-5cf6-9d24-d396bbfa4d8b` 对应的具体 WinRT 接口
  版本未确认(不影响结论,拿到 dump 后自然明确)。

## 8. 根因闭环(2026-11-18 23:08,真实崩溃 dump + 栈走查 + 上游源码对齐)

**复现步骤(用户提供,确定性)**:全屏播放 → 退出全屏 → 立刻拖动窗口移动位置
→ 卡死 → 崩溃。dump 已捕获:`data/crash-dumps/qaq-bx-ui.exe.10476.dmp`
(0.2.0.13,full,1.0GB)。

### 8.1 dump 证据

- exception: `0xc0000005` **读 NULL**(info[0]=0, info[1]=0x0),
  故障 `Microsoft.UI.Xaml.Controls.dll RVA 0x359595`(PE_ts 8b577a7c,
  与 0.2.0.2/0.2.0.6/0.2.0.12 三次历史崩溃**同指令同偏移**);Rip==ExceptionAddress,
  崩溃线程 9248(UI 线程)。
- 启发式栈走查(`scripts/crash-sym/minidump-stackscan.py` + 本地缓存 PDB 符号化),
  故障点向外的调用链(sp 从低到高 = 子→父):

  | sp | 模块内 RVA | 符号(最近 public) |
  |---|---|---|
  | fault | 0x359595 | `consume_IUIElement<D>::XamlRoot()`(§3 故障指令) |
  | +0x0058 | 0x4cb733 | `TitleBar::GetAppWindowId+0x1F` |
  | +0x0098 | 0x4cbb43 | `TitleBar::GetInputNonClientPointerSource+0x27` |
  | +0x00F8 | 0x4ceebe | `TitleBar::UpdateIconRegion+0x22` |
  | +0x0118 | 0x4cd65a | `TitleBar::OnWindowRectChanged+0x1A` |

  → 调用链逐字对应上游源码
  `OnWindowRectChanged → UpdateIconRegion → GetInputNonClientPointerSource
  → GetAppWindowId → XamlRoot()`(controls/dev/TitleBar/TitleBar.cpp)。

### 8.2 上游缺陷(主因):`WindowRectChanged` 订阅泄漏 → 幽灵回调

main@2026-11-17 与 2.6.5 二进制同构,源码事实:

1. `UpdateIcon()`(设 IconSource 时)用**裸 this** 订阅
   `InputNonClientPointerSource.WindowRectChanged({this,&TitleBar::OnWindowRectChanged})`,
   **非 auto_revoke**;
2. 撤销的唯一门 = 再次进 `UpdateIcon` 且 IconSource 为空(else 分支 revoke token);
3. **`~TitleBar()` 不撤销 `m_windowRectChangedToken`**(它只 revoke
   size/flowDirection/InputActivation);
4. 事件源 `InputNonClientPointerSource` 是**窗口级系统对象,寿命 ≫ TitleBar**
   ⇒ TitleBar 析构(或析构中)后事件仍回调死 this;
5. 幽灵回调走到 `GetAppWindowId` 里的 `XamlRoot()`:source 层
   `if (auto xamlRoot = XamlRoot())` 的判空**救不了**——炸点在 winrt
   consume 生成模板内部(§3 反汇编:成员判空后的空分支错误 fall-through +
   QI HRESULT 未检查即解引用),对象半拆除时成员槽已清零 ⇒ AV。

### 8.3 我们的放大器(共因,非直接肇因)

- **reactor fork 硬编码** `SetAutoRefreshDragRegions(true)`
  (`crates/libs/reactor/src/native/winui/window/mod.rs:636`,`put_` 半边绑定
  已在 `bindings.txt:157`):LayoutUpdated 常驻订阅,每次布局 pass 全子树遍历
  `UpdateInteractableElementsList` + 逐元素 `GetBounds`(内含 XamlRoot)
  —— 拖框 60~125Hz 事件风暴下 CPU 打满 = 用户感知的「卡顿/卡死」;
  同时放大在鬼对象上执行代码的窗口期。同事 0.4.0 前瞻锁定的正是这一半。
- **app 触发面**:`chrome.rs:52` 设了应用图标(订阅门成立);
  `shell.rs` 存在两套含 title_bar 的组合结构,结构切换 + reactor 延迟
  fade-out retirement 制造「TitleBar 正死、RectChanged 正来」的窗口。
- 为什么探针没测出(§4.6):探针持强引用、对象从未死亡;幽灵需要
  「TitleBar 先死 + 事件源活着」这个探针未覆盖的组合。

### 8.4 修复方案(分层;A 是断头, B/C 降概率与去风暴)

**落地状态(2026-11-18 23:5x)**:A+B 已实现,打包 **0.2.0.15** 已安装待复现验证;
C 未动。fork 分支 `fix/titlebar-ghost-rectchanged`(基于同事 335c541):
`1ac775d`(撤销门+Recompute 贯通,347 测试绿)、`68fb482`(TitleBar 声明面补
element_ref)。app 侧:chrome.rs 声明 `auto_refresh_drag_regions(false)`+挂引用;
shell.rs view() 结构指纹(路由/全屏/搜索展开/宽度桶)变化时按需 Recompute 一次
——imperative 队列跑在 reconcile 后,元素必已发布。回归场景 **E16**(ui-edge:
全屏→退出→真鼠标拖标题条×20,断言存活+无新 dump)。回滚=fork/主仓各自
checkout 丢弃(同事 WIP 已安全在 master)。

- **A[fork,治本]**:reactor 释放/退役 TitleBar 投影前,显式
  `SetIconSource(null)`(走官方撤销门)+ `window.SetTitleBar(null)`,再放引用。
  上游修复(destructor 改 auto_revoke revoker)落地前这是唯一可靠防护。
- **B[fork+app,治「卡顿」]**:翻默认 `AutoRefreshDragRegions=false`;
  `bindings.txt` 增补 `ITitleBar2::RecomputeDragRegions`(实测 2.6.5 二进制存在,
  PDB/DLL 符号确认;当前 bindings.rs 0 命中)→ tool-reactor 再生 →
  app 仅在路由切换/档位翻转/WindowResizeSettled 手动 Recompute 一次。
  窗口移动本身不改 Content 相对矩形,无需重算——正好掐掉拖框风暴。
- **C[上游 issue,要素齐备]**:标题建议
  "TitleBar leaks WindowRectChanged subscription; callback into destroyed
  TitleBar crashes in consume_IUIElement::XamlRoot (unchecked QI HRESULT)";
  附:复现步骤(全屏退出后立刻拖窗,打包应用 WinAppSDK 2.6.5/OS 26300)、
  §8.1 栈表、§3 反汇编、源码引文(§8.2)、dump 可复现符号化命令。
  修复建议:成员化 `winrt::auto_revoke` revoker 或 destructor revoke。
- **回归测试**:ui-edge 新场景 E12:全屏→退出→`MoveWindow` 连续 20 次
  (间隔 ≤100ms)→ 断言进程存活 + 窗口位置生效 + 无 UI 线程挂起;
  当前 E10(全屏二次进入)失败与本案同窗,修好后应一并转绿。

### 8.5 归因终版(替代 §4 表)

| 假设 | 判定 |
|---|---|
| 我们代码直接越界 | 排除(故障在框架 DLL,调用方也是框架 handler) |
| reactor 硬编码 AutoRefresh=true | **成立,共因/放大器**(卡顿风暴 + 扩大死亡窗口暴露面) |
| WinAppSDK 2.6 TitleBar 订阅泄漏 | **成立,主因**(dump 栈 + 源码双重实锤,main 分支至 2026-11-17 仍未修) |
| consume_IUIElement::XamlRoot 防御缺失 | 成立,次要(bad-null fall-through + QI 未检查) |
