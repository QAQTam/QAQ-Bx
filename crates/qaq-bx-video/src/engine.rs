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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;

use windows::Win32::d3d11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::core::Interface;
use windows_canvas::{ColorF, GpuDevice, Rect, SwapChain};
use windows_text::{TextFormat, WordWrapping};

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
pub(crate) const RENDER_BUDGET: Duration = Duration::from_millis(8);
/// P1 resize 合并窗口:拖拽期间的真实 ResizeBuffers 至多每此间隔一次,
/// 静止后应用最终尺寸并重挂一次;拖拽过程 0 次重挂(消除卡顿主因)。
const RESIZE_COALESCE: Duration = Duration::from_millis(120);

/// 待挂载 swapchain 的原始 COM 指针:引擎侧 `IUnknown::into_raw` 转移所有权,
/// UI 侧 `IUnknown::from_raw` 接管(接手方 Drop 正确 Release,引用计数闭合)。
/// 两侧现同用 fork 的 windows-core 0.101;仍走裸指针是为跨线程传递 +
/// 与 canvas/reactor 各自的 bindings 类型解耦,不再是版本不互通。
pub(crate) struct SendRaw(*mut c_void);
unsafe impl Send for SendRaw {}
unsafe impl Sync for SendRaw {}

impl SendRaw {
    /// 包裹裸指针(视频通道与弹幕透明层共用挂载协议;所有权与 `into_raw` 对侧闭合)。
    pub(crate) fn new(raw: *mut c_void) -> Self {
        Self(raw)
    }

    pub(crate) fn into_raw(self) -> *mut c_void {
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

/// 面板 swapchain 挂载代际协调槽（VideoEngine 与 DanmakuStage 共用）：
/// 通过 `attach_gen` 单调代次隔离旧面板/Resize 前的迟到 completion 回调，
/// 并在面板卸载时同步复位 `attached`，防止全屏切换时出现偶发黑屏。
pub(crate) struct SurfaceMountSlot {
    pending: Mutex<Option<(u64, SendRaw)>>,
    attached: AtomicBool,
    attach_gen: AtomicU64,
}

impl SurfaceMountSlot {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(None),
            attached: AtomicBool::new(false),
            attach_gen: AtomicU64::new(1),
        })
    }

    pub(crate) fn current_gen(&self) -> u64 {
        self.attach_gen.load(Ordering::SeqCst)
    }

    /// 面板卸载：同步清 `attached` 并推进代次（作废旧面板在途 completion 回调）。
    pub(crate) fn invalidate_and_detach(&self) -> u64 {
        self.attached.store(false, Ordering::SeqCst);
        let next = self.attach_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut slot) = self.pending.lock()
            && slot.as_ref().is_some_and(|(gen_id, _)| *gen_id != next)
        {
            *slot = None;
        }
        next
    }

    /// ResizeBuffers 等引擎主动重挂：同步清 `attached` 并以新代次发布待挂载 swapchain。
    pub(crate) fn republish_new_gen(&self, raw: *mut c_void) -> u64 {
        self.attached.store(false, Ordering::SeqCst);
        let next = self.attach_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some((next, SendRaw::new(raw)));
        } else {
            drop(SendRaw::new(raw));
        }
        next
    }

    /// 仅当 `gen` 仍为当前有效代次时写入 `pending`（过期直接 drop 释放 COM 引用）。
    pub(crate) fn publish_if_current(&self, gen_id: u64, raw: *mut c_void) -> bool {
        let holder = SendRaw::new(raw);
        if self.attach_gen.load(Ordering::SeqCst) != gen_id {
            return false;
        }
        if let Ok(mut slot) = self.pending.lock() {
            if self.attach_gen.load(Ordering::SeqCst) != gen_id {
                return false;
            }
            *slot = Some((gen_id, holder));
            return true;
        }
        false
    }

    pub(crate) fn take_pending(&self) -> Option<(u64, *mut c_void)> {
        let (gen_id, chain) = self.pending.lock().ok()?.take()?;
        Some((gen_id, chain.into_raw()))
    }

    /// completion 回写：过期 `gen` 一律忽略并返回 `None`；当前 `gen` 返回 `Some(ok)`。
    pub(crate) fn mark_attached(&self, gen_id: u64, ok: bool) -> Option<bool> {
        if self.attach_gen.load(Ordering::SeqCst) != gen_id {
            return None;
        }
        if ok {
            self.attached.store(true, Ordering::SeqCst);
        }
        Some(ok)
    }

    pub(crate) fn is_attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
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

pub(crate) fn configure_swap_chain(chain: &mut SwapChain, scale_x: f32, scale_y: f32) {
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
    /// 缓存不足暂停缓冲(mpv paused-for-cache)。
    pub buffering: bool,
    /// 当前文件已完成解复用头解析(MPV_EVENT_FILE_LOADED)。
    pub file_loaded: bool,
    /// 已开始首帧重放或推进进度(MPV_EVENT_PLAYBACK_RESTART / time_pos > 0)。
    pub playback_started: bool,
    /// 最近一次已受理并下发 mpv loadfile 的单调装载代次。
    pub load_seq: u64,
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
    /// 轨道全满溢出策略(D1:丢弃=网页端口径 / 强放最早空出轨道)。
    pub danmaku_overflow: DanmakuOverflow,
    /// 弹幕显示密度(官方 dmdensity 口径,1.0=最密;越低碰撞容忍 A 越大、
    /// 上屏越稀)。clamp 0.1..=1.0。
    pub danmaku_density: f32,
    /// 弹幕显示区域(官方 dmarea 口径,%:可用高度=视频区高度×area/100,
    /// 字幕池弹幕豁免)。clamp 10..=100。
    pub danmaku_area: u8,
    /// 滚动速度倍率(官方 speedplus;1.0=官方默认手感,>1 更快)。clamp 0.5..=2.0。
    pub danmaku_speed_plus: f32,
    /// 防挡字幕(官方 preventshade:碰撞可用高度×0.85,底部 15% 让位)。
    pub danmaku_prevent_shade: bool,
}

/// 弹幕样式热更新参数(单一参数源;VideoEngine/DanmakuStage 同结构、同夹取口径)。
///
/// UI 侧由设置构建后下发;引擎线程 `normalized()` 后写回 VideoConfig 并按需重跑分配。
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuStyle {
    pub enabled: bool,
    pub opacity: f32,
    pub max_lanes: usize,
    pub font_family: String,
    pub font_scale: f32,
    /// 显示密度(官方 dmdensity;1.0=最密,越低碰撞容忍 A 越大)。
    pub density: f32,
    /// 显示区域(官方 dmarea,%;字幕池弹幕豁免)。
    pub area: u8,
    /// 滚动速度倍率(官方 speedplus;1.0=官方默认手感)。
    pub speed_plus: f32,
    /// 防挡字幕(官方 preventshade:可用高度×0.85)。
    pub prevent_shade: bool,
}

impl DanmakuStyle {
    /// 从引擎初始配置取当前值(启动播种/重播种复用)。
    pub fn from_config(cfg: &VideoConfig) -> Self {
        Self {
            enabled: cfg.danmaku_enabled,
            opacity: cfg.danmaku_opacity,
            max_lanes: cfg.danmaku_max_lanes,
            font_family: cfg.danmaku_font_family.clone(),
            font_scale: cfg.danmaku_font_scale,
            density: cfg.danmaku_density,
            area: cfg.danmaku_area,
            speed_plus: cfg.danmaku_speed_plus,
            prevent_shade: cfg.danmaku_prevent_shade,
        }
    }

    /// 与 settings.normalized 同口径的钳位(引擎侧自防御,不信任调用方)。
    pub fn normalized(mut self) -> Self {
        self.opacity = if self.opacity.is_finite() {
            self.opacity.clamp(0.1, 1.0)
        } else {
            0.92
        };
        self.max_lanes = self.max_lanes.clamp(1, 20);
        self.font_scale = if self.font_scale.is_finite() {
            self.font_scale.clamp(0.5, 2.0)
        } else {
            1.0
        };
        self.density = if self.density.is_finite() {
            self.density.clamp(0.1, 1.0)
        } else {
            1.0
        };
        self.area = self.area.clamp(10, 100);
        self.speed_plus = if self.speed_plus.is_finite() {
            self.speed_plus.clamp(0.5, 2.0)
        } else {
            1.0
        };
        self
    }

    /// 写回引擎配置(调用前必须 `normalized()`;字体族/字号变化由调用方重建格式)。
    pub(crate) fn apply_to(self, cfg: &mut VideoConfig) {
        cfg.danmaku_enabled = self.enabled;
        cfg.danmaku_opacity = self.opacity;
        cfg.danmaku_max_lanes = self.max_lanes;
        cfg.danmaku_font_family = self.font_family;
        cfg.danmaku_font_scale = self.font_scale;
        cfg.danmaku_density = self.density;
        cfg.danmaku_area = self.area;
        cfg.danmaku_speed_plus = self.speed_plus;
        cfg.danmaku_prevent_shade = self.prevent_shade;
    }
}

/// 轨道溢出策略(设计稿 D1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DanmakuOverflow {
    /// 丢弃(与 B 站网页一致:放不下不上屏)。
    #[default]
    Drop,
    /// findMin 强放:挑最早完全空出的轨道,允许有限重叠兜底(字太多的例外)。
    Force,
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
            danmaku_overflow: DanmakuOverflow::Drop,
            danmaku_density: 1.0,
            danmaku_area: 100,
            danmaku_speed_plus: 1.0,
            danmaku_prevent_shade: false,
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
        seq: u64,
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
    /// 运行时更新弹幕开关与绘制参数(单一参数源 [`DanmakuStyle`])。
    SetDanmakuConfig { style: DanmakuStyle },
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
    /// 面板卸载:同步复位 attached 并递增 gen,引擎线程据此决定是否把 swapchain 放回待挂载槽。
    Detach {
        gen_id: u64,
    },
    /// mpv update_callback 唤醒（新帧到达待合成）。
    FrameDirty,
    Quit,
}

/// 弹幕 overlay 条目(qaq-bx-core DanmakuElem 的引擎侧镜像,避免跨 crate 依赖)。
///
/// 零分配契约(#2):文本以 [`ContentRef`] 引用分段原始缓冲,只有进入过滤/绘制
/// 的条目才解码;`mid_hash` 为 CRC32 直解的 `u32`(v0.7 屏蔽同源)。
#[derive(Debug, Clone)]
pub struct OverlayDanmaku {
    /// 出现时刻(毫秒)。
    pub progress_ms: i32,
    /// 1=滚动 4=底部 5=顶部。
    pub mode: i32,
    pub fontsize: i32,
    /// 0xRRGGBB。
    pub color: u32,
    pub content: ContentRef,
    /// 分配结果(D1/D3):滚动=band 序号(0..3,y_off 见下),顶/底=驻留槽位
    /// (ingest 时分配;渲染 stateless,x/y 仍由 t 与 progress 推导)。
    pub lane: u16,
    /// 滚动弹幕打包 y(DIP):官方 3-band 连续 y 打包结果(含 offsetTop 基线),
    /// 渲染直接取用;顶/底驻留弹幕不使用(恒 0,y 由槽位推导)。
    pub y_off: f32,
    /// 估算文本宽度 DIP(准入判据与 x 推导共用;D2 排版缓存就绪前为估算值)。
    pub est_width: f32,
    /// 发送者 mid CRC32 hash(网页版用户屏蔽同源;None = 未下发/非法)。
    pub mid_hash: Option<u32>,
    /// 云屏蔽权重(0 = 未下发;越低越疑似垃圾)。
    pub weight: u32,
    /// 弹幕池(0=普通 1=字幕;1 号池在官方碰撞分配中豁免显示区域限制)。
    pub pool: u8,
}

