//! 视频引擎:UI ↔ 渲染线程桥(SwapChainPanel 挂载 + mpv 播放)。
//!
//! 线程模型:引擎线程常驻,独占 device + composition swapchain + mpv 实例
//! (`mpv_render_*` 任意线程合法,单线程串行最简)。UI 侧仅经 `Arc<VideoEngine>`
//! 交互:命令走 channel(非阻塞),状态经 `Arc<Mutex<PlaybackStatus>>` 快照读。
//!
//! swapchain 挂载必须在 UI 线程(`ISwapChainPanelNative::SetSwapChain` 经
//! reactor `request_set_swap_chain` 排队):引擎线程创建完 swapchain 放入
//! `pending` 槽,UI(`VideoSurface`)取出并挂载,completion 回写 `attached`;
//! 面板卸载(路由切走)时 UI 发 `Detach`,引擎把 swapchain 放回待挂载槽。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::Interface;
use windows::Win32::d3d11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows_canvas::{ColorF, GpuDevice, Rect, SwapChain};
use windows_text::TextFormat;

use crate::ffi::{mpv_d3d11_fbo, MpvLib, MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG};
use crate::player::{MpvEvent, MpvPlayer, PropertyValue};

/// 与 `bili-core::wbi::BROWSER_UA` 保持一致(本 crate 不依赖 bili-core,m4s
/// 防盗链:无 Referer 403 实测锚 `docs/bilibili-api-inventory.md`)。
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
/// m4s 分轨请求头(loadfile 全局设置,分轨同效)。
const BILI_HEADERS: &[(&str, &str)] =
    &[("Referer", "https://www.bilibili.com/"), ("User-Agent", BROWSER_UA)];

/// 初始 swapchain 尺寸(挂载前占位;首个 Metrics 到达即 ResizeBuffers)。
const INITIAL_WIDTH: u32 = 1280;
const INITIAL_HEIGHT: u32 = 720;
/// 渲染节拍 = 命令轮询间隔(25fps 视频远够;节拍内 Present(1) 对齐 vsync)。
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// 待挂载 swapchain 的原始 COM 指针:引擎侧 `IUnknown::into_raw` 转移所有权,
/// UI 侧 `IUnknown::from_raw` 接管(接手方 Drop 正确 Release,引用计数闭合)。
/// 两侧现同用 fork 的 windows-core 0.101;仍走裸指针是为跨线程传递 +
/// 与 canvas/reactor 各自的 bindings 类型解耦,不再是版本不互通。
struct SendRaw(*mut c_void);
unsafe impl Send for SendRaw {}
unsafe impl Sync for SendRaw {}

/// 播放状态快照(引擎事件线程写,UI 节拍读)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackStatus {
    /// 当前播放位置(秒;mpv time-pos)。
    pub time_pos: f64,
    /// 媒体总长(秒;0 = 尚未加载)。
    pub duration: f64,
    /// mpv pause 属性。
    pub pause: bool,
    /// 播放到末尾(keep-open 停在末帧)。
    pub eof: bool,
}

impl PlaybackStatus {
    /// 引擎是否已持有媒体(duration 或位置非零)。
    pub fn has_media(&self) -> bool {
        self.duration > 0.0 || self.time_pos > 0.0
    }
}

/// 引擎初始化与运行时可调配置（硬件解码 + 弹幕叠画参数）。
#[derive(Debug, Clone, PartialEq)]
pub struct VideoConfig {
    pub hardware_decode: bool,
    pub danmaku_enabled: bool,
    pub danmaku_opacity: f32,
    pub danmaku_max_lanes: usize,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            hardware_decode: true,
            danmaku_enabled: true,
            danmaku_opacity: 0.92,
            danmaku_max_lanes: ROLL_LANES,
        }
    }
}

