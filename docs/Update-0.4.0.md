# QAQ-Bx v0.4.0 更新说明:移动底栏「悬浮液态玻璃」+ 竖屏模式(实验)

> 状态:**已实施并验收**(逐项带证据)。分支 `feat/v0.4.0`,
> 主提交:`c290054`(竖屏模式)→ `5e3f7ec`(调研)→ `914c196`(玻璃主体)
> → `12a975c`(打磨:z 序修复 + 主题自适应 tint)→ 本文(版本对齐)。

## G. 移动底栏「悬浮液态玻璃」(iOS 26 风格)✅

移动形态(宽度 < 860 DIP)底部 5-Tab 导航栏脱离边缘,改为悬浮胶囊 +
composition 液态玻璃:效果采样 XAML 背景内容做 blur(saturation(backdrop)),
叠加主题自适应 tint 霜面,Tab 行浮在玻璃之上。**纯 app 侧实现,零 fork 改动、
零新依赖**(windows-numerics 为 fork 自带本地库的 path 引用)。

调研与三路线决策见 `docs/dev-notes/liquid-glass-research.md`(本轮走路线 0+2;
路线 1 自定义 HLSL 折射/色散/菲涅尔高光涉及 Win2D 解冻/fork 增能力,未做)。

| 项 | 内容 | 落点 / 证据 |
|---|---|---|
| G1 悬浮胶囊 | 左右/底 10 DIP 悬浮 + 全胶囊圆角 32 + CardStroke 1px 描边;内容跨行铺到栏下(Backdrop 有物可采) | `widgets/chrome.rs::compact_bottom_bar`;`widgets/glass.rs::capsule` 常量(单测 `capsule_constants_are_consistent`) |
| G2 手写效果图 | `IGraphicsEffect`/`IGraphicsEffectD2D1Interop`/`IGraphicsEffectSource` 三接口 COM 节点手写(fork core 的 ComCell 3 vtable 槽位),blur(σ=9.0)·saturation(1.15) 经 `Compositor.CreateEffectFactory` 接入;效果图不能做形状填充(E_INVALIDARG,实测),走 SpriteVisual.Brush + CompositionGeometricClip 圆角裁剪 | `widgets/glass_fx.rs`;`widgets/glass.rs::build_effect_brush`;失败回退 tint-only(日志 `[glass] effect graph failed at {step}`) |
| G3 生命周期 | 子组件范式自持 `ElementRef<Grid>` + `use_effect_guard` 注册 composition host 观察(根组件 AppShell 的 use_effect 不触发,实测);Ready 零尺寸先存 compositor,Metrics 有效尺寸补建;resize 仅 SetSize 无重建 | `widgets/glass.rs` `GlassObserver`/`glass_metrics`(单测覆盖非法尺寸) |
| G4 **z 序修复** | 实测 `SetElementChildVisual` 的 child visual 盖在宿主 XAML 内容**之上**——玻璃与 Tab 行同网格混排时 Tab 行整体被毛玻璃化(只透出模糊鬼影,首版误判为 tint 对比度问题)。Tab 行改为玻璃宿主的兄弟节点后绘制(XAML 文档序在后 = z 序在上) | `chrome.rs::compact_bottom_bar`(玻璃宿主空内容 + `tab_row` 兄弟);修复前/后对照截图见验收 |
| G5 主题自适应 tint | `GlassBar` 经 reactor `on_color_scheme` 订阅(订阅即回报初值,变化仅派发);`CompositionColorBrush.SetColor` 原地切换,不重建视觉。浅色白霜 a=0x4D / 深色近黑 a=0x66(暗玻璃衬托白图标) | `glass.rs::tint_color`(单测 `tint_color_adapts_to_scheme`);`GlassMessage::Scheme` |
| G6 对比度 | 未选 Tab 图标/文字 opacity 0.72 → 0.85 | `chrome.rs` `tab_btn` |
| G7 浮层遮挡回归修复 | 竖屏(移动形态)下点「更多」:浮层底锚定在跨行内容(row1+row2)内、底边距 10 DIP,下半截落在玻璃胶囊带(底悬浮 10 + 胶囊高 64)内,被 composition 玻璃 child visual 盖住(同 G4 z 序机制,XAML 文档序在前者被玻璃盖住)。修复:新增 `capsule::FLOATING_CONTENT_BOTTOM_MARGIN`(= HEIGHT + FLOAT_MARGIN×2),「更多」浮层与手柄提示胶囊(compact)底边距统一抬出胶囊带并留一个悬浮留白呼吸间距;单测 `floating_content_bottom_margin_clears_capsule_band` / `compact_more_popup_bottom_margin_clears_glass_capsule_band` 钉住常量不漂移 | `widgets/glass.rs::capsule`;`widgets/chrome.rs::compact_more_popup`;`gamepad.rs::gamepad_hint_bar(compact)` |

