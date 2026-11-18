# QAQ-Bx 长程任务本地文档(本轮: 0.2.0 接线与体验补完)

> 本文档由 agent 会话维护,任何 handoff/异常中断后,新会话以本文为唯一事实源。
> 更新纪律:每完成一项任务,立即把「核验表格」对应行改成 ✅ 并附证据(命令+输出摘要)。

## 0. 任务目标与完成判据

用户原始指令(2026-11-18 夜):
1. 找到全部未接线的功能,禁止使用子代理,Goal 模式逐一接线测试,补完体验。
2. 用户反馈修复:
   - F1 全屏时控制控件又粗又大 → 改透明或默认黑色背景;
   - F2 新增每个播放页的画质调节;
   - F3 弹幕字体和大小可改;
   - F4 部分视频首次播放:无画面有声音 / 有画面无声音 → 定位根因;
   - F5 设计卡片大小曲线,确保高 DPI 时不会渲染异常。
3. 锁定窗口变化时卡顿但 CPU/GPU 占用低的问题;性能基线:debug 播放视频 ≤150MB、release ≤100MB,无法实现则报告。
4. 完成后设计测试确保无回归;产出可供长程任务与 handoff 后新队友使用的本地文档 + 本地交接核验表格。

完成判据:
- [ ] cargo test --workspace 全绿
- [ ] just selftest all 11 场景全 PASS(新增场景另行标注)
- [ ] scripts/ui-edge.ps1 关键场景 exit 0
- [ ] 性能基线给出实测数字(工作集 MB),debug/release 各一份;未达标则写明差距与原因
- [ ] 本核验表格全部 ✅ 或标注「未验证/需登录」

## 1. 未接线功能审计(2026-11-18 全量扫描结论)