/// UI → 引擎线程命令。
pub enum EngineCmd {
    /// 加载分轨(B 站头固定在引擎侧;audio_url=None 纯视频)。
    Load { video_url: String, audio_url: Option<String>, autoplay: bool },
    /// 全片弹幕(按 progress 排序;引擎 D2D overlay 每帧读取)。
    SetDanmaku { items: Vec<OverlayDanmaku> },
    /// 运行时更新弹幕开关与绘制参数。
    SetDanmakuConfig { enabled: bool, opacity: f32, max_lanes: usize },
    /// 运行时切换硬解（d3d11va / no）。
    SetHardwareDecode(bool),
    Play,
    Pause,
    Stop,
    Seek { secs: f64 },
    Volume { pct: f64 },
    /// 面板物理像素尺寸变化(0 视为面板未就绪,忽略)。
    Resize { width: u32, height: u32 },
    /// 面板卸载:attached 复位,swapchain 放回待挂载槽(重进播放页再挂)。
    Detach,
    /// mpv update_callback 唤醒（新帧到达待合成）。
    FrameDirty,
    Quit,
}

/// 弹幕 overlay 条目(bili-core DanmakuElem 的引擎侧镜像,避免跨 crate 依赖)。
#[derive(Debug, Clone)]
pub struct OverlayDanmaku {
    /// 出现时刻(毫秒)。
    pub progress_ms: i32,
    /// 1=滚动 4=底部 5=顶部。
    pub mode: i32,
    pub fontsize: i32,
    /// 0xRRGGBB。
    pub color: u32,
    pub content: String,
}

/// 视频引擎句柄(线程安全;UI 侧克隆共享)。
pub struct VideoEngine {
    cmd_tx: Sender<EngineCmd>,
    /// 引擎线程产出的待挂载 swapchain(UI 线程取走挂载;失败放回)。
    pending: Arc<Mutex<Option<SendRaw>>>,
    attached: Arc<AtomicBool>,
    status: Arc<Mutex<PlaybackStatus>>,
    error: Arc<Mutex<Option<String>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

// UI 只比较句柄同一性(reactor Input 约束需要 PartialEq)。
impl PartialEq for VideoEngine {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pending, &other.pending)
    }
}

impl VideoEngine {
    /// 创建引擎并启动线程（默认配置）。
    pub fn new() -> Arc<Self> {
        Self::with_config(VideoConfig::default())
    }

