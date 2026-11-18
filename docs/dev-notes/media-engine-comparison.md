# 系统媒体引擎(MediaPlayerElement)可行性调研——GitHub 同类客户端实证(2026-11)

> 问题:能不能换用 Windows 自带视频引擎(WinUI 的 MediaPlayerElement)?
> 方法:GitHub 检索知名 B 站客户端源码,读其播放器实现。
> 结论:**可行,且有成熟先例**;上一轮分析中"Referer 需本地代理 / DASH 分轨需 MSE 喂流"两个阻塞均有更轻的正解,工程量重估为 1~2 周(原估 2~4 周)。

## 1. 标杆:Richasy/Bili.Uwp(8066★,archived)

读到的关键实现(`src/ViewModels/ViewModels.Uwp/Core/`):

- **UI 全部用 `MediaPlayerElement`**(BiliMediaPlayer.xaml,4 处形态复用),不用 mpv。
- **双引擎 ViewModel**:
  - `INativePlayerViewModel`(系统引擎,默认)
  - `IFFmpegPlayerViewModel`(FFmpegInteropX 兜底,处理系统解不了的流/直播)
- **Native 路线(正解核心)**:
  1. 自带 Referer 头的 HttpClient 拉 B 站 DASH JSON;
  2. 用仓库内置模板 `src/App/Assets/DashVideoTemplate.xml`(**标准 MPD**,video/audio 两个 AdaptationSet)把 DASH JSON 填成合法 MPD——**分轨问题由 DASH manifest 表达,系统解析器自己拉两条流,不需要 MSE 喂流、不需要本地代理**;
  3. `AdaptiveMediaSource.CreateFromStreamAsync(mpdStream, baseUrl, "application/dash+xml", 带头HttpClient)`(`NativePlayerViewModel.Methods.cs:67`),`DownloadRequested` 事件逐段补头/改 URL,`AllSegmentsIndependent=true`;
  4. `MediaSource.CreateFromAdaptiveMediaSource` → 塞给 MPE。
- **FFmpeg 兜底路线**:`HttpRandomAccessStream`(带头 HTTP range 流)→ FFmpegMediaSource;video/audio 各自 `CreateMediaPlaybackItem` 分开播放器同步;直播 `CreateFromUriAsync` 直吃 FLV。
- **Referer 注入**:`httpClient.DefaultRequestHeaders.Add("Referer", "https://www.bilibili.com")`(Methods.cs:26)。

## 2. 其他同类

- **xiaoyaocz/biliuwp**(哔哩 UWP):`fnval=16` DASH 与 MP4 双模式(PlayerAPI.cs),UWP + MPE 体系。
- **GreenShadeZhang/BiliBili-WinUI**(28★):Bili.Uwp 的 WinUI3 移植尝试(GPL,久未维护)——证明 UWP→WinUI3 迁移无原则性障碍。
- **soda-rx/Bili_Play**(15★,WinUI3 哔哩音乐客户端)。

## 3. 对本项目的迁移蓝图(修正版)

| 项 | 结论 |
|---|---|
| DASH 分轨 | **MPD 模板生成**(抄 Bili.Uwp 模式,~1 天),系统 DASH 解析器拉分轨,无需 MSE |
| Referer/UA | `AdaptiveMediaSource` 注入带头 HttpClient + `DownloadRequested` 补头,无需本地代理 |
| UI | reactor fork 补 `MediaPlayerElement` handle/imperative 绑定(~1-2 天) |
| 引擎层 | 新增 `NativePlayer` 与现有 mpv `MpvPlayer` 并列,**双引擎**:native 优先,mpv 兜底(现有管线零废弃) |
| 直播 FLV | native 不支持;直播页走 mpv(现状保留),或试验 HLS |
| 弹幕 | 改为独立透明 Canvas 层叠在 MPE 上(video-channel.md §2 原设计) |
| SMTC | MediaPlayer 自带,smtc.rs 简化 |
| 包体 | 去掉 libmpv-2.dll(117MB,appx 81MB 的大头);若保留 mpv 兜底则按需下载/可选组件 |
| 帧访问 | mpv 直渲 backbuffer → MPE 需 `VideoFrame` 截图等 API(截图功能需重验) |

## 4. 风险

- Rust 侧 WinRT 绑定需在 fork 开 `Media_Streaming`/`Media_Playback` feature(fork feature 树按 SDK 头名生成,应齐)。
- MPE 在 WinUI3 的实际表现(合成/伸缩)需 MVP 实测——Bili.Uwp 是 UWP,WinUI3 移植仍需自证。
- 番剧/影视区冷门编码:系统解码器覆盖不如 mpv,双引擎兜底保底。
- seek/换流(`SelectStreamByQn` 定点换流)在 AdaptiveMediaSource 下 = 重建 source 带位置,需对齐现有体验。

## 5. 建议路径

1. **Spike(MVP,~3 天)**:fork 加 MPE 绑定;临时页面挂 MPE;MPD 模板 + AdaptiveMediaSource 播一个真实 BV 的 1080p DASH;验证伸缩/seek/画质切换。
2. MVP 通过 → 按 §3 蓝图正式迁移,native 为主引擎、mpv 降级为兜底(直播/解不了的流)。
3. Spike 不通过 → 维持 mpv(黑屏类问题已修复且有 E11 回归)。

## 6. Spike 实测结果(2026-11,分支 feat/v0.3-native-player)——**MVP 成立**

全链(Rust + fork windows crate + reactor MPE)跑通,`scripts/ui-edge.ps1 -Scenario E12` 可重复验证:

