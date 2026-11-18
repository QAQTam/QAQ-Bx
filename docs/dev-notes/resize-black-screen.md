# 快速伸缩窗口视频区黑屏排查与修复(2026-11)

> 症状:部分情况下快速伸缩窗口大小导致视频区黑屏;切换视频后短暂亮屏然后又黑屏。
> 结论:不是 D3D/ResizeBuffers 问题,是 **UI 层 VideoSurface 组件被布局档位切换反复销毁重建**,
> 且卸载清理主动 `request_clear_swap_chain` 清黑,重挂依赖下一个 metrics 事件/250ms 节拍,
> 抖动期间高频「清黑 → 空窗 → 重挂」,时序差时长时间黑屏。

## 1. 排查方法(可复现)

- `scripts/ui-edge.ps1 -Scenario E11`(本轮新增):`QAQ_BX_VIDEO` 直开播放页 →
  30 次大↔小交替 `SetWindowPos`(55%↔100%,每步 55ms)→ 截图 ×4 + 视频区平均亮度检测 +
  stderr 全量采集(`target/ui-demo/e11-stderr.log`)。
- 引擎/挂载路径临时 stderr 探针(仿 F4 锚风格,排查后已全部移除):
  CREATE/DROP、metrics 事件、apply resize、detach→requeue、mark_attached。

## 2. 实测证据(E11 修复前)

- `ResizeBuffers` **零失败**,每次 `SetSwapChain` completion 均 ok —— 引擎链路本身健康。
- 窗口伸缩跨布局档位(843x420 / 260x420 / 673x360 / 1256x360 四个槽位)时,
  `VideoSurface CREATE → DROP → detach→requeue` 配对出现 9+ 次:
  **每跨一个档位组件整体销毁重建**(pip/非 pip 分支、loading 分支树形变化 → 组件路径变化)。
- 每次卸载 cleanup 都 `request_clear_swap_chain` → 视频区被主动清黑;
  新面板首个 metrics 是 `w=0 h=0`(无效被丢),真实 metrics 到达/250ms tick 才重挂。
- 快速伸缩 = 上述循环高频发生 → 视频区持续闪黑;最后一次 clear 后若 mount 驱动
  不及时(pending 空手 / tick 停),黑屏一直持续到下一次尺寸变化 —— 「部分情况下」黑屏。

## 3. 修复(两处,均为 UI 层,引擎零改动)

1. `widgets/video_surface.rs` 卸载清理:**移除 `request_clear_swap_chain`**。
   组件卸载时 panel 元素随视图树销毁,SwapChain 引用由 XAML 释放,主动 clear 只会把
   视频区清黑拉长空窗;引擎侧 `detach_panel` 已把 swapchain 放回待挂载槽,新面板首个
   metrics 事件即重挂。
2. `pages/player.rs` `surface_with_loading`:**loading 态不得改变树形**。
   原 `if loading { Grid(surface, overlay) } else { surface }` 让组件路径随 loading 翻转
   变化(切视频必翻转 → 「短暂亮屏又黑」)。现两种状态共用同一 Grid 结构,仅切
   覆盖层 opacity 与 `ProgressRing::is_active`。

已知残留(非黑屏,低优先):pip/非 pip 分支结构不同仍会触发组件重建(实测仍有
CREATE/DROP),但清黑已移除 + metrics 事件即时重挂,视觉无空窗;后续可在
`floating_surface` 宿主上做结构统一彻底消除重建。

## 4. 验证

- 修复后 E11:视频区平均亮度 110–130(全程有画面,无黑屏收敛失败);
  末轮时序干净:`apply resize 843x420 → mark_attached ok=true` 收敛。
- `cargo test --workspace` 全绿(core 56 / fix 6 / login 7 / ui 60 / video 9+1 ignored)。
- 截图:`target/ui-demo/frames-edge/e11-*.png`(画面中的左右黑边为 4:3 MV 正常 letterbox)。
