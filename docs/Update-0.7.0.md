# QAQ-Bx v0.7.0 更新说明：竖屏卡片度量重构与高 DPI 自适应防线

> 状态：**已实施并核验**（单元测试 + `selftest mobile` + `480×860` / `859×860` 真窗 `PrintWindow` 视觉验收）。

## 一、竖屏布局与卡片度量重构 ✅

针对竖屏模式此前默认 `859×800` 准正方形窗口下「2 列封面过大（占卡高 75.1%）、12px 标题过小（占卡宽 2.91%）且 1 行标题留白过多」的失准问题，按 [portrait-mode.md](dev-notes/portrait-mode.md) 双轴模型收口修改纯值层度量：

| 项 | 变更内容 | 落点 |
|---|---|---|
| **竖屏默认尺寸** | 新增 `DEFAULT_PORTRAIT_WIDTH = 480.0`、`DEFAULT_PORTRAIT_HEIGHT = 860.0`（9:16 竖屏手感），保留 `PORTRAIT_MAX_WIDTH_DIPS = 859.0` 作为拖拽上限与贴边宽 | [layout.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/layout.rs)、[shell/mod.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/shell/mod.rs) |
| **竖屏 / Mobile 三列自适应** | 取消 `ShellChromeMode::Mobile` 写死 `cols = 2`，新增 `MOBILE_CARD_MIN_WIDTH = 186.5`（`clamp(2, 3)`）：`Compact`（`390..599` DIP）保持 **2 列**（179..283 DIP）；`Medium`（`600..859` DIP）自动切为 **3 列**（186..273 DIP），与 `860` DIP 桌面 3 列（236 DIP）平滑衔接 | [layout.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/layout.rs) |
| **紧凑卡字号与文本槽** | `CARD_TITLE_FONT_COMPACT` `12.0 → 13.5`、`CARD_AUTHOR_FONT_COMPACT` `11.0 → 12.0`，边距收口 `8.0 / 10.0`、`MOBILE_CARD_TEXT_ALLOWANCE = 84.0`（保留 4 DIP 抗分数倍 DPI 字体膨胀余量） | [layout.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/layout.rs) |

## 二、高 DPI 代码层自适应与矩阵防线 ✅

| 项 | 变更内容 | 落点 |
|---|---|---|
| **消除双重定宽冲突** | `populated_card_view` 新增 `stretch` 标志（`grid_col.is_some()`）：在 `FeedRow` 的 `Star(1.0)` 列内不再重复写 `.width(metrics.card_width)`，消除分数倍 DPI 下 1px 余数分配与边框圆角裁切引起的毛刺/裂边风险 | [video_card.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/widgets/video_card.rs) |
| **物理像素向内取整与 DPI 权威源** | `winrt_ext.rs` 抽离 `resolve_dpi_scale` / `dips_to_outer_min_px` / `dips_to_outer_max_px`：优先使用 `GetDpiForWindow`，下限 `.ceil()`、上限 `.floor()` 向内取整，数学保证 `100%~225%` 任意分数倍 DPI 下不逃逸出 `[390, 860)` DIP | [winrt_ext.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/winrt_ext.rs) |
| **DPI × 宽度矩阵单测** | 新增 `mobile_and_portrait_dpi_matrix_preserves_bounds_and_16_9` 与 `dpi_scale_and_inward_pixel_rounding_preserve_portrait_band` 覆盖 `1.0..2.25` 倍率；同步 `selftest` 竖屏/Medium 3 列与 480 DIP 2 列断言 | [layout.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/layout.rs)、[winrt_ext.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/winrt_ext.rs)、[selftest.rs](file:///d:/QAQ-Bx/crates/qaq-bx-ui/src/selftest.rs) |

## 三、核验记录

- `cargo test --workspace`：全绿通过（`qaq-bx-core` 97、`feed_fixtures` 6、`qaq-bx-plugin-login` 7、`qaq-bx-ui` 136、`qaq-bx-video` 52，0 失败）。
- `just selftest mobile`：`[PASS] mobile`，验证 `1280` 桌面 5 列 → `390` Mobile 2 列 → `640` Medium 3 列 → `860` Desktop 3 列 → `1280` 5 列全路径。
- 真窗 `PrintWindow` 视觉验收（96 DPI）：
  - 启动默认客户区 `480×860`（2 列、`224×126` 封面、13.5px 标题）；
  - 强拉宽至 `1200px` 后被 presenter 精确钳制在 `859×860`（3 列、`273×154` 封面、搜索浮窗按钮就位）。
