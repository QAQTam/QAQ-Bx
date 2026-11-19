# 底栏「悬浮液态玻璃」(iOS 26 风格)调研与技术路线

> 2026-11-19 由上一会话收集(官方 web 搜索未启用,GitHub/README 直抓)。
> 目标:竖屏/移动底栏(`compact_bottom_bar`,chrome.rs:237;shell.rs 挂载于
> grid_row 2)升级为 Apple iOS 26 Liquid Glass 风格——悬浮(脱离边距)+ 胶囊圆角 +
> 折射/色散/菲涅尔高光材质。「怎么做」留给实施会话,本文给地图与雷区。

## 0. 社区实现盘点(GitHub 实测抓取,2026-11-19)

| 仓库 | 技术 | 对我们( Rust/自包含 fork )可移植性 |
|---|---|---|
| [luckyelysia/LiquidGlassWinUI](https://github.com/luckyelysia/LiquidGlassWinUI) | C# `XamlCompositionBrushBase`,**IAT hook `wuceffectsi.dll`** 注入自定义 HLSL 进 DWM composition 管线;28 参数材质(折射/色散/菲涅尔/眩光/bloom/色调/色温/vibrance);MIT | ❌ 不可直接用:C#/.NET8 + **硬绑 WinAppSDK 2.2.0** 的 IAT hook(内部符号版本脆),我们是 Rust + fork 自带 WinUI 构建。**但 README 是最好的材质参数规格书** |
| [SuGar0218/TryLiquidGlass-WinUI3](https://github.com/SuGar0218/TryLiquidGlass-WinUI3) | C# 实验 | 参考 |
| [pratikone/liquid-glass-WinUI](https://github.com/pratikone/liquid-glass-WinUI) | C# + **Win2D** 复刻 iOS 26 | 技术路线参考;引用 [archisvaze/liquid-glass](https://github.com/archisvaze/liquid-glass)(iOS 折射 shader 的通用参考实现) |
| [Millennium-Science-Technology-R-D-Inst/WinUI.Composition.Hlsl](https://github.com/Millennium-Science-Technology-R-D-Inst/WinUI.Composition.Hlsl) | **C++/WinRT**,HLSL composition effects + 主题感知液态玻璃控件,持续维护 | HLSL→composition 注册 technique 参考(C++/WinRT 侧做法可翻译) |
| [medfa12/liquid-glass-winui](https://github.com/medfa12/liquid-glass-winui) | D3D11 折射,逆向 macOS QuartzCore(GLSL) | shader 数学参考 |
| [VeiQiuLab/AuroraGlass](https://github.com/VeiQiuLab/AuroraGlass) | C++,Win32/WPF/WinUI3 SDK | 参考 |
| [rahulbag3/LiquidMonitor](https://github.com/rahulbag3/LiquidMonitor) | **屏幕捕获**式玻璃背板 | 若 composition backdrop 采样不到 SwapChainPanel 的兜底思路 |

Apple 官方 HIG:developer.apple.com/design/human-interface-guidelines/liquid-glass
(本次 fetch 404,疑似反爬;材质语义以 luckyelysia 参数模型 + archisvaze 为准即可)。

## 1. 三条技术路线(按成本排序;先 0 后 1)

### 路线 0(首推 spike):纯 stock composition effect graph——零新依赖
`Microsoft.UI.Composition`(我们 winmd 源已含)+ **fork 已有的 composition 互操作点**
(`ObserveCompositionHost`/`SetCompositionChildVisual` 绑定已存在!):
1. `Compositor.CreateBackdropBrush()` 采样元素后方内容
2. 效果图:`GaussianBlurEffect → SaturationEffect → ColorSourceEffect(tint) →
   CompositeEffect`,叠加边框高光近似(渐变 Border 描边)
3. `ElementCompositionPreview.SetElementChildVisual` 把效果刷视觉垫到底栏下
4. 底栏本体:脱离边缘 margin + 胶囊 corner_radius + 细描边
- 得到:约 80% 观感(悬浮玻璃胶囊),无折射/色散
- 全部 API 在 Microsoft.UI.winmd(app build.rs 白名单直出),**纯 app 侧,零 fork/零 C#**
- 雷:backdrop 采样是否覆盖 SwapChainPanel(mpv 视频)/弹幕 stage 这类独立
  composition 子树——**spike 第一问**(采样不到 → LiquidMonitor 的屏幕捕获兜底思路,
  或先只在非播放页启用)

### 路线 1(完整液态玻璃):自定义 HLSL(折射/色散/菲涅尔)
- 需要 Win2D(PixelShaderEffect)或 wuceffectsi 自定义效果通道(C#/C++ 侧)——
  **与 perf-architecture-review §8.4 的 Win2D 否决冲突**(该否决针对播放面;
  UI 材质场景如需解冻,先向用户报备),或 fork 增能力(与 v0.3 并行会话协调)
- 决策留给实施会话:先做路线 0,拿真机观感对比 iOS 参考图,再判断是否值得升级

### 反路线(不采用)
- `DesktopAcrylicBackdrop`/系统材质:窗口级,做不到元素级悬浮胶囊
- WebView2/直接合成整条栏位截图:性能与正确性都不如 composition 通道

## 2. 性能预算(本仓库文化:量化先行)
- 玻璃区域 ≈ 859×~90px 条带(竖屏带内),backdrop blur 每帧重采样——E14 已有
  帧时间量化基建;落地后跑一次 E14 对照(玻璃开/关)。
- mpv 播放页 + 弹幕 + blur 的 CPU/GPU 叠加,在 4c8T/8G 约束下必须实测
  (reuse QAQ_BX_DATA_DIR 隔离启动 + ui-edge 截图/亮度技法)。

## 3. 布局改造点(实现会话的直接锚)
- `chrome.rs:237 compact_bottom_bar` → 外观包一层「悬浮胶囊」容器(margin +
  corner_radius + glass 背景)
- `shell.rs` 底栏挂载处(grid_row 2)→ 行结构从贴底改为悬浮(margin 留白)
- 需要元素级 child visual → 用 fork 已绑的 `SetCompositionChildVisual`;
  新 winmd filter 按 build.rs 既有格式加(Compositor/CreateBackdropBrush/各 Effect)

## 4. 验收口径(建议)
- 用户睡前授权:「任意操作屏幕直到最终可用」——观感对标 iOS 26 底栏参考图,
  交互不回退(点击/更多浮层/游戏手柄焦点),性能无肉眼卡顿。
- 回归:cargo test --workspace 全绿 + selftest boot/stress + E8(移动形态走查)
  + E14 对照。
