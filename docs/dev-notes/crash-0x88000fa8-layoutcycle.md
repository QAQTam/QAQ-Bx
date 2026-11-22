# 崩溃根因分析:Microsoft.ui.xaml.dll FailFastWithStowedExceptions(0xC000027B)——滚动条/布局循环

> 记录时间:2026-11-22。分析人:QAQ(自动化会话)。
> 结论状态:**根因已闭环**——dump 全链路解析 + 上游源码逐字对齐。
> 故障 = **XAML 布局循环(layout cycle)250 次迭代不收敛 → 引擎主动抛
> `AG_E_LAYOUT_CYCLE`(0x88000fa8)→ 无人处理 → `ErrorHelper::ProcessUnhandledError`
> → `FailFastWithStowedExceptions` → 0xC000027B fail-fast**。
> 振荡回路 = **ScrollViewer(ScrollBar)↔ ItemsRepeater(StackLayout 变高行)**
> 双向互相失效。现场 = 播放页(评论区)。
> 修复方案见 §6(本次为"锁定",A/B/C 待落地)。
> 方法论与工具沿用 titlebar 案(`crash-0x359595-xamlroot.md`);新增 stowed-exception
> 解析工具 `scripts/crash-sym/minidump-stowed.py`(§2)。

## 1. 现象与事件

- 新崩溃 dump:`data/crash-dumps/qaq-bx-ui.exe.3108.dmp`(full,920,449,732 B,
  2026-11-21 23:41 落盘)。应用 `QAQ-Bx-Dev_0.6.0.5`(WinAppSDK 自包含打包)。
- 与 titlebar 案(0xC0000005 AV)**不同类**:异常码 **0xC000027B(stowed
  exception fail-fast)**,故障模块 `Microsoft.ui.xaml.dll`(引擎本体,非
  Controls.dll),ExceptionAddress RVA **0x3d1699**。
- WER 交接点 Rip 在 KERNELBASE,栈窗仅 8KB——直接栈走查无意义(与 titlebar
  案不同);必须解析 stowed exception 包(54 条记录,每条自带栈回溯)。

## 2. 定位过程(工具已入库,可复现)

1. `python scripts/crash-sym/minidump-info.py <dmp>` → 异常流:
   `thread=11884 code=0xc000027b params=[0x24a30064200, 0x36]`。
2. **新工具 `minidump-stowed.py`**(本次新增,写入 `scripts/crash-sym/`):
   解析 0xC000027B 的 stowed 包。Win11 24H2+ 记录格式已升级为 **'SE02'**
   (旧文献的 'SESG' V1/V2 布局已不适用,实测布局见工具头注释):
   `+00 Size, +04 Sig 'SE02', +08 HRESULT, +0C tid, +20 帧数组指针(完整 VA),
   +28 origin tag('XAML'/'XAMW'), +30 栈上下文{hash,tid,count,frames}`。
   本 dump 共 **54 条记录**(49 条 rc=1 'XAMW' 警告 + 5 条 'XAML' 错误)。
