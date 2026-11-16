# Handoff — 真实评论区数据与点击后 UP 主头像接入

> 记录时间：2026-11-16  
> 工作区：`d:\biliplay-rs`（基线版本 `0.0.1`）

---

## 1. 已完成（Completed）

### 1.1 `bili-core` 评论区客户端与数据契约
- 新增 [reply.rs](crates/bili-core/src/reply.rs) 并通过 [lib.rs](crates/bili-core/src/lib.rs) 导出 `ReplyClient`、`ReplySort`、`ReplyCursor`、`ReplyItem`、`ReplyPage`、`SubReplyPage`：
  - 主楼分页：`GET https://api.bilibili.com/x/v2/reply/main?type=1&oid={aid}&mode={3|2}[&next=..&pagination_str=..]`（无 WBI，沿用 `httpx::get_json_with_ticket` 防盗链头 + Cookie + `bili_ticket`，规避 `/x/v2/reply/wbi/main` 的 `-403` 风控）。
  - 楼中楼分页：`GET https://api.bilibili.com/x/v2/reply/reply?type=1&oid={aid}&root={rpid}&pn={pn}&ps={ps}`。
  - 解析字段：`upper.mid`、`top_replies`（置顶）、`replies`（含楼中楼预览）、`up_action`（UP 主点赞/回复）、`reply_control`（`time_desc`、`location` 自动剥离 `"IP属地："` 前缀、`is_up_top`）、`cursor`（`next`、`pagination_reply.next_offset`、`is_end`、`all_count`）。
  - 头像 URL 统一归一化（`//` 与 `http://` 升 `https://`）。
- [loose.rs](crates/bili-core/src/loose.rs)：新增 `u64_any_or_zero` / `i64_any_or_zero` 处理 B 站响应中可能出现的 `null` 或字符串数字。
- [wbi.rs](crates/bili-core/src/wbi.rs)：将 `encode_component` 开放为 `pub(crate)` 供 `reply.rs` 编码 `pagination_str` 复用。
- [player.rs](crates/bili-core/src/player.rs)：为 `VideoMeta`（`/x/web-interface/view`）补充解析 `desc`、`pubdate`、`owner: Option<Owner>`、`stat: Option<CardStat>`，供缺卡片元数据场景补全 UP 主头像、昵称、简介与 `aid`。
- 单测与真实接口冒烟：
  - `reply::tests` 3 个单测覆盖 URL 构造、主楼/置顶/`null` 字段反序列化、楼中楼分页 `has_more` 计算。
  - `player::tests::video_meta_parses_owner_desc_stat_and_minimal_shape` 覆盖 `VideoMeta` 完整与最小 JSON 解析。
  - `cargo test -p bili-core reply::tests::live_reply_smoke -- --ignored` 已对 `BV1GJ411x7h7`（`aid=80433022`）实弹验证主评论第 1/2 页与楼中楼接口通过。

### 1.2 点击后 UP 主头像与评论区 UI 接入（`bili-ui`）
- [data.rs](crates/bili-ui/src/data.rs)：`VideoItem` 新增 `author_mid: u64` 与 `author_face_url: Option<String>`。
- [auth.rs](crates/bili-ui/src/auth.rs)：`avatar_thumbnail_url` 增加 `//` 协议相对地址升级 `https://` 处理（`@96w_96h_1c.jpg` 缩略图）。
- [theme.rs](crates/bili-ui/src/theme.rs) & [account.rs](crates/bili-ui/src/pages/account.rs)：抽出共享 `theme::avatar_view`（已解码 `Image::source_data` + `PersonPicture` 首字兜底），去除 `account.rs` 重复实现。
- [pages/player.rs](crates/bili-ui/src/pages/player.rs)：
  - `PlayerPage` 与 `HalfPanePreview` 同步接入真实 UP 主异步头像（圆形裁剪 `36x36` / `30x30`，不再仅显示 `PersonPicture` 姓名首字），以及登录态评论输入栏当前用户头像。
  - 评论区接入 `CommentsSnapshot`（`rev + Arc::ptr_eq` O(1) 变更检测，避免 250ms 播放节拍重复比对整表）与 `CommentCallbacks`：
    - 展示总评论数、按热度/按时间排序切换、主楼列表（`keyed_children` 唯一键防重）、置顶/UP主/LV/IP属地/UP主觉得很赞徽标、加载中/空态/错误重试、加载更多评论与终态提示。
    - 楼中楼预览与「展开全部 / 加载更多回复 / 收起回复」交互（含子回复头像展示）。
  - 清理原 `player.rs` 中“接口待接入/即将接入/布局样式预览”等解释性提示文本（对齐 `AGENTS.md` #1）。