    /// 带初始配置创建引擎并启动线程。线程失败(dll 缺失/设备不可用)记入
    /// [`VideoEngine::error`],UI 显示提示,不 panic。
    pub fn with_config(config: VideoConfig) -> Arc<Self> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let wakeup_tx = cmd_tx.clone();
        let pending = Arc::new(Mutex::new(None));
        let attached = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(PlaybackStatus::default()));
        let error = Arc::new(Mutex::new(None));
        let thread = {
            let pending = pending.clone();
            let attached = attached.clone();
            let status = status.clone();
            let error = error.clone();
            std::thread::Builder::new()
                .name("video-engine".into())
                .spawn(move || {
                    engine_thread(wakeup_tx, cmd_rx, pending, attached, status, error, config)
                })
                .ok()
        };
        Arc::new(Self {
            cmd_tx,
            pending,
            attached,
            status,
            error,
            thread: Mutex::new(thread),
        })
    }

    // ---- UI → 引擎命令(发送失败仅线程已死,静默) ----

    /// 加载 B 站分轨(loadfile replace;默认自动播放)。
    pub fn load(&self, video_url: &str, audio_url: Option<&str>) {
        self.load_with_options(video_url, audio_url, true);
    }

    /// 加载 B 站分轨并指定是否自动播放。
    pub fn load_with_options(&self, video_url: &str, audio_url: Option<&str>, autoplay: bool) {
        _ = self.cmd_tx.send(EngineCmd::Load {
            video_url: video_url.to_string(),
            audio_url: audio_url.map(str::to_string),
            autoplay,
        });
    }

    /// 全片弹幕(SetDanmaku;排序由调用方保证)。
    pub fn set_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmaku { items });
    }

    /// 动态调整弹幕开关、不透明度与最大轨道数。
    pub fn set_danmaku_config(&self, enabled: bool, opacity: f32, max_lanes: usize) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmakuConfig {
            enabled,
            opacity: opacity.clamp(0.1, 1.0),
            max_lanes: max_lanes.clamp(1, 20),
        });
    }

    /// 动态切换硬件解码（d3d11va / no）。
    pub fn set_hardware_decode(&self, enabled: bool) {
        _ = self.cmd_tx.send(EngineCmd::SetHardwareDecode(enabled));
    }

    pub fn play(&self) {
        _ = self.cmd_tx.send(EngineCmd::Play);
    }

    pub fn pause(&self) {
        _ = self.cmd_tx.send(EngineCmd::Pause);
    }

    pub fn stop(&self) {
        _ = self.cmd_tx.send(EngineCmd::Stop);
    }

    pub fn seek(&self, secs: f64) {
        _ = self.cmd_tx.send(EngineCmd::Seek { secs });
    }

    pub fn set_volume(&self, pct: f64) {
        _ = self.cmd_tx.send(EngineCmd::Volume { pct });
    }

    /// 面板物理像素尺寸上报(DIP × composition scale)。
    pub fn set_metrics(&self, dip_width: f64, dip_height: f64, scale_x: f32) {
        let width = (dip_width * f64::from(scale_x.max(0.01))).round().max(0.0) as u32;
        let height = (dip_height * f64::from(scale_x.max(0.01))).round().max(0.0) as u32;
        if width == 0 || height == 0 {
            return;
        }
        _ = self.cmd_tx.send(EngineCmd::Resize { width, height });
    }

    /// 面板卸载(VideoSurface cleanup)。
    pub fn detach_panel(&self) {
        _ = self.cmd_tx.send(EngineCmd::Detach);
    }

    // ---- 挂载协调(UI 线程专用) ----

    /// 取走待挂载 swapchain 原始指针(接管所有权;无则 None:引擎尚未创建
    /// 完成或已挂载)。UI 侧经 `IUnknown::from_raw` 接管。
    pub fn take_pending(&self) -> Option<*mut c_void> {
        let chain = self.pending.lock().ok()?.take()?;
        Some(chain.0)
    }

    /// 挂载请求入队失败(面板尚未实例化):放回指针(接管所有权),下轮再试。
    pub fn put_back_pending(&self, raw: *mut c_void) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some(SendRaw(raw));
        }
    }

    /// 挂载 completion 回写(成功 → attached + 唤醒渲染首帧;失败 → 记错误)。
    pub fn mark_attached(&self, ok: bool) {
        if ok {
            self.attached.store(true, Ordering::SeqCst);
            _ = self.cmd_tx.send(EngineCmd::FrameDirty);
        } else {
            self.set_error("swapchain 挂载失败(SetSwapChain)");
        }
    }

    pub fn attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
    }

    // ---- 状态读 ----

    /// 播放状态快照。
    pub fn status(&self) -> PlaybackStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 引擎级错误(dll/设备/挂载;UI 显示一次)。
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok()?.clone()
    }

    fn set_error(&self, message: &str) {
        if let Ok(mut slot) = self.error.lock() {
            *slot = Some(message.to_string());
        }
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        _ = self.cmd_tx.send(EngineCmd::Quit);
        if let Ok(mut guard) = self.thread.lock() {
            if let Some(thread) = guard.take() {
                let _ = thread.join();
            }
        }
    }
}

// ---------- 引擎线程 ----------

struct DanmakuFonts {
    small: TextFormat,
    medium: TextFormat,
    large: TextFormat,
}

impl DanmakuFonts {
    fn new() -> Self {
        Self {
            small: TextFormat::new("Noto Sans SC", 16.0),
            medium: TextFormat::new("Noto Sans SC", 22.0),
            large: TextFormat::new("Noto Sans SC", 30.0),
        }
    }
}

unsafe extern "C" fn on_mpv_frame_dirty(ctx: *mut c_void) {
    if !ctx.is_null() {
        let tx = unsafe { &*(ctx as *const Sender<EngineCmd>) };
        let _ = tx.send(EngineCmd::FrameDirty);
    }
}

