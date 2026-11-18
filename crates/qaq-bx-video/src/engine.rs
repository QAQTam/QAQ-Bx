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

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::d3d11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::core::Interface;
use windows_canvas::{ColorF, GpuDevice, Rect, SwapChain};
use windows_text::TextFormat;

use crate::ffi::{MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_STRING, MpvLib, mpv_d3d11_fbo};
use crate::player::{MpvEvent, MpvPlayer, PropertyValue};

/// 与 `qaq-bx-core::wbi::BROWSER_UA` 保持一致(本 crate 不依赖 qaq-bx-core,m4s
/// 防盗链:无 Referer 403 实测锚 `docs/bilibili-api-inventory.md`)。
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
/// m4s 分轨请求头(loadfile 全局设置,分轨同效)。
const BILI_HEADERS: &[(&str, &str)] = &[
    ("Referer", "https://www.bilibili.com/"),
    ("User-Agent", BROWSER_UA),
];

/// 初始 swapchain 尺寸(挂载前占位;首个 Metrics 到达即 ResizeBuffers)。
const INITIAL_WIDTH: u32 = 1280;
const INITIAL_HEIGHT: u32 = 720;
/// 合并 resize 轮询节拍(拖拽期间轻轮询用)。
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// 弹幕活动期渲染节拍预算:实际帧率由 Present(1) 的 vsync 阻塞决定
/// (60/120/144Hz 跟随显示器满刷新),该预算仅在 Present 不阻塞时防空转。
const RENDER_BUDGET: Duration = Duration::from_millis(8);
/// P1 resize 合并窗口:拖拽期间的真实 ResizeBuffers 至多每此间隔一次,
/// 静止后应用最终尺寸并重挂一次;拖拽过程 0 次重挂(消除卡顿主因)。
const RESIZE_COALESCE: Duration = Duration::from_millis(120);

/// 待挂载 swapchain 的原始 COM 指针:引擎侧 `IUnknown::into_raw` 转移所有权,
/// UI 侧 `IUnknown::from_raw` 接管(接手方 Drop 正确 Release,引用计数闭合)。
/// 两侧现同用 fork 的 windows-core 0.101;仍走裸指针是为跨线程传递 +
/// 与 canvas/reactor 各自的 bindings 类型解耦,不再是版本不互通。
struct SendRaw(*mut c_void);
unsafe impl Send for SendRaw {}
unsafe impl Sync for SendRaw {}

impl SendRaw {
    fn into_raw(self) -> *mut c_void {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

impl Drop for SendRaw {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                drop(windows::core::IUnknown::from_raw(self.0));
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

/// 面板物理像素尺寸与组合缩放因子(由 DIP × CompositionScale 换算)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceMetrics {
    pub width: u32,
    pub height: u32,
    pub scale_x: f32,
    pub scale_y: f32,
}

fn sanitize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale.max(0.01)
    } else {
        1.0
    }
}

/// 将面板逻辑尺寸(DIP)与组合缩放因子换算为物理像素尺寸与规范化缩放。
pub fn compute_surface_metrics(
    dip_width: f64,
    dip_height: f64,
    scale_x: f32,
    scale_y: f32,
) -> Option<SurfaceMetrics> {
    if !dip_width.is_finite() || !dip_height.is_finite() || dip_width <= 0.0 || dip_height <= 0.0 {
        return None;
    }
    let sx = sanitize_scale(scale_x);
    let sy = sanitize_scale(scale_y);
    let width = (dip_width * f64::from(sx)).round().max(0.0) as u32;
    let height = (dip_height * f64::from(sy)).round().max(0.0) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    Some(SurfaceMetrics {
        width,
        height,
        scale_x: sx,
        scale_y: sy,
    })
}

fn configure_swap_chain(chain: &mut SwapChain, scale_x: f32, scale_y: f32) {
    chain.set_dpi(96.0 * scale_x, 96.0 * scale_y);
    chain.set_composition_scale(scale_x, scale_y);
}

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
    /// 当前生效视频解码格式(mpv video-format;空 = 视频轨未解出,F4 探测锚)。
    pub video_format: String,
    /// 当前生效音频编解码名(mpv audio-codec-name;空 = 音频轨未解出,F4 探测锚)。
    pub audio_codec: String,
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
    /// 弹幕字体族(DWrite 名称;缺省 Noto Sans SC;F3)。
    pub danmaku_font_family: String,
    /// 弹幕字号缩放(1.0 = 原始 16/22/30px;F3)。
    pub danmaku_font_scale: f32,
    /// 弹幕过滤规则(网页版对齐;SetDanmakuFilter 热更新)。
    pub danmaku_filter: DanmakuFilter,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            hardware_decode: true,
            danmaku_enabled: true,
            danmaku_opacity: 0.92,
            danmaku_max_lanes: ROLL_LANES,
            danmaku_font_family: DANMAKU_DEFAULT_FONT.into(),
            danmaku_font_scale: 1.0,
            danmaku_filter: DanmakuFilter::default(),
        }
    }
}

/// 弹幕默认字体(与 UI DEFAULT_FONT_FAMILY 一致)。
pub const DANMAKU_DEFAULT_FONT: &str = "Noto Sans SC";

