# API 契约锚（OpenAPI + fixtures）

本目录是 biliplay 对 B 站接口的**消费契约**：`openapi/` = 结构锚（人工维护的 OpenAPI 3.1），
`fixtures/` = 行为锚（真机捕获的截断样本）。两者由 bili-core 的测试共同守卫。

## 目录

```
api/
├── openapi/
│   └── bilibili-feed-v1.yaml   # feed 组契约（rcmd/ogv/popular/precious/ranking）
└── fixtures/
    ├── feed-rcmd.json          # 首页推荐（goto=av/ad/live 多态卡）
    ├── feed-ogv.json           # 剧集卡（season_type=1 样本）
    ├── feed-popular.json       # 综合热门
    ├── feed-ranking.json       # 排行榜
    └── feed-precious.json      # 入站必刷
```

全量真机样本（未截断）：`.qaqh/research/samples/feed/`（含每次捕获的完整 URL 与时间戳）。
捕获脚本：`.qaqh/research/capture-feed-apis.mjs`；fixture 生成：`make-fixtures.mjs`。

## 版本与向下兼容策略

1. **契约代**：spec 的 `x-contract-version`（当前 `1`）。bili-core 类型与 spec 同代演进。
2. **服务器新增字段 = 兼容**。Rust serde 不启用 `deny_unknown_fields`；spec 全部对象
   `additionalProperties: true`。升级零动作。
3. **服务器删除/改名 required 字段 = 破坏性**。`fixtures` 测试红灯 → 评估影响 →
   迁移 → 升 `x-contract-version: 2`（旧代 spec 保留存档）。
4. **required 的口径**：只锚"我们消费且实测该卡型必然存在"的字段。
   例：`FeedCard` 只 required `id/title/goto`（ad/live 卡缺 bvid）；
   av 卡的完整要求由 `AvCardFields` 文档锚表达，Rust 侧按 `goto` 分派后消费。
5. **数值口径**：ID 与计数一律 64 位（实测 aid `117619373907551` 已溢出 u32；
   广告卡 `id` 可为 0）。
6. **行为口径**（实测记录在 spec description）：
   - rcmd 无必需参数；翻页锚 `fresh_idx/fresh_idx_1h/brush`；
   - 翻页跨页有重复 → 客户端去重；
   - ogv `season_type ∈ {1,2,3,4,5,7}`；
   - precious/ranking 一次全量。

## 为什么手写 serde 而不做 OpenAPI codegen

- 目标是**消费端宽松解析**而非接口实现：手写类型 + `#[serde(default)]` 本身就是强类型守卫；
- codegen（progenitor/typify）依赖重、生成的类型对"required 宽松 + additionalProperties"
  语义表达不佳，违背 8GB 机器的依赖轻量约束；
- spec 的价值在**人审 + 版本锚 + 变更警报**，不在生成代码。
  未来如需更强的 spec↔types 一致性，可加 `schemars` 从 Rust 类型生成 JSON Schema 与 spec 比对。

## 变更流程

1. 真机行为变化 → 重跑 `capture-feed-apis.mjs`（新样本入 research/samples）；
2. 结构差异 → 更新 spec（若破坏 required）→ 升 `x-contract-version` → 迁移 bili-core 类型；
3. 刷新 fixtures：`node .qaqh/research/make-fixtures.mjs`；
4. `cargo test -p bili-core` 全绿才算契约迁移完成。