/// 引擎线程主体:环境搭建(失败记 error 退出)→ 事件阻塞 + 帧回调驱动循环。
fn engine_thread(
    wakeup_tx: Sender<EngineCmd>,
    cmd_rx: Receiver<EngineCmd>,
    pending: Arc<Mutex<Option<SendRaw>>>,
    attached: Arc<AtomicBool>,
    status: Arc<Mutex<PlaybackStatus>>,
    error: Arc<Mutex<Option<String>>>,
    initial_config: VideoConfig,
) {
    let report_error = |message: String| {
        if let Ok(mut slot) = error.lock() {
            *slot = Some(message);
        }
    };

    // 1) libmpv(dll 缺失即不可用,记录后退出线程)。
    let lib = match MpvLib::load() {
        Ok(lib) => lib,
        Err(e) => {
            report_error(format!("libmpv 加载失败:{e}"));
            return;
        }
    };

    // 2) D3D11 device(BGRA、非 SINGLETHREADED → d3d11va 可用)→ canvas
    //    GpuDevice 包裹(同 device 的 D2D 弹幕 overlay)→ composition swapchain。
    let mut surface = match create_surface() {
        Ok(surface) => surface,
        Err(e) => {
            report_error(format!("D3D11 surface 创建失败:{e}"));
            return;
        }
    };
    eprintln!("ENG-B2: surface ok");
    // 3) swapchain 交付 UI 挂载(UI 线程 SetSwapChain)。
    match surface.chain.into_unknown_raw() {
        Ok(raw) => {
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(SendRaw(raw));
            }
        }
        Err(e) => {
            report_error(format!("swapchain → IUnknown 失败:{e}"));
            return;
        }
    }

    // 4) mpv 实例 + D3D11 渲染上下文(hardware 解码;系统默认音频输出)。
    let mut player = match MpvPlayer::new(lib) {
        Ok(player) => player,
        Err(e) => {
            report_error(format!("mpv 初始化失败:{e}"));
            return;
        }
    };
    let hwdec = if initial_config.hardware_decode { "d3d11va" } else { "no" };
    if let Err(e) = player.set_options(&[("hwdec", hwdec), ("terminal", "no")]) {
        report_error(format!("mpv 选项失败:{e}"));
        return;
    }
    if let Err(e) = player.create_render_context_d3d11(&crate::ffi::mpv_d3d11_init_params {
        device: surface.gpu.d3d_device_raw(),
    }) {
        report_error(format!("mpv D3D11 渲染上下文失败:{e}"));
        return;
    }
    eprintln!("ENG-E: mpv render ctx ok");

    // 5) 注册 mpv render update callback（新帧到位即精确唤醒渲染线程，取代空转轮询）。
    let wakeup_ptr = Box::into_raw(Box::new(wakeup_tx));
    if let Err(e) = player.set_update_callback(Some(on_mpv_frame_dirty), wakeup_ptr.cast()) {
        report_error(format!("mpv 渲染回调失败:{e}"));
        drop(player);
        unsafe { drop(Box::from_raw(wakeup_ptr)) };
        return;
    }

    // 6) 属性观察 → 状态快照(time-pos/duration/pause/eof)。
    if let Err(e) = observe_status(&mut player, status.clone()) {
        report_error(format!("mpv 属性观察失败:{e}"));
        let _ = player.set_update_callback(None, std::ptr::null_mut());
        drop(player);
        unsafe { drop(Box::from_raw(wakeup_ptr)) };
        return;
    }

    // 7) 渲染循环：未挂载或暂停静止时阻塞等待指令/新帧回调（0 空转）；
    //    开弹幕播放态按 FRAME_INTERVAL 平滑叠画滚动弹幕，关弹幕仅随 mpv 新帧刷新。
    let fonts = DanmakuFonts::new();
    let mut width = INITIAL_WIDTH;
    let mut height = INITIAL_HEIGHT;
    let mut need_repaint = true;
    let mut mpv_dirty = true;
    let mut overlay: Vec<OverlayDanmaku> = Vec::new();
    let mut cfg = initial_config;
    let mut first_pending: Option<EngineCmd> = None;
    'main: loop {
        // 命令段：若有已取出的首条指令先消费，再一口气排空。
        loop {
            let next_cmd = match first_pending.take() {
                Some(cmd) => Ok(cmd),
                None => cmd_rx.try_recv(),
            };
            match next_cmd {
                Ok(cmd) => match cmd {
                    EngineCmd::Load { video_url, audio_url, autoplay } => {
                        if let Ok(mut s) = status.lock() {
                            *s = PlaybackStatus::default();
                        }
                        let pause_val = if autoplay { "no" } else { "yes" };
                        if let Err(e) = player.set_options(&[("pause", pause_val)]) {
                            report_error(format!("mpv pause 选项失败:{e}"));
                        }
                        if let Err(e) =
                            player.loadfile(&video_url, audio_url.as_deref(), BILI_HEADERS)
                        {
                            report_error(format!("loadfile 失败:{e}"));
                        }
                        need_repaint = true;
                        mpv_dirty = true;
                    }
                    EngineCmd::SetDanmaku { items } => {
                        overlay = items;
                        need_repaint = true;
                    }
                    EngineCmd::SetDanmakuConfig { enabled, opacity, max_lanes } => {
                        cfg.danmaku_enabled = enabled;
                        cfg.danmaku_opacity = opacity.clamp(0.1, 1.0);
                        cfg.danmaku_max_lanes = max_lanes.clamp(1, 20);
                        need_repaint = true;
                    }
                    EngineCmd::SetHardwareDecode(enabled) => {
                        cfg.hardware_decode = enabled;
                        let hw = if enabled { "d3d11va" } else { "no" };
                        if let Err(e) = player.set_options(&[("hwdec", hw)]) {
                            report_error(format!("切换硬解({hw})失败:{e}"));
                        }
                        need_repaint = true;
                    }
                    EngineCmd::Play => {
                        _ = player.play();
                        need_repaint = true;
                    }
                    EngineCmd::Pause => {
                        _ = player.pause();
                        need_repaint = true;
                    }
                    EngineCmd::Stop => {
                        _ = player.stop();
                        overlay.clear();
                        overlay.shrink_to_fit();
                        if let Ok(mut s) = status.lock() {
                            *s = PlaybackStatus::default();
                        }
                        need_repaint = true;
                        mpv_dirty = true;
                    }
                    EngineCmd::Seek { secs } => {
                        _ = player.seek(secs.max(0.0));
                        need_repaint = true;
                        mpv_dirty = true;
                    }
                    EngineCmd::Volume { pct } => {
                        _ = player.set_volume(pct.clamp(0.0, 100.0));
                    }
                    EngineCmd::Resize { width: w, height: h } => {
                        if (w, h) != (width, height) {
                            if let Err(e) = surface.chain.resize(w, h) {
                                report_error(format!("ResizeBuffers({w}x{h}) 失败:{e}"));
                            } else {
                                width = w;
                                height = h;
                                need_repaint = true;
                                attached.store(false, Ordering::SeqCst);
                                if let Ok(raw) = surface.chain.into_unknown_raw()
                                    && let Ok(mut slot) = pending.lock()
                                {
                                    *slot = Some(SendRaw(raw));
                                }
                            }
                        }
                    }
                    EngineCmd::Detach => {
                        attached.store(false, Ordering::SeqCst);
                        if let Ok(raw) = surface.chain.into_unknown_raw()
                            && let Ok(mut slot) = pending.lock()
                        {
                            *slot = Some(SendRaw(raw));
                        }
                    }
                    EngineCmd::FrameDirty => {
                        mpv_dirty = true;
                    }
                    EngineCmd::Quit => break 'main,
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'main,
            }
        }

        let is_attached = attached.load(Ordering::SeqCst);
        let is_paused = status.lock().map(|s| s.pause).unwrap_or(false);
        let danmaku_active = cfg.danmaku_enabled && !overlay.is_empty();

        if is_attached && (need_repaint || mpv_dirty || (!is_paused && danmaku_active)) {
            need_repaint = false;
            mpv_dirty = false;
            let t = status.lock().map(|s| s.time_pos).unwrap_or(0.0);
            if let Err(e) = render_present(
                &mut surface,
                &player,
                &overlay,
                &cfg,
                &fonts,
                t,
                width,
                height,
            ) {
                need_repaint = true;
                let _ = e;
            }
        }

        // 空闲（未挂载、暂停静止、或关弹幕等待下一视频帧）阻塞休眠，消除空转功耗。
        if !is_attached || is_paused || !danmaku_active {
            if !need_repaint && !mpv_dirty {
                match cmd_rx.recv() {
                    Ok(cmd) => {
                        first_pending = Some(cmd);
                        continue;
                    }
                    Err(_) => break 'main,
                }
            }
        } else {
            std::thread::sleep(FRAME_INTERVAL);
        }
    }

    // 严格释放序：先清回调 → 销毁 mpv（render_context_free 等待在飞回调完成）→ 释 wakeup 盒 → 释 D3D surface。
    let _ = player.set_update_callback(None, std::ptr::null_mut());
    drop(player);
    unsafe { drop(Box::from_raw(wakeup_ptr)) };
    drop(surface);
}