/// UI → 引擎线程命令。
pub enum EngineCmd {
    /// 加载分轨(B 站头固定在引擎侧;audio_url=None 纯视频)。
    /// `start_secs > 0` 从该位置起播(画质/分 P 无缝切换:先记位后重载)。
    Load {
        video_url: String,
        audio_url: Option<String>,
        start_secs: f64,
        autoplay: bool,
    },
    /// 全片弹幕(按 progress 排序;引擎 D2D overlay 每帧读取)。
    SetDanmaku {
        items: Vec<OverlayDanmaku>,
    },
    /// 预存弹幕分段到货:增量归并进 overlay(保持 progress 升序不变量)。
    AppendDanmaku {
        items: Vec<OverlayDanmaku>,
    },
    /// 运行时更新弹幕开关与绘制参数。
    SetDanmakuConfig {
        enabled: bool,
        opacity: f32,
        max_lanes: usize,
        font_family: String,
        font_scale: f32,
    },
    /// 运行时更新弹幕过滤规则(引擎侧编译,并对已缓存弹幕全量重滤)。
    SetDanmakuFilter {
        filter: DanmakuFilter,
    },
    /// 运行时切换硬解（d3d11va / no）。
    SetHardwareDecode(bool),
    Play,
    Pause,
    Stop,
    Seek {
        secs: f64,
    },
    Volume {
        pct: f64,
    },
    /// 面板物理像素尺寸与组合缩放变化(0 视为面板未就绪,忽略)。
    Resize(SurfaceMetrics),
    /// 面板卸载:attached 复位,swapchain 放回待挂载槽(重进播放页再挂)。
    Detach,
    /// mpv update_callback 唤醒（新帧到达待合成）。
    FrameDirty,
    Quit,
}

/// 弹幕 overlay 条目(qaq-bx-core DanmakuElem 的引擎侧镜像,避免跨 crate 依赖)。
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
    /// 发送者 mid CRC32 hash(网页版用户屏蔽同源;空 = 未下发)。
    pub mid_hash: String,
    /// 云屏蔽权重(0 = 未下发;越低越疑似垃圾)。
    pub weight: u32,
}

/// 弹幕过滤规则(网页版弹幕屏蔽面板对齐)。UI 侧原样下发,引擎线程编译执行。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DanmakuFilter {
    /// 屏蔽滚动弹幕(mode 1/2/3)。
    pub block_scroll: bool,
    /// 屏蔽底部弹幕(mode 4)。
    pub block_bottom: bool,
    /// 屏蔽顶部弹幕(mode 5)。
    pub block_top: bool,
    /// 屏蔽彩色弹幕(非 0xFFFFFF)。
    pub block_color: bool,
    /// 过滤重复弹幕(同文本 4 秒窗口内重复出现只保留首条)。
    pub block_repeat: bool,
    /// 智能屏蔽权重阈值(0=关;仅 weight>0 且 weight<阈值 的弹幕被屏蔽)。
    pub weight_min: u32,
    /// 屏蔽词规则(每行一条):`re:` 前缀=正则,`u:` 前缀=用户 midHash,
    /// 其余=关键词(忽略大小写子串)。无效正则静默丢弃。
    pub rules: Vec<String>,
}

/// 重复弹幕过滤窗口(毫秒;同文本该窗口内再次出现即丢弃)。
const REPEAT_WINDOW_MS: i32 = 4_000;
/// 去重表容量上限(超过即裁剪,防长视频内存无界)。
const DEDUP_MAP_CAP: usize = 8192;

/// 编译后的过滤规则(引擎线程私有;正则编译失败丢弃该条规则)。
struct CompiledDanmakuFilter {
    cfg: DanmakuFilter,
    keywords: Vec<String>,
    regexes: Vec<regex::Regex>,
    users: Vec<String>,
}

impl CompiledDanmakuFilter {
    fn compile(cfg: &DanmakuFilter) -> Self {
        let mut keywords = Vec::new();
        let mut regexes = Vec::new();
        let mut users = Vec::new();
        for raw in &cfg.rules {
            let rule = raw.trim();
            if let Some(pat) = rule.strip_prefix("re:") {
                if let Ok(re) = regex::Regex::new(pat.trim()) {
                    regexes.push(re);
                }
            } else if let Some(hash) = rule.strip_prefix("u:") {
                let hash = hash.trim().to_string();
                if !hash.is_empty() {
                    users.push(hash);
                }
            } else {
                let kw = rule.to_lowercase();
                if !kw.is_empty() {
                    keywords.push(kw);
                }
            }
        }
        Self {
            cfg: cfg.clone(),
            keywords,
            regexes,
            users,
        }
    }

    /// 全部规则为空(默认态):跳过过滤与去重,零开销直通。
    fn is_passthrough(&self) -> bool {
        !self.cfg.block_scroll
            && !self.cfg.block_bottom
            && !self.cfg.block_top
            && !self.cfg.block_color
            && !self.cfg.block_repeat
            && self.cfg.weight_min == 0
            && self.keywords.is_empty()
            && self.regexes.is_empty()
            && self.users.is_empty()
    }

    /// 单条弹幕是否放行(不含重复过滤;窗口去重需跨条状态,由 filter_batch 驱动)。
    fn passes(&self, e: &OverlayDanmaku) -> bool {
        if self.cfg.block_scroll && matches!(e.mode, 1 | 2 | 3) {
            return false;
        }
        if self.cfg.block_bottom && e.mode == 4 {
            return false;
        }
        if self.cfg.block_top && e.mode == 5 {
            return false;
        }
        if self.cfg.block_color && e.color & 0xFF_FF_FF != 0xFF_FF_FF {
            return false;
        }
        if self.cfg.weight_min > 0 && e.weight > 0 && e.weight < self.cfg.weight_min {
            return false;
        }
        if !self.users.is_empty()
            && self
                .users
                .iter()
                .any(|u| e.mid_hash.eq_ignore_ascii_case(u))
        {
            return false;
        }
        if !self.keywords.is_empty() {
            let lower = e.content.to_lowercase();
            if self.keywords.iter().any(|kw| lower.contains(kw.as_str())) {
                return false;
            }
        }
        if self.regexes.iter().any(|re| re.is_match(&e.content)) {
            return false;
        }
        true
    }

