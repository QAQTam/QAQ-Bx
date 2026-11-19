# 后端零拷贝审计(2026-11-19)

范围:`qaq-bx-core`、`qaq-bx-video`。结论基于代码阅读,未跑基准。
前提:P2 实测内存由 mpv/解码池/DLL 主导,零拷贝收益在于播放热路径 CPU、瞬时峰值、首帧延迟,而非总内存。

| 优先级 | 项 | 位置 | 改法 | 工单 |
|---|---|---|---|---|
| 🔴 P0 | 分块 `to_vec` 复制 + 整块下完才交付 | stream_proxy.rs L573-617 | `Bytes` 贯通 + `bytes_stream` 边收边转发 | 见 issue「stream_proxy」 |
| 🟠 P1 | 弹幕每条 2 次 String 分配 | danmaku.rs L166/175、httpx.rs L101 | 段 `Bytes` + 偏移;mid_hash→u32;上屏才转 UTF-16 | 见 issue「danmaku」 |
| 🟡 P2 | JSON 二次解析 / Value 中转 | player.rs L161-178/257-263、live.rs L48、native.rs L925 | 强类型 + Visitor | 见 issue「JSON」 |

不做:全面 serde borrow(生命周期污染 UI)、rkyv/mmap(数据 KB 级)。

验收统一用 `scripts/perf-probe.ps1` 前后对比,数据写入对应 Update 文档。