/// 弹幕文本引用:分段原始 protobuf 缓冲的 `(off,len)` 切片(`Bytes` 引用计数,
/// clone 无拷贝);`text()` 上屏/过滤命中时才 lossy 解码为 UTF-16 供 D2D。
#[derive(Debug, Clone)]
pub struct ContentRef {
    data: Bytes,
    off: u32,
    len: u32,
}

impl ContentRef {
    pub fn new(data: Bytes, off: u32, len: u32) -> Self {
        Self { data, off, len }
    }

    pub fn bytes(&self) -> &[u8] {
        let start = (self.off as usize).min(self.data.len());
        let end = start
            .saturating_add(self.len as usize)
            .min(self.data.len());
        &self.data[start..end]
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.bytes())
    }
}

impl From<&str> for ContentRef {
    fn from(s: &str) -> Self {
        let data = Bytes::copy_from_slice(s.as_bytes());
        let len = data.len() as u32;
        Self { data, off: 0, len }
    }
}

impl From<String> for ContentRef {
    fn from(s: String) -> Self {
        let data = Bytes::from(s.into_bytes());
        let len = data.len() as u32;
        Self { data, off: 0, len }
    }
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
pub(crate) struct CompiledDanmakuFilter {
    cfg: DanmakuFilter,
    keywords: Vec<String>,
    regexes: Vec<regex::Regex>,
    users: Vec<u32>,
}

impl CompiledDanmakuFilter {
    pub(crate) fn compile(cfg: &DanmakuFilter) -> Self {
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
                // #2:midHash 规则编译期直解 u32(与元素侧同型),非法静默丢弃。
                if let Ok(v) = u32::from_str_radix(hash.trim(), 16) {
                    users.push(v);
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
            && e.mid_hash.is_some_and(|h| self.users.contains(&h))
        {
            return false;
        }
        if !self.keywords.is_empty() {
            let lower = e.content.text().to_lowercase();
            if self.keywords.iter().any(|kw| lower.contains(kw.as_str())) {
                return false;
            }
        }
        if !self.regexes.is_empty() {
            let text = e.content.text();
            if self.regexes.iter().any(|re| re.is_match(text.as_ref())) {
                return false;
            }
        }
        true
    }

    /// 批量过滤:先按 progress 升序(保证去重窗口语义:保留首条),再逐条
    /// 规则匹配 + 窗口去重;`dedup` 跨批持久(分段预存链逐段到货共用)。
    pub(crate) fn filter_batch(
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
                let key = e.content.text().to_lowercase();
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

// ---------- 轨道分配(设计稿 D1 滚动避让 + D3 顶/底驻留堆叠) ----------

/// 字号档位基准 em(与 `DanmakuFonts` 16/22/30 对齐)。
fn font_em(fontsize: i32) -> f32 {
    match fontsize {
        ..=20 => 16.0,
        21..=28 => 22.0,
        _ => 30.0,
    }
}

/// 估算文本宽度(DIP;D2 排版缓存就绪前的兜底,§5 允许 D1/D2 解耦):
/// 全宽字符 ≈ 1.0em、半宽 ≈ 0.55em。准入与绘制共用同一估算,
/// 保证「尾部过缘」时刻与 x 推导一致。
pub(crate) fn danmaku_est_width(fontsize: i32, font_scale: f32, text: &str) -> f32 {
    let scale = if font_scale.is_finite() {
        font_scale.clamp(0.5, 2.0)
    } else {
        1.0
    };
    let em = font_em(fontsize) * scale;
    let w: f32 = text
        .chars()
        .map(|c| {
            let u = c as u32;
            let full = matches!(
                u,
                0x1100..=0x11FF         // Hangul Jamo
                    | 0x2E80..=0xA4CF   // CJK 部首/符号/统一表意/谚文兼容
                    | 0xAC00..=0xD7A3   // Hangul 音节
                    | 0xF900..=0xFAFF   // CJK 兼容表意
                    | 0x00B7            // 间隔号 ·(Noto Sans SC 实测 1.0em)
                    | 0x2014..=0x2015   // 破折线 — ―(实测 — 0.894em)
                    | 0x2018..=0x201D   // 弯引号 ‘ ’ “ ”(实测 1.0em)
                    | 0x2025..=0x2026   // 两点引导/省略号 ‥ …(实测 1.0em)
                    | 0x2190..=0x2199   // 箭头(实测 ↑ 1.0em)
                    | 0x2600..=0x27BF   // 杂项符号/装饰 ★☆♥ 等(实测 ★ 1.0em)
                    | 0xFF00..=0xFF60   // 全角形式
                    | 0xFFE0..=0xFFE6   // 全角符号
                    | 0x20000..=0x2FA1F // CJK 扩展 B–F
                    | 0x30000..=0x3234F // CJK 扩展 G–H
            );
            if full {
                em
            } else {
                em * 0.55
            }
        })
        .sum();
    w.max(em)
}

/// 滚动弹幕打包起点 y(DIP;沿用渲染基线 8.0。官方 offsetTop 默认 4,为设置项)。
const BAND_OFFSET_TOP: f32 = 8.0;

/// 滚动弹幕打包行盒高:官方 DOM 口径 1.125×字号(em 已含档位与全局缩放)。
/// 行距不再是离散 30px 行距——小字号可填进大字号下方的空隙(官方 2D packing 观感)。
fn band_item_height(fontsize: i32, font_scale: f32) -> f64 {
    1.125 * f64::from(font_em(fontsize)) * f64::from(font_scale)
}

/// band 占用记录(官方 spaceManager band 列表项;按 (bottom,退场时刻) 有序)。
#[derive(Debug, Clone, Copy)]
struct BandOccupant {
    /// 占用 y 区间顶(DIP)。
    y: f64,
    /// 占用 y 区间底(y + 行盒高)。
    bottom: f64,
    /// 尾部越过右缘时刻(追尾判据 a;= spawn + 字宽/速度)。
    to_right: f64,
    /// 完全退场时刻(追尾判据 b 与过期剔除共用;= 官方 endTime)。
    to_left: f64,
}

/// 官方 hasDm:自 `from` 起扫 band 列表,存在 y 区间覆盖 [v,x] 的条目则为占用。
/// (列表按 bottom 升序:bottom < v 的在候选上方跳过,y > x 的已到候选下方中止。)
fn band_has_dm(list: &[BandOccupant], from: usize, v: f64, x: f64) -> bool {
    for s in list.get(from..).unwrap_or(&[]) {
        if s.bottom < v {
            continue;
        }
        if s.y > x {
            break;
        }
        return true;
    }
    false
}

/// 占用记录按 (bottom, 退场时刻) 有序插入(官方 binsert 口径)。
fn insert_occupant(
    band: &mut Vec<BandOccupant>,
    y: f64,
    item_h: f64,
    spawn: f64,
    est_width: f32,
    travel: &RollTravel,
) {
    let occ = BandOccupant {
        y,
        bottom: y + item_h,
        to_right: spawn + f64::from(est_width) / travel.speed,
        to_left: spawn + travel.duration,
    };
    let pos = band.partition_point(|s| (s.bottom, s.to_left) < (occ.bottom, occ.to_left));
    band.insert(pos, occ);
}

/// realloc 时点的播放头估计(只读:复用外推锚但不重锚;暂停/无锚退回 raw_t)。
/// 跳变治理口径:progress_ms/1000 < 该值 的条目视为飞行中,保留原 y 分配。
pub(crate) fn playhead_estimate(anchor: &Option<(f64, Instant)>, raw_t: f64, paused: bool) -> f64 {
    if paused {
        return raw_t;
    }
    match anchor {
        Some((t0, at)) if (raw_t - t0).abs() < TIMEPOS_EPSILON => {
            t0 + at.elapsed().as_secs_f64().min(TIMEPOS_EXTRAP_CAP)
        }
        _ => raw_t,
    }
}

/// ingest 统一重分配入口(D1 触发时机:分段到货 / 过滤变化 / 尺寸与样式变化)。
/// `playhead_secs`:当前播放头(秒);其之前的条目保留原分配不再重排(跳变治理)。
/// 全量重排(新视频/换 P)传 `f64::NEG_INFINITY`。
pub(crate) fn realloc_lanes(
    overlay: Vec<OverlayDanmaku>,
    cfg: &VideoConfig,
    dip_width: f32,
    dip_height: f32,
    playhead_secs: f64,
) -> Vec<OverlayDanmaku> {
    allocate_lanes(overlay, cfg, dip_width, dip_height, playhead_secs)
}

/// 轨道/槽位分配(官方判据移植;`docs/dev-notes/bilibili-danmaku-engine-research.md` §3.3/§3.4)。
///
/// 滚动(mode 1|2|3)=官方 spaceManager.add 的 3-band 连续 y 打包(2D packing):
///   - 时长:屏宽分段+Logistic(`roll_base_secs`),速度=(屏宽+字宽)/时长×speedPlus;
///   - 几何:屏外 50px 起笔、尾部越过左缘 50px 退场(`roll_travel`);
///   - 空间:band 0 为常规占用列(占用区间按 bottom 有序),候选 y 自 offsetTop
///     逐条走查——空隙(S.y > 候选底)直接放入;y 区间重叠时按追尾双判据
///     (对前条 S):(a) `spawn >= S.尾过右缘` (b) `本条头部到左缘 >= S.退场 + A`
///     放行同 y,否则 `v = S.bottom + 1` 连续下移(1px 步进,非离散轨道);
///   - 高度:候选 bottom ≤ g = min(preventShade?0.85:1 × 高, offsetTop+轨道上限×行距);
///   - area(显示区域%):非特权候选 bottom ≤ m = 高×area/100,越界整条丢弃
///     (官方口径:不试下一 band);字幕池(pool=1)特权豁免 area 且可溢出 band 1/2;
///   - 溢出策略只有丢弃,无强插路径(官方口径);`DanmakuOverflow::Force` 为
///     项目扩展:落 band 0 走查最深处并钳回可用高度内(≈旧 findMin 近似)。
///   - 行盒高 = 1.125×字号(官方 DOM 口径):小字号弹幕可填进大字号下方的空隙。
/// 顶/底(mode 4|5)按 `PIN_SECONDS` 驻留槽位堆叠,槽位受 area 约束(离散模型)。
/// 跳变治理:`playhead_secs` 之前出现(已起飞)的条目保留原 lane/y_off,并以其占用
/// 播种 band 列表与驻留槽——分段到货/样式热更不再引起可见弹幕跳位。
/// 渲染保持 stateless:lane/y_off/est_width 只是分配结果缓存,x 仍由 t 推导。
/// 返回保留条目(progress 升序;y_off/lane/est_width 已回填;溢出条目移除)。
pub(crate) fn allocate_lanes(
    mut items: Vec<OverlayDanmaku>,
    cfg: &VideoConfig,
    width: f32,
    height: f32,
    playhead_secs: f64,
) -> Vec<OverlayDanmaku> {
    let max_lanes = cfg.danmaku_max_lanes.clamp(1, 20);
    if items.is_empty() {
        return items;
    }
    items.sort_by_key(|e| e.progress_ms);
    let w = f64::from(width.max(1.0));
    let h = f64::from(height.max(1.0));
    let font_scale = if cfg.danmaku_font_scale.is_finite() {
        cfg.danmaku_font_scale.clamp(0.5, 2.0)
    } else {
        1.0
    };
    let density = if cfg.danmaku_density.is_finite() {
        f64::from(cfg.danmaku_density.clamp(0.1, 1.0))
    } else {
        1.0
    };
    let pitch = 30.0 * f64::from(font_scale);
    // 官方 spaceManager.add 高度口径:g = 可用高度下界(候选 bottom 上限),
    // preventShade ×0.85;再钉上「轨道上限」设置(项目扩展,官方无此帽)。
    let shade = if cfg.danmaku_prevent_shade { 0.85 } else { 1.0 };
    let g = (shade * h).min(f64::from(BAND_OFFSET_TOP) + max_lanes as f64 * pitch);
    // area 下界 m(非特权候选 bottom 上限;官方 m 用容器高,本项目 overlay 即视频区)。
    let area = u16::from(cfg.danmaku_area.clamp(10, 100));
    let area_bottom = h * f64::from(area) / 100.0;
    // 顶/底驻留槽上限(离散模型沿用:行距口径与 draw 侧一致)。
    let slots_cap = (((shade * h) * f64::from(area) / 100.0 - 8.0) / pitch).floor() as usize;
    let slots_cap = slots_cap.clamp(1, max_lanes);
    // 3 个 band 占用列表(band 0=常规主列;band 1/2=特权溢出)与驻留槽释放时刻。
    let mut bands: [Vec<BandOccupant>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut pin_end_top: Vec<f64> = Vec::new();
    let mut pin_end_bottom: Vec<f64> = Vec::new();

    // 跳变治理:progress < 播放头的条目(已起飞/已过期)保留原分配,以其占用
    // 播种占用表——后打包条目绕开飞行中的弹幕,可见层不重排。
    let mut kept: Vec<OverlayDanmaku> = Vec::with_capacity(items.len());
    let mut ahead: Vec<OverlayDanmaku> = Vec::with_capacity(items.len());
    for e in items {
        let spawn = f64::from(e.progress_ms.max(0)) / 1000.0;
        // 严格 <:spawn 瞬间的弹幕还在屏外右缘(elapsed≈0),重排不可见;
        // 且 playhead=0(未起播)时 progress_ms=0 的条目也必须参与打包。
        if spawn < playhead_secs {
            match e.mode {
                1 | 2 | 3 => {
                    let lane = usize::from(e.lane);
                    if lane < 3 && e.y_off > 0.0 && e.est_width > 0.0 {
                        let travel =
                            roll_travel(f64::from(e.est_width), w, cfg.danmaku_speed_plus);
                        let y = f64::from(e.y_off);
                        bands[lane].push(BandOccupant {
                            y,
                            bottom: y + band_item_height(e.fontsize, font_scale),
                            to_right: spawn + f64::from(e.est_width) / travel.speed,
                            to_left: spawn + travel.duration,
                        });
                    }
                }
                4 | 5 => {
                    let ends = if e.mode == 5 {
                        &mut pin_end_top
                    } else {
                        &mut pin_end_bottom
                    };
                    let slot = usize::from(e.lane);
                    if slot >= ends.len() {
                        ends.resize(slot + 1, 0.0);
                    }
                    ends[slot] = ends[slot].max(spawn + PIN_SECONDS);
                }
                _ => {}
            }
            kept.push(e);
        } else {
            ahead.push(e);
        }
    }
    for band in &mut bands {
        band.sort_by(|a, b| a.bottom.total_cmp(&b.bottom).then(a.to_left.total_cmp(&b.to_left)));
    }

    // 官方 spaceManager.add:逐条时间轴模拟打包(ahead 已升序)。
    for mut e in ahead {
        e.est_width = danmaku_est_width(e.fontsize, font_scale, &e.content.text());
        let spawn = f64::from(e.progress_ms.max(0)) / 1000.0;
        let placed = match e.mode {
            1 | 2 | 3 => {
                let travel = roll_travel(f64::from(e.est_width), w, cfg.danmaku_speed_plus);
                let a_tol = (travel.duration * (1.0 - density) / 2.0).floor() + 0.1;
                // 本条头部到左缘的时刻(含官方右侧 50px 跑道;追尾判据 b 左端)。
                let head_left_at = spawn + (w + ROLL_SAFE_PX) / travel.speed;
                // 字幕池弹幕=官方特权口径(豁免 area、可溢出 band 1/2)。
                let privileged = e.pool == 1;
                let item_h = band_item_height(e.fontsize, font_scale);
                // 官方 destroy=endTime 过期即移出占用表(等价语义)。
                for band in &mut bands {
                    band.retain(|s| s.to_left > spawn);
                }
                let mut chosen: Option<(usize, f64)> = None;
                let mut deepest = f64::from(BAND_OFFSET_TOP);
                'bands: for b in 0..3 {
                    let list = &bands[b];
                    let mut v = f64::from(BAND_OFFSET_TOP);
                    let mut x = v + item_h;
                    if list.is_empty() {
                        chosen = Some((b, v));
                        break 'bands;
                    }
                    for (si, s) in list.iter().enumerate() {
                        if x > g {
                            break;
                        }
                        if s.bottom <= v {
                            continue; // 前条在候选上方,不挡。
                        }
                        if s.y > x {
                            // 候选整段落在前条上方:空隙,放入。
                            chosen = Some((b, v));
                            break 'bands;
                        }
                        // y 区间重叠:追尾双判据(官方 t.x>S.x+S.w && t.middle>S.end+A)。
                        if spawn >= s.to_right && head_left_at >= s.to_left + a_tol {
                            if x <= s.bottom && !band_has_dm(list, si + 1, v, x) {
                                chosen = Some((b, v));
                                break 'bands;
                            }
                            continue;
                        }
                        // 走查下移(官方 v = S.bottom + 1,1px 连续步进)。
                        v = s.bottom + 1.0;
                        x = v + item_h;
                        deepest = deepest.max(v);
                        if !privileged && x > area_bottom {
                            // 官方:area 下界越界整条丢弃(不试下一 band)。
                            break 'bands;
                        }
                    }
                    if x <= g {
                        // 走查耗尽仍可用:落在最后候选位(可与追尾放行的前条同 y)。
                        chosen = Some((b, v));
                        break 'bands;
                    }
                    if !privileged {
                        break 'bands;
                    }
                    // 特权弹幕 band 0 走满仍放不下 → 溢出下一 band(官方口径)。
                }
                match chosen {
                    Some((b, y)) => {
                        insert_occupant(&mut bands[b], y, item_h, spawn, e.est_width, &travel);
                        e.lane = b as u16;
                        e.y_off = y as f32;
                        true
                    }
                    None if cfg.danmaku_overflow == DanmakuOverflow::Force => {
                        // 项目扩展:官方无强插路径;Force 落 band 0 走查最深处,
                        // 钳回可用高度内(≈旧 findMin 的"最早空出行"近似)。
                        let y = deepest.min(g - item_h).max(f64::from(BAND_OFFSET_TOP));
                        insert_occupant(&mut bands[0], y, item_h, spawn, e.est_width, &travel);
                        e.lane = 0;
                        e.y_off = y as f32;
                        true
                    }
                    None => false,
                }
            }
            4 | 5 => {
                let slots_for_item = if e.pool == 1 { max_lanes } else { slots_cap };
                let ends = if e.mode == 5 {
                    &mut pin_end_top
                } else {
                    &mut pin_end_bottom
                };
                let free = ends.iter().position(|&end| spawn >= end);
                let chosen = match free {
                    Some(i) => Some(i),
                    None if ends.len() < slots_for_item => {
                        ends.push(0.0);
                        Some(ends.len() - 1)
                    }
                    None if cfg.danmaku_overflow == DanmakuOverflow::Force => ends
                        .iter()
                        .enumerate()
                        .min_by(|&(ia, &ea), &(ib, &eb)| {
                            ea.partial_cmp(&eb)
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then(ia.cmp(&ib))
                        })
                        .map(|(i, _)| i),
                    None => None,
                };
                if let Some(i) = chosen {
                    ends[i] = spawn + PIN_SECONDS;
                    e.lane = i as u16;
                }
                chosen.is_some()
            }
            // 其他 mode 原样保留(渲染跳过),不参与分配。
            _ => true,
        };
        if placed {
            kept.push(e);
        }
    }
    kept
}

/// 视频引擎句柄(线程安全;UI 侧克隆共享)。
pub struct VideoEngine {
    cmd_tx: Sender<EngineCmd>,
    /// swapchain 挂载代际协调槽(UI 线程取走挂载;失败放回;卸载递增代次防迟到回写)。
    mount: Arc<SurfaceMountSlot>,
    load_seq: Arc<AtomicU64>,
    status: Arc<Mutex<PlaybackStatus>>,
    error: Arc<Mutex<Option<String>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

// UI 只比较句柄同一性(reactor Input 约束需要 PartialEq)。
impl PartialEq for VideoEngine {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mount, &other.mount)
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
        let mount = SurfaceMountSlot::new();
        let load_seq = Arc::new(AtomicU64::new(0));
        let status = Arc::new(Mutex::new(PlaybackStatus::default()));
        let error = Arc::new(Mutex::new(None));
        let thread = {
            let mount = mount.clone();
            let status = status.clone();
            let error = error.clone();
            std::thread::Builder::new()
                .name("video-engine".into())
                .spawn(move || engine_thread(wakeup_tx, cmd_rx, mount, status, error, config))
                .ok()
        };
        Arc::new(Self {
            cmd_tx,
            mount,
            load_seq,
            status,
            error,
            thread: Mutex::new(thread),
        })
    }

    // ---- UI → 引擎命令(发送失败仅线程已死,静默) ----

    /// 加载 B 站分轨(loadfile replace;默认自动播放),返回本次单调装载世代。
    pub fn load(&self, video_url: &str, audio_url: Option<&str>) -> u64 {
        self.load_with_options(video_url, audio_url, 0.0, true)
    }

    /// 加载 B 站分轨并指定起播位置与是否自动播放,返回本次单调装载世代。
    pub fn load_with_options(
        &self,
        video_url: &str,
        audio_url: Option<&str>,
        start_secs: f64,
        autoplay: bool,
    ) -> u64 {
        let seq = self.load_seq.fetch_add(1, Ordering::SeqCst) + 1;
        _ = self.cmd_tx.send(EngineCmd::Load {
            seq,
            video_url: video_url.to_string(),
            audio_url: audio_url.map(str::to_string),
            start_secs: start_secs.max(0.0),
            autoplay,
        });
        seq
    }

    /// 最近一次发起 `load` 的单调代次。
    pub fn load_seq(&self) -> u64 {
        self.load_seq.load(Ordering::SeqCst)
    }

    /// 全片弹幕(SetDanmaku;排序由调用方保证)。
    pub fn set_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmaku { items });
    }