/// D3D11 surface:fork 的 windows-canvas 高层封装(GpuDevice 自建 device +
/// composition swapchain),全部走 windows-core 0.101。
struct VideoSurface {
    gpu: GpuDevice,
    chain: SwapChain,
    /// immediate context:mpv render 留下的 D3D 状态会毒化 D2D flush,
    /// 每帧 render 后复位 OM 状态(D2D/D3D 同 device 交替的隔离要求)。
    context: ID3D11DeviceContext,
}

fn create_surface() -> Result<VideoSurface, String> {
    // GpuDevice 自己创建 D3D11 device(BGRA、非 SINGLETHREADED)与 D2D/DXGI 侧,
    // 统一使用 fork 的 windows-core 0.101;原先 0.61↔0.101 的 IUnknown 裸指针桥
    // (into_raw / from_existing_raw)不再需要。
    let gpu = GpuDevice::new_or_warp().map_err(|e| e.to_string())?;
    let chain = gpu
        .create_swap_chain(INITIAL_WIDTH, INITIAL_HEIGHT)
        .map_err(|e| e.to_string())?;
    // canvas 自带 bindings,其 ID3D11Device 与 windows crate 的不是同一类型:
    // 借 d3d_device_raw() 拿回 fork 的 ID3D11Device,只为取 immediate context。
    let device_raw = gpu.d3d_device_raw();
    let device: ID3D11Device = unsafe {
        windows::core::IUnknown::from_raw_borrowed(&device_raw)
            .ok_or_else(|| "d3d_device_raw 为空".to_string())?
            .cast::<ID3D11Device>()
            .map_err(|e| e.to_string())?
    };
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe { device.GetImmediateContext(&mut context as *mut _) };
    let context = context.ok_or_else(|| "无 immediate context".to_string())?;
    Ok(VideoSurface { gpu, chain, context })
}

