# UI 拖拽重排性能审计(2026-11-19)

范围:`qaq-bx-ui` 首页网格 resize 通路 + reactor fork 图片缓存。代码阅读结论,未跑新基准(E14 `RESIZE-STORM` 可量化)。

## 现状通路

`WindowResized`(coalesce 合并)→ 每次重算 `ViewportLayout` → `should_rebuild_grid`(列数/卡态/卡宽 ≥2 DIP)→ 150ms 去抖 → `WindowResizeSettled` → `rebuild_home_source()`。

## 热点

| 级别 | 问题 | 位置 | 改法 |
|---|---|---|---|
| 🔴 P0 | **行 key 含卡宽**:`row_revision` 编码 `card_width` 低 11 位,行 key = `(rev<<24)^row_idx`。F5 连续卡宽曲线下几乎每次拖拽收口卡宽都变 → 所有已实现行 key 全变 → ItemsRepeater 拆掉并重建全部可见+预热行(约 60 卡 × ~10 元素)一次性压在 UI 线程,即"松手卡一下" | shell.rs `current_row_revision` L1950;video_card.rs L70 | key 只编码 `stream_gen/cols/compact`;卡宽经 `FeedRowInput.metrics` 走 `input_changed` 原地改属性(diff 而非重挂) |
| 🔴 P0 | **卡片显式定宽**:`Border.width(card_width)`,拖拽 150ms 内网格不跟手(溢出/留白),只能等 Rust 侧重建 | video_card.rs L412-457 | 行改 Grid N 个 star 列、卡片 Stretch,宽度交给 XAML 布局连续跟随,零 Rust 开销;封面高按行宽统一下发(单属性)。终局:fork 暴露 `UniformGridLayout`(MinItemWidth + ItemsStretch=Fill),列数也交给原生,只有分页逻辑需要 cols |
| 🟠 P1 | **AppShell 整树重渲染**:每条 `WindowResized` 都改 `viewport_layout`,7.9k 行根组件 view 全量 diff,拖拽期间每帧一次 | shell.rs L5764-5818 | 原始宽高存非渲染字段,仅 tier/cols/compact 变化才触发 view;HUD 宽度等用独立子组件订阅 |
| 🟠 P1 | **滚动驱逐整源重建**:`sync_offscreen_covers` 任一封面置空 → `rebuild_home_source` → 全部 slots `chunk.to_vec()` 深拷贝(String 字段)O(N) | shell.rs L2044;video_card.rs L57-60 | slots 改 `Rc<VideoItem>`;按行局部更新而非整源重建 |
| 🟡 P2 | 移动玻璃:拖拽中 composition blur(σ9)每帧随 SetSize 重采样,弱核显吃力 | widgets/glass.rs | 风暴期切 tint-only,settled 恢复 blur |
| 🟡 P2 | 封面下载双拷贝:`bytes.to_vec()` 再转 `Arc<[u8]>` | auth.rs L338 | `Arc::<[u8]>::from(&bytes[..])` 一次拷贝 |

## 已有的好设计(保留)

- reactor `ImageCache` 按 `Arc<[u8]>` 指针复用 BitmapImage,重挂不重解码。
- 封面 CDN 裁剪 `@480w_270h_1c` + WebP;480 张 LRU;离屏去引用释放纹理。
- `coalesce_key` 合并 resize 消息;E14 风暴探针。

## 验收建议

E14 拖拽脚本前后对比:settled 后首帧耗时、风暴期 UI 线程最大间隔 `max_gap_ms`、CPU 峰值;4c8G 真机体感。

## 后续优化方向(WinUI 用法层)

结论:WinUI 合成层由 GPU 驱动、动画可走独立合成线程,4c8G 上瓶颈是 UI 线程 XAML 元素创建/拆除,改用法而非换框架(换框架丢失 UIA 自动化、IME、无障碍)。

1. 卡片元素拍平:单 Grid + 行列定义,目标每卡 ≤6 元素。
2. 延迟挂载:悬浮层/角标/不可播遮罩按需创建。
3. ItemsRepeater 预热缓冲在弱机降到 0.5–1 屏。
4. 动画一律 Composition(Visual Scale/Offset/Opacity),禁止 Storyboard 动 Width/Height。
5. ThemeShadow 仅给当前悬浮/聚焦的单一元素。
6. 降级开关:跟随 `UISettings.AnimationsEnabled` + 设置项「减少动画」,电池模式关模糊与动画。

## 悬停展开预览卡(Store 风格)设计要点

- 不原地展开(会触发 ItemsRepeater 重排);根布局覆盖 Canvas 内**全局唯一** `PeekCard`,`TransformToVisual` 定位到悬停卡片。
- 不用 Popup(窗口化定位/DPI/焦点问题)、不用 Flyout/TeachingTip(外框不可定制)。
- z 序必须在液态玻璃宿主之后(参见 v0.4 G4/G7)。
- 时序:进入 300ms 预取简介 → 1s 弹出 → 离开宽限 200ms;手柄/键盘焦点停留 1s 同样触发。
- 动画:Composition `Vector3KeyFrameAnimation` Scale 1.0→1.15(中心点居中)+ Opacity。
- 复用 `video_card.rs` 现有 HoverState(on_enter/on_expand/on_preresolve),改为卡片只上报、Shell 驱动单例。

## 实施状态(2026-11-19,refs #4)

- 🔴 两项 P0 已实施:
  - `video_card::home_row_revision(stream_gen, cards)` 只编码结构因子,卡宽不再进行 key;`should_rebuild_grid` 收窄为 cols/compact,纯宽度收口走新增 `metrics_refresh_needed`(≥2 DIP)的轻量原地刷新;reactor `reconcile_virtual_items` 对同 `key_revision`+同 key 集合复用行容器,`FeedRow/VideoCard` 经 input diff 原地改属性。
  - `FeedRow` 行容器 `StackPanel`→`Grid` N 等分 star 列 + `column_spacing`;`VideoCardInput.grid_col: Option<i32>`,行内卡不带显式宽(`Grid.Column` 附加在既有外层 Border,零新增元素),静态页(热门/搜索)保留显式宽不受影响;行高/封面高仍按行统一下发。
  - settled 仅在结构性变化时清活动行/游标(`pending_grid_structural`),纯宽度收口保留滚动游标语义。
- 验证:`cargo test --workspace` 223 passed(新增 revision 稳定性/重建判定单测);`just selftest all` 15/15 PASS(mobile/half-pane/popular/search 覆盖两条网格通路)。
- ⚠ E14 口径说明:E14 直开播放页路由,不经过首页行网格,风暴计数器(max_gap/消费数)被网络解码波动主导——修复前后两样本 pre {167,197} vs post {97,178}ms,无法据此判定收益;「松手卡一下」的消除属结构性(remount→diff),待真机首页拖框体感与后续把 E14 改为首页路由探针再定量。`scripts/home-storm-probe.ps1` 为首页路由补充探针(修复后 30×55ms 风暴仅消费 3 条)。
- 未做:终局 fork `UniformGridLayout`(列数交给原生)仍为可选项。