- **fork 生成管线**:schema.toml `MediaPlayerElement(reference)` + bindings.txt 成员清单
  (`IMediaPlayerElement::{get/put_Source, get/put_AutoPlay, get_MediaPlayer}` +
  `IMediaPlayerElement::CreateInstance` + `Windows::Media::Playback::IMediaPlayer::{}`)
  → `cargo run -p tool-reactor` 自动出绑定,vtable 中间成员 `usize` 占位,ABI 正确。
- **WinUI3 MPE 关键语义(踩坑实录)**:MPE 的 `MediaPlayer` 属性**只读**,且未设
  `Source` 前为 null——外部引擎无法先拿 player。正解 = **原子两步 imperative**
  `SetMediaSourceGetPlayer`:先 `SetSource(引擎产出的 MediaSource)`(迫使 MPE 建
  player),同一请求里读回 player 交给引擎接管。协议:引擎 `build_source`(MPD→
  MediaSource,阻塞 join)→ pending 槽 → `NativeSurface` 每拍 `take_pending_source_raw`
  → request → completion 交回 player → `attach_player`(关 AutoPlay、订
  Ended/Failed、应用 pending 的 autoplay/start)。
- **B 站 JSON 字段坑(实测锚)**:dash 条目**同键双形**——`SegmentBase`(Pascal,带
  真值)/`segment_base`(snake,非对象形态)、`mimeType`/`mime_type`、
  `baseUrl`/`base_url` 并存。serde 用 `rename="SegmentBase" + alias` + 宽容
  `seg_base_loose`(非法形态→None,杜绝解码炸)。
- **播放实证**:`native heartbeat pos/dur` 锚,dur=212.3s(与 MV 时长一致,双轨
  拉流生效);pos 持续推进;30 次快速伸缩(55%↔100%)后无卡死;MediaFailed 零。
- **Referer 生效**:直接吃 CDN 签名 m4s URL 的 `CreateFromStreamWithDownloaderAsync`
  + `DefaultRequestHeaders.SetReferer` + `DownloadRequested` 音频重定向,全链成功
  (guest 480P 实测;1080P 需登录态复测)。
- **E11(mpv 路径)回归**:亮度 123–127 正常,互不干扰。

### spike 已知限制(正式版待办)

1. `build_source` 在 UI 线程阻塞 join(网络慢时卡 UI)→ 正式版移工作线程。✅M1
2. live(FLV)未接:native 不支持,直播页仍走 mpv(现状保留)。✅保留不改
3. 弹幕层:native 模式下 mpv 引擎 idle,弹幕 overlay 不出(需独立透明 Canvas 层,见 §3)。✅M4
4. SMTC 未切:MPE/MediaPlayer 自带 SystemTransportControls 可替换 smtc.rs 手写桥。✅M5
5. 登录态 1080P/画质切换真机复测;截图像素检测在 TOPMOST 竞争下不稳定(E12 断言以 stderr 锚点为准)。✅M6

## 7. 正式化实测(2026-11-18,分支 feat/v0.3-native-player)

四项待办全部落地并真机验收,逐条证据见 `docs/Update-0.3.0.md` M 段。这里只记
**踩过的坑与判据**,避免后续会话重新踩:

1. **同一份 `MediaSource` 不能绑两个 `MediaPlayer`。** 窗口伸缩会让播放页组件树
   重建(与黑屏那次同源,见 `resize-black-screen.md`),两个 `NativeSurface` 实例
   各自有自己的"在途"标志却共用同一份产物 ⇒ 第二次 `SetSource` 触发
   `MediaFailed(MediaPlayerError::SourceNotSupported)`,UI 显示「视频通道异常」。
   这个错**只在真机 GUI 复现**(selftest 无 UI 不触发),而且**时隐时现**,一开始
   很容易误判成"移到工作线程把套间搞坏了"。判据要盯 stderr 的
   `native media opened` 是否出现(出现=源真被管线接受)。
   ⇒ 修复:注入权全局单飞(`LoadLedger::claim/release`),面板重建一律换**新产物**。
2. **`MediaPlayerElement.MediaPlayer` 只读**未变;`MediaSource` 一旦被某元素绑走,
   该元素销毁后**不要指望**新元素复用同一份源(第 1 条的同一条约束)。
3. **音量/起播时机不能在装载前落**:`attach_player` 之前 `SetVolume` 无对象。
   引擎侧改为"期望音量记账 + attach 统一施加";`want_play` 记账同理避免
   「静音旧媒体」把用户播放意愿冲掉(否则面板重建后续播变成暂停)。
4. **WinUI ComboBox 的下拉项只有 Expand 之后才进 UIA 树**——写自动化点击测试
   (画质切换)时必须先 `ExpandCollapsePattern.Expand()` 再找 ListItem,否则
   `FindAll(ListItem)` 返回空,以为"没有画质菜单"。
5. **单元线程里不要 `CoInitializeEx`**:一个需要 COM 的单测与并发的 D3D/D2D
   线程同进程跑,测试二进制以 `STATUS_ACCESS_VIOLATION` 崩在退出阶段(与断言无关,
   且只在 `--test-threads` 并发时复现)。这类"要套间"的路径改由 E12/E13 真机场景覆盖。
6. **诊断锚(保留,勿删)**:`native load qn=… codec=… w=… h=…`、
   `native build_source ok(seq=…)`、`native attach:`、`native media opened`、
   `native media failed: error=… extended=0x…`(`ExtendedErrorCode` 才是 MF 的真错)、
   `native surface detached/remount`、`smtc native: 控件已绑定/status=`、
   `STAGE-A/STAGE-B`。E12/E13 的断言全部以这些锚点为准,不用像素。


