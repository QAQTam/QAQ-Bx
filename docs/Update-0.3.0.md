# QAQ-Bx v0.3.0 计划:弹幕引擎升级(轨道避让 / 排版缓存 / 密度控制)

> 状态:**计划**(未实施)。本文由 2026-11-19 只读调研产出,实施时逐项回填 ✅ 与证据,
> 完成后本文自然转为 v0.3.0 更新说明(沿用 `Update-0.2.0.md` 体例)。

## 0. 背景:0.2.x 已落地的弹幕基础(本轮已完成,验收证据在 git 历史)

| 项 | 内容 | 落点 |
|---|---|---|
| 帧节奏 | 引擎循环「Present(1) vsync 阻塞 + 固定 sleep 16ms」双节拍互击导致隔帧错拍(一快一慢);改为 `RENDER_BUDGET=8ms` 截止线 + `recv_timeout`,帧率由 vsync 主导跟随显示器满刷新 | `qaq-bx-video/src/engine.rs` 渲染循环尾 |
| 时基平滑 | mpv `time-pos` 按视频帧步进(33–42ms 跳变) → `interpolated_danmaku_time()` 墙钟外推重锚(±20ms 容差 / 0.25s 封顶),暂停冻结锚点 | `engine.rs` D2D overlay 段 |
| 分段预存 | 全片弹幕串行拉完才上屏 → 链式分段预取(播放头段优先,`DanmakuPart` 逐段 `AppendDanmaku` 归并,失败跳段续链) | `qaq-bx-ui/src/shell.rs fetch_danmaku*` |
| 本地过滤 | 类型(滚动/顶部/底部/彩色) + 屏蔽词(`re:` 正则 / `u:` 用户 midHash / 关键词) + 重复弹幕 4s 窗口去重 + 智能权重阈值(`DanmakuFilter`,ingest 过滤 + 热更新重滤) | `engine.rs` `CompiledDanmakuFilter`;`core/settings.rs`;设置页弹幕卡 |
| 解析扩展 | protobuf 补 `midHash`(field 6)/`weight`(field 12) | `qaq-bx-core/src/danmaku.rs` |

## 1. v0.3 目标

弹幕渲染质量对齐主流三方引擎:轨道互不重叠、顶部/底部自动堆叠、同屏密度可控,
同时把 60fps 下的每帧 CPU 开销(逐帧排版 + 逐帧建刷)降到可忽略。

**非目标(明确不做)**:高级弹幕(mode 7 定位/旋转)、表情/图片弹幕、云端屏蔽词 API 同步
(`/x/dm/filter/user` 需登录态写接口,另行立项)、libass 字幕管线(放弃 D2D 逐帧控制,否决)。

## 2. 只读调研结论(算法参照源)

### 2.1 hiHkm/DanmakuFactory(MIT,可移植算法)

C 版 xml→ass 转换器;轨道模型在 `src/AssFile/AssFile.c`(MIT 许可,可参考实现)。
核心:每**像素行**维护两个时间戳(`R2LToRightTime` / `R2LToLeftTime`),滚动弹幕
R2L(右→左)准入判据:

```text
准入条件(对轨道 y 的每个像素行 cnt):
  (a) now.time >= ToRightTime[y]        // 入场瞬间,同轨上一条已"完全进入屏幕"
  (b) now.time + rollTime * screenW / (screenW + textLen) >= ToLeftTime[y]
                                        // 新条到达左端时,上一条已"完全退出屏幕"
登记(spawn 后):
  ToRightTime[y] = now.time + rollTime * textLen / (screenW + textLen)  // 尾部过右缘
  ToLeftTime[y]  = now.time + rollTime                                  // 尾部过左缘(横穿完成)
```

- 速度模型:`rollTime` 恒定(默认 12s),**速度随文本宽度变化** `v = (screenW + textLen) / rollTime`
  ——与本项目现模型同构(`ROLL_SECONDS` 恒定)。
- 溢出策略:全部轨道放不下时,`density == -1` 则丢弃;否则 `findMin()` 挑**最早空出**的轨道强放
  (对应 danmaku2ass 的 `FindAlternativeRow`)。
- 密度屏蔽:同屏活跃弹幕(结束时刻 > now)计数 ≥ `density` → 丢弃;同屏同文本重复屏蔽
  (比本项目的 4s 窗口去重弱,两者可并存)。
- 顶部/底部:`fixEndTime[y]` 驻留区间占用,从上(顶)/从下(底)找首个空闲槽,超时即释放。

### 2.2 m13253/danmaku2ass(GPL-3,仅借鉴思想,禁止移植代码)

`TestFreeRows`(L699)对滚动弹幕给出等价的双条件判据(上一条已完全进入 + 尾部已离开右缘),
`FindAlternativeRow` 兜底挑最早释放行;顶部/底部按 `spawn + duration_still > 新 spawn` 判驻留冲突。
**许可注意**:GPL-3 与本项目许可不兼容,只提炼算法思想并独立实现,不复制代码文本。

### 2.3 结论

- 无需引入外部引擎/依赖;轨道避让是 O(n) 的单遍时间轴模拟,在 ingest(分段预存到货)时做即可。
- 渲染保持 **stateless**(x 由 t 与 spawn 推导)不变:轨道分配结果作为字段存进 `OverlayDanmaku`,
  seek/插值/暂停天然正确;轨道重分配只在 ① 分段到货 ② 过滤规则变化 ③ 面板尺寸变化 三个时机触发。

