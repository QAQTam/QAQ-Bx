# 后端零拷贝审计(2026-11-19)

范围:`qaq-bx-core`、`qaq-bx-video`。结论基于代码阅读,未跑基准。
前提:P2 实测内存由 mpv/解码池/DLL 主导,零拷贝收益在于播放热路径 CPU、瞬时峰值、首帧延迟,而非总内存。

| 优先级 | 项 | 位置 | 改法 | 工单 | 状态(2026-11-19) |
|---|---|---|---|---|---|
| 🔴 P0 | 分块 `to_vec` 复制 + 整块下完才交付 | stream_proxy.rs | `Bytes` 贯通 + `bytes_stream` 边收边转发 | #1 | ✅ 已实施;新增时序/续拉回归测试;perf-probe 真机数字待回放实测 |
| 🟠 P1 | 弹幕每条 2 次 String 分配 | danmaku.rs、httpx.rs | 段 `Bytes` + 偏移;mid_hash→u32;上屏才转 UTF-16 | #2 | ✅ 已实施;core `DanmakuSegment` + video `ContentRef`,上屏点收拢解码 |
| 🟡 P2 | JSON 二次解析 / Value 中转 | player.rs、live.rs、native.rs | 强类型 + Visitor | #3 | ✅ 已实施;`loose_str`/`seg_base_loose` 手写 Visitor;snake 键形独立强类型字段 |

实施注记:
- #1 leader 任务改为 spawn 模型,`ChunkUpdate::Streaming` 按片广播连续前缀快照
  (`Arc<ChunkBuf>`,发布仅按片引用计数);候选中途截断时从已发布缺口续拉,
  已发字节绝对位置不变,天然免回退;终态单片 0 拷贝直转缓存。
- #2 解析期不再物化文本:`DanmakuElem` 存 `(content_off, content_len)`,
  `mid_hash: Option<u32>`(8 位 hex 直解,非法 = None);`DanmakuFilter` 的
  `u:` 规则编译期直解 u32。
- #3 行为差异:nested 容器/null 形态的 `initialization` 由旧「to_string 保留」
  改为置空(避免向 MPD 写垃圾字节范围);其余形态逐一由单测钉住。

不做:全面 serde borrow(生命周期污染 UI)、rkyv/mmap(数据 KB 级)。

验收统一用 `scripts/perf-probe.ps1` 前后对比,数据写入对应 Update 文档。