    /// 批量过滤:先按 progress 升序(保证去重窗口语义:保留首条),再逐条
    /// 规则匹配 + 窗口去重;`dedup` 跨批持久(分段预存链逐段到货共用)。
    fn filter_batch(
        &self,
        mut items: Vec<OverlayDanmaku>,
        dedup: &mut HashMap<String, i32>,
    ) -> Vec<OverlayDanmaku> {
        if self.is_passthrough() {
            // 默认态直通:不动去重表、不重排序,保持零拷贝原语义。
            return items;
        }
        items.sort_by_key(|e| e.progress_ms);
        let mut kept = Vec::with_capacity(items.len());
        for e in items {
            if !self.passes(&e) {
                continue;
            }
            if self.cfg.block_repeat {
                let key = e.content.to_lowercase();
                let dup = dedup
                    .get(&key)
                    .is_some_and(|&last| (e.progress_ms - last).abs() < REPEAT_WINDOW_MS);
                if dup {
                    continue;
                }
                if dedup.len() >= DEDUP_MAP_CAP {
                    // 裁剪:仅保留最近 2 分钟的记录,仍满则整表重置。
                    let max = dedup.values().copied().max().unwrap_or(0);
                    dedup.retain(|_, v| max - *v < 120_000);
                    if dedup.len() >= DEDUP_MAP_CAP {
                        dedup.clear();
                    }
                }
                dedup.insert(key, e.progress_ms);
            }
            kept.push(e);
        }
        kept
    }
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
        self.load_with_options(video_url, audio_url, 0.0, true);
    }

    /// 加载 B 站分轨并指定起播位置与是否自动播放。
    pub fn load_with_options(
        &self,
        video_url: &str,
        audio_url: Option<&str>,
        start_secs: f64,
        autoplay: bool,
    ) {
        _ = self.cmd_tx.send(EngineCmd::Load {
            video_url: video_url.to_string(),
            audio_url: audio_url.map(str::to_string),
            start_secs: start_secs.max(0.0),
            autoplay,
        });
    }

    /// 全片弹幕(SetDanmaku;排序由调用方保证)。
    pub fn set_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmaku { items });
    }

    /// 增量追加弹幕分段(预存链;引擎侧归并回 progress 升序)。
    pub fn append_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(EngineCmd::AppendDanmaku { items });
    }

    /// 动态调整弹幕开关、不透明度与最大轨道数。
    pub fn set_danmaku_config(
        &self,
        enabled: bool,
        opacity: f32,
        max_lanes: usize,
        font_family: &str,
        font_scale: f32,
    ) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmakuConfig {
            enabled,
            opacity: opacity.clamp(0.1, 1.0),
            max_lanes: max_lanes.clamp(1, 20),
            font_family: if font_family.trim().is_empty() {
                DANMAKU_DEFAULT_FONT.to_string()
            } else {
                font_family.trim().to_string()
            },
            font_scale: if font_scale.is_finite() {
                font_scale.clamp(0.5, 2.0)
            } else {
                1.0
            },
        });
    }

    /// 动态更新弹幕过滤规则(网页版对齐;规则变化时引擎全量重滤已缓存弹幕)。
    pub fn set_danmaku_filter(&self, filter: DanmakuFilter) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmakuFilter { filter });
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

    /// 面板物理像素尺寸与组合缩放上报(DIP × composition scale)。
    pub fn set_metrics(&self, dip_width: f64, dip_height: f64, scale_x: f32, scale_y: f32) {
        let Some(metrics) = compute_surface_metrics(dip_width, dip_height, scale_x, scale_y) else {
            return;
        };
        _ = self.cmd_tx.send(EngineCmd::Resize(metrics));
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
        Some(chain.into_raw())
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
        Self::with(DANMAKU_DEFAULT_FONT, 1.0)
    }

    /// 按字体族与字号缩放构建(F3);轨道行高与定宽估算随缩放联动。
    fn with(family: &str, scale: f32) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(0.5, 2.0)
        } else {
            1.0
        };
        Self {
            small: TextFormat::new(family, 16.0 * scale),
            medium: TextFormat::new(family, 22.0 * scale),
            large: TextFormat::new(family, 30.0 * scale),
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
    let hwdec = if initial_config.hardware_decode {
        "d3d11va"
    } else {
        "no"
    };
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
    let mut fonts = DanmakuFonts::new();
    let mut width = INITIAL_WIDTH;
    let mut height = INITIAL_HEIGHT;
    let mut scale_x = 1.0_f32;
    let mut scale_y = 1.0_f32;
    let mut need_repaint = true;
    let mut mpv_dirty = true;
    let mut overlay: Vec<OverlayDanmaku> = Vec::new();
    // 弹幕过滤:规则编译态 + 重复弹幕去重表(跨分段预存批持久)。
    let mut dm_filter = CompiledDanmakuFilter::compile(&initial_config.danmaku_filter);
    let mut cfg = initial_config;
    let mut first_pending: Option<EngineCmd> = None;
    let mut dm_dedup: HashMap<String, i32> = HashMap::new();
    // 弹幕时基锚点:(锚 time-pos, 锚定墙钟)——mpv time-pos 按视频帧步进,
    // 两次更新之间用墙钟线性外推,属性一变即重锚。
    let mut dm_time_anchor: Option<(f64, Instant)> = None;
    // 弹幕活动期的下一帧渲染截止线(RENDER_BUDGET 节拍)。
    let mut next_frame = Instant::now();
    // P1 resize 合并状态:拖拽期间只记最新目标尺寸,120ms 节流应用。
    let mut pending_resize: Option<SurfaceMetrics> = None;
    let mut resize_first_seen: Option<Instant> = None;
    'main: loop {
        // 命令段：若有已取出的首条指令先消费，再一口气排空。
        loop {
            let next_cmd = match first_pending.take() {
                Some(cmd) => Ok(cmd),
                None => cmd_rx.try_recv(),
            };
            match next_cmd {
                Ok(cmd) => match cmd {
                    EngineCmd::Load {
                        video_url,
                        audio_url,
                        start_secs,
                        autoplay,
                    } => {
                        if let Ok(mut s) = status.lock() {
                            *s = PlaybackStatus::default();
                        }
                        let pause_val = if autoplay { "no" } else { "yes" };
                        // start 是全局持久选项:每次 load 前显式重置(0 = 片头);
                        // 定点换流(画质/分 P)带当前位置,mpv 直达省二次 seek。
                        let start = if start_secs > 0.0 {
                            format!("{start_secs:.3}")
                        } else {
                            "0".to_string()
                        };
                        // F4 诊断锚:起播参数一条日志(播放异常时对照 mpv 轨道属性)。
                        eprintln!(
                            "[qaq-bx-video] loadfile start={start} autoplay={autoplay} audio_attached={}",
                            audio_url.is_some()
                        );
                        if let Err(e) =
                            player.set_options(&[("pause", pause_val), ("start", &start)])
                        {
                            report_error(format!("mpv pause/start 选项失败:{e}"));
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
                        // 全量替换:重置去重表(新视频/分P 清层语义)。
                        overlay = dm_filter.filter_batch(items, &mut dm_dedup);
                        need_repaint = true;
                    }
                    EngineCmd::AppendDanmaku { items } => {
                        let kept = dm_filter.filter_batch(items, &mut dm_dedup);
                        if !kept.is_empty() {
                            overlay.extend(kept);
                            // 归并回 progress 升序(渲染二分扫描的不变量)。
                            overlay.sort_by_key(|e| e.progress_ms);
                            need_repaint = true;
                        }
                    }
                    EngineCmd::SetDanmakuFilter { filter } => {
                        dm_filter = CompiledDanmakuFilter::compile(&filter);
                        // 规则变化:已缓存弹幕全量重滤(升序重跑,保留窗口内首条)。
                        dm_dedup.clear();
                        let items = std::mem::take(&mut overlay);
                        overlay = dm_filter.filter_batch(items, &mut dm_dedup);
                        need_repaint = true;
                    }
                    EngineCmd::SetDanmakuConfig {
                        enabled,
                        opacity,
                        max_lanes,
                        font_family,
                        font_scale,
                    } => {
                        cfg.danmaku_enabled = enabled;
                        cfg.danmaku_opacity = opacity.clamp(0.1, 1.0);
                        cfg.danmaku_max_lanes = max_lanes.clamp(1, 20);
                        // 字体族/字号变化 → 重建 DWrite 格式(F3)。
                        if cfg.danmaku_font_family != font_family
                            || (cfg.danmaku_font_scale - font_scale).abs() > f32::EPSILON
                        {
                            fonts = DanmakuFonts::with(&font_family, font_scale);
                        }
                        cfg.danmaku_font_family = font_family;
                        cfg.danmaku_font_scale = font_scale;
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
                    EngineCmd::Resize(metrics) => {
                        let size_changed = (metrics.width, metrics.height) != (width, height);
                        let scale_changed = (metrics.scale_x - scale_x).abs() > f32::EPSILON
                            || (metrics.scale_y - scale_y).abs() > f32::EPSILON;
                        if size_changed || scale_changed {
                            // 只记最新目标;真实 ResizeBuffers 由节流窗口统一应用。
                            pending_resize = Some(metrics);
                            if resize_first_seen.is_none() {
                                resize_first_seen = Some(Instant::now());
                            }
                            need_repaint = true;
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

        // P1:合并窗口到期 → 应用最新面板尺寸(一次 ResizeBuffers + 一次重挂)。
        if let (Some(metrics), Some(seen)) = (pending_resize, resize_first_seen)
            && seen.elapsed() >= RESIZE_COALESCE
        {
            pending_resize = None;
            resize_first_seen = None;
            let size_changed = (metrics.width, metrics.height) != (width, height);
            let scale_changed = (metrics.scale_x - scale_x).abs() > f32::EPSILON
                || (metrics.scale_y - scale_y).abs() > f32::EPSILON;
            if size_changed {
                if let Err(e) = surface.chain.resize(metrics.width, metrics.height) {
                    report_error(format!(
                        "ResizeBuffers({}x{}) 失败:{e}",
                        metrics.width, metrics.height
                    ));
                } else {
                    width = metrics.width;
                    height = metrics.height;
                    scale_x = metrics.scale_x;
                    scale_y = metrics.scale_y;
                    configure_swap_chain(&mut surface.chain, scale_x, scale_y);
                    need_repaint = true;
                    attached.store(false, Ordering::SeqCst);
                    if let Ok(raw) = surface.chain.into_unknown_raw()
                        && let Ok(mut slot) = pending.lock()
                    {
                        *slot = Some(SendRaw(raw));
                    }
                }
            } else if scale_changed {
                scale_x = metrics.scale_x;
                scale_y = metrics.scale_y;
                configure_swap_chain(&mut surface.chain, scale_x, scale_y);
                need_repaint = true;
            }
        }

        let is_attached = attached.load(Ordering::SeqCst);
        let (is_paused, raw_t) = match status.lock() {
            Ok(s) => (s.pause, s.time_pos),
            Err(_) => (false, 0.0),
        };
        let danmaku_active = cfg.danmaku_enabled && !overlay.is_empty();

        if is_attached && (need_repaint || mpv_dirty || (!is_paused && danmaku_active)) {
            need_repaint = false;
            mpv_dirty = false;
            // 弹幕时基插值:mpv time-pos 随视频帧步进(24/30fps ≈ 33–42ms 跳变),
            // 直接取值会让满刷新率下的滚动呈锯齿。播放中在两次属性更新之间用
            // 墙钟线性外推;属性一变即重锚,seek/起播自动校正;暂停冻结并失效
            // 锚点,恢复播放从当前 time-pos 重锚。
            let t = if danmaku_active && !is_paused {
                interpolated_danmaku_time(&mut dm_time_anchor, raw_t, Instant::now())
            } else {
                dm_time_anchor = None;
                raw_t
            };
            if let Err(e) = render_present(
                &mut surface,
                &player,
                &overlay,
                &cfg,
                &fonts,
                t,
                width as f32 / scale_x,
                height as f32 / scale_y,
            ) {
                need_repaint = true;
                let _ = e;
            }
        }

        // 空闲（未挂载、暂停静止、或关弹幕等待下一视频帧）阻塞休眠，消除空转功耗。
        // P1:有待应用的合并 resize 时保持轻节拍轮询,到点应用,不空转不长眠。
        if !is_attached || is_paused || !danmaku_active {
            if !need_repaint && !mpv_dirty {
                if pending_resize.is_some() {
                    std::thread::sleep(FRAME_INTERVAL);
                } else {
                    match cmd_rx.recv() {
                        Ok(cmd) => {
                            first_pending = Some(cmd);
                            continue;
                        }
                        Err(_) => break 'main,
                    }
                }
            }
        } else {
            // 弹幕平滑节拍:帧率由 Present(1) 的 vsync 阻塞决定(显示器满刷新);
            // 原先固定 sleep 16ms 会把下一次 Present 推过 vblank —— 一帧 16ms
            // 一帧 33ms 的隔帧错拍即弹幕「忽快忽慢」的根源。此处仅按
            // RENDER_BUDGET 防空转,等待用 recv_timeout,命令照收不延迟。
            let now = Instant::now();
            if next_frame < now {
                next_frame = now;
            }
            next_frame += RENDER_BUDGET;
            match cmd_rx.recv_timeout(next_frame - now) {
                Ok(cmd) => {
                    first_pending = Some(cmd);
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break 'main,
            }
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
    Ok(VideoSurface {
        gpu,
        chain,
        context,
    })
}

/// 单帧:mpv 视频帧进 backbuffer → D2D 弹幕 overlay → Present(vsync)。
fn render_present(
    surface: &mut VideoSurface,
    player: &MpvPlayer,
    overlay: &[OverlayDanmaku],
    cfg: &VideoConfig,
    fonts: &DanmakuFonts,
    t: f64,
    dip_width: f32,
    dip_height: f32,
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
        draw_danmaku_overlay(&session, overlay, cfg, fonts, t, dip_width, dip_height);
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
/// time-pos 视为「未更新」的容差(秒;小于相邻两次属性更新的典型步进)。
const TIMEPOS_EPSILON: f64 = 0.020;
/// 墙钟外推上限(秒;属性停更/卡顿时冻结,防漂移)。
const TIMEPOS_EXTRAP_CAP: f64 = 0.25;

/// 弹幕时基:mpv time-pos 随视频帧步进,直接取值会让滚动锯齿化。
/// 播放中在两次属性更新之间用墙钟线性外推;`raw_t` 与锚值差在容差内视为
/// 同值(未更新)则外推,否则重锚。锚定时刻用饱和差,供测试注入虚拟时刻。
fn interpolated_danmaku_time(
    anchor: &mut Option<(f64, Instant)>,
    raw_t: f64,
    now: Instant,
) -> f64 {
    match *anchor {
        Some((t0, at)) if (raw_t - t0).abs() < TIMEPOS_EPSILON => {
            t0 + now
                .saturating_duration_since(at)
                .as_secs_f64()
                .min(TIMEPOS_EXTRAP_CAP)
        }
        _ => {
            *anchor = Some((raw_t, now));
            raw_t
        }
    }
}

/// 画一帧弹幕(DIP 坐标,与 `SwapChain::set_dpi` 对齐;黑色四向描边 + 主色本体)。
fn draw_danmaku_overlay(
    session: &windows_canvas::DrawingSession<'_>,
    items: &[OverlayDanmaku],
    cfg: &VideoConfig,
    fonts: &DanmakuFonts,
    t: f64,
    dip_width: f32,
    dip_height: f32,
) {
    let small = &fonts.small;
    let medium = &fonts.medium;
    let large = &fonts.large;
    if !cfg.danmaku_enabled || items.is_empty() || t <= 0.0 {
        return;
    }
    let width_f = dip_width;
    let height_f = dip_height;
    let max_lanes = cfg.danmaku_max_lanes.max(1);
    let alpha = cfg.danmaku_opacity.clamp(0.1, 1.0);
    let shadow_alpha = (0.85 * alpha).clamp(0.1, 0.9);
    // 行高/估算宽随字号缩放(F3),保证大字号弹幕不重叠。
    let font_scale = if cfg.danmaku_font_scale.is_finite() {
        cfg.danmaku_font_scale.clamp(0.5, 2.0)
    } else {
        1.0
    };
    let lane_pitch = 30.0 * font_scale;
    let char_width = 22.0 * font_scale;
    let text_slot_h = 34.0 * font_scale;
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
                let lane = usize::try_from(elem.progress_ms.unsigned_abs() % max_lanes as u32)
                    .unwrap_or(0);
                let x = (1.0 - elapsed / ROLL_SECONDS) * f64::from(width_f) * 1.15 - elapsed * 40.0;
                (x as f32, 8.0 + lane as f32 * lane_pitch)
            }
            4 | 5 => {
                if elapsed > PIN_SECONDS {
                    continue;
                }
                let est_w = elem.content.chars().count() as f32 * char_width;
                let x = (width_f - est_w).max(8.0) / 2.0;
                let y = match elem.mode {
                    4 => (height_f - lane_pitch - 14.0).max(0.0),
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
        let rect = Rect {
            left: x,
            top: y,
            right: x + 600.0,
            bottom: y + text_slot_h,
        };
        // 描边(近似):黑底四向偏移各画一次。
        let Ok(shadow) = session.create_solid_brush(ColorF::new(0.0, 0.0, 0.0, shadow_alpha))
        else {
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
                    bottom: y + dy + text_slot_h,
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
        let (r, g, b) = if luminance < 0.12 {
            (1.0, 1.0, 1.0)
        } else {
            (r, g, b)
        };
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
    // F4 首播音画缺失探测:解码轨道缺失(如捆绑 mpv 不含 AV1/AC-3 解码器,
    // 或外挂音轨拉取失败)时对应格式属性为空字符串。
    player.observe_property(5, "video-format", MPV_FORMAT_STRING)?;
    player.observe_property(6, "audio-codec-name", MPV_FORMAT_STRING)?;
    // P2 诊断:硬解实际生效路径(d3d11va / 失败回退 sw)。
    player.observe_property(7, "hwdec-current", MPV_FORMAT_STRING)?;
    player.start_event_thread(Arc::new(move |event| match event {
        MpvEvent::PropertyChange { name, value } => {
            let Ok(mut s) = status.lock() else { return };
            match (name.as_str(), value) {
                ("time-pos", PropertyValue::Double(v)) => s.time_pos = v.max(0.0),
                ("duration", PropertyValue::Double(v)) => s.duration = v.max(0.0),
                ("pause", PropertyValue::Flag(v)) => s.pause = v,
                ("eof-reached", PropertyValue::Flag(v)) => s.eof = v,
                ("video-format", PropertyValue::Text(v)) => s.video_format = v,
                ("audio-codec-name", PropertyValue::Text(v)) => s.audio_codec = v,
                ("hwdec-current", PropertyValue::Text(v)) => {
                    if !v.is_empty() {
                        eprintln!("[qaq-bx-video] hwdec-current={v}");
                    }
                }
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

    #[test]
    fn compute_surface_metrics_scales_dips_and_handles_anisotropic_dpi() {
        assert_eq!(
            compute_surface_metrics(844.0, 420.0, 1.0, 1.0),
            Some(SurfaceMetrics {
                width: 844,
                height: 420,
                scale_x: 1.0,
                scale_y: 1.0
            })
        );
        assert_eq!(
            compute_surface_metrics(844.0, 420.0, 1.25, 1.25),
            Some(SurfaceMetrics {
                width: 1055,
                height: 525,
                scale_x: 1.25,
                scale_y: 1.25
            })
        );
        assert_eq!(
            compute_surface_metrics(844.0, 420.0, 1.5, 1.5),
            Some(SurfaceMetrics {
                width: 1266,
                height: 630,
                scale_x: 1.5,
                scale_y: 1.5
            })
        );
        assert_eq!(
            compute_surface_metrics(844.0, 420.0, 1.75, 1.75),
            Some(SurfaceMetrics {
                width: 1477,
                height: 735,
                scale_x: 1.75,
                scale_y: 1.75
            })
        );
        assert_eq!(
            compute_surface_metrics(844.0, 420.0, 2.0, 2.0),
            Some(SurfaceMetrics {
                width: 1688,
                height: 840,
                scale_x: 2.0,
                scale_y: 2.0
            })
        );
        // 纵横缩放独立计算，避免以 scale_x 覆盖 scale_y。
        assert_eq!(
            compute_surface_metrics(400.0, 200.0, 1.5, 2.0),
            Some(SurfaceMetrics {
                width: 600,
                height: 400,
                scale_x: 1.5,
                scale_y: 2.0
            })
        );

        // 非法或未布局尺寸返回 None；异常缩放因子回退 1.0。
        assert_eq!(compute_surface_metrics(0.0, 420.0, 1.5, 1.5), None);
        assert_eq!(compute_surface_metrics(844.0, 0.0, 1.5, 1.5), None);
        assert_eq!(compute_surface_metrics(-10.0, 420.0, 1.5, 1.5), None);
        assert_eq!(compute_surface_metrics(f64::NAN, 420.0, 1.5, 1.5), None);
        assert_eq!(
            compute_surface_metrics(844.0, 420.0, 0.0, f32::NAN),
            Some(SurfaceMetrics {
                width: 844,
                height: 420,
                scale_x: 1.0,
                scale_y: 1.0
            })
        );
    }

    #[test]
    fn configure_swap_chain_sets_inverse_matrix_transform() {
        use windows::Win32::dxgi::{DXGI_MATRIX_3X2_F, IDXGISwapChain2};

        let gpu = GpuDevice::new_warp().expect("WARP GPU 设备应可用");
        let mut chain = gpu
            .create_swap_chain(1266, 630)
            .expect("创建 composition swapchain");
        configure_swap_chain(&mut chain, 1.5, 1.5);

        let sc2: IDXGISwapChain2 = chain.to_interface().expect("IDXGISwapChain2");
        let mut matrix = DXGI_MATRIX_3X2_F::default();
        let hr = unsafe { sc2.GetMatrixTransform(&mut matrix) };
        assert!(hr.is_ok(), "GetMatrixTransform 失败: {hr:?}");
        assert!((matrix._11 - 1.0 / 1.5).abs() < 1e-5, "_11={}", matrix._11);
        assert!((matrix._22 - 1.0 / 1.5).abs() < 1e-5, "_22={}", matrix._22);
        assert_eq!(matrix._12, 0.0);
        assert_eq!(matrix._21, 0.0);
        assert_eq!(matrix._31, 0.0);
        assert_eq!(matrix._32, 0.0);

        // ResizeBuffers 后重配缩放矩阵（各向异性 + 175%/200%）及 DIP 弹幕绘制不崩溃。
        chain.resize(549, 412).expect("resize swapchain");
        configure_swap_chain(&mut chain, 1.5, 2.0);
        let hr = unsafe { sc2.GetMatrixTransform(&mut matrix) };
        assert!(hr.is_ok(), "resize 后 GetMatrixTransform 失败: {hr:?}");
        assert!((matrix._11 - 1.0 / 1.5).abs() < 1e-5, "_11={}", matrix._11);
        assert!((matrix._22 - 0.5).abs() < 1e-5, "_22={}", matrix._22);

        let fonts = DanmakuFonts::new();
        let cfg = VideoConfig::default();
        let items = vec![
            OverlayDanmaku {
                progress_ms: 1000,
                mode: 1,
                fontsize: 22,
                color: 0xFFFFFF,
                content: "滚动".into(),
                mid_hash: String::new(),
                weight: 0,
            },
            OverlayDanmaku {
                progress_ms: 1000,
                mode: 4,
                fontsize: 22,
                color: 0xFFFFFF,
                content: "底端".into(),
                mid_hash: String::new(),
                weight: 0,
            },
            OverlayDanmaku {
                progress_ms: 1000,
                mode: 5,
                fontsize: 22,
                color: 0xFFFFFF,
                content: "顶端".into(),
                mid_hash: String::new(),
                weight: 0,
            },
        ];
        let session = chain.begin_draw().expect("D2D begin_draw");
        draw_danmaku_overlay(&session, &items, &cfg, &fonts, 1.5, 366.0, 206.0);
        drop(session);
        chain.present().expect("present");
    }

    /// F3:自定义字体族与字号缩放构建 + 绘制不崩溃(WARP 设备)。
    #[test]
    fn danmaku_fonts_scale_and_alternate_family_render_on_warp() {
        let gpu = GpuDevice::new_warp().expect("WARP GPU 设备应可用");
        let mut chain = gpu.create_swap_chain(640, 360).expect("创建 swapchain");
        let fonts = DanmakuFonts::with("Microsoft YaHei", 1.5);
        let cfg = VideoConfig {
            danmaku_font_family: "Microsoft YaHei".into(),
            danmaku_font_scale: 1.5,
            ..VideoConfig::default()
        };
        let items = vec![
            OverlayDanmaku {
                progress_ms: 500,
                mode: 1,
                fontsize: 25,
                color: 0xFFFFFF,
                content: "大字号滚动".into(),
                mid_hash: String::new(),
                weight: 0,
            },
            OverlayDanmaku {
                progress_ms: 500,
                mode: 4,
                fontsize: 25,
                color: 0xFFFFFF,
                content: "底部".into(),
                mid_hash: String::new(),
                weight: 0,
            },
            OverlayDanmaku {
                progress_ms: 500,
                mode: 5,
                fontsize: 25,
                color: 0xFFFFFF,
                content: "顶部".into(),
                mid_hash: String::new(),
                weight: 0,
            },
        ];
        let session = chain.begin_draw().expect("begin_draw");
        draw_danmaku_overlay(&session, &items, &cfg, &fonts, 1.0, 640.0, 360.0);
        drop(session);
        chain.present().expect("present");
        // 极端缩放不 panic(0.5x / 2x / NaN 回退)。
        let tiny = DanmakuFonts::with(DANMAKU_DEFAULT_FONT, 0.5);
        let huge = DanmakuFonts::with(DANMAKU_DEFAULT_FONT, 2.0);
        let bogus = DanmakuFonts::with("", f32::NAN);
        let _ = (tiny, huge, bogus);
    }

    #[test]
    fn video_engine_set_metrics_handles_valid_and_invalid_inputs_without_panic() {
        let engine = VideoEngine::with_config(VideoConfig::default());
        engine.set_metrics(844.0, 420.0, 1.5, 1.5);
        engine.set_metrics(366.0, 206.0, 2.0, 2.0);
        engine.set_metrics(0.0, 0.0, 1.5, 1.5);
        engine.set_metrics(-1.0, 420.0, 1.5, 1.5);
        engine.set_metrics(f64::NAN, f64::INFINITY, 0.0, f32::NAN);
        engine.detach_panel();
    }

    /// 引擎线程独立验证(无 UI):B 站分轨 → 状态快照到位。
    /// 需 libmpv-2.dll 就位 + QAQ_BX_TEST_URL/QAQ_BX_TEST_AUDIO(可由
    /// `qaq-bx playurl <bvid>` 存档的 samples/playurl-ok.json 提取)。
    #[test]
    #[ignore = "需 libmpv-2.dll + QAQ_BX_TEST_URL;跑法:cargo test -p qaq-bx-video -- --ignored"]
    fn engine_thread_produces_status() {
        let Ok(video_url) = std::env::var("QAQ_BX_TEST_URL") else {
            eprintln!("跳过(未设 QAQ_BX_TEST_URL)");
            return;
        };
        let audio_url = std::env::var("QAQ_BX_TEST_AUDIO").ok();
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
        assert!(loaded, "状态快照未到位: {:?} err={:?}", engine.status(), engine.error());
    }

    #[test]
    fn danmaku_time_extrapolates_between_timepos_updates() {
        let mut anchor = None;
        let now = Instant::now();
        // 首见 time-pos:直接重锚,不外推。
        assert_eq!(interpolated_danmaku_time(&mut anchor, 10.0, now), 10.0);
        // 属性 100ms 未更新:墙钟外推 0.1s。
        let later = now + Duration::from_millis(100);
        let t = interpolated_danmaku_time(&mut anchor, 10.0, later);
        assert!((t - 10.1).abs() < 1e-6, "t={t}");
        // 属性跳变(> 容差):重锚,不叠加旧外推。
        assert_eq!(interpolated_danmaku_time(&mut anchor, 10.5, later), 10.5);
        // 属性小幅步进(视频帧间典型 16.7ms < 容差):视为同值,自旧锚外推
        // 10.5 + 17ms = 10.517 ≈ raw_t,滚动连续无跳变。
        let later2 = later + Duration::from_millis(17);
        let t2 = interpolated_danmaku_time(&mut anchor, 10.5167, later2);
        assert!((t2 - 10.517).abs() < 0.002, "t2={t2}");
    }

    #[test]
    fn danmaku_time_extrapolation_is_capped() {
        // 属性停更 900ms:外推封顶 0.25s,防卡顿漂移。
        let mut anchor = Some((0.0, Instant::now() - Duration::from_millis(900)));
        let t = interpolated_danmaku_time(&mut anchor, 0.0, Instant::now());
        assert!((t - TIMEPOS_EXTRAP_CAP).abs() < 1e-6, "t={t}");
        // 时钟回拨/异常时刻:饱和差不 panic,退化为 0 外推。
        let mut anchor = Some((5.0, Instant::now() + Duration::from_secs(10)));
        let t = interpolated_danmaku_time(&mut anchor, 5.0, Instant::now());
        assert!((t - 5.0).abs() < 1e-6, "t={t}");
    }

    #[test]
    fn danmaku_filter_rules_match_web_panel_semantics() {
        let mk = |content: &str, mode: i32, color: u32| OverlayDanmaku {
            progress_ms: 1000,
            mode,
            fontsize: 22,
            color,
            content: content.into(),
            mid_hash: String::new(),
            weight: 0,
        };
        // 关键词:忽略大小写子串。
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            rules: vec!["前方高能".into()],
            ..Default::default()
        });
        assert!(!f.passes(&mk("前方高能警告", 1, 0xFFFFFF)));
        assert!(f.passes(&mk("没有关键词", 1, 0xFFFFFF)));
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            rules: vec!["AWA".into()],
            ..Default::default()
        });
        assert!(!f.passes(&mk("awa纯文字", 1, 0xFFFFFF)));
        // 正则:re: 前缀;无效正则静默丢弃,不影响放行。
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            rules: vec!["re:^\\d+$".into()],
            ..Default::default()
        });
        assert!(!f.passes(&mk("12345", 1, 0xFFFFFF)));
        assert!(f.passes(&mk("123abc", 1, 0xFFFFFF)));
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            rules: vec!["re:([bad".into()],
            ..Default::default()
        });
        assert!(f.passes(&mk("任意内容", 1, 0xFFFFFF)));
        // 用户 midHash:忽略大小写比对。
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            rules: vec!["u:ABCD1234".into()],
            ..Default::default()
        });
        let mut e = mk("正常内容", 1, 0xFFFFFF);
        assert!(f.passes(&e));
        e.mid_hash = "abcd1234".into();
        assert!(!f.passes(&e));
        // 类型屏蔽:滚动放行,顶部/底部屏蔽。
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            block_top: true,
            block_bottom: true,
            ..Default::default()
        });
        assert!(f.passes(&mk("滚动", 1, 0xFFFFFF)));
        assert!(!f.passes(&mk("顶部", 5, 0xFFFFFF)));
        assert!(!f.passes(&mk("底部", 4, 0xFFFFFF)));
        // 彩色屏蔽:仅非白色。
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            block_color: true,
            ..Default::default()
        });
        assert!(!f.passes(&mk("彩色", 1, 0x00FF00)));
        assert!(f.passes(&mk("白色", 1, 0xFFFFFF)));
        // 智能权重:阈值 6;weight=3 屏蔽,=6 放行,未下发(0)不受阈值影响。
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            weight_min: 6,
            ..Default::default()
        });
        let mut e = mk("低权重", 1, 0xFFFFFF);
        e.weight = 3;
        assert!(!f.passes(&e));
        e.weight = 6;
        assert!(f.passes(&e));
        e.weight = 0;
        assert!(f.passes(&e));
    }

    #[test]
    fn danmaku_filter_repeat_window_keeps_first_and_persists_across_batches() {
        let mk = |content: &str, ms: i32| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize: 22,
            color: 0xFFFFFF,
            content: content.into(),
            mid_hash: String::new(),
            weight: 0,
        };
        let f = CompiledDanmakuFilter::compile(&DanmakuFilter {
            block_repeat: true,
            ..Default::default()
        });
        let mut dedup = HashMap::new();
        // 4s 窗口内同文本重复:保留首条,窗口外放行。
        let kept = f.filter_batch(vec![mk("草", 1000), mk("草", 3000), mk("草", 9000)], &mut dedup);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].progress_ms, 1000);
        assert_eq!(kept[1].progress_ms, 9000);
        // 去重表跨批持久(分段预存链逐段到货),窗口以「最后保留条」为基准:
        // 距 9000ms 3s → 窗口内丢弃;距 9000ms 5s → 窗口外放行。
        let kept2 = f.filter_batch(vec![mk("草", 6000)], &mut dedup);
        assert!(kept2.is_empty());
        let kept3 = f.filter_batch(vec![mk("草", 14000)], &mut dedup);
        assert_eq!(kept3.len(), 1);
        // 默认态直通:不经过去重表,原样放行。
        let pass = CompiledDanmakuFilter::compile(&DanmakuFilter::default());
        assert!(pass.is_passthrough());
        let kept4 = pass.filter_batch(vec![mk("草", 6000)], &mut dedup);
        assert_eq!(kept4.len(), 1);
    }
}