3. **新工具 `batch-sym.py`**:每个 PDB 只解析一次,批量 RVA→最近 public。
   PDB 经 msdl 重新下载缓存(Controls.dll GUID 与 2.6.5 相同
   `08e7203c…age2`,引擎 `Microsoft.ui.xaml.pdb GUID 53a92aa4…age2`,
   均落 `%LOCALAPPDATA%\qaq-bx\crash-sym\`;app PDB 用
   `--pdb target/debug/qaq_bx_ui.pdb`,GUID `2be4cbeb…age1` **匹配校验通过**)。
4. 帧归属:ui.xaml.dll(3601 帧)/ CoreMessagingXP(638)/ qaq-bx-ui.exe(462)/
   Controls.dll(24)。核对的引擎帧落在一段无 publics 的
   ~1MB 区域(0x2b3xxx–0x39cxxx,ICF/模板代码)——用 capstone 反汇编逐点识别,
   并以**紧邻 call 的立即数与被调函数名**定位真实函数(§4)。

## 3. dump 证据

### 3.1 故障点

- ExceptionAddress = `Microsoft.ui.xaml.dll+0x3d1699`
  = **`FailFastWithStowedExceptions+0x61`**(publics 逐字)。
- 反汇编:该函数检查全局 stowed 包非空 → 调 `std::_Func_class` 包装器
  (0x3d1374)→ jmp 0x3d1699 处调 RaiseFailFastException;另一分支
  `mov r8d, 0x800`——引擎自定义 HRESULT 的 **facility 0x800**(XAML)。
- 崩溃线程 11884 原始栈(启发式扫描):`FailFastWithStowedExceptions ×4 →
  ErrorHelper::ProcessUnhandledError+0xf0 → ErrorHelper::ReportUnhandledError+0xd3
  → [0x30bdc5 无 publics 捕获点] → CXcpDispatcher::OnReentrancyProtectedWindowMessage
  → MessageTimerCallback → CoreMessaging 分派 → FrameworkApplication 消息泵 →
  qaq-bx-ui main`。即:**错误逃逸到 XAML 分派边界,UnhandledError 无人处理,
  引擎按设计 fail-fast**。

### 3.2 stowed 包明细(54 条)

| # | ResultCode | origin | 帧 | 含义 |
|---|---|---|---|---|
| [0] | **0x88000fa8** | XAML | 40 | **`AG_E_LAYOUT_CYCLE`(致命)**:raise 站 0x311a9e 反汇编 `mov r14d,0x88000fa8; call [stow helper]`;外层栈 `OnTick→NWDrawMainTree→NWDrawTree→raise` |
| [1][2][4] | 0x80000011 = **RO_E_METADATA_INVALID_TYPE_FORMAT** | XAML | 54/56/48 | reactor reconcile:`set_properties→SetValue(Option<IInspectable>)→…→NotifyPropertyChanged→PropertyValue::AreEqual→MetadataAPI::GetClassInfoFromWinRTPropertyType` 处 stow。**非致命,但显示我们对 ContentControl 写入的 IInspectable 值类型无法被 XAML 元数据解析** |
| [3] | 0x80040111 = **CLASS_E_CLASSNOTAVAILABLE** | XAML | 140 | XBF 模板加载中 binding connect:`PropertyAccessPathStep::ReConnect→ConnectToPropertyOnSource→TryGetDependencyPropertyByName→RunClassConstructorIfNecessary→GetActivationFactory` 处显式 `mov eax,0x80040111`。**非致命**(IFC 恢复);栈里含 30 层 `CUIElement::Measure` 递归 + Controls.dll `ItemsRepeater::MeasureOverride/StackLayout::MeasureOverride/FlowLayoutAlgorithm` 全家 |
| [5..53] ×49 | 1(类 S_FALSE) | **XAMW** | 61–119 | **布局循环警告上下文**(`WarningContextLog`,`LayoutCycleCountdown: N`):引擎在最后 `WarningLayoutIterations=8` 次迭代里记录的"又没收敛"现场 |

### 3.3 警告记录(§3.2 XAMW)两种形态——振荡回路的实锤

**(a) ScrollBar 形态**(stowed[7],119 帧,读向内→外):

```
CLayoutManager::UpdateLayout+0x178                    ← 本次 UpdateLayout 第 N≥242 轮
 ← CUIElement::Measure ×30 层递归(深层视觉树)
 ← ScrollContentPresenter::MeasureOverride+0x98b
 ← VerifyScrollData+0x198
 ← ScrollViewer::InvalidateScrollInfoImpl+0x2d3
 ← ScrollViewer::InvalidateScrollInfo_TryUpdateValues+0x7e9
 ← SetValueByKnownIndex<double> → CoreImports::DependencyObject_SetValue
 ← CScrollContentControl::SetValue → CContentControl/CControl/CFrameworkElement::SetValue
 ← CDependencyObject::UpdateEffectiveValue/SetEffectiveValue
 ← CControl::OnPropertyChanged+0x112 → CControl::RefreshSubscriptions+0x99
 ← CRangeBase::SetValue+0x26e → CRangeBase::SetMaximum+0x14 → SetDoubleValueHelper
 ← (ScrollBar 收到新 Maximum) NotifyPropertyChanged
 ← ScrollBar::OnPropertyChanged2+0x1a → RangeBase::HandlePropertyChanged
 ← OnMaximumChangedProtected → ScrollBar::OnMaximumChanged+0x27
 ← ScrollBar::UpdateTrackLayout+0x29c → put_Margin(SetValueByKnownIndex<double>)
 ← CDependencyObject::SetValue → UpdateEffectiveValue → SetEffectiveValue+0x755
 ← CUIElement::PropagateLayoutDirty+0x1a → raise(rc=1 'XAMW')
```

**(b) ItemsRepeater 形态**(stowed[19]/[20]):

```
stowed[19]: ItemsRepeater::MeasureOverride+0x2b7 → ViewportManagerWithPlatformFeatures::
            SetLayoutExtent+0x174 → UIElementGenerated::InvalidateArrange+0x47 → raise
stowed[20]: EffectiveViewportChanged 委托 UntypedRaise → ViewportManager::
            OnEffectiveViewportChanged+0x5b → UpdateViewport+0x80 → TryInvalidateMeasure
            → consume_IUIElement::InvalidateMeasure<ItemsRepeater> → raise
```

两形态交替出现(49 条),即:**measure → repeater 实现区(extent)变化 →
invalidate arrange/measure → viewport 变化 → 再 invalidate → 同时 ScrollViewer
把新 extent 推给 ScrollBar → UpdateTrackLayout 改 Margin → 又 invalidate**
——单次 `UpdateLayout` 内 250 轮不收敛。

## 4. 上游源码对齐(microsoft-ui-xaml main@aeec1c7f,2026-11-17;本地
sparse clone `D:/Projects/microsoft-ui-xaml`)

1. `dxaml/xcp/core/inc/LayoutManager.h:151` **`MaxLayoutIterations = 250`**;
   `:154 WarningLayoutIterations = 8`;`m_layoutCycleWarningContextsCountdown`
   注释:"between WarningLayoutIterations-1 and 0 … close to the **250 limit**"。
2. `LayoutManager.cpp:470`(UpdateLayout 尾部):`count == -1`(250 轮后仍 dirty)→
   **`IFC_EXTRA_INFO(AgError(AG_E_LAYOUT_CYCLE), &extraInfo)`**,extraInfo =
   最后 8 轮的迭代倒计时+属性写入记录——即 dump 里那 49 条 XAMW 的来源。
3. `dxaml/xcp/inc/xcperrorresource.h:166`:`AG_E_LAYOUT_CYCLE = 4008`
   → `0x88000fa8`(`DebugWriter.cpp:90` 注释逐字)。
4. `ErrorHelper.cpp:317`:`AG_E_LAYOUT_CYCLE → MAKE_HRESULT(SEVERITY_ERROR,
   FACILITY_XAML, E_LAYOUTCYCLE)` → `ReportUnhandledError →
   ProcessUnhandledError → FailFastWithStowedExceptions`(§3.1 故障点)。
5. 警告上下文的三个采集点(与 XAMW 栈逐字对应):
   - `uielement.cpp:600-645`:CUIElement::SetValue 路径
     ("SetValue(PropertyName=Value)");
   - `uielement.cpp:3640-3672`:CUIElement::InvalidateMeasure /
     InvalidateArrange 的非布局元素分支(0x2ad712/0x2ad80a raise 站,
     `StoreLayoutCycleWarningContext → ShouldDebugBreak(Low) → __debugbreak`);
   - **`ScrollBar_Partial.cpp:1691-1748`:`ScrollBar::StoreLayoutCycleWarningContext`
     ——注释原文 "in order to ease layout cycles' debugging (that involve a
     ScrollBar)"**,记录 trackLength/repeatButtonsLength/min/max/value/viewport/
     indicatorSize/minSize/actualSize;调用方 `UpdateTrackLayout`。
6. 反汇编确认 raise 语义:0x2ad712=`call StoreLayoutCycleWarningContext;
   mov cl,1; call LayoutCycleDebugSettings::ShouldDebugBreak; …(未开断言)`——
   生产构建只 stow 警告不中断;250 轮后升级为 AG_E_LAYOUT_CYCLE 错误。

## 5. 现场还原(app 侧)

- `ui-trace.log`(包内 LocalCache)pid=3108 会话:
  `app start → player(半屏展开) → player×2 → home → search → player →
  search → player(打开视频 #0008)` → 约 85s 后崩。**崩在播放页**。
- 播放页结构(`pages/player.rs`):左列
  `ScrollViewer::new().grid_row(2).content(info_and_comments)`(:3143),
  其中评论区 = **ItemsRepeater 变高行**(「一行一张评论卡(变高行,StackLayout
  支持)」:1443、:1691-1695);右栏相关推荐 `ScrollViewer::new()`(:1113)
  已用「显式定宽」配方(:113 注释:**"ScrollViewer 横向量测不可依赖"**——
  首页卡已吃过一次亏,评论行未定宽)。
- 归因表:

| 假设 | 判定 |
|---|---|
| 我们代码直接越界/panic | 排除(故障在引擎 fail-fast 路径,异常码与栈都是引擎主动行为) |
| ScrollViewer(ScrollBar)↔ItemsRepeater 变高行布局振荡 | **成立,主因**(dump 两种 XAMW 形态 + 源码 250 轮逻辑逐字对齐) |
| 引擎对不收敛布局 fail-fast | 成立,机制使然(设计行为;ScrollBar 周期调试钩子证明这是已知问题区) |
| reactor reconcile 的 IInspectable 值触发 RO_E_METADATA_INVALID_TYPE_FORMAT stow | 成立,**次要噪声**(包内 3 条;未触发崩溃,但任何一条逃逸到 UnhandledError 边界同样会 fail-fast——见 §6-B) |

- 为什么是 0x80000011/0x80040111 的旁证:reactor 对 ContentControl 类属性写入
  裸 Rust IInspectable(如虚拟源对象),XAML `PropertyValue::AreEqual` /
  binding connect 需要其 WinRT 元数据类型 → 解析失败 stow。**不影响本次致命链,
  但说明"自定义 IInspectable 当属性值"在框架比较/绑定路径上是裸奔的。**

## 6. 修复方案(分层;A 已落地,其余见状态)

- **A[reactor fork,断环——已落地]**:fork 补绑定
  `IItemsRepeater::{put_HorizontalCacheLength, put_VerticalCacheLength}`
  (bindings.txt + 重新生成),`NativeVirtualItems::new` 创建 repeater 时
  **双 CacheLength 置 0**(原默认 2.0 = 视口两侧各两屏)。机制:extent 估算
  每轮精修 → realization 窗口大幅移动 → extent 再变,CacheLength 直接决定
  每轮窗口移动幅度(即 dump §3.3 里指示块 0.3%/轮的爬行速率)。
  验证:reactor 测试绿、app workspace 279 用例绿、`just dev` 出
  **0.6.0.6** 已装机。**注意**:feed_pager 的"离屏封面保留"启发式按
  缓存 1-2 屏设计(§5 注释),缓存归零后 fast-scroll 视觉需观察;
  如有闪现可回调 0.5/1.0 档位。
- **A2[app 侧,实测未区分]**:新增压测脚本
  `scripts/ui-stress-layoutcycle.ps1`(直开播放页 QAQ_BX_VIDEO + 评论区
  滚轮 10Hz + 窗口 ±1px 震荡 4Hz × 180s)。**A/B 对照:修复前/后均存活、
  均零新 dump——压测未命中真实触发器,无区分度**(触发器大概率在评论快照
  换代/楼中楼展开/特定高度分布,见 §7)。脚本保留作 E17 回归底座。
- **B[app,保命——2.6.5 上不可行,已证伪]**:实测 WinAppSDK 2.6.5 winmd
  **无 `Application.UnhandledError` API**(后续版本才引入)——引擎对
  AG_E_LAYOUT_CYCLE 的 fail-fast 在当前版本上**无法订阅拦截**。升级
  WinAppSDK 后此选项重新开放。
- **C[上游 issue,要素齐备,待写]**:microsoft/microsoft-ui-xaml
  "Layout cycle fail-fast (AG_E_LAYOUT_CYCLE 0x88000fa8) with ScrollViewer +
  ItemsRepeater(StackLayout) variable-height rows"。附:§3.3 两形态栈表、
  §4 源码引文(含引擎自己的 ScrollBar 周期调试钩子)、§2 符号化工具链、
  最小复现工程 `D:/QAQ-Work/layoutcycle-repro`(两轮几何均未引爆,继续
  逼近时用作基座)。
- **D[reactor,噪声待修]**:PropertyValue::AreEqual 对裸 Rust IInspectable
  属性值 stow RO_E_METADATA_INVALID_TYPE_FORMAT(§3.2 [1][2][4])——排查
  哪些属性写入携带自定义 IInspectable,必要时改投影类型。

## 7. 未验证项

- **定责边界(核心待决)**:振荡环 100% 在引擎内部(XAMW 栈零 app 帧,
  写入者全是引擎滚动信息/指示块逻辑),但"纯 WinUI 标准组合即可引爆"
  未被最小复现证实(`layoutcycle-repro` 两轮几何 + app A/B 压测均未引爆)
  → **不能断言纯上游缺陷,也不能归罪 app 代码**。终审 = 0.6.0.6 实网观察:
  不崩 → 缓解成立;仍崩 → 新 dump 直接给出残余路径。app 层反馈环假设
  (content_height 参与闭环)已证伪:`ViewportLayout::content_height() =
  window_height − chrome`,来自窗口事件而非布局输出。
- 振荡的**精确首因**(评论快照换代 reset VirtualSource / 楼中楼展开 /
  特定评论高度分布)未隔离——下次真实 dump 的 XAMW 串(SetValue 值)可
  直接定位翻转属性,heap-scan-strings.py 现成。
- stowed 记录 `+0C` 字段(54 条一致 = 11885,相邻于崩溃线程 11884)按
  ThreadId 解读,未二次验证。
- 54 条包内 49 条警告对应最后 8 轮(≈6 次写入/轮)的折算未逐条对账。