- [shell.rs](crates/bili-ui/src/shell.rs)：
  - 集成 `ReplyClient` 与 `CommentsSessionState`（热度/时间双桶独立缓存，切换排序不丢失已拉取页；按 `rpid` 客户端去重防重叠）。
  - `video_item(FeedCard)` 透传首页推荐卡 `owner.mid` 与 `owner.face`（首页网格不提前下载头像，仅点击进半窗/播放页后触发，契合 8GB 内存约束）。
  - 点击视频（首页卡片、热门占位、通知/私信链接、`BILIPLAY_VIDEO`）后通过 `kickoff_video_extras` / `absorb_video_meta` 统一拉取 UP 主头像与第 1 页评论；若 `cid > 0` 但缺 `aid` 或 `author_face_url`，后台单飞补拉 `video_meta`。
  - 统一 `spawn_avatars` / `Message::AvatarLoaded`：以 `playback_epoch` 取消已切走视频的在飞头像下载，`avatar_inflight: HashMap<String, u64>` 区分持久通知头像（`0`）与播放头像（`playback_gen`），并用 80ms `FlushPlaybackAvatars` 合并批刷评论头像减少 UI 重绘；`avatar_cache` 容量扩至 256。
  - 抽出 `half_pane_layer` 复用桌面端与窄屏模式半窗渲染。
  - 新增单测：`video_item_carries_owner_mid_and_normalized_face_url`、`comments_session_tracks_avatar_refs_and_builds_snapshot`。

---

## 2. 当前状态（Current）

- 代码改动清单（未 commit）：
  - 新增：`crates/bili-core/src/reply.rs`、`handoff.md`
  - 修改：`crates/bili-core/src/lib.rs`、`crates/bili-core/src/loose.rs`、`crates/bili-core/src/player.rs`、`crates/bili-core/src/wbi.rs`、`crates/bili-ui/src/auth.rs`、`crates/bili-ui/src/data.rs`、`crates/bili-ui/src/pages/account.rs`、`crates/bili-ui/src/pages/player.rs`、`crates/bili-ui/src/shell.rs`、`crates/bili-ui/src/theme.rs`
- 编译与测试进度：
  - 最后一次修改（[pages/player.rs](crates/bili-ui/src/pages/player.rs)）完成后已执行 `cargo test --workspace` 全量通过（exit 0：`bili-core` 40 passed + 5 fixture tests、`bili-plugin-login` 7 passed、`bili-ui` 18 passed、`bili-video` 1 passed；其余 `--ignored` 测试中 `reply::tests::live_reply_smoke` 此前已实网验证通过）。

---

## 3. 待决事项（Pending）

1. **集成测试与自动化点击测试（`AGENTS.md` #4）**：
   - 尚未执行 `just demo`（`scripts/ui-demo.ps1`）或真机启动 `bili-ui.exe` 点击验证，需实机确认无 panic 及无布局截断/溢出（特别是半窗宽度拖拽到 340px 时评论卡片与楼中楼排版、圆形头像 `Border::corner_radius` 在 WinUI 3 下的裁剪效果）。
2. **极端场景 UI 走查**：
   - 评论区关闭（`code=12002`）、0 评论视频、无本地凭据（访客态）打开视频、快速连续切视频、以及从半窗预览展开至全屏播放页切换期间的状态衔接。
3. **评论输入栏范围确认**：
   - 当前保留了 `TextBox` + `发布` 按钮（纯 UI 控件，未接 `POST /x/v2/reply/add` 写接口），待确认后续是否需要单独接入发评与点赞接口或暂保持只读。
4. **文档与自更新打包（`AGENTS.md` #5）**：
   - 尚未更新 `docs/ui-design.md`（§4 第 129 行仍写“评论区占位”），尚未新增 `docs/Update-<X.Y.Z>.md`，也尚未执行 `just dev` 生成自更新 AppX。
5. **令牌规范核查（`AGENTS.md` #7）**：
   - 本机 `data/credentials.json` 为接手前已存在文件，本次未额外写入测试凭据；后续若做需要登录态的自动化测试，结束后注意恢复/清理临时令牌。

---

## 4. 下一步（Next Steps）

1. 构建并运行真机 UI 验证：
   - 执行 `cargo build -p bili-ui` 与 `.\scripts\ui-demo.ps1`（或设置 `BILIPLAY_VIDEO=BV1GJ411x7h7` 启动 `bili-ui.exe` 并查看 `target/ui-demo/frames/` 截图与 `data/crash.log`）。
   - 重点验证：点击卡片后半窗/全屏 UP 主头像显示、评论主楼与头像渲染、切换「按热度 / 按时间」、点击「加载更多评论」、楼中楼「点击展开 / 加载更多回复 / 收起回复」交互。
2. 视情况在 `scripts/ui-demo.ps1` 增补评论排序切换与楼中楼展开的自动化点击用例。
3. 更新 `docs/ui-design.md` 与 `docs/Update-0.0.1.md`（或目标版本号文档），最后运行 `just dev` 打包末尾修订号自更新 AppX。