/// 单帧:mpv 视频帧进 backbuffer → D2D 弹幕 overlay → Present(vsync)。
fn render_present(
    surface: &mut VideoSurface,
    player: &MpvPlayer,
    overlay: &[OverlayDanmaku],
    cfg: &VideoConfig,
    fonts: &DanmakuFonts,
    t: f64,
    width: u32,
    height: u32,
) -> Result<(), crate::error::BiliVideoError> {
    // 1) mpv 渲染视频帧到 buffer 0(每帧现取:FLIP 轮转)。
    // 注意:with_backbuffer 给的是 IUnknown raw,mpv 不做 QI 直接当
    // ID3D11Texture2D* 用 —— 必须先 cast 出正确的接口指针(vtable 对齐)。
    let mpv_result: std::cell::Cell<Option<crate::error::BiliVideoError>> =
        std::cell::Cell::new(None);
    surface
        .chain
        .with_backbuffer(|unk_raw| unsafe {
            // 借用语义:from_raw_borrowed 不接管 GetBuffer 的 +1 引用(该引用
            // 由 with_backbuffer 外层 wrapper 析构时释放一次)。若用 from_raw
            // 接管再 drop,会与外层形成双重 Release,backbuffer 计数被破坏,
            // 后续 ResizeBuffers 报 DXGI_ERROR_INVALID_CALL(实测踩坑)。
            let unknown = match windows::core::IUnknown::from_raw_borrowed(&unk_raw) {
                Some(unknown) => unknown,
                None => {
                    mpv_result.set(Some(crate::error::BiliVideoError::LibmpvMissing(
                        "backbuffer 指针为空".into(),
                    )));
                    return;
                }
            };
            match unknown.cast::<ID3D11Texture2D>() {
                Ok(tex) => {
                    let raw = Interface::as_raw(&tex);
                    if let Err(e) = player.render_d3d11(&mpv_d3d11_fbo { tex: raw }) {
                        mpv_result.set(Some(e));
                    }
                    drop(tex);
                }
                Err(e) => {
                    mpv_result.set(Some(crate::error::BiliVideoError::LibmpvMissing(format!(
                        "backbuffer → ID3D11Texture2D 失败:{e}"
                    ))));
                }
            }
        })
        .map_err(|e| crate::error::BiliVideoError::LibmpvMissing(format!("GetBuffer 失败:{e}")))?;
    if let Some(e) = mpv_result.take() {
        return Err(e);
    }

    // mpv render 留下的 D3D11 状态(OM target/viewport/管线)必须在 D2D
    // flush 前复位 —— D2D device context 与 mpv 共享同一 immediate context。
    unsafe {
        surface.context.OMSetRenderTargets(0, None, None);
        surface.context.Flush();
    }

    // 2) D2D 弹幕 overlay(同帧叠加;关闭或无弹幕时跳过 BeginDraw/EndDraw 省开销)。
    if cfg.danmaku_enabled && !overlay.is_empty() && t > 0.0 {
        let session = surface
            .chain
            .begin_draw()
            .map_err(|_| crate::error::BiliVideoError::MpvApi(-1))?;
        draw_danmaku_overlay(&session, overlay, cfg, fonts, t, width, height);
        drop(session);
    }

    // 3) Present(vsync;失败按可重试帧错误处理)。
    surface
        .chain
        .present()
        .map_err(|_| crate::error::BiliVideoError::MpvApi(-1))?;
    Ok(())
}

