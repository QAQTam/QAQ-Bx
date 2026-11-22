# B 站网页版弹幕引擎抓取研究（headless 完整版）

> 2026-11-22。触发：D1 实施回填后，§2.1/§2.2 只有两个外部开源引擎参照（DanmakuFactory /
> danmaku2ass），缺官方网页引擎本体。本轮用 headless Chrome 抓下 B 站现行网页播放器
> **全部弹幕子系统代码**（bootstrap + 49/49 懒加载 widget 全量枚举）与真实弹幕数据接口
> 响应，并完成算法解剖与本项目交叉验证。
> **完整研究笔记（自足）：`D:/QAQ-Work/bili-dm-engine/notes/RESEARCH.md`**（不入库，
> 同 §2.2 研究缓存纪律；B 站 JS 为专有代码，只提炼算法思想、禁止复制代码文本）。

## 1. 素材与可信度

- 抓取：playwright-core + 系统 Chrome headless，视频 `BV1JpyazXEUD`；网络响应捕获
  JS/弹幕数据 → `core.a37280df.js`（bootstrap+数据层 2.1MB）、`jinkela/video`（4MB）、
  `dm/web/view`、`wbi/seg.so`（331 条真实弹幕 protobuf）。
- 补全：core.js 的 webpack runtime 暴露完整 chunk 映射（含命名），按表全量拉齐 49 个
  `widgets/npd.*.js`，其中包含会话未触发的 `bas-danmaku`（高级弹幕）等。
- 关键文件已 prettier 反压缩到 `pretty/`（12 个，core 11.4 万行/491 1.6 万行）。
- sourcemap 官方未发布（全 404）；反压缩行号锚点见 RESEARCH.md §6。
- 风控插曲：headless 被 geetest 滑块拦截（video 元素被替换），但静态 chunk 补全不受影响；
  交互式抓取（设置面板懒加载等）需登录态，本轮未做。

## 2. 引擎身份与架构（2024-2026 现行版）

| 模块 | 文件 | 角色 |
|---|---|---|
| 数据层 | core.js 内 `DanmakuStore` | `//api.bilibili.com/x/v2/dm/web/`（wbi），pageSize=360/段，`allSegment/allHistory/basSegment`，`preTime=1` 提前量 |
| **常规弹幕引擎** | `npd.491` | **DOM + CSS 变量动画渲染（非 canvas）**；碰撞池按运动形态分池；spaceManager 分配；时间轴排序在 blob worker；DOM 回收池 common/inside/rotate |
| **特殊弹幕引擎** | `npd.922`（内部名 `danmaku-x`） | 导出 CommandRender/CommandSpaceManager/DPX/ECommonMode/ESpecialMode；Shooter/BlackHole/Firework 等模式复用同一碰撞池架构 |
| 特殊弹幕空间分配 | `npd.779` blob worker | 积分图（summed-area table）+ 从屏心螺旋找空位 + 缩放/旋转插入 |
| 高级弹幕入口 | `npd.819 bas-danmaku`（557B） | 仅 inline blob-worker 工厂；BAS worker 源码以字符串内嵌在使用方 chunk，无独立文件 |

