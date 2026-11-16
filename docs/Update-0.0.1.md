# Update 0.0.1 — 真实评论区数据与 UP 主头像

> 记录时间:2026-11-17。Cargo workspace 版本 `0.0.1`,末尾修订号由 `just dev` 打包时自动递增。

## 新增

- **真实评论区接入**(`bili-core` 新增 `reply.rs`):
  - 主楼分页走 `/x/v2/reply/main`(无 WBI 签名,规避 `-403` 风控),楼中楼走 `/x/v2/reply/reply`;
  - UI 支持总评论数、按热度/按时间排序切换(双桶缓存,切换不丢页)、置顶/UP主/LV/IP属地/UP主觉得很赞徽标、楼中楼预览与「展开全部/加载更多回复/收起回复」、空态/加载中/错误重试;
  - 评论区关闭等非 0 错误码统一映射为 `BiliError::Api`,评论区显示错误态与重试(暂无 12002 专属文案)。
- **UP 主真实头像**:首页推荐卡透传 `owner.mid/face`,点击进半窗/播放页后异步拉取并圆形裁剪显示;登录态评论输入栏展示当前用户头像;头像缓存容量扩至 256,80ms 批刷合并重绘。
- **视频元数据补全**:点击时若缺 `aid`/头像,后台单飞补拉 `video_meta`。
- **综合搜索接入**(`bili-core` 新增 `search.rs`):`/x/web-interface/wbi/search/all/v2`(WBI 签名,实测 code=0);标题栏/搜索页输入 → 结果卡片网格,标题高亮标记与 HTML 实体剥除、`H:MM:SS` 时长解析、封面经 ImageCache 异步填充;搜索结果可直点播放(`open_video` 增加 search_index 回退链)。

## 变更

- `bili-core`:新增 `u64_any_or_zero`/`i64_any_or_zero` 宽松反序列化;`VideoMeta` 补充 `desc/owner/stat` 解析。
- `bili-ui`:评论区接入 `CommentsSnapshot`(`rev + Arc::ptr_eq` O(1) 变更检测)与 `CommentCallbacks`;抽出 `theme::avatar_view` 共享头像组件。

## 测试与脚本

- `cargo test --workspace` 全量通过(`bili-core` 40+5 fixture、`bili-plugin-login` 7、`bili-video` 1);评论区真实接口冒烟(`live_reply_smoke`)已对 BV1GJ411x7h7 实网验证。
- 真机自动化点击(`just demo`,8 场景):首页 → 悬停 → 滚动 → 进播放页 → 评论区排序切换 → 滚动评论区+PiP → 返回保活 → 设置页,全部通过,全程无 panic。
- **内部状态自测**（`just selftest`，新增 `--selftest` 模式）：基于 reactor `test` 模块(RecordingAdapter + ComponentHost),不开窗口、不模拟鼠标、不读用户凭据(隔离数据目录),直接挂载 AppShell 派发 Message 断言状态。六个场景:boot(挂载+feed)、nav(路由/历史栈)、comments(真实评论解析 + 排序切换 + 双桶缓存)、rapid-switch(快速连切,播放代次取消路径)、stress(240 条消息风暴追查 try_wake panic 类问题)、search(真实搜索 → 点开首条结果),全套 <8s,可重复、可进 CI。
- 极端场景(`scripts/ui-edge.ps1` 新增):访客态(无凭据)可正常浏览并读取公开评论;快速连续切视频(播放代次取消路径)无 panic。
- `scripts/ui-demo.ps1` 修复:`Add-Type` 缺少 `System.Drawing` 程序集引用导致脚本首次运行即编译失败;设置页导航点击坐标修正(收起态导航为 44px 窄条,原坐标落入迷你播放条)。

## 已知问题

- 2026-11-16 22:43 曾记录一次 `try_wake` 回调 panic(`panic in a function that cannot unwind`,见 `data/crash.log`),本轮回放未复现;计划引入内部测试二进制(`--selftest`,直接派发 Message 断言状态,不再依赖鼠标模拟)专项追查。

## 范围外

- 评论写接口(发评/点赞)未接入,输入栏当前仅 UI 占位。
- 播放页右栏「接下来播放 · 相关推荐」为占位样本数据,待接入相关视频 API。
- 搜索功能未实现,暂无法自动化定位 0 评论视频;0 评论的空列表解析路径已由单测覆盖(reply.rs 空字段),实机空态渲染验证待找到测试 BV 后补充。