    /// 增量追加弹幕分段(预存链;引擎侧归并回 progress 升序)。
    pub fn append_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(EngineCmd::AppendDanmaku { items });
    }

    /// 动态调整弹幕样式(开关/透明度/轨道/字体/密度/区域/速度/防挡字幕;
    /// 单一参数源 [`DanmakuStyle`],引擎线程 normalized 后写回配置)。
    pub fn set_danmaku_config(&self, style: DanmakuStyle) {
        _ = self.cmd_tx.send(EngineCmd::SetDanmakuConfig { style });
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

    /// 面板卸载(VideoSurface cleanup):同步清 `attached` 并推进代次,
    /// 拦截退役面板的迟到 completion 回写,然后通知引擎线程重新投递 swapchain。
    pub fn detach_panel(&self) {
        let gen_id = self.mount.invalidate_and_detach();
        _ = self.cmd_tx.send(EngineCmd::Detach { gen_id });
    }

    // ---- 挂载协调(UI 线程专用) ----

    /// 取走待挂载 swapchain 原始指针及其代次(接管所有权;无则 None)。UI 侧经
    /// `IUnknown::from_raw` 接管。
    pub fn take_pending(&self) -> Option<(u64, *mut c_void)> {
        self.mount.take_pending()
    }

    /// 挂载请求入队失败或面板瞬态不可用(`Unavailable`):仅当 `gen_id` 未过期时
    /// 放回指针重试(过期则 drop 释放 COM 引用)。
    pub fn put_back_pending(&self, gen_id: u64, raw: *mut c_void) {
        let _ = self.mount.publish_if_current(gen_id, raw);
    }

    /// 挂载 completion 回写(仅当前 `gen_id` 生效:成功 → attached + 唤醒首帧;
    /// 真实错误 → 记错误;过期回调静默丢弃)。
    pub fn mark_attached(&self, gen_id: u64, ok: bool) {
        match self.mount.mark_attached(gen_id, ok) {
            Some(true) => {
                _ = self.cmd_tx.send(EngineCmd::FrameDirty);
            }
            Some(false) => {
                self.set_error("swapchain 挂载失败(SetSwapChain)");
            }
            None => {}
        }
    }

    pub fn attached(&self) -> bool {
        self.mount.is_attached()
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

pub(crate) struct DanmakuFonts {
    small: TextFormat,
    medium: TextFormat,
    large: TextFormat,
}

impl DanmakuFonts {
    pub(crate) fn new() -> Self {
        Self::with(DANMAKU_DEFAULT_FONT, 1.0)
    }

    /// 按字体族与字号缩放构建(F3);轨道行高与定宽估算随缩放联动。
    pub(crate) fn with(family: &str, scale: f32) -> Self {
        let scale = if scale.is_finite() {
            scale.clamp(0.5, 2.0)
        } else {
            1.0
        };
        // 单行不变量:弹幕永远单行绘制,禁用折行。est_width 若略小于
        // DWrite 实际排版宽,应表现为「尾部溢出」而非折成第二行——
        // 默认 Wrap + DrawText 无 CLIP 时折行会叠进下一轨道
        // (2026-11-22 弹幕折行排查根因)。
        Self {
            small: TextFormat::new(family, 16.0 * scale)
                .with_word_wrapping(WordWrapping::NoWrap),
            medium: TextFormat::new(family, 22.0 * scale)
                .with_word_wrapping(WordWrapping::NoWrap),
            large: TextFormat::new(family, 30.0 * scale)
                .with_word_wrapping(WordWrapping::NoWrap),
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
    mount: Arc<SurfaceMountSlot>,
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
            let gen_id = mount.current_gen();
            let _ = mount.publish_if_current(gen_id, raw);
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

    // 6) 属性观察 → 状态快照(time-pos/duration/pause/eof/paused-for-cache)。
    if let Err(e) = observe_status(&mut player, status.clone(), error.clone()) {
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
    // 诊断:render_present 失败限频日志节拍(全屏白屏排查 2026-11-22)。
    let mut last_render_err_log: Option<Instant> = None;
    let mut active_load_seq: u64 = 0;
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
                        seq,
                        video_url,
                        audio_url,
                        start_secs,
                        autoplay,
                    } => {
                        active_load_seq = seq;
                        if let Ok(mut e) = error.lock() {
                            *e = None;
                        }
                        if let Ok(mut s) = status.lock() {
                            *s = PlaybackStatus {
                                load_seq: seq,
                                ..PlaybackStatus::default()
                            };
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
                        // D1:ingest 出口统一轨道/槽位分配(升序重排在分配内完成);
                        // 全量替换=新视频语义,无飞行条目,全量重排。
                        overlay = realloc_lanes(
                            std::mem::take(&mut overlay),
                            &cfg,
                            width as f32 / scale_x,
                            height as f32 / scale_y,
                            f64::NEG_INFINITY,
                        );
                        need_repaint = true;
                    }
                    EngineCmd::AppendDanmaku { items } => {
                        let kept = dm_filter.filter_batch(items, &mut dm_dedup);
                        if !kept.is_empty() {
                            overlay.extend(kept);
                            // D1:分段到货触发重排;跳变治理:已起飞条目保留原
                            // y 分配,仅未出现的条目参与 packing(O(n) 单遍毫秒级)。
                            let (ph_paused, ph_raw) = match status.lock() {
                                Ok(s) => (s.pause, s.time_pos),
                                Err(_) => (false, 0.0),
                            };
                            overlay = realloc_lanes(
                                std::mem::take(&mut overlay),
                                &cfg,
                                width as f32 / scale_x,
                                height as f32 / scale_y,
                                playhead_estimate(&dm_time_anchor, ph_raw, ph_paused),
                            );
                            need_repaint = true;
                        }
                    }
                    EngineCmd::SetDanmakuFilter { filter } => {
                        dm_filter = CompiledDanmakuFilter::compile(&filter);
                        // 规则变化:已缓存弹幕全量重滤(升序重跑,保留窗口内首条)。
                        dm_dedup.clear();
                        let items = std::mem::take(&mut overlay);
                        // D1:重滤后重跑分配;跳变治理同上(可见层不重排)。
                        let (ph_paused, ph_raw) = match status.lock() {
                            Ok(s) => (s.pause, s.time_pos),
                            Err(_) => (false, 0.0),
                        };
                        overlay = realloc_lanes(
                            dm_filter.filter_batch(items, &mut dm_dedup),
                            &cfg,
                            width as f32 / scale_x,
                            height as f32 / scale_y,
                            playhead_estimate(&dm_time_anchor, ph_raw, ph_paused),
                        );
                        need_repaint = true;
                    }
                    EngineCmd::SetDanmakuConfig { style } => {
                        // 字体族/字号变化 → 重建 DWrite 格式(F3);normalized 防
                        // 手工编辑 settings.json 的越界值(与 settings.normalized 同口径)。
                        let style = style.normalized();
                        let font_changed = cfg.danmaku_font_family != style.font_family
                            || (cfg.danmaku_font_scale - style.font_scale).abs() > f32::EPSILON;
                        if font_changed {
                            fonts = DanmakuFonts::with(&style.font_family, style.font_scale);
                        }
                        style.apply_to(&mut cfg);
                        // D1:轨道数/字号/密度/区域/速度/防挡字幕均影响准入与
                        // 行高,变化即重跑分配。
                        if !overlay.is_empty() {
                            let (ph_paused, ph_raw) = match status.lock() {
                                Ok(s) => (s.pause, s.time_pos),
                                Err(_) => (false, 0.0),
                            };
                            overlay = realloc_lanes(
                                std::mem::take(&mut overlay),
                                &cfg,
                                width as f32 / scale_x,
                                height as f32 / scale_y,
                                playhead_estimate(&dm_time_anchor, ph_raw, ph_paused),
                            );
                        }
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
                        if let Ok(mut e) = error.lock() {
                            *e = None;
                        }
                        if let Ok(mut s) = status.lock() {
                            *s = PlaybackStatus {
                                load_seq: active_load_seq,
                                ..PlaybackStatus::default()
                            };
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
                    EngineCmd::Detach { gen_id } => {
                        if let Ok(raw) = surface.chain.into_unknown_raw() {
                            let _ = mount.publish_if_current(gen_id, raw);
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
                    // D1 触发时机③:宽度/缩放变化影响准入窗口,重跑分配。
                    if !overlay.is_empty() {
                        let (ph_paused, ph_raw) = match status.lock() {
                            Ok(s) => (s.pause, s.time_pos),
                            Err(_) => (false, 0.0),
                        };
                        overlay = realloc_lanes(
                            std::mem::take(&mut overlay),
                            &cfg,
                            width as f32 / scale_x,
                            height as f32 / scale_y,
                            playhead_estimate(&dm_time_anchor, ph_raw, ph_paused),
                        );
                    }
                    configure_swap_chain(&mut surface.chain, scale_x, scale_y);
                    need_repaint = true;
                    eprintln!(
                        "ENG-RS: resize {}x{} (dip {}x{}) video_format={:?} gen={}",
                        metrics.width,
                        metrics.height,
                        width as f32 / scale_x,
                        height as f32 / scale_y,
                        status.lock().ok().map(|s| s.video_format.clone()).unwrap_or_default(),
                        mount.current_gen(),
                    );
                    if let Ok(raw) = surface.chain.into_unknown_raw() {
                        let _ = mount.republish_new_gen(raw);
                    }
                }
            } else if scale_changed {
                scale_x = metrics.scale_x;
                scale_y = metrics.scale_y;
                configure_swap_chain(&mut surface.chain, scale_x, scale_y);
                need_repaint = true;
            }
        }

        let is_attached = mount.is_attached();
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
                // 诊断:此错误原先被静默吞掉(仅重绘节流),全屏白屏排查 2026-11-22
                // 需要现场证据 —— 限频 1s 打到 stderr(打包环境丢失,直跑调试可见)。
                let now = Instant::now();
                if last_render_err_log.map(|t| now.duration_since(t) >= Duration::from_secs(1)).unwrap_or(true) {
                    last_render_err_log = Some(now);
                    eprintln!("ENG-R: render_present 失败({width}x{height}):{e:?}");
                }
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

/// 滚动弹幕基准时长分段表(官方 `tz()`,npd.491 抓取研究 RESEARCH.md §3.3):
/// `(屏宽上界, maxTime, minTime)`;命中规则=第一个 `屏宽 ≤ 上界` 的段的**前一段**。
const ROLL_TIME_BRACKETS: [(f64, f64, f64); 8] = [
    (558.0, 13.0, 7.0),
    (888.0, 14.0, 8.0),
    (1082.0, 14.0, 9.0),
    (1246.0, 15.0, 10.0),
    (1452.0, 16.0, 11.0),
    (1744.0, 18.0, 13.0),
    (2314.0, 20.0, 14.0),
    (2560.0, 22.0, 16.0),
];
/// 屏宽超出最大段时每 500px 追加的时长(官方同款)。
const ROLL_BRACKET_PX: f64 = 500.0;
/// 官方 `defaultDuration` 兜底:屏宽 <558 或参数非法时恒定 4.5s。
const ROLL_FALLBACK_SECS: f64 = 4.5;
/// 官方 SAFE_LEFT/RIGHT(秒级几何余量,px):弹幕从屏外 50px 起笔、尾部越过
/// 左缘 50px 才算退场——准入判据与 x 推导共用,保证「尾部过缘」时刻一致。
const ROLL_SAFE_PX: f64 = 50.0;
/// 底部/顶部弹幕驻留时长(秒)。
const PIN_SECONDS: f64 = 4.0;
/// 滚动弹幕默认轨道数。
const ROLL_LANES: usize = 10;
/// time-pos 视为「未更新」的容差(秒;小于相邻两次属性更新的典型步进)。
const TIMEPOS_EPSILON: f64 = 0.020;
/// 墙钟外推上限(秒;属性停更/卡顿时冻结,防漂移)。
const TIMEPOS_EXTRAP_CAP: f64 = 0.25;

/// 滚动弹幕基准时长(官方 `tz()` 移植;`docs/dev-notes/bilibili-danmaku-engine-research.md` §3.3)。
///
/// 屏宽分段 `[minTime, maxTime]` 内,以 Logistic 曲线把「字宽/屏宽比」映射为
/// 时长:常规文本实际≈段内 minTime(1920 屏≈13.1s),超长弹幕才向 maxTime 弯折;
/// 屏宽 <558(官方小窗口径)或参数非法回退 4.5s。分配与渲染共用,换屏宽时
/// 由 realloc 重算。
pub(crate) fn roll_base_secs(dm_width: f64, screen_width: f64) -> f64 {
    if !dm_width.is_finite() || !screen_width.is_finite() || screen_width < ROLL_TIME_BRACKETS[0].0 {
        return ROLL_FALLBACK_SECS;
    }
    let dm_width = dm_width.max(1.0);
    let last = ROLL_TIME_BRACKETS.last().expect("分段表非空");
    let (max_time, min_time) = if screen_width > last.0 {
        // 官方:超出最大段每 500px 各 +1s(4K 屏更慢的手感来源)。
        let extra = ((screen_width - last.0) / ROLL_BRACKET_PX).floor();
        (last.1 + extra, last.2 + extra)
    } else {
        // 命中规则:第一个 `屏宽 ≤ 上界` 的段取其前一段(min/max)。
        let mut picked = (last.1, last.2);
        for pair in ROLL_TIME_BRACKETS.windows(2) {
            if screen_width <= pair[1].0 {
                picked = (pair[0].1, pair[0].2);
                break;
            }
        }
        picked
    };
    let span = max_time - min_time;
    if span <= 0.0 {
        return min_time;
    }
    // Logistic 中点(官方公式):u=(a-s)/0.1-1, l=ln(u)/0.2+0.2。
    let u = span / 0.1 - 1.0;
    if u <= 0.0 {
        return min_time;
    }
    let mid = u.ln() / 0.2 + 0.2;
    let secs = min_time + span / (1.0 + (-0.2 * (dm_width / screen_width - mid)).exp());
    if secs.is_finite() && secs > 0.0 {
        secs
    } else {
        ROLL_FALLBACK_SECS
    }
}

/// 滚动弹幕运动学(官方几何):`speed=(屏宽+字宽)/base×speedPlus`,从屏外
/// `ROLL_SAFE_PX` 起笔,尾部越过左缘 `ROLL_SAFE_PX` 才算退场。准入与渲染共用,
/// 保证「尾部过缘」时刻与 x 推导一致。
pub(crate) struct RollTravel {
    /// 恒速 px/s(duration 模型:时长定速度,字越宽跑得越快)。
    pub speed: f64,
    /// 屏外起笔到完全退场的总时长(秒;含双侧 50px 跑道)。
    pub duration: f64,
}

pub(crate) fn roll_travel(dm_width: f64, screen_width: f64, speed_plus: f32) -> RollTravel {
    let w = if screen_width.is_finite() && screen_width > 0.0 {
        screen_width
    } else {
        1.0
    };
    let tw = if dm_width.is_finite() && dm_width > 0.0 {
        dm_width
    } else {
        1.0
    };
    let base = roll_base_secs(tw, w);
    let plus = if speed_plus.is_finite() {
        f64::from(speed_plus.clamp(0.5, 2.0))
    } else {
        1.0
    };
    let speed = (w + tw) / base * plus;
    let duration = (w + ROLL_SAFE_PX + tw + ROLL_SAFE_PX) / speed;
    RollTravel { speed, duration }
}

/// 渲染扫描窗口上界(秒):最宽基准(分段 maxTime)× 跑道系数 ÷ 最低倍速,
/// 覆盖任意字宽的 duration 上界,窗口裁剪不漏条。
pub(crate) fn roll_window_secs(screen_width: f64, speed_plus: f32) -> f64 {
    let w = if screen_width.is_finite() && screen_width > 0.0 {
        screen_width
    } else {
        1.0
    };
    let plus = if speed_plus.is_finite() {
        f64::from(speed_plus.clamp(0.5, 2.0))
    } else {
        1.0
    };
    // maxTime(含超界段 +1s/500px)为 base 上界;跑道系数 ≤ 1+2×SAFE/W。
    let last = ROLL_TIME_BRACKETS.last().expect("分段表非空");
    let extra = if w > last.0 {
        ((w - last.0) / ROLL_BRACKET_PX).floor()
    } else {
        0.0
    };
    let base_max = last.1 + extra;
    base_max * (1.0 + 2.0 * ROLL_SAFE_PX / w) / plus
}

/// 弹幕时基:mpv time-pos 随视频帧步进,直接取值会让滚动锯齿化。
/// 播放中在两次属性更新之间用墙钟线性外推;`raw_t` 与锚值差在容差内视为
/// 同值(未更新)则外推,否则重锚。锚定时刻用饱和差,供测试注入虚拟时刻。
pub(crate) fn interpolated_danmaku_time(
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
pub(crate) fn draw_danmaku_overlay(
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
    let alpha = cfg.danmaku_opacity.clamp(0.1, 1.0);
    let shadow_alpha = (0.85 * alpha).clamp(0.1, 0.9);
    // 行高/估算宽随字号缩放(F3),保证大字号弹幕不重叠。
    let font_scale = if cfg.danmaku_font_scale.is_finite() {
        cfg.danmaku_font_scale.clamp(0.5, 2.0)
    } else {
        1.0
    };
    let speed_plus = cfg.danmaku_speed_plus;
    let char_width = 22.0 * font_scale;
    let text_slot_h = 34.0 * font_scale;
    // 窗口起点(排序数据二分;[t-滚动窗口上界, t] 的滚动 + [t-PIN, t] 的顶底)。
    let window = roll_window_secs(f64::from(width_f), speed_plus);
    let start = ((t - window) * 1000.0) as i32;
    let begin = items.partition_point(|e| e.progress_ms < start);
    for elem in &items[begin.min(items.len())..] {
        let progress = f64::from(elem.progress_ms) / 1000.0;
        if progress > t {
            break; // 排序后早退
        }
        let elapsed = t - progress;
        // #2:解码收拢到上屏点——每帧仅对时间窗内条目做一次 lossy。
        let content = elem.content.text();
        let (x, y) = match elem.mode {
            1 | 2 | 3 => {
                // 官方滚动几何(与 allocate_lanes 共用 roll_travel):屏外
                // SAFE_RIGHT 起笔,恒速横穿,尾部越过左缘 SAFE_RIGHT 退场。
                let travel = roll_travel(
                    f64::from(elem.est_width.max(1.0)),
                    f64::from(width_f),
                    speed_plus,
                );
                if elapsed > travel.duration {
                    continue;
                }
                let x = width_f as f64 + ROLL_SAFE_PX - travel.speed * elapsed;
                (x as f32, elem.y_off)
            }
            4 | 5 => {
                if elapsed > PIN_SECONDS {
                    continue;
                }
                // D3:驻留槽位堆叠(顶自上而下 / 底自下而上),替换固定 y 互叠。
                let slot = f32::from(elem.lane);
                let x = (width_f - elem.est_width.max(char_width)).max(8.0) / 2.0;
                let y = match elem.mode {
                    4 => (height_f - (slot + 1.0) * text_slot_h - 14.0).max(0.0),
                    _ => 8.0 + slot * text_slot_h,
                };
                (x, y)
            }
            _ => continue,
        };
        // 画框随估算宽度伸缩(旧实现固定 600px,长文裁切/短文留白)。
        let draw_w = elem.est_width.max(char_width);
        let format = match elem.fontsize {
            ..=20 => &small,
            21..=28 => &medium,
            _ => &large,
        };
        let rect = Rect {
            left: x,
            top: y,
            right: x + draw_w,
            bottom: y + text_slot_h,
        };
        // 描边(近似):黑底四向偏移各画一次。
        let Ok(shadow) = session.create_solid_brush(ColorF::new(0.0, 0.0, 0.0, shadow_alpha))
        else {
            continue;
        };
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            session.draw_text(
                &content,
                format,
                &Rect {
                    left: x + dx,
                    top: y + dy,
                    right: x + dx + draw_w,
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
            session.draw_text(&content, format, &rect, &brush);
        }
    }
}

/// 观察 time-pos/duration/pause/eof-reached/paused-for-cache → 状态快照。
fn observe_status(
    player: &mut MpvPlayer,
    status: Arc<Mutex<PlaybackStatus>>,
    error: Arc<Mutex<Option<String>>>,
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
    // 缓冲中状态:缺缓冲暂停时必须驱动 UI 恢复「视频加载中…」动画。
    player.observe_property(8, "paused-for-cache", MPV_FORMAT_FLAG)?;
    player.start_event_thread(Arc::new(move |event| apply_mpv_event(&status, &error, event)))?;
    Ok(())
}

fn apply_mpv_event(
    status: &Arc<Mutex<PlaybackStatus>>,
    error: &Arc<Mutex<Option<String>>>,
    event: MpvEvent,
) {
    const MPV_END_FILE_REASON_EOF: i32 = 0;
    const MPV_END_FILE_REASON_ERROR: i32 = 4;

    match event {
        MpvEvent::FileLoaded => {
            if let Ok(mut s) = status.lock() {
                s.file_loaded = true;
            }
        }
        MpvEvent::PlaybackRestart => {
            if let Ok(mut s) = status.lock() {
                s.playback_started = true;
                s.buffering = false;
            }
        }
        MpvEvent::PropertyChange { name, value } => {
            let Ok(mut s) = status.lock() else { return };
            match (name.as_str(), value) {
                ("time-pos", PropertyValue::Double(v)) => {
                    s.time_pos = v.max(0.0);
                    if s.time_pos > 0.0 {
                        s.playback_started = true;
                    }
                }
                ("duration", PropertyValue::Double(v)) => s.duration = v.max(0.0),
                ("pause", PropertyValue::Flag(v)) => s.pause = v,
                ("eof-reached", PropertyValue::Flag(v)) => s.eof = v,
                ("paused-for-cache", PropertyValue::Flag(v)) => s.buffering = v,
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
        // keep-open:EOF 停在末帧;STOP(切源/replace) 不视为播放完毕也不报错;
        // ERROR(网络/解码失败) 必须写入错误槽使 UI 收起卡死转圈并提示/回退。
        MpvEvent::EndFile { reason, error: code } => {
            if reason == MPV_END_FILE_REASON_EOF {
                if let Ok(mut s) = status.lock() {
                    s.eof = true;
                    s.buffering = false;
                }
            } else if reason == MPV_END_FILE_REASON_ERROR {
                if let Ok(mut s) = status.lock() {
                    s.buffering = false;
                    s.file_loaded = false;
                    s.playback_started = false;
                }
                if let Ok(mut slot) = error.lock() {
                    *slot = Some(format!("mpv 播放失败(code={code})"));
                }
            }
        }
        _ => {}
    }
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
                lane: 0,
                y_off: 0.0,
                est_width: 0.0,
                mid_hash: None,
                weight: 0,
                pool: 0,
            },
            OverlayDanmaku {
                progress_ms: 1000,
                mode: 4,
                fontsize: 22,
                color: 0xFFFFFF,
                content: "底端".into(),
                lane: 0,
                y_off: 0.0,
                est_width: 0.0,
                mid_hash: None,
                weight: 0,
                pool: 0,
            },
            OverlayDanmaku {
                progress_ms: 1000,
                mode: 5,
                fontsize: 22,
                color: 0xFFFFFF,
                content: "顶端".into(),
                lane: 0,
                y_off: 0.0,
                est_width: 0.0,
                mid_hash: None,
                weight: 0,
                pool: 0,
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
                lane: 0,
                y_off: 0.0,
                est_width: 0.0,
                mid_hash: None,
                weight: 0,
                pool: 0,
            },
            OverlayDanmaku {
                progress_ms: 500,
                mode: 4,
                fontsize: 25,
                color: 0xFFFFFF,
                content: "底部".into(),
                lane: 0,
                y_off: 0.0,
                est_width: 0.0,
                mid_hash: None,
                weight: 0,
                pool: 0,
            },
            OverlayDanmaku {
                progress_ms: 500,
                mode: 5,
                fontsize: 25,
                color: 0xFFFFFF,
                content: "顶部".into(),
                lane: 0,
                y_off: 0.0,
                est_width: 0.0,
                mid_hash: None,
                weight: 0,
                pool: 0,
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
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
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
        e.mid_hash = Some(0xabcd_1234);
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
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
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

    #[test]
    fn surface_mount_slot_ignores_stale_generation_and_clears_attached_on_detach() {
        let gpu = GpuDevice::new_warp().expect("WARP GPU 设备应可用");
        let chain = gpu.create_swap_chain(320, 180).expect("swapchain");
        let raw1 = chain.into_unknown_raw().expect("unknown raw1");
        let raw2 = chain.into_unknown_raw().expect("unknown raw2");

        let slot = SurfaceMountSlot::new();
        let g1 = slot.current_gen();
        assert!(slot.publish_if_current(g1, raw1));
        assert_eq!(slot.mark_attached(g1, true), Some(true));
        assert!(slot.is_attached());

        // 卸载瞬间同步清 attached 并推进代次；旧代次延迟回写或放回必须被拦截。
        let g2 = slot.invalidate_and_detach();
        assert_ne!(g2, g1);
        assert!(!slot.is_attached());
        assert_eq!(slot.mark_attached(g1, true), None);
        assert!(!slot.is_attached());
        assert!(!slot.publish_if_current(g1, raw2));
        assert!(slot.take_pending().is_none());

        // 新代次重新投递并可被认领/放回重试/成功挂载。
        let raw3 = chain.into_unknown_raw().expect("unknown raw3");
        assert!(slot.publish_if_current(g2, raw3));
        let (taken_gen, taken_raw) = slot.take_pending().expect("pending for g2");
        assert_eq!(taken_gen, g2);
        assert!(slot.publish_if_current(taken_gen, taken_raw));
        let (retry_gen, retry_raw) = slot.take_pending().expect("retry pending");
        assert_eq!(retry_gen, g2);
        drop(SendRaw::new(retry_raw));
        assert_eq!(slot.mark_attached(g2, true), Some(true));
        assert!(slot.is_attached());
    }

    #[test]
    fn video_engine_load_advances_load_seq_and_detach_clears_attached() {
        let engine = VideoEngine::new();
        assert_eq!(engine.load_seq(), 0);
        let s1 = engine.load("https://example.com/v1.m4s", Some("https://example.com/a1.m4s"));
        let s2 = engine.load_with_options("https://example.com/v2.m4s", None, 12.0, false);
        assert_eq!(s1, 1);
        assert_eq!(s2, 2);
        assert_eq!(engine.load_seq(), 2);

        let g1 = engine.mount.current_gen();
        assert_eq!(engine.mount.mark_attached(g1, true), Some(true));
        assert!(engine.attached());
        engine.detach_panel();
        assert!(!engine.attached(), "detach_panel 必须同步清 attached");
        // 旧面板的迟到回调不得把 attached 写回 true
        engine.mark_attached(g1, true);
        assert!(!engine.attached());
    }

    #[test]
    fn apply_mpv_event_tracks_buffering_lifecycle_and_end_file_reasons() {
        let status = Arc::new(Mutex::new(PlaybackStatus::default()));
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        apply_mpv_event(&status, &error, MpvEvent::FileLoaded);
        apply_mpv_event(
            &status,
            &error,
            MpvEvent::PropertyChange {
                name: "paused-for-cache".into(),
                value: PropertyValue::Flag(true),
            },
        );
        {
            let s = status.lock().unwrap();
            assert!(s.file_loaded);
            assert!(s.buffering);
            assert!(!s.playback_started);
        }

        apply_mpv_event(&status, &error, MpvEvent::PlaybackRestart);
        {
            let s = status.lock().unwrap();
            assert!(s.playback_started);
            assert!(!s.buffering);
        }

        // STOP (reason=2，如切源 replace) 不置 eof 也不写 error
        apply_mpv_event(&status, &error, MpvEvent::EndFile { reason: 2, error: 0 });
        assert!(!status.lock().unwrap().eof);
        assert!(error.lock().unwrap().is_none());

        // ERROR (reason=4) 写错误槽且不清成 eof
        apply_mpv_event(
            &status,
            &error,
            MpvEvent::EndFile {
                reason: 4,
                error: -13,
            },
        );
        assert!(!status.lock().unwrap().eof);
        assert_eq!(
            error.lock().unwrap().as_deref(),
            Some("mpv 播放失败(code=-13)")
        );

        // EOF (reason=0) 置 eof
        apply_mpv_event(&status, &error, MpvEvent::EndFile { reason: 0, error: 0 });
        assert!(status.lock().unwrap().eof);
    }

    #[test]
    fn mpv_player_initializes_network_options_and_comma_ua_cleanly() {
        let Ok(lib) = crate::ffi::MpvLib::load() else {
            return;
        };
        let player = MpvPlayer::new(lib).expect("MpvPlayer::new 含网络超时选项应成功");
        player
            .loadfile("null://", None, BILI_HEADERS)
            .expect("带逗号 User-Agent 与 Referer 单独设项应成功");
    }

    /// D1 前置:全宽/半宽估算与字号缩放联动(准入与绘制共用同一估算)。
    #[test]
    fn danmaku_est_width_full_and_half_width_scale() {
        // medium 档 em=22(fontsize 25 → 21..=28)。
        assert!((danmaku_est_width(25, 1.0, "啊啊") - 44.0).abs() < 1e-4);
        assert!((danmaku_est_width(25, 1.0, "ab") - 22.0 * 0.55 * 2.0).abs() < 1e-4);
        assert!((danmaku_est_width(25, 2.0, "啊") - 44.0).abs() < 1e-4);
        // 小/大档位基准 16/30。
        assert!((danmaku_est_width(18, 1.0, "啊") - 16.0).abs() < 1e-4);
        assert!((danmaku_est_width(36, 1.0, "啊") - 30.0).abs() < 1e-4);
        // NaN 缩放回退 1.0;空串至少一个 em。
        assert!((danmaku_est_width(25, f32::NAN, "啊") - 22.0).abs() < 1e-4);
        assert!((danmaku_est_width(25, 1.0, "") - 22.0).abs() < 1e-4);
    }

    /// 折行回归(2026-11-22 截图实拍样本):以 est 宽为布局上限时必须单行。
    /// 曾因估算 em 与绘制档位脱节 + TextFormat 默认 WordWrapping::Wrap
    /// (中文逐字可断)+ DrawText 无 CLIP 而折成第二行、叠进下一轨道。
    /// 「绝活哥」→「绝活/哥」、「是时代变得太快」→ 5+2 即当年实拍折行点。
    #[test]
    fn danmaku_formats_never_wrap_within_est_width() {
        use windows_text::TextLayout;
        let fonts = DanmakuFonts::new();
        for (text, fontsize) in [
            ("是时代变得太快", 25),
            ("绝活哥", 25),
            ("话说哥终究赢不了版本神?就", 25),
            // 半宽混排:est 按半角 0.55em 高估,天然 ≥ natural,更不该折。
            ("数字: 经典数学 / AI数学: 现代数学 了?", 25),
            // 全角名单内新增字符(… — “”)不再被 0.55em 低估。
            ("是时代变得…太快—“”", 36),
        ] {
            let est = danmaku_est_width(fontsize, 1.0, text);
            let format = match fontsize {
                ..=20 => &fonts.small,
                21..=28 => &fonts.medium,
                _ => &fonts.large,
            };
            let layout = TextLayout::new(text, format, est, 34.0);
            assert_eq!(
                layout.metrics().line_count,
                1,
                "est={est:.1} {text:?} 不应折行"
            );
        }
        // NoWrap 判据:布局宽砍半(远小于任何真实排版宽)仍必须单行——
        // 这一条只有禁用折行才可能通过,防止 WordWrapping 被回退成默认 Wrap。
        let cramped = TextLayout::new("绝活哥", &fonts.medium, 30.0, 34.0);
        assert_eq!(cramped.metrics().line_count, 1, "NoWrap 不应折行");
    }

    /// D1 验收:band packing 分配结果回填,同 band 内任意 y 区间重叠的对
    /// 必须满足官方追尾双判据(独立重算校验);落位全部在可用高度内。
    #[test]
    fn allocate_lanes_overlapping_pairs_satisfy_tailgate() {
        let mk = |ms: i32, long: bool| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from(if long {
                "很长很长很长很长很长的弹幕文本"
            } else {
                "短弹幕"
            }),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let items: Vec<_> = (0..60).map(|i| mk(i * 250, i % 3 == 0)).collect();
        let cfg = VideoConfig {
            danmaku_max_lanes: 10,
            ..VideoConfig::default()
        };
        let kept = allocate_lanes(items, &cfg, 1000.0, 600.0, f64::NEG_INFINITY);
        assert!(!kept.is_empty(), "适度密度下应有条目保留");
        let w = 1000.0_f64;
        let item_h = 1.125 * 22.0; // fontsize 25 → em 22(官方行盒 1.125×字号)。
        assert!(
            kept.iter().all(|e| (e.lane as usize) < 3),
            "常规弹幕只落 band 0..2(特权溢出口径)"
        );
        // g = min(高 600, offsetTop 8 + 10 轨×行距 30) = 308:落位不得越过。
        assert!(
            kept.iter()
                .all(|e| f64::from(e.y_off) + item_h <= 308.0 + 1e-6),
            "落位不得超过可用高度 g"
        );
        // 独立重算(官方判据):同 band 内 y 区间重叠的对,后条入场时前条尾部
        // 已过右缘(a);后条头部到左缘(含右侧跑道)时前条已完全退场+密度容忍(b)。
        for lane in 0..3usize {
            let band: Vec<&OverlayDanmaku> = kept
                .iter()
                .filter(|e| e.lane as usize == lane)
                .collect();
            for (i, cur) in band.iter().enumerate() {
                for prev in &band[..i] {
                    let (py, cy) = (f64::from(prev.y_off), f64::from(cur.y_off));
                    let overlap = py < cy + item_h && cy < py + item_h;
                    if !overlap {
                        continue;
                    }
                    let (ps, cs) = (
                        f64::from(prev.progress_ms) / 1000.0,
                        f64::from(cur.progress_ms) / 1000.0,
                    );
                    let (pw, cw) = (f64::from(prev.est_width), f64::from(cur.est_width));
                    let prev_travel = roll_travel(pw, w, 1.0);
                    let cur_travel = roll_travel(cw, w, 1.0);
                    assert!(
                        cs >= ps + pw / prev_travel.speed,
                        "band {lane}@{}: 条件 a 被破坏",
                        cur.progress_ms
                    );
                    assert!(
                        cs + (w + ROLL_SAFE_PX) / cur_travel.speed
                            >= ps + prev_travel.duration,
                        "band {lane}@{}: 条件 b 被破坏",
                        cur.progress_ms
                    );
                }
            }
        }
    }

    /// D1 验收:逐段到货的增量分配与一次到位全量分配结果一致。
    #[test]
    fn allocate_lanes_incremental_matches_full() {
        let mk = |ms: i32, long: bool| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from(if long { "很长很长很长的弹幕" } else { "短" }),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let all: Vec<_> = (0..40).map(|i| mk(i * 400, i % 2 == 0)).collect();
        let cfg = VideoConfig {
            danmaku_max_lanes: 8,
            ..VideoConfig::default()
        };
        let full = allocate_lanes(all.clone(), &cfg, 1200.0, 700.0, f64::NEG_INFINITY);
        // 分段预存链语义:每段到货即对累计集合全量重跑。
        let mut incremental: Vec<OverlayDanmaku> = Vec::new();
        for chunk in all.chunks(14) {
            incremental.extend(chunk.iter().cloned());
            incremental = allocate_lanes(incremental, &cfg, 1200.0, 700.0, f64::NEG_INFINITY);
        }
        assert_eq!(full.len(), incremental.len());
        for (a, b) in full.iter().zip(&incremental) {
            assert_eq!(a.progress_ms, b.progress_ms);
            assert_eq!(a.lane, b.lane, "progress {} 车道漂移", a.progress_ms);
            assert!(
                (a.y_off - b.y_off).abs() < 1e-3,
                "progress {} y 漂移",
                a.progress_ms
            );
            assert!((a.est_width - b.est_width).abs() < 1e-3);
        }
    }

    /// D1 验收:轨道全满 drop 丢弃、force 强放不丢条目(字太多的兜底例外)。
    #[test]
    fn allocate_lanes_overflow_drop_vs_force() {
        let mk = |ms: i32| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("同刻弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        // 12 条同刻弹幕:同刻入场必互相冲突(尾部尚未过右缘),走查连续下移。
        // g = min(600, 8 + 10×30) = 308,行盒 24.75 → band 0 容量 11 条
        // (v=8+25.75k ≤ 283.25 → k=0..10),第 12 条越界丢弃(官方 Drop 口径)。
        let items: Vec<_> = (0..12).map(|_| mk(5000)).collect();
        let drop_cfg = VideoConfig {
            danmaku_max_lanes: 10,
            ..VideoConfig::default()
        };
        let force_cfg = VideoConfig {
            danmaku_max_lanes: 10,
            danmaku_overflow: DanmakuOverflow::Force,
            ..VideoConfig::default()
        };
        let dropped = allocate_lanes(items.clone(), &drop_cfg, 1000.0, 600.0, f64::NEG_INFINITY);
        assert_eq!(dropped.len(), 11, "band 0 走满(308px/24.75px 行盒=11 条)后丢弃");
        assert!(
            dropped
                .iter()
                .all(|e| f64::from(e.y_off) + 24.75 <= 308.0 + 1e-6),
            "落位全部在可用高度内"
        );
        let forced = allocate_lanes(items, &force_cfg, 1000.0, 600.0, f64::NEG_INFINITY);
        assert_eq!(forced.len(), 12, "force 策略不丢条目");
        assert!(forced.iter().all(|e| (e.lane as usize) < 3));
    }

    /// D3 验收:同刻驻留弹幕各占独立槽位(顶/底独立),超时释放后槽位复用。
    #[test]
    fn allocate_lanes_pin_slots_stack_and_release() {
        let mk_pin = |ms: i32, mode: i32| OverlayDanmaku {
            progress_ms: ms,
            mode,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("驻留"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let pin_cfg = VideoConfig {
            danmaku_max_lanes: 10,
            ..VideoConfig::default()
        };
        let kept = allocate_lanes(
            vec![mk_pin(1000, 5), mk_pin(1000, 5), mk_pin(1000, 5)],
            &pin_cfg,
            1000.0,
            600.0,
            f64::NEG_INFINITY,
        );
        assert_eq!(kept.len(), 3, "顶部槽位足够时全部保留");
        let mut slots: Vec<u16> = kept.iter().map(|e| e.lane).collect();
        slots.sort();
        assert_eq!(slots, vec![0, 1, 2], "同刻顶部弹幕堆叠到不同槽");
        // 顶/底槽序独立:同刻一顶一底都落槽 0。
        let both = allocate_lanes(
            vec![mk_pin(1000, 4), mk_pin(1000, 5)],
            &pin_cfg,
            1000.0,
            600.0,
            f64::NEG_INFINITY,
        );
        assert_eq!(both[0].lane, 0);
        assert_eq!(both[1].lane, 0);
        // PIN_SECONDS 后槽位释放,新条复用槽 0。
        let reused = allocate_lanes(vec![mk_pin(5000, 5)], &pin_cfg, 1000.0, 600.0, f64::NEG_INFINITY);
        assert_eq!(reused[0].lane, 0);
    }

    /// 官方 tz 分段表边界(抓取研究 §3.3):屏宽 <558 兜底 4.5s;命中规则=
    /// 第一个 `屏宽 ≤ 上界` 的段取前一段 [min,max];>2560 每 500px 各 +1s。
    /// Logistic 中点在 ratio≈20+,常规文本实际≈段内 minTime。
    #[test]
    fn roll_base_secs_follows_official_brackets() {
        // 屏宽 <558(官方小窗口径)与非法参数 → 4.5s 兜底。
        assert!((roll_base_secs(200.0, 500.0) - 4.5).abs() < 1e-9);
        assert!((roll_base_secs(f64::NAN, 1920.0) - 4.5).abs() < 1e-9);
        // 常规短文本 ≈ 段内 minTime(容差覆盖 Logistic 尾部)。
        let short = 100.0;
        assert!(
            (roll_base_secs(short, 558.0) - 7.0).abs() < 0.15,
            "558px 段应≈7s, 实得 {}",
            roll_base_secs(short, 558.0)
        );
        assert!((roll_base_secs(short, 888.0) - 7.0).abs() < 0.15, "888 仍属第一段");
        assert!((roll_base_secs(short, 900.0) - 8.0).abs() < 0.15, "900 落第二段");
        assert!((roll_base_secs(short, 1246.0) - 9.0).abs() < 0.15, "1246 边界取前一段");
        // 2560 恰在边界上:官方命中规则取前一段 (2314 → min 14)。
        assert!((roll_base_secs(short, 2560.0) - 14.0).abs() < 0.15);
        // 超界段:4000px = 2560 + floor(1440/500)=2 段外加成 → min 16+2=18。
        assert!((roll_base_secs(short, 4000.0) - 18.0).abs() < 0.15);
        // 超长弹幕向 maxTime 弯折(单调),但不超过 maxTime 上界。
        let long = roll_base_secs(4000.0, 1000.0);
        assert!(long > roll_base_secs(short, 1000.0), "字越长时长越长");
        assert!(long < 14.0, "1000px 段 maxTime=14, 实得 {long}");
        // 1920 屏常规文本 ≈13.1s(官方手感锚点;1744-2314 段 min=13)。
        assert!(
            (roll_base_secs(short, 1920.0) - 13.1).abs() < 0.2,
            "1920 屏应≈13.1s, 实得 {}",
            roll_base_secs(short, 1920.0)
        );
    }

    /// 运动学一致性:speed=(屏宽+字宽)/base×speedPlus,duration 含双侧 50px
    /// 跑道;渲染扫描窗口必须 ≥ 任意条目的 duration(窗口裁剪不漏条)。
    #[test]
    fn roll_travel_and_window_consistent() {
        let w = 1000.0_f64;
        let tw = 220.0_f64;
        let travel = roll_travel(tw, w, 1.0);
        let base = roll_base_secs(tw, w);
        assert!((travel.speed - (w + tw) / base).abs() < 1e-9);
        assert!((travel.duration - (w + 2.0 * ROLL_SAFE_PX + tw) / travel.speed).abs() < 1e-9);
        // speedPlus 放慢(0.5)时长翻倍,加速(2.0)减半。
        assert!((roll_travel(tw, w, 0.5).duration - travel.duration * 2.0).abs() < 1e-6);
        assert!((roll_travel(tw, w, 2.0).duration - travel.duration / 2.0).abs() < 1e-6);
        // 窗口覆盖任意字宽的 duration(含极宽/极窄文本与多屏宽)。
        for width in [360.0, 1000.0, 1920.0, 4000.0] {
            let window = roll_window_secs(width, 1.0);
            for tw in [16.0, 220.0, 2000.0, 8000.0] {
                assert!(
                    roll_travel(tw, width, 1.0).duration <= window,
                    "w={width} tw={tw}: duration 超出扫描窗口"
                );
            }
        }
    }

    /// 官方密度容忍 A:density 1.0(最密,A≈0.1)可接纳的间隔,在低密度
    /// (A=floor(dur×(1-d))/2+0.1)下被拒绝。max_lanes=1 隔离出纯 A 效应。
    #[test]
    fn allocate_lanes_density_tolerance_orders_admission() {
        let mk = |ms: i32| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("同宽弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        // 同宽 → 同速同时长;1000px 屏/88px 宽:base≈8.1s,dur≈8.84s,
        // head_left-delta≈7.82s → Δ=2s 在 density=1.0(A≈0.1)可复用同一轨,
        // 在 density=0.2(A≈3.6)被拒(Drop 下丢弃)。
        let dense = allocate_lanes(
            vec![mk(0), mk(2000)],
            &VideoConfig {
                danmaku_max_lanes: 1,
                ..VideoConfig::default()
            },
            1000.0,
            600.0,
            f64::NEG_INFINITY,
        );
        assert_eq!(dense.len(), 2, "最密档 2s 间隔应复用同一轨");
        let sparse = allocate_lanes(
            vec![mk(0), mk(2000)],
            &VideoConfig {
                danmaku_max_lanes: 1,
                danmaku_density: 0.2,
                ..VideoConfig::default()
            },
            1000.0,
            600.0,
            f64::NEG_INFINITY,
        );
        assert_eq!(sparse.len(), 1, "低密度档 2s 间隔不足以复用同一轨");
    }

    /// 官方 area + 字幕池豁免:area 30% → area 下界 m=90px,行盒 24.75 的
    /// 同刻弹幕只能堆 3 条(8/33.75/59.5,候选底 84.25≤90);字幕池(pool=1)
    /// 特权豁免 area,band 0 容量 11 条 + band 1 溢出 1 条 = 12 条全保。
    #[test]
    fn allocate_lanes_area_limits_and_subtitle_pool_exempt() {
        let mk = |pool: u8| OverlayDanmaku {
            progress_ms: 5000,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("同刻弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool,
        };
        let cfg = VideoConfig {
            danmaku_max_lanes: 10,
            danmaku_area: 30,
            ..VideoConfig::default()
        };
        let normal: Vec<_> = (0..12).map(|_| mk(0)).collect();
        let kept = allocate_lanes(normal, &cfg, 1000.0, 300.0, f64::NEG_INFINITY);
        assert_eq!(kept.len(), 3, "area 30%(m=90px)只允许 3 条连续 y 堆叠");
        assert!(
            kept.iter()
                .all(|e| f64::from(e.y_off) + 24.75 <= 90.0 + 1e-6),
            "普通弹幕落位不得超过 area 下界"
        );
        let subtitle: Vec<_> = (0..12).map(|_| mk(1)).collect();
        let exempted = allocate_lanes(subtitle, &cfg, 1000.0, 300.0, f64::NEG_INFINITY);
        assert_eq!(exempted.len(), 12, "字幕池弹幕豁免 area,band 溢出后不丢");
        assert_eq!(
            exempted.iter().filter(|e| e.lane == 0).count(),
            11,
            "band 0 容量 11 条"
        );
        assert_eq!(
            exempted.iter().filter(|e| e.lane == 1).count(),
            1,
            "第 12 条特权溢出 band 1(官方 3-band 口径)"
        );
    }

    /// 高度感知:窗口过矮时可用高度 g 收缩,连续 y 堆叠数随之减少——
    /// 206px 高窗口 g=206,行盒 24.75 → 7 条(8+25.75k ≤ 181.25 → k=0..6)。
    #[test]
    fn allocate_lanes_height_aware_lane_count() {
        let mk = || OverlayDanmaku {
            progress_ms: 5000,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("同刻弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let cfg = VideoConfig {
            danmaku_max_lanes: 10,
            ..VideoConfig::default()
        };
        let items: Vec<_> = (0..12).map(|_| mk()).collect();
        let kept = allocate_lanes(items, &cfg, 1000.0, 206.0, f64::NEG_INFINITY);
        assert_eq!(kept.len(), 7, "矮窗口按高度收缩到 7 条连续 y 堆叠");
        assert!(kept.iter().all(|e| (e.lane as usize) < 3));
        assert!(
            kept.iter()
                .all(|e| f64::from(e.y_off) + 24.75 <= 206.0 + 1e-6),
            "落位全部在容器高内"
        );
    }

    /// 官方 preventShade(防挡字幕):碰撞可用高度×0.85。300px 高窗关闭时
    /// g=300 → 11 条连续 y 堆叠,开启时 g=255 → 9 条(底部 15% 让位)。
    #[test]
    fn allocate_lanes_prevent_shade_shrinks_usable_height() {
        let mk = || OverlayDanmaku {
            progress_ms: 5000,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("同刻弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let cfg = VideoConfig {
            danmaku_max_lanes: 10,
            ..VideoConfig::default()
        };
        let items: Vec<_> = (0..12).map(|_| mk()).collect();
        let plain = allocate_lanes(items.clone(), &cfg, 1000.0, 300.0, f64::NEG_INFINITY);
        assert_eq!(plain.len(), 11, "无防挡字幕:300px 容 11 条");
        let cfg_shade = VideoConfig {
            danmaku_max_lanes: 10,
            danmaku_prevent_shade: true,
            ..VideoConfig::default()
        };
        let shaded = allocate_lanes(items, &cfg_shade, 1000.0, 300.0, f64::NEG_INFINITY);
        assert_eq!(shaded.len(), 9, "防挡字幕:底部 15% 让位 → 9 条");
        assert!(
            shaded
                .iter()
                .all(|e| f64::from(e.y_off) + 24.75 <= 255.0 + 1e-6),
            "防挡字幕落位不得超过 0.85×高"
        );
    }

    /// 官方 2D packing 的连续 y:混排字号按各自行盒(1.125×字号)紧凑堆叠,
    /// 小字号落位不在旧 30px 行距栅格上(小字号 y=大字号行盒底+1)。
    #[test]
    fn allocate_lanes_mixed_sizes_pack_continuous_y() {
        let mk = |ms: i32, fontsize: i32| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize,
            color: 0xFFFFFF,
            content: ContentRef::from("混排弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let cfg = VideoConfig {
            danmaku_max_lanes: 6,
            ..VideoConfig::default()
        };
        // 同刻大字号(em30,行盒 33.75)+小字号(em16,行盒 18):后者贴前者下方。
        let kept = allocate_lanes(
            vec![mk(5000, 36), mk(5000, 18)],
            &cfg,
            1000.0,
            600.0,
            f64::NEG_INFINITY,
        );
        assert_eq!(kept.len(), 2);
        let (tall_y, small_y) = (f64::from(kept[0].y_off), f64::from(kept[1].y_off));
        assert!((tall_y - 8.0).abs() < 1e-6, "首条落打包起点");
        let expect = 8.0 + 1.125 * 30.0 + 1.0; // 走查下移 = 大行盒底 + 1px。
        assert!(
            (small_y - expect).abs() < 1e-4,
            "小字号应连续贴排 y={small_y},期望 {expect}(非 30px 栅格 38)"
        );
    }

    /// 跳变治理:playhead 之前的条目(已起飞)保留原 y 分配,不参与重排;
    /// 后打包条目以飞行占用播种,绕开可见弹幕(重叠必须满足追尾双判据)。
    #[test]
    fn realloc_preserves_flying_y_and_packs_around_them() {
        let mk = |ms: i32| OverlayDanmaku {
            progress_ms: ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("跳变治理弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let cfg = VideoConfig {
            danmaku_max_lanes: 4,
            ..VideoConfig::default()
        };
        // 第一遍:全量打包两条同刻弹幕(必然上下堆叠)。
        let first = allocate_lanes(
            vec![mk(1000), mk(1000)],
            &cfg,
            1000.0,
            600.0,
            f64::NEG_INFINITY,
        );
        assert_eq!(first.len(), 2);
        let (y0, y1) = (first[0].y_off, first[1].y_off);
        assert!((y1 - (y0 + 1.125 * 22.0 + 1.0)).abs() < 1e-4, "第二条紧贴第一条下方");
        // 第二遍:playhead=2s(两条均已起飞)→ y 保持不变,可见层零跳变。
        let reflowed = allocate_lanes(first.clone(), &cfg, 1000.0, 600.0, 2.0);
        assert_eq!(reflowed[0].y_off, y0, "飞行条目 y 不变");
        assert_eq!(reflowed[1].y_off, y1, "飞行条目 y 不变");
        // 第三遍:playhead=5s(第一条飞行中,第二条为新到货)→ 第一条保持,
        // 第二条重打包且与飞行条目重叠时必须满足追尾双判据(官方同 y 复用口径)。
        let mixed = allocate_lanes(
            vec![first[0].clone(), mk(9000)],
            &cfg,
            1000.0,
            600.0,
            5.0,
        );
        assert_eq!(mixed.len(), 2);
        assert_eq!(mixed[0].y_off, y0, "飞行条目 y 不变");
        let flying = &mixed[0];
        let ahead = &mixed[1];
        assert!(f64::from(ahead.y_off) >= f64::from(BAND_OFFSET_TOP));
        let overlap = f64::from(ahead.y_off) < f64::from(flying.y_off) + 1.125 * 22.0
            && f64::from(flying.y_off) < f64::from(ahead.y_off) + 1.125 * 22.0;
        if overlap {
            let (ps, cs) = (
                f64::from(flying.progress_ms) / 1000.0,
                f64::from(ahead.progress_ms) / 1000.0,
            );
            let flying_travel = roll_travel(f64::from(flying.est_width), 1000.0, 1.0);
            let ahead_travel = roll_travel(f64::from(ahead.est_width), 1000.0, 1.0);
            assert!(
                cs >= ps + f64::from(flying.est_width) / flying_travel.speed,
                "与飞行条目同 y 必须满足追尾判据 a"
            );
            assert!(
                cs + (1000.0 + ROLL_SAFE_PX) / ahead_travel.speed
                    >= ps + flying_travel.duration,
                "与飞行条目同 y 必须满足追尾判据 b"
            );
        }
    }

    /// playhead=0(未起播)时 progress_ms=0 的条目必须参与打包——严格 <
    /// 边界防止 0 时刻弹幕被误判为飞行条目(y_off=0 直落顶端裁切)。
    #[test]
    fn allocate_lanes_packs_zero_progress_items_at_playhead_zero() {
        let mk = || OverlayDanmaku {
            progress_ms: 0,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: ContentRef::from("零时刻弹幕"),
            lane: 0,
            y_off: 0.0,
            est_width: 0.0,
            mid_hash: None,
            weight: 0,
            pool: 0,
        };
        let cfg = VideoConfig {
            danmaku_max_lanes: 4,
            ..VideoConfig::default()
        };
        let kept = allocate_lanes(vec![mk(), mk()], &cfg, 1000.0, 600.0, 0.0);
        assert_eq!(kept.len(), 2, "零时刻弹幕不得被跳过打包");
        assert!(
            kept.iter()
                .all(|e| f64::from(e.y_off) >= f64::from(BAND_OFFSET_TOP)),
            "全部条目必须获得合法打包 y"
        );
    }

    /// DanmakuStyle.normalized 钳位口径与 settings.normalized 一致
    /// (防 settings.json 手工越界值直灌引擎线程)。
    #[test]
    fn danmaku_style_normalizes_out_of_range_values() {
        let style = DanmakuStyle {
            enabled: true,
            opacity: 5.0,
            max_lanes: 0,
            font_family: "  ".into(),
            font_scale: f32::NAN,
            density: -1.0,
            area: 200,
            speed_plus: 9.0,
            prevent_shade: true,
        }
        .normalized();
        assert!((style.opacity - 1.0).abs() < f32::EPSILON);
        assert_eq!(style.max_lanes, 1);
        assert!((style.density - 0.1).abs() < f32::EPSILON);
        assert_eq!(style.area, 100);
        assert!((style.speed_plus - 2.0).abs() < f32::EPSILON);
        // 字体族空白不在此钳(与旧行为一致:引擎线程 DanmakuFonts::with 处理;
        // normalized 只管数值域)。
        assert_eq!(style.font_scale, 1.0);
    }
}