模式枚举：ECommonMode `Roll=1/Bottom=4/Top=5/Reverse=6`（与 protobuf 一致）；
ESpecialMode 2000-2020（Firework*/BlackHole/Picture/Shooter/**Bidirection** 等）。

## 3. 算法要点（对照 D1-D5）

1. **时长/速度模型**（`tz()`，屏宽分段 + Logistic）：分段区间 [558,888]→7-13s、
   (888,1082]→8-14s、…、(2314,2560]→14-20s、>2560→16-22s（每 500px 各 +1s）；
   Logistic 中点在 ratio≈20+，**常规弹幕实际时长≈该屏宽
   段 minTime**（1920 屏≈13.1s）；`speed=(屏宽+字宽)/时长×speedPlus×(speedSync?videoSpeed:1)`，
   `speedsync` 默认 false；`setting.duration=4.5` 仅兜底。与本项目 duration 模型同构
   （项目 ROLL_SECONDS=10 对应官方 558-888px 段的下界区间）。
2. **滚动碰撞分配**：`spaceManager.add()` 最多扫 **3 个 y-band**，band 内按 (y,endTime)
   二分有序；追尾双判据 `t.x > S.x+S.width && t.middle > S.endTime + A`，
   `A = floor(duration×(1-density))/2 + 0.1`（密度容忍）；`area`(显示区域%) 限制 +
   特权弹幕豁免（pool1/border/高赞/UpProfile/LiveToVod）；`preventShade` 时可用高度×0.85；
   **溢出只有丢弃，无强插**。与 D1 的离散行+findMin 是两种哲学（官方更"贴"但会丢更多）。
3. **密度**：官方两层 = ingest 速率闸 `validateLive`（1s 窗，限额
   `floor(density×speed/2/3)`）+ 碰撞容忍 A，**不是**同屏活跃计数（D5 原计划口径）。
4. **渲染无状态**：`x = (middle - renderTime) × speed`，middle=头部到达左缘时刻 ——
   与本项目 stateless 渲染不变式完全同构；hover 暂停靠重算 middle/endTime。
5. **双向弹幕**：Roll 且非 border/池1/高赞时 50% 随机反向，独立 keyframes。
6. **数据 schema**（官方 protobufjs JSON 全量提取）：`DmWebViewReply`、
   `DanmuWebPlayerConfig`（弹幕设置 25 项：aiSwitch/aiLevel/preventshade/dmarea/
   dmdensity/speedplus/fontsize/fullscreensync…）、`DanmakuElem`。

## 4. ⚠️ 交叉验证发现的项目 bug（P1，待修复）

**`qaq-bx-core/src/danmaku.rs` 的 weight 字段号错误**：
- 官方 schema 与真实 seg.so wire 扫描（331 条，`bili-dm-engine/scan-seg.js`）一致：
  `weight = field 9 (varint)`，`dmid = field 12 (string, wire2)`。
- `danmaku.rs:240` 按 `(12, 0)` varint 匹配 → 真实数据永不命中 → `weight` 恒 0。
- 后果：`engine.rs:502` 智能屏蔽（`danmaku_weight_min` 默认 5，`weight>0 && weight<阈值`
  才屏蔽）**从未生效过**（静默失效）。`mid_hash`(field 6) 不受影响。
- 修复建议：`(12, 0) => weight` 改 `(9, 0)`；可选补记 `(12, 2) dmid`。修复时按项目
  宪法补解析用例（用本轮抓取的真实 seg.so 片段作 fixture）。

## 5. 移植路线（官方判据 → Rust，2026-11-22 分析）

> **实施状态（2026-11-22）**：P0 全部 + P1 参数面已落地并测试
> （`Update-0.6.0.md` §DM：tz 模型/weight 修复/密度容忍 A/area/字幕池豁免/
> 高度感知轨道数,workspace 289 绿）；P2 band packing 与设置面板 UI 接线未做。

我们与官方**架构已同构**（无状态渲染 + 单遍分配模拟 + est_width 准入/绘制共用），
移植的是判据与数据结构，不是重写：

- **P0 判据对齐（小步）**：① `roll_secs(width)` 移植官方 `tz()` 屏宽分段表+Logistic
  （替代恒 `ROLL_SECONDS=10`，分配/渲染/窗口裁剪三处共用）；② danmaku.rs weight
  字段号 `(12,0)→(9,0)` 修复 + 真实 seg.so fixture 测试；③ est_width 对照官方
  1.02×getLen×fontSize / 1.125×fontSize 校准系数与 emoji 宽度。
- **P1 参数面（D5 对齐）**：`danmaku_density`(默认 1.0)、`danmaku_area`(默认 100%)、
  `danmaku_speed_plus`(默认 1) 入 VideoConfig + SetDanmakuConfig 通道；滚动准入 (b)
  加密度容忍 `A = floor(roll_secs×(1-density))/2 + 0.1`；area 离散化为有效 lane 数、
  特权弹幕（pool1/border/高赞）豁免。
- **P2 band packing 标准版（D1 迭代）**：allocate_lanes 滚动分支替换为官方 3-band
  算法（band 内 y 区间下移压缩 + 追尾双判据 + hasDm 前视；跨 band 独立 bin）；返回
  lane=band、新增 y_off 缓存；渲染 y=8+y_off。跳变治理：重排仅作用于 progress_ms >
  当前播放头的条目（官方在线决策的离线近似）。
- **P3 远期**：speedSync（先有播放倍速功能）、Reverse(mode 6) 双向、表情/图片弹幕
  （PrefixImage + expressions 资源）、直播洪流闸 validateLive（做直播弹幕时）。
- **不做**：DOM 渲染层（D2D 管线是我们的性能正位）；按需拉段（预存链全量 MB 级
  可接受，保持简单）；防挡脸（dmask）完整版——已查明官方实现=**服务端预生成人形
  剪影（`dm_mask.fps/mask_url` 随播放器 API 下发，Range 分段拉取）+ 客户端 SVG
  点命中测试**（`isPointInFill`，仅 Chrome 系启用），非客户端实时识别；接入是
  数据侧中型工程，先做几何版 preventShade（底部 15% 让位，纯碰撞可用高度×0.85）。
- **测试**：tz 表边界/A 公式/band packing 场景/特权豁免/area 截断纯函数单测 +
  331 条真实 seg.so 回放大样本 + ui-edge 弹幕设置面板热更新回归。

原 §5 的三点建议（weight 修复、D5 口径、Reverse 立项）并入上述 P0/P1/P3。

## 6. 素材索引

- 研究缓存（不入库）：`D:/QAQ-Work/bili-dm-engine/`
  - `js/` 全部脚本（84 文件）、`data/` 弹幕接口真实响应、`pretty/` 反压缩可读版、
    `notes/RESEARCH.md` 完整笔记（含 pretty 行号锚点表）、`scan-seg.js` proto 字段扫描、
    `capture.js`/`fetch-all.js` 复现脚本、`manifest*.json`。
- 建议的后续动作：① 修 danmaku.rs weight 字段号 + fixture 测试；② D5 采纳「速率闸 +
  追尾容忍 A」口径讨论；③ 可选：Reverse(mode 6) 双向弹幕 feature 立项评估。
