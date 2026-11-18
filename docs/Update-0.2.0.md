# QAQ-Bx v0.2.0 更新说明

## 新增功能：沉浸式全屏播放

- **无边框全屏切换**（`crates/qaq-bx-ui/build.rs`、`crates/qaq-bx-ui/src/winrt_ext.rs`）
  - 接入 WinUI `AppWindow::GetFromWindowId`、`SetPresenterByKind(AppWindowPresenterKind::FullScreen / Default)`、`AppWindowPresenter::Kind` 与 `TextBox` 焦点祖先判定（`set_window_fullscreen`、`is_window_fullscreen`、`is_text_input_focused`）。
  - 系统层面贴合当前显示器无边框铺满，退出全屏精确恢复进入前的窗口/客户区尺寸；从首页右侧半窗进入全屏并退出时保留 `home_half_pane` 状态。
- **全屏画布与自隐悬浮 HUD**（`crates/qaq-bx-ui/src/pages/fullscreen.rs`、`crates/qaq-bx-ui/src/pages/mod.rs`）
  - 新增独立低耦合组件 `FullscreenPlayer`，保持 `VideoSurface` 稳定挂载于首层 `keyed("fs-canvas", ...)`（重用 `D3D11 + libmpv + D2D` 弹幕渲染管线，不销毁重建视频引擎）。
  - 顶部 HUD（返回、`item.title`/`author`、「退出全屏」按钮）与底部 HUD（进度条、播放/暂停、时间、静音/音量滑块、实时弹幕开关、退出全屏按钮）使用 `ThemeBrush::SolidBackground` 悬浮呈现；播放态无指针/HUD 操作 3 秒自动隐藏，暂停/加载中/视频异常提示/鼠标悬停 HUD 期间保持常驻。
- **多端全屏入口、双击与快捷键/手柄路由**（`crates/qaq-bx-ui/src/pages/player.rs`、`crates/qaq-bx-ui/src/shell.rs`、`crates/qaq-bx-ui/src/gamepad.rs`）
  - 为 `PlayerPage` 桌面双栏传输条、Mobile 单列传输条及首页半窗传输条新增全屏按钮（`fullscreen_toggle_button`），并支持视频画面左键双击（`DoubleClickTracker`，380ms / 8px 窗口）切换全屏。
  - `AppShell` 通过 `PreviewKeyDown` 统一路由 `F11`、`Esc`（仅全屏退出）、`F`、`Space`、`←/→`、`↑/↓`、`M`、`D`（经 `is_text_input_focused` 避让输入框与系统修饰键，并在 HUD 自动隐藏时由 `root_ref.request_focus()` 保持键盘焦点）；手柄 `Route::Player` 补充 `X 全屏`、`B` 优先退出全屏；发起导航、切页、全局搜索或停止播放时自动退出全屏。
  - 更新设计文档 [ui-design.md](ui-design.md)。

## 测试与工程验证

- **单元测试**（`cargo test --workspace`）：
  - 新增并全部通过 `winrt_ext::tests::fullscreen_helpers_handle_null_hwnd_without_panic`、`pages::fullscreen::tests::{double_click_tracker_respects_window_distance_and_reset, resolve_player_hotkey_guards_typing_repeats_and_modifiers, hud_auto_hide_and_pointer_jitter_predicates}`、`pages::player::tests::transport_bars_with_fullscreen_button_fit_minimum_widths`、`shell::tests::mount_pulse_covers_half_pane_fullscreen_and_stops_when_attached`（`qaq-bx-core` 44 + 6 fixtures、`qaq-bx-plugin-login` 7、`qaq-bx-ui` 51、`qaq-bx-video` 4，共计 112 passed）。
- **集成自检**（`just selftest all`）：
  - 更新 `half-pane` 并新增第 11 项 `fullscreen` 集成场景（无视频守护、全屏进出、快捷键路由、`Back` 优先退出全屏、导航离开收口），全部 11 项场景 `[PASS]`（无 panic）。
- **UI 自动化与视觉走查**：
  - 修复并扩展 `scripts/ui-edge.ps1`（`EnumWindows` 主窗口识别、`-Scenario` 过滤及 `E10` 全屏场景），`E10` 运行通过且 `e17..e20` 截图视觉验收正常（锁屏会话下自动回退 UIA `InvokePattern` 完成进入全屏、尺寸铺满与恢复、HUD 3 秒自动隐藏验证；交互桌面下覆盖 `Esc` / 双击 / `F` 键流程）。
  - `scripts/ui-demo.ps1 -AutoExit -SkipBuild` 运行正常（`ExitCode=0`）。
- **Dev AppX 打包**（`just dev`）：
  - 生成自签名包 `package/debug/QAQ-Bx-Dev_0.2.0.2_x64.appx`（`v0.2.0.2`），确认打包目录 `target/appx-staging/debug` 不含 `credentials.json` 或登录凭据。

## 新增功能：体验补完与接线（2026-11-18 夜,`just dev` 自增修订号分发）