// ---------- D2D 弹幕 overlay ----------

/// 滚动弹幕横穿屏幕时长(秒)。
const ROLL_SECONDS: f64 = 8.0;
/// 底部/顶部弹幕驻留时长(秒)。
const PIN_SECONDS: f64 = 4.0;
/// 滚动弹幕默认轨道数。
const ROLL_LANES: usize = 10;

/// 画一帧弹幕(物理像素坐标;黑色四向描边 + 主色本体)。
fn draw_danmaku_overlay(
    session: &windows_canvas::DrawingSession<'_>,
    items: &[OverlayDanmaku],
    cfg: &VideoConfig,
    fonts: &DanmakuFonts,
    t: f64,
    width: u32,
    height: u32,
) {
    let small = &fonts.small;
    let medium = &fonts.medium;
    let large = &fonts.large;
    if !cfg.danmaku_enabled || items.is_empty() || t <= 0.0 {
        return;
    }
    let width_f = width as f32;
    let height_f = height as f32;
    let max_lanes = cfg.danmaku_max_lanes.max(1);
    let alpha = cfg.danmaku_opacity.clamp(0.1, 1.0);
    let shadow_alpha = (0.85 * alpha).clamp(0.1, 0.9);
    // 窗口起点(排序数据二分;[t-ROLL_SECONDS, t] 的滚动 + [t-PIN, t] 的顶底)。
    let start = ((t - ROLL_SECONDS) * 1000.0) as i32;
    let begin = items.partition_point(|e| e.progress_ms < start);
    for elem in &items[begin.min(items.len())..] {
        let progress = f64::from(elem.progress_ms) / 1000.0;
        if progress > t {
            break; // 排序后早退
        }
        let elapsed = t - progress;
        let (x, y) = match elem.mode {
            1 | 2 | 3 => {
                if elapsed > ROLL_SECONDS {
                    continue;
                }
                let lane =
                    usize::try_from(elem.progress_ms.unsigned_abs() % max_lanes as u32).unwrap_or(0);
                let x = (1.0 - elapsed / ROLL_SECONDS) * f64::from(width_f) * 1.15
                    - elapsed * 40.0;
                (x as f32, 8.0 + lane as f32 * 30.0)
            }
            4 | 5 => {
                if elapsed > PIN_SECONDS {
                    continue;
                }
                let est_w = elem.content.chars().count() as f32 * 22.0;
                let x = (width_f - est_w).max(8.0) / 2.0;
                let y = match elem.mode {
                    4 => (height_f - 44.0).max(0.0),
                    _ => 8.0,
                };
                (x, y)
            }
            _ => continue,
        };
        let format = match elem.fontsize {
            ..=20 => &small,
            21..=28 => &medium,
            _ => &large,
        };
        let rect = Rect { left: x, top: y, right: x + 600.0, bottom: y + 34.0 };
        // 描边(近似):黑底四向偏移各画一次。
        let Ok(shadow) = session.create_solid_brush(ColorF::new(0.0, 0.0, 0.0, shadow_alpha)) else {
            continue;
        };
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            session.draw_text(
                &elem.content,
                format,
                &Rect {
                    left: x + dx,
                    top: y + dy,
                    right: x + dx + 600.0,
                    bottom: y + dy + 34.0,
                },
                &shadow,
            );
        }
        // 本体(0xRRGGBB;亮度过低反白兜底可读)。
        let rgb = elem.color & 0xFF_FF_FF;
        let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
        let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
        let b = (rgb & 0xFF) as f32 / 255.0;
        let luminance = 0.299 * r + 0.587 * g + 0.114 * b;
        let (r, g, b) = if luminance < 0.12 { (1.0, 1.0, 1.0) } else { (r, g, b) };
        if let Ok(brush) = session.create_solid_brush(ColorF::new(r, g, b, alpha)) {
            session.draw_text(&elem.content, format, &rect, &brush);
        }
    }
}