| 编号 | 功能 | 现状证据 | 处置 |
|---|---|---|---|
| U1 | 评论「发布」按钮 | `pages/player.rs:1059-1062` 无 on_click | 本轮接线(登录门控) |
| U2 | 搜索建议词 search/default | `docs/ui-design.md:152` 明示未接入 | 本轮接线 |
| U3 | 多分 P 选择(view.pages 已解析但 UI 未用) | `core/player.rs` VideoMeta.pages 消费面为空 | 本轮接线 |
| U4 | 直播页 | `router.rs:100` stub_page("直播") | 本轮接 guest 端点 |
| U5 | 关注/收藏/历史/稍后再看 ×4 | 同上 stub(均需 SESSDATA) | 维持空态+登录门控文案,登录后另开任务(上游 handoff #1 既定决策) |
| U6 | 播放心跳/续播 history+heartbeat | inventory §8 未实现 | 记录为后续(需登录) |
| U7 | player/wbi/v2 播放器配置/字幕 | 未实现 | 记录为后续 |
| U8 | 评论点赞写接口 | 只读 | 记录为后续(需登录) |
| U9 | 热门「换一换」 | handoff 待决 #3 | 维持现状 |
| — | 设置页 Panic! 按钮 | 有意为之(AGENTS.md #4 panic 测试) | 不动 |

## 2. 已完成(逐项附证据)

- F1 ✅ 全屏HUD:共享控件 `on_dark` 参数(hud_chip_background/hud_chip_border/hud_foreground/hud_scrim_color 纯函数,α=168 黑),HUD 恒紧凑控件+白前景;单测 `hud_dark_chips_use_translucent_black_and_white_foreground`。
- F2 ✅ 画质菜单:core `quality_label/quality_menu/select_stream_by_qn/ResolvedMedia/resolve_full_with_cid`;engine `Load.start_secs` 定点换流;shell `SelectQuality`(记忆进度);UI `quality_selector`(桌面/单列/全屏);测试 core 3 + ui 1。
- F3 ✅ 弹幕字体字号:`AppSettings.danmaku_font_family/scale`(serde default,钳位 0.5..2.0);engine `SetDanmakuConfig{font_family,font_scale}` + `DanmakuFonts::with`;行高/估宽随缩放;设置页 ComboBox+滑杆;video 5 tests。
- F4 ✅ 根因与修复:捆绑 libmpv 缺 AV1/AC-3 等解码器或外挂音轨拉取失败 → mpv `video-format`/`audio-codec-name` 空。engine 观察 5/6/7 号属性;shell `missing_media_track`(3s 宽限)+`apply_video_fallback`(排除失败桶)/`apply_audio_fallback`(次高音轨);core `select_stream_excluding/select_audio_excluding`;诊断日志 `[qaq-bx-video] loadfile`/`hwdec-current`。
- F5 ✅ 卡片曲线:Desktop 列数按 MIN 200 推导、卡宽整 DIP 分满、封面 16:9 取整;单测 `desktop_card_curve_stays_integer_aligned_and_16_9`;feed_pager 启发式同步(1280→5列/220卡)。
- P1 ✅ 卡顿修复:engine `RESIZE_COALESCE=120ms` 合并 ResizeBuffers(拖拽 0 次重挂,静止后 1 次);UI `GRID_REBUILD_DEBOUNCE=150ms` `WindowResizeSettled` 去抖 VirtualSource 重建。
- U1(代码完成) 评论发布:core `ReplyClient::add`(reply/add, csrf=bili_jct, 空/超长客户端拒)+httpx `post_form_with_ticket`;UI composer 受控 TextBox+发布按钮+行内错误提示+登录门控;**真机发帖验证挂起(无登录凭据)**。
- U2(代码完成) 搜索默认词:core `SearchClient::default_word`(WBI);启动后台拉取;标题栏占位词替换 + 搜索页空态「大家在搜」chip 点击即搜。
- U3 ✅ 多分P:`pages_menu_from` 纯函数;shell `SelectPage`(重取 playurl(cid) → 定点换流+弹幕重拉);UI `pages_selector`(桌面/单列播控条,分P<2 不渲染)。
- U4 ✅ 直播页:core `live.rs`(getRoomList guest 实测 code=0;playUrl 房间流直链;`-352` 风控的新版端点不可用已记录);`pages/live.rs`(LiveCard+ImageCache 封面管线);shell `LivePageState/ensure_live/start_live_playback`(bvid=`live:<roomid>`,评论 aid=0 走「暂无评论区」态);selftest live 场景。
- 补充修复:mobile/mobile 场景 grid_cols 断言随 F5 曲线 4→5;P1 去抖保留 `home_state.grid_cols` 即时更新(分页目标依赖)。

## 3. 交接核验表格

| 任务 | 状态 | 证据(命令/测试名/输出) |
|---|---|---|
| F1 全屏HUD样式 | ✅ | ui 测试 `hud_dark_chips_use_translucent_black_and_white_foreground` |
| F2 画质调节 | ✅ | core 47→50 测试(quality_menu/select_by_qn/label);`quality_selector_requires_two_tiers` |
| F3 弹幕字体字号 | ✅ | core `danmaku_font_defaults_for_legacy_configs_and_normalizes_extremes`;video `danmaku_fonts_scale_and_alternate_family_render_on_warp` |
| F4 首播音画根因 | ✅(代码) | shell `missing_media_track_requires_media_grace_and_empty_format`;core `select_stream_excluding_skips_failed_codec_buckets` |
| F5 卡片曲线高DPI | ✅ | layout `desktop_card_curve_stays_integer_aligned_and_16_9`;feed_pager 3 处启发式同步 |
| U1 评论发布 | ⚠️ 代码完成 | core `reply_add_params_locks_the_write_contract`/`reply_add_rejects_empty_and_oversized_messages`;真机发帖**需登录未验证** |
| U2 搜索默认词 | ✅ | 启动拉取+标题栏占位+空态 chip;selftest search PASS(占位逻辑无回归) |
| U3 多分P | ✅ 代码完成 | shell SelectPage/PageStreamReady + pages_selector UI;单测 `pages_menu_from`(经 ui tests);多P视频实机未验(本轮测试视频均单P) |
| U4 直播页 | ✅ | core `live.rs`(`parse_live_rooms` 宽松契约+`live_list_smoke` ignored);页面 `pages/live.rs`;selftest **live 场景 PASS**(实网列表+点首房 mpv 直链+Back) |
| P1 resize卡顿 | ✅ 代码完成 | engine RESIZE_COALESCE 120ms + shell WindowResizeSettled 150ms(grid_cols 即时/重建去抖);实机拖拽待人工体感复核 |
| P2 性能基线 | ❌ 判定不可达 | debug 播放 290MB(裁剪后)/无播放 130MB;release 播放 289.5MB/私有 526MB;hwdec=d3d11va 正常。原因与杠杆见 §6 |
| T1/T2 回归验证 | ✅ | `cargo test --workspace` core53/fix6/login7/ui57/video5 全绿;`just selftest all` **12/12 PASS**(新增 live) |
| D2 文档同步 | ✅ | `docs/Update-0.2.0.md` 增补(2026-11-18 夜段);`ui-design.md` 同步(评论区/设置页/直播/搜索默认词);`handoff.md` 重写指向本文 |
| P3 打包 | ✅ | `package/debug/QAQ-Bx-Dev_0.2.0.5_x64.appx`(自签,just dev);`package-appx.ps1` 排除 selftest-data/data/credentials |

## 4. 关键代码地图(新队友必读)

- 播放链路: `shell.rs start_playback(L1193)` → `spawn_resolve_media(L2437)` → `MediaReady→apply_resolved_media(L1571)` → `engine.load_with_options`。
- 引擎: `qaq-bx-video/src/engine.rs`(D3D11+libmpv+D2D 弹幕;命令面 EngineCmd;状态 PlaybackStatus)。
- 换流(画质/分P): engine `Load{start_secs}`(本轮新增) = `set start=<sec>` + loadfile replace。
- 设置: `core/settings.rs AppSettings`(serde default 向后兼容,normalized 钳位) → `shell.rs video_config_from(L288)` / `UpdateSettings(L3884)`。
- 布局: `ui/layout.rs CardMetrics::for_viewport`(F5 曲线落点)。
- 全屏: `pages/fullscreen.rs`(HUD 样式落点 L405/L501)。
- selftest: `ui/selftest.rs`(`just selftest <scenario>`,不开窗派发 Message)。
- 实机点击: `scripts/ui-edge.ps1 -Scenario E<n>`。

## 5. 坑与约定(继承上游)

- AGENTS.md: 禁解释性文本进设置页;新功能必须带测试;4c8T 低耦合;禁 shell timeout/wait/重定向;`just dev` 自更新末尾修订号。
- 热门 pn 从 1 起(pn=0 → -400)。
- `ResourceOverrides` 装箱 Thickness 会 WinUI 0x80000011 崩溃 → Mobile 卡用 Border 点击释放。
- swapchain: `with_backbuffer` 用 from_raw_borrowed(双 Release 会 INVALID_CALL);resize 后重挂靠 250ms mount_tick 脉冲。
- 涉及登录态的测试结束后必须移除令牌(AGENTS.md #7)。
- windows-rs 走本地 fork path(Cargo.toml [workspace.dependencies])。

## 6. 实测记录区(网络/性能数据)

- 内存探针:`scripts/perf-probe.ps1`(QAQ_BX_VIDEO=BV1GJ411x7h7,50s 后采样×5):
  - debug 播放(旧缓存 32+16MiB):工作集 344.1→345.0MB,私有 566.1MB;
  - debug 播放(裁剪后 8+4MiB,readahead 15s):工作集 290.0→290.9MB,私有 525.4MB(−54MB);
  - debug 无播放(首页 48 槽):工作集 130.6MB,私有 74.1MB → 播放增量 ~160MB;
  - **release 播放:工作集 289.5→290.0MB,私有 526.5MB(与 debug 基本一致,内存被
    映射 dll(libmpv/ffmpeg/XAML/WinUI)与解码/交换链主导,非 Rust 代码页);
    `hwdec-current=d3d11va` 硬解确认生效**;
  - 结论:**debug≤150MB / release≤100MB 均不可达**——WinUI3 XAML 运行时+首页
    本身 ≈130MB 工作集(尚未播放),libmpv+D3D 播放增量 ≈160MB 为映射页与解码池,
    非 Rust 堆可调项。已做优化:mpv demuxer 缓存 32+16MiB→8+4MiB(−54MB)。
  未来杠杆(如需继续逼近):自绘轻量播放器替代 mpv、砍首页预载、ImageCache 上限下调、
  mpv fork 裁剪模块。最终结论见核验表 P2 行。
- libmpv-2.dll 位于 `.qaqh/mpv/mpv-dev/`(121MB);target/debug 需手动拷贝(打包脚本同源)。
