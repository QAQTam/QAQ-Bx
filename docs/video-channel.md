# 视频嵌入通道调研定案（mpv D3D11 render API ↔ SwapChainPanel）

> 结论先行：**可行，链路已全部核实**。libmpv 自 2024 年起提供原生 D3D11 render API
> （`MPV_RENDER_API_TYPE_D3D11`，`include/mpv/render_d3d11.h`），mpv 可直接把视频帧渲染进
> 我们创建的 `ID3D11Texture2D`（swapchain backbuffer）；reactor 的
> `ElementRef<SwapChainPanel>::request_set_swap_chain(IUnknown)` 内部 cast 成 `IDXGISwapChain`
> 调 `ISwapChainPanelNative::SetSwapChain`（`native/bindings.rs:16191`、`native/winui/adapter.rs:594`）。
> 两端无缝对接，GPU 零拷贝，硬解走同一设备上的 d3d11va。

## 1. 证据链（本轮核实）

| 环节 | 证据 |
|---|---|
| mpv 支持 D3D11 render | `include/mpv/render.h`（master）：`Supported backends: Direct3D 11 via MPV_RENDER_API_TYPE_D3D11, see render_d3d11.h` |
| D3D11 render 契约 | `render_d3d11.h`：`mpv_d3d11_init_params{device}`（我们传 ID3D11Device）；`mpv_render_context_render(MPV_RENDER_PARAM_D3D11_FBO{tex})` 渲染进调用方拥有的 `ID3D11Texture2D`（RENDER_TARGET bind + USAGE_DEFAULT，BGRA8/R8G8B8A8/R10G10B10A2/R16G16B16A16，**sRGB 不支持**）；present 由调用方负责（官方原文："for example, via a DXGI swap chain"） |
| 硬解 | `d3d11va` 直接在传入 device 上解码（解码线程）——device **不能**带 `D3D11_CREATE_DEVICE_SINGLETHREADED`（否则只支持 d3d11va-copy）；RX560（UVD 6.3）支持 H.264/HEVC(含 Main10) d3d11va |
| 线程模型 | D3D11 后端无 per-thread context，任意线程可调 `mpv_render_*`；渲染期间其它线程不得动 immediate context |
| reactor 挂载 | `request_set_swap_chain(IUnknown)` → `ImperativeRequest::SetSwapChain` → panel.cast::<ISwapChainPanelNative>().SetSwapChain(cast::<IDXGISwapChain>) → 返回 Result 回调 |
| 尺寸/DPI 事件 | `ElementRef<SwapChainPanel>::observe_surface_metrics` / `SwapChainPanelEvent::Metrics{width,height,scale_x,scale_y}` + `request_surface_frame` |

## 2. 数据流设计（正式实现蓝图）

```
[UI 线程 / PlayerPage]
  SwapChainPanel（keyed 挂在视频面 Grid）
    ├─ ElementRef<SwapChainPanel> 存入 PlayerVideo 组件
    ├─ observe_surface_metrics → Message::VideoSurfaceMetrics(w,h,scale)
    └─ request_set_swap_chain(IUnknown=IDXGISwapChain1)  ← 首次挂载
[渲染线程] VideoRenderer（独立线程， Owns Device）
  ID3D11Device（BGRA 支持 + 无 SINGLETHREADED）
  IDXGISwapChain2：DXGI_FORMAT_B8G8R8A8_UNORM，BIND_RENDER_TARGET，
                   SWAP_EFFECT_FLIP_SEQUENTIAL/DISCARD，buffer≥2，SCALING_STRETCH
  libmpv：
    mpv_create() → set options：
      hwdec=d3d11va, ao=wasapi, cache=yes,
      http-header-fields="Referer: https://www.bilibili.com"（+User-Agent）
    mpv_render_context_create(API_TYPE_D3D11, device)
    mpv_observe_property(time-pos/duration/pause/eof-reached) → channel → AppShell Message
  循环（update callback 驱动）：
    mpv_render_context_render(fbo = swapchain backbuffer)
    Present(sync_interval=1)
  resize：Metrics 变化 → ResizeBuffers → 重新挂 swapchain
[UI 状态回注]
  AppShell 收到 position/duration/pause → 驱动进度条/迷你条；
  播放/暂停/seek/volume → channel → mpv property（pause/speed/volume 或 seek command）
```

加载 DASH 分轨：`loadfile(video.m4s)` 后 `audio-files-add(audio.m4s)`（或 loadfile 时的
`options` 表传 `audio-files`）；二者均带防盗链头。B 站 302/鉴权 URL 有时效（约 2.5h），
续播前重新调 playurl。

## 3. 绑定与依赖