### G-fix 验收实测(2026-11-19)

- 自动化点击(390×760 / 845×1000 / 420×680 真机):点击底栏「更多」Tab → 浮层完整浮在玻璃胶囊上方,6 项全部可见可点,无玻璃遮挡;再点击 Tab 收起正常。
- `cargo test --workspace` 全绿;selftest boot / stress(4.8s)/ mobile / e8 PASS。

### 验收实测(2026-11-19,96 DPI 真机)

- `cargo test --workspace` 全绿(core 61 / ui 78 / video 25 / core-cli 7 / 6)。
- selftest:boot PASS(0.0s)/ stress PASS(4.1s,基线 4.4s)/ mobile PASS(0.2s)/ e8 通过。
- E14(桌面播放页 resize storm,玻璃不在其路径上)干净跑完:CPU phase A=23.1% / B=14.8%(8 逻辑核),storm 合并 consumed=47 / produced≈54。
- 真机截图(D:/tmp/glass-e2e/,未入库):浅色(`fix-light-zoom.png`)白霜 + 深色图标锐利;深色(`fix-dark-zoom.png`)暗霜 + 白图标清晰;玻璃上 Tab 点击导航正常(搜索页 ⇄ 首页实测);打包应用 0.4.0.1 竖屏启动(`packaged-portrait.png`)玻璃正常。
- 运行时主题翻转(注册表 AppsUseLightTheme + ImmersiveColorSet 广播):日志先后出现 `[glass] tint adapted to Light` / `[glass] tint adapted to Dark`,玻璃跟随系统主题实时切换,无需重启。

### 已知限制(诚实清单)

- 折射/色散/菲涅尔高光(iOS 26 完整观感)未做——属路线 1(自定义 HLSL,涉 Win2D 解冻 / fork 增能力),动之前需向用户报备。
- composition backdrop 对 **SwapChainPanel**(mpv 视频 / 弹幕)的采样:当前移动布局下播放页不与底栏重叠,该子问题未触发、未验证。
- 玻璃参数(模糊 σ、饱和度、tint α)烘焙在 `glass.rs::material`/常量,无用户设置项。
- 玻璃仅移动形态(chrome: Mobile)底栏;桌面形态无底栏不受影响。
- 移动形态 resize storm 的开/关量化对照未脚本化(理论开销极小:resize 路径仅 composition SetSize;交互目测无卡顿)。

## P. 竖屏模式(实验)✅

`settings.portrait_mode`(默认关,设置页「窗口」卡 ToggleSwitch):
窗口宽度钳制在底栏边界带 [390, 859] DIP(presenter 级硬钳制)、没收系统最大化
(WS_MAXIMIZEBOX CLEAR)、右缘贴合最大化(工作区全高)+ 还原;全屏播放正交保留。
设计文档 `docs/dev-notes/portrait-mode.md`(双轴隔离模型)。真机验证证据见提交 `c290054`。

## 版本

- workspace 版本 0.2.0 → **0.4.0**(`Cargo.toml [workspace.package]`,成员
  `version.workspace` 继承;Cargo.lock 同步)。打包(`just dev`/`release-sign`)
  的版本号自此取 0.4.0.x。