- **播放体验五项反馈修复**：
  - 全屏 HUD 改半透明黑底衬（α=168 纯黑,`hud_scrim_color`）+ 恒紧凑控件 + 白色前景,普通播放条仍走主题色（`pages/player.rs` `hud_*` 纯函数 + `pages/fullscreen.rs` 重排）。
  - **播放页画质调节**（F2）：`PlayerClient::resolve_full_with_cid` 全量 DASH 下发 → `quality_menu`/`select_stream_by_qn` 纯函数选流;引擎 `Load.start_secs` 定点换流（`mpv start` 直达,记忆进度与播放态）;桌面/单列/全屏三处 ComboBox 菜单（单档不渲染）。
  - **弹幕字体与字号**（F3）：`AppSettings.danmaku_font_family/danmaku_font_scale`（0.5–2.0,旧配置 serde 默认补齐）;引擎 `SetDanmakuConfig` 动态重建 DWrite 格式,行高/估宽随缩放联动;设置页字体 ComboBox + 字号滑杆。
  - **首播音画缺失根因定位与自动回退**（F4）：捆绑 libmpv 缺 AV1/AC-3 等解码器或外挂音轨拉取失败时,mpv `video-format`/`audio-codec-name` 为空 → 引擎观察属性上抛 → shell 3s 宽限探测（`missing_media_track`）→ 自动排除失败编码桶重选（记忆进度）或换次高音轨,每代次限一次;`loadfile`/`hwdec-current` 诊断日志。
  - **卡片大小曲线**（F5）：桌面列数按最小卡宽 200 DIP 推导、卡宽整 DIP 分满剩余宽（1280x800 由 4 列 236px+232px 死白 → 5 列 220px 满排）,封面高精确 16:9 取整,消除 `UniformToFill` 亚像素裁切与高 DPI 边缘毛刺;单测锁定整数对齐契约。
- **窗口拖拽卡顿修复**（P1）：引擎侧 `RESIZE_COALESCE=120ms` 合并 ResizeBuffers（拖拽期间 0 次 swapchain 重挂,静止后 1 次）;UI 侧网格 `VirtualSource` 重建去抖 150ms（`WindowResizeSettled`）,`grid_cols` 轻量字段即时更新保分页目标。
- **未接线功能接线**（U1–U4）：
  - 评论发布：`ReplyClient::add`（`x/v2/reply/add`,csrf=bili_jct,空/超长客户端先拒）+ 表单 POST 助手;composer 受控输入、发布按钮登录门控与行内错误提示。真机发帖需登录验证（本轮无凭据,挂起）。
  - 搜索默认词：`SearchClient::default_word`（WBI）;启动后台拉取,标题栏占位词与搜索页「大家在搜」一键即搜。
  - 多分 P：`pages_menu_from` → 分 P ComboBox（桌面/单列）,切换重取 playurl(cid) 定点换流,弹幕跟随 cid 重拉。
  - 直播页真实数据（guest 可用 `room/v1/Area/getRoomList`,新版 `xlive/second/getList` 无 buvid 风控 -352 不可用）：`LiveClient`（列表 + 房间 FLV 直链）、`pages/live.rs` 卡片网格 + ImageCache 封面、点击经 mpv 直播播放（`live:<roomid>` 通路,评论区降级「暂无评论区」）。
- **性能基线结论**（P2,`scripts/perf-probe.ps1` 实测）：debug 播放 290MB / 空转 130MB;release 播放 289.5MB（私有 526MB,`hwdec-current=d3d11va` 确认）。内存由映射 dll（libmpv/XAML/WinUI）与解码池主导,**debug≤150MB / release≤100MB 不可达**;已将 mpv demuxer 缓存 32+16MiB 裁至 8+4MiB（−54MB）。
- **测试**：`cargo test --workspace` 全绿（core 53 + fixtures 6 + login 7 + ui 57 + video 5,新增画质菜单/弹幕字体/轨道回退/直播解析/发评契约/卡片曲线等 20+ 用例）;`just selftest all` **12/12 PASS**（新增第 12 项 `live` 实网场景:导航→真实房间列表→点首房 mpv 直链→Back）。

## 修复:充电/付费专稿误报「playurl 无 dash」+ 首页付费稿件过滤(2026-11-18)

- **根因**(真机实证,样本 BV1zonn67E6W):充电专稿对未解锁账号 `playurl` 返回 `code=0` + `dash=null` + `durl=[试看片段]`(`result="suee"`,试看 180s vs 全片 1223s)——服务端对未解锁稿件的固定降级形态,与 fnval/清晰度权限无关;纯 DASH 播放链路把 `dash:null` 解析为 `None` 后误报「playurl 无 dash(fnval 或权限)」。`view` 的 `rights` 无任何充电标志(全 0),`copyright=3` 在普通视频普遍存在,均不可作判定;唯一服务端权威标志为 `view.is_upower_exclusive`,卡片源(rcmd/popular/related)原始响应均不携带该字段 → 卡片层静态过滤不可行,采用「检出→打标→跳过」。
- **core**(`qaq-bx-core/src/player.rs`):`PlayData` 捕获 `durl[]/timelength`;`dash_missing_reason` 纯函数区分「未解锁试看降级 / 旧格式 durl / 真异常」,文案含稳定标记 `TRIAL_DOWNGRADE_MARK`,`is_trial_downgrade_reason` 供 UI 判定;CLI `playurl` 探测复用同文案。
- **首页过滤(三层防线)**:Hover 预解析(`MediaPreResolved`)与直点(`MediaFailed`)命中试看降级 → shell `unplayable` 登记(bvid→原因,会话级)并刷新网格;已登记稿件 `PreResolveVideo` 直接跳过不再发 playurl;首页卡面叠「充电专稿」角标(`widgets/video_card.rs` `unplayable_badge`,桌面/Mobile 双形态)。
- **测试**:core 新增 `playdata_parses_trial_downgrade_shape`/`dash_missing_reason_distinguishes_trial_downgrade`(锁定文案稳定标记);selftest 新增第 13 项 `unplayable` 场景(登记/去重/非试看错误不登记);`cargo test --workspace` 全绿。
- **工程**:顺手修复 `qaq-bx-video/src/engine.rs` 工作区既有编译破损(`initial_config` move 后借用,两行顺序对调,语义不变)。