- **FFI 最小集**（建议手写，~15 函数）：mpv_create/mpv_initialize/mpv_set_option_string/
  mpv_command/mpv_observe_property/mpv_wait_event/mpv_terminate_destroy +
  mpv_render_context_create/free/render/set_update_context…（client.h/render.h/render_d3d11.h）。
  crates.io 的 `libmpv-rs`（0.9）依赖其自身 fork 的 libmpv-sys 且默认不带 render_d3d11 新 API，
  不如手写可控。声明文件放 `crates/qaq-bx-video/src/ffi.rs`（新 crate，下阶段建）。
- **libmpv 二进制**：mpv 官方 GitHub releases（libmpv.7z, x86_64）随包分发 `libmpv-2.dll`，
  运行时放 exe 旁。**版本要求**：D3D11 render 为 2024 年 master 引入，发布版最低版本
  **待实测确认（候选 mpv 0.40+）**；若发布版无 D3D11 API，保底顺序见 §5。
- 弹幕覆盖层：windows-canvas 组件（D2D 自管设备与交换链）叠在视频 panel 之上
  （Grid 同 cell 两层）——与本通道互不干扰。

## 4. 版本/兼容约束（写死）

- SwapChainPanel 挂载的 swapchain 必须：flip-model（FLIP_SEQUENTIAL/FLIP_DISCARD）、
  ≥2 buffer、B8G8R8A8_UNORM、SCALING_STRETCH —— MediaPlayerElement 同机制，XAML 合成器原生支持。
- backbuffer 纹理即 mpv 渲染目标：BGRA8（sRGB 不支持）→ 恰好同时满足两方要求。
- mpv 渲染不改 our state：每次 render 后如需自绘（OSD 层）要重设状态；骨架期不自绘，无影响。
- 交换链重建（ResizeBuffers）期间 UI 线程与渲染线程以 channel 序列化操作。
- **backbuffer COM 引用计数（实测踩坑，2026-11）**：`with_backbuffer` 里 `GetBuffer(0)`
  的 +1 引用由外层 wrapper 析构释放一次；闭包内取 raw 必须**借用**
  （`from_raw_borrowed`）再 `cast::<ID3D11Texture2D>()`（QI 引用归 cast 结果所有、用完即释）。
  若闭包内用 `from_raw` 接管再 drop，会与外层形成**双重 Release**：计数被打穿后首次
  `ResizeBuffers` 即报 `DXGI_ERROR_INVALID_CALL (0x887A0001)`（表面看是 resize 坏了，
   根因在渲染路径）。统一后的 fork `windows-core` 中 `from_raw` 语义为接管所有权、
   **不 AddRef**。

## 5. 保底顺序（若 D3D11 render API 不可用）

1. **OpenGL render API + WGL 不可见窗口 → 读回 GL 纹理 → CopyResource/直拷到 BGRA swapchain**
   （多一次 GPU 拷贝，无 CPU 参与）——GL 设备与 D3D11 互操作需 nvog/CopyResource 路线，
   复杂度高于备选 2，仅在前两者都失败时考虑。
2. **MPV_RENDER_API_TYPE_SW**：CPU 渲染 BGRA → 每帧 memcpy 进 backbuffer 的 staging →
   UpdateSubresource。1080p30 在 E3-1230 V2 可承受；1080p60 勉强。适合先打通链路
   （不依赖 mpv 版本，任何 libmpv 都有）——**建议下阶段第一支 sample 先用 SW 通路验证
   SwapChainPanel 挂载，再切 D3D11**。
3. mpv 子进程 `--wid=<HWND>`：WinUI3 XAML HWND 内嵌子 HWND 存在合成层级/焦点坑，
   仅作最后手段（弹幕覆盖会被盖住）。

## 6. 下一步（新会话任务清单）

1. 建 `crates/qaq-bx-video`：ffi.rs（libmpv-2.dll 动态加载：Delay-load 或运行时 LoadLibrary +
   GetProcAddress，避免硬链接）→ SW 渲染 sample：本地 mp4 → SwapChainPanel 出图。
2. 换 D3D11 render API + flip swapchain + d3d11va 硬解 sample。
3. playurl → loadfile 串接（qaq-bx-core 补 playurl 客户端：WBI 签名已在 research 定案），
   Referer 头 + audio-files 外挂音轨。
4. mpv_observe_property → AppShell Message 桥（播放状态单源改造：Shell 的模拟 timer 换成
   mpv time-pos 回传）。
5. 播放页视频面从占位 Border 换成 SwapChainPanel + canvas 弹幕层。

## 7. 本轮未验证项（诚实清单）

- 未下载/运行 libmpv：D3D11 render API 的**发布版最低版本**未实测（master 头文件已核实存在）。
- 未实测 flip-model swapchain 经 `request_set_swap_chain` 挂载后的呈现（机制同 MPE，理论无坑，
  需 sample 确认）。
- 未实测 B 站 m4s 经 mpv http 播放（Referer 头生效性）——research 阶段已验证 m4s 可直接拉流
  （requests 带 Referer 返回 200），mpv 侧仅是头部透传。
