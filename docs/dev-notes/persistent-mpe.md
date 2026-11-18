# 常驻播放层(park)设计 —— MediaPlayer 升 tier1(2026-11-19 拍板,当日落地)

> 决策:直接上 P0-1 常驻 MPE 大重构;完成后「播放引擎」默认 **native**,mpv 自动降级
> 为回退(回退链必须先落地)。本文是唯一权威设计,实施时逐条回填证据。
> **结果:方案比原设计简化——不需要 fork `ElementHost`,播放层垫底(z-order)即可,
> 见 §6 实施结果。**

## 1. 目标与不做什么

目标:
1. **后台播放**:离开播放页(设置页/首页/任意路由)音乐/进度不断,回到播放页画面即续。
2. **无缝引擎切换**:正在播的 VOD 换引擎不重开视频(M7 的"只记账"口径废止)。
3. **消 M3 黑屏**:切页/全屏不再销毁 MPE,断点重建路径退居兜底。
4. **默认 native**:mpv 降级为回退(解码缺失/失败自动接管)。

不做什么(本轮):
- 直播(FLV)仍恒 mpv, MF 不支持, 不动 `LiveReady`。
- mpv 引擎不删:回退通道 + 非 MF 支持容器(试看/特殊编码)的兜底。
- 半屏抽屉、PiP 滚动行为保持现有交互。

## 2. 现状与根因

- 组件树按 key 复用,但**页面卸载即销毁子树**:MPE 随 NativeSurface 卸载,
  其自持 MediaPlayer 一并死亡 ⇒ 无后台播放;重建即 M3 换源(数百 ms 黑屏)。
- fork 没有"把已存在元素重挂进容器"的能力(`ElementRef` 只支持
  创建-绑定方向), ⇒ 无法让同一个 MPE 跟着页面走。
- `reconcile_playback_visibility` 的「离开播放页即关播」是**规则**,不是缺陷;
  tier1 后该规则改为「离开播放页即**收进停车场**(audio 继续)」。

## 3. 目标架构

```
AppShell(唯一所有者)
 ├─ PlaybackStage(常驻组件,shell 作用域,永不过渡卸载)
 │   ├─ ElementHost(fork 新增:RehostElement imperative 的宿主)
 │   │    └─ MediaPlayerElement(全生命周期唯一实例)
 │   └─ DanmakuSurface(透明层,同样常驻)
 ├─ 页面(player / fullscreen / half-pane)只渲染「插槽」(纯 Border 占位),
 │   通过 shell 状态请求 rehost:slot(player-cell / fullscreen / parked)
 └─ 状态机: PlaybackSlot { Player, Fullscreen, HalfPane, Parked, None }
```

关键机制:
1. **fork `ElementHost`**(新 widget):声明面 `host_state(Embed|Park)`,imperative
   `RehostElement { element_ref, target }` —— 把 ElementRef 已绑定的元素
   `Panel.Children.Add` 进宿主(自动从旧父移除,WinUI 语义);卸载时元素交还
   停车场而非销毁。
2. **停车场**:shell 根部一个 `Visibility=Collapsed` 的常驻宿主。**spike 必答**:
   collapsed 的 MPE 是否继续出声/计时?若停,退化为 1×1 + Opacity 0 + 屏外坐标。
3. **注入协议不变**:`claim_pending_source` 单飞仍由 PlaybackStage 驱动;M3 的
   断点重建只在「真的换元素」时触发(tier1 后应几乎不存在,保留为兜底)。
4. **mpv 通道不动**:VideoSurface 仍随页面挂载(回退通道,不追求常驻)。

## 4. 状态机与规则变更

- 「离开播放页即关播」→「离开播放页即 Parked」:`reconcile_playback_visibility`
  不再 `stop_playback`,改 `slot = Parked`;关播只剩显式入口(迷你条关闭/Over)。
- M7「只记账」口径废止:引擎切换改为**当次生效**(常驻 MPE 后 mpv→native 即
  立刻换源定点续播;native→mpv 先卸 MPE 源再 loadfile)。
- `stop_playback`(显式关播)仍统一下线两侧资源。

## 5. 回退链(与默认 native 同步落地,先于切默认)

- native `MediaFailed`/装载失败 → **自动回退 mpv**:同分轨 `load_with_options`
  定点续播,弹一次通知;会话内按 codec 前缀记忆(同 F4 口径),同桶不再重试 native。
- 失败判据沿用锚点:`native media failed: error=… extended=0x…`。
- 「播放引擎」默认值翻成 native(selftest 种子仍显式写 mpv,保持无头场景快)。

## 6. 实施结果(2026-11-19,S1–S4 全部落地,方案比原设计简化一档)

**方案变更:不需要 fork `ElementHost`。** 真机 spike 实测"垫底层"即可:
shell 的 keyed_children 按键复用组件——只要播放层**始终保留在键列表里**(垫在活动页
之下),组件与 MPE 就不过渡卸载,后台播放/免重建全部成立。原设计的"重挂/停车场"
问题被 z-order 直接绕开。

| 阶段 | 内容 | 状态 |
|---|---|---|
| S1 spike | 播放层垫底(键稳定)+ reconcile 改 Parked(不再关播) | ✅ 真机:进设置页 20s,`native heartbeat pos=30.1/30.3` 持续推进;**零** `detached/remount`(一次 attach 打全程) |
| S2 接管 | `player_page_content` 抽取单源 + park 层垫底 + `mount_pulse_active` 增加 park 参数(后台装载/注入由 250ms 节拍驱动) | ✅ |
| S3 无缝切换 | `EngineSwitchAction::{ReloadNative,ReloadMpv}` 当次生效:同分轨同进度定点续播;弹幕换代重落位(`danmaku_epoch` + `reseed_danmaku`);直播豁免保留 | ✅ 真机:`resume=40.2` → `loadfile start=40.246` 同进度接管 |
| S4 回退+默认 | `native_load_choice` 入口按会话级 codec 桶黑名单跳过;`PositionTick` native `status.error` → `fallback_native_to_mpv`(同进度接管 + 通知);默认翻 **native** | ✅ 构建/测试;真机构造解码失败路径待补(依赖缺扩展环境) |

验收:E11/E12/E13/E15/E16(titlebar 拖框回归,并行会话)/E17(tier1)全 PASS;
selftest 14 场景全通过(`engine` + park 语义断言已改写);cargo test --workspace 全绿。

## 7. 遗留与风险

- park 期间透明层/视频继续解码(被遮挡,GPU 白耗)——优化项:park 时 stage 停绘。
- 后台播放没有 UI 入口(无迷你条):回播放页靠 Back 历史/重开卡片;迷你条属后续 UI 项。
- native 失败自动回退的真机触发用例缺"缺扩展环境",当前靠 E17 全链路 + 与切换共享
  `switch_to_mpv_now` 路径兜底。
- park 层渲染整个播放页(含评论/相关推荐),内存占用与可见时一致;后续可换
  轻量占位内容(只保留 surface 子树)。