/// 观察 time-pos/duration/pause/eof-reached → 状态快照。
fn observe_status(
    player: &mut MpvPlayer,
    status: Arc<Mutex<PlaybackStatus>>,
) -> Result<(), crate::error::BiliVideoError> {
    player.observe_property(1, "time-pos", MPV_FORMAT_DOUBLE)?;
    player.observe_property(2, "duration", MPV_FORMAT_DOUBLE)?;
    player.observe_property(3, "pause", MPV_FORMAT_FLAG)?;
    player.observe_property(4, "eof-reached", MPV_FORMAT_FLAG)?;
    player.start_event_thread(Arc::new(move |event| match event {
        MpvEvent::PropertyChange { name, value } => {
            let Ok(mut s) = status.lock() else { return };
            match (name.as_str(), value) {
                ("time-pos", PropertyValue::Double(v)) => s.time_pos = v.max(0.0),
                ("duration", PropertyValue::Double(v)) => s.duration = v.max(0.0),
                ("pause", PropertyValue::Flag(v)) => s.pause = v,
                ("eof-reached", PropertyValue::Flag(v)) => s.eof = v,
                _ => {}
            }
        }
        // keep-open:EOF 停在末帧(keep-open-pause 默认 yes → pause 置位),
        // eof-reached 属性双保险。
        MpvEvent::EndFile { .. } => {
            if let Ok(mut s) = status.lock() {
                s.eof = true;
            }
        }
        _ => {}
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 引擎线程独立验证(无 UI):B 站分轨 → 状态快照到位。
    /// 需 libmpv-2.dll 就位 + BILIPLAY_TEST_URL/BILIPLAY_TEST_AUDIO(可由
    /// `biliplay playurl <bvid>` 存档的 samples/playurl-ok.json 提取)。
    #[test]
    #[ignore = "需 libmpv-2.dll + BILIPLAY_TEST_URL;跑法:cargo test -p bili-video -- --ignored"]
    fn engine_thread_produces_status() {
        let Ok(video_url) = std::env::var("BILIPLAY_TEST_URL") else {
            eprintln!("跳过(未设 BILIPLAY_TEST_URL)");
            return;
        };
        let audio_url = std::env::var("BILIPLAY_TEST_AUDIO").ok();
        let engine = VideoEngine::new();
        // 引擎初始化(device + swapchain + mpv)最宽数秒。
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            engine.error().is_none(),
            "引擎初始化失败: {:?}",
            engine.error()
        );
        engine.load(&video_url, audio_url.as_deref());
        let mut loaded = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            if engine.status().has_media() {
                loaded = true;
                break;
            }
        }
        assert!(
            loaded,
            "状态快照未到位: {:?} err={:?}",
            engine.status(),
            engine.error()
        );
    }
}