## 3. 任务拆解

### D1 滚动弹幕轨道避让(P0)

- [ ] `OverlayDanmaku` 增加分配结果字段:`lane: u16` + `est_width: f32`(精确宽度,替代 `chars×char_width` 估算)。
- [ ] 引擎线程新增 `LaneAllocator`:输入按 progress 升序的弹幕流 + 面板 DIP 宽度 + `ROLL_SECONDS`,
      逐条用 §2.1 双条件判据选轨;全满 → 默认丢弃(与 B 站网页一致),设置项 `danmaku_overflow: drop|force`
      提供 `findMin` 强放兜底。
- [ ] 触发时机:`AppendDanmaku`(增量:从排序插入点继续模拟)/`SetDanmaku`/`SetDanmakuFilter`(全量重跑)/
      `Resize`(宽度变化影响准入窗口,全量重跑)。去重表、过滤、轨道分配统一进 ingest 管线
      `filter_batch → allocate_lanes`。
- [ ] `draw_danmaku_overlay` 改读 `elem.lane`,删除 `progress_ms % max_lanes` 哈希分轨。
- [ ] 速度模型:维持恒定 `ROLL_SECONDS`;`danmaku_speed_mode: duration|velocity` 设置项预埋接口
      (velocity 模型 = B 站网页式恒速,duration 随宽度伸缩;实现放 D4)。
- 测试:同轨不重叠(构造宽度混排序列,断言任意两同轨条目区间不交)、增量到货与全量重跑结果一致、
  丢弃/强放两策略、resize 后重分配。

### D2 D2D 排版/刷子缓存(P0,性能)

- [ ] `TextLayoutCache`:key = `(content 弱引用或 hash+字符串, format_generation)`,LRU 上限 512 条;
      值 = `windows_text::TextLayout`(含 `GetMetrics` 精确宽度)。`draw_text` → `draw_text_layout`。
- [ ] `BrushCache`:key = `(0xRRGGBB, alpha)`(描边黑 + 主色两族),上限 256;设备丢失/字体变化清空。
- [ ] 失效:字体族/字号缩放/`font_scale` 变化 → `format_generation += 1` 全量失效;swapchain 重建 → 双清。
- [ ] `est_width` 从 layout metrics 取(D1 依赖),移除 `char_width` 估宽常量。
- [ ] 描边仍走 4 向偏移 `DrawTextLayout`(D2D 文本描边无廉价替代),开销降为 layout 复用。
- 测试:缓存命中/失效(字号变化后 layout 重建)、LRU 容量、WARP 渲染冒烟(复用现有 `danmaku_fonts_scale_...` 模式)。

### D3 顶部/底部驻留堆叠(P1)

- [ ] 驻留弹幕 ingress 时按 `progress` 升序做 `fixEndTime` 槽位分配:顶/底各自独立槽序列,
      槽 i 空闲条件 `spawn_new >= fixEndTime[i]`,分配后 `fixEndTime[i] = spawn + PIN_SECONDS`;
      全满按溢出策略(D1 同款)。
- [ ] 渲染:y = 槽位基线(顶部自上而下 / 底部自下而上),替换现固定 `y=8.0` / `height-...` 的互叠。
- 测试:同刻 5 条顶驻留 → 5 个不同槽;槽随超时释放(时间轴模拟断言)。

### D4 恒定速度模型(可选项,P2)

- [ ] `velocity` 模型:`v` 恒定(默认 `screenW / ROLL_SECONDS`),时长 `duration = (screenW + est) / v`;
      轨道准入判据不变(公式里 rollTime 换 duration)。
- [ ] 设置页暴露`弹幕速度模式`(默认维持 duration 模型,零迁移成本)。

### D5 同屏密度上限(P2)

- [ ] `danmaku_density: u32`(0=不限):ingest 时统计「结束时刻 > 当条 spawn」的活跃数,超限丢弃;
      实现(参照 §2.1 的滑动窗口计数,或简化为按 lane 空闲即近似)。
- 测试:构造高频弹幕流,断言同屏峰值 ≤ density。

## 4. 验收判据(实施时逐项回填)

- [ ] `cargo test --workspace` 全绿(video 新增避让/缓存/堆叠用例)
- [ ] `just selftest all` 12/12 PASS(half-pane 含弹幕开关断言)
- [ ] 弹幕多的视频实测:任意时刻同轨无重叠(肉眼 + 可选 debug 叠加轨道线)
- [ ] 60fps 满刷新下引擎线程 CPU 占用对比 0.2.x 有可测量下降(任务管理器,记录数字)
- [ ] 高频弹幕(如千项/分钟)不卡帧;分段预存链到货时无渲染停顿(ingest O(n) ≤ 5ms)

## 5. 风险与备注

- 轨道分配依赖面板宽度:窗口拖拽期间的合并 resize(`RESIZE_COALESCE`)触发全量重跑,
  长视频(2 万条)O(n) 单遍 ≈ 毫秒级,可接受;必要时按 viewport 内窗口惰性重跑。
- `est_width` 由 layout metrics 提供,D2 缓存未就绪前先用估算兜底,避免 D1/D2 强耦合。
- 预存链乱序到货(播放头段优先)不影响正确性:重分配按 progress 全序重跑,与到货顺序无关。
- 研究源码缓存文件(danmaku2ass.py 等)仅供本轮调研,未入库,调研后已清理。
