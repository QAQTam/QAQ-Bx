# 崩溃根因分析:Microsoft.UI.Xaml.Controls.dll RVA 0x359595(0xC0000005)

> 记录时间:2026-11-18。分析人:QAQ(自动化会话)。
> 结论状态:**故障指令已定位并符号化;触发链需要下一次崩溃的完整转储才能闭环**(转储捕获已布好)。

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

## 5. 已布置的捕获与工具(本仓库)

1. **WER LocalDumps(已生效)**:`HKCU\...\LocalDumps\qaq-bx-ui.exe`
   → `D:\QAQ-Bx\data\crash-dumps`,DumpType=2(full),DumpCount=10。
   下一次崩溃(哪怕再隔一周)会自动留完整 dump。
2. **UI 轨迹日志**:`crates/qaq-bx-ui/src/trace.rs` → `data/ui-trace.log`,
   记录 app start + 全部 11 处 route 切换(带来源标注),512KiB 上限截断;
   与 dump 时间戳配对还原现场。
3. **符号化工具**:`scripts/crash-sym/symbolize.py`(用法见该目录 README),
   已用本案例验证:输入模块路径 + 偏移即可输出故障符号与反汇编。

## 6. 下一步(拿到 dump 后)

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
- 未尝试主动复现(用户明确说复现难;优先布好捕获而非盲试)。
- GUID `c3c01020-320c-5cf6-9d24-d396bbfa4d8b` 对应的具体 WinRT 接口
  版本未确认(不影响结论,拿到 dump 后自然明确)。
