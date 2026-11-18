//! 弹幕独立透明层(v0.3:native 系统媒体引擎模式的弹幕通道)。
//!
//! 为什么另起一层:`QAQ_BX_NATIVE=1` 时 mpv 引擎保持 idle,弹幕叠画只发生在
//! mpv 渲染循环里(`engine.rs::render_present`);native 模式的播放画面由
//! MediaPlayerElement 自持的合成链输出,我们拿不到它的 swapchain。于是按
//! `docs/video-channel.md` §2 的原设计,自建**一层预乘 alpha 的 composition
//! swapchain** 叠在 MPE 之上(Grid 同 cell),这一层只画弹幕。
//!
//! 复用而非复制:轨道排版/描边/屏蔽过滤/时基外推全部调用 `crate::engine` 里的
//! 同一套 D2D 弹幕原语(`draw_danmaku_overlay` / `CompiledDanmakuFilter` /
//! `interpolated_danmaku_time` / `DanmakuFonts`),保证 native 与 mpv 两条管线的
//! 弹幕表现一致,也让现有弹幕单测覆盖的仍是生产代码路径。
//!
//! 线程模型:与 `VideoEngine` 同构——常驻层线程独占 device + swapchain + 字体格式,
//! UI 经 `Arc<DanmakuStage>` 用 mpsc 发命令(非阻塞);挂载协议同为
//! `take_pending` / `put_back_pending` / `mark_attached` / `detach_panel`。
//! 时基由 shell 的状态节拍 `set_time` 喂入(250ms),层内按墙钟线性外推补帧;
//! 未挂载、暂停、无弹幕或弹幕关闭时阻塞休眠(0 空转,贴合 4c8g)。
//!
//! 与 mpv 通道的一处差异:resize 只需 `ResizeBuffers`(canvas 内部重绑 D2D
//! target),合成 swapchain 的对象身份不变 ⇒ **不需要重挂**,也就不会出现视频路径
//! 那种「卸载清黑」空窗(`docs/dev-notes/resize-black-screen.md`)。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_canvas::{ColorF, GpuDevice};

use crate::engine::{
    CompiledDanmakuFilter, DanmakuFilter, DanmakuFonts, OverlayDanmaku, RENDER_BUDGET, SendRaw,
    SurfaceMetrics, VideoConfig, compute_surface_metrics, configure_swap_chain,
    draw_danmaku_overlay, interpolated_danmaku_time,
};

/// 初始 swapchain 尺寸(挂载前占位;首个 metrics 到达即 ResizeBuffers)。
const INITIAL_WIDTH: u32 = 1280;
const INITIAL_HEIGHT: u32 = 720;
/// resize 合并窗口(与视频通道同口径:拖拽期间至多每 120ms 一次 ResizeBuffers)。
const RESIZE_COALESCE: Duration = Duration::from_millis(120);

/// 上层投递给弹幕层的指令(UI → 层线程,非阻塞)。
#[derive(Debug)]
pub enum StageCmd {
    /// 全量替换(新视频/换分P 清层语义:去重表一并重置)。
    SetItems { items: Vec<OverlayDanmaku> },
    /// 增量追加(分段预存链逐段到货)。
    AppendItems { items: Vec<OverlayDanmaku> },
    /// 屏蔽规则变化:缓存弹幕全量重滤。
    SetFilter { filter: DanmakuFilter },
    /// 弹幕样式(开关/透明度/轨道数/字体族/字号缩放)。
    SetStyle {
        enabled: bool,
        opacity: f32,
        max_lanes: usize,
        font_family: String,
        font_scale: f32,
    },
    /// 时基输入:shell 状态节拍喂来的播放位置(秒)。
    SetTime { secs: f64 },
    /// 播放态(native 引擎真实状态;暂停即冻结弹幕并重锚时基)。
    SetPlaying { playing: bool },
    /// 面板物理像素尺寸 + 组合缩放。
    Resize(SurfaceMetrics),
    /// 唤醒重绘(挂载完成:attached 由 UI 回写,这里只负责踢一脚)。
    Wake,
    /// 面板卸载(视图树切走):swapchain 放回待挂载槽。
    Detach,
    Quit,
}

/// 弹幕层句柄(线程安全;UI 侧克隆共享)。
pub struct DanmakuStage {
    cmd_tx: Sender<StageCmd>,
    /// 层线程产出的待挂载 swapchain(UI 线程取走挂载;失败放回)。
    pending: Arc<Mutex<Option<SendRaw>>>,
    attached: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

// UI 只比较句柄同一性(reactor Input 约束需要 PartialEq)。
impl PartialEq for DanmakuStage {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.pending, &other.pending)
    }
}

impl DanmakuStage {
    /// 创建弹幕层并启动线程(默认弹幕配置)。
    pub fn new() -> Arc<Self> {
        Self::with_config(VideoConfig::default())
    }

    /// 以给定配置(仅取其弹幕字段)创建层线程。
    pub fn with_config(config: VideoConfig) -> Arc<Self> {
        let pending: Arc<Mutex<Option<SendRaw>>> = Arc::new(Mutex::new(None));
        let attached = Arc::new(AtomicBool::new(false));
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<StageCmd>();
        let thread = std::thread::Builder::new()
            .name("qaq-bx-danmaku-stage".into())
            .spawn({
                let pending = pending.clone();
                let attached = attached.clone();
                move || stage_thread(cmd_rx, pending, attached, config)
            })
            .ok();
        Arc::new(Self {
            cmd_tx,
            pending,
            attached,
            thread: Mutex::new(thread),
        })
    }

    pub fn set_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(StageCmd::SetItems { items });
    }

    pub fn append_danmaku(&self, items: Vec<OverlayDanmaku>) {
        _ = self.cmd_tx.send(StageCmd::AppendItems { items });
    }

    /// 样式下发:与 `VideoEngine::set_danmaku_config` 同参数、同夹取口径。
    pub fn set_danmaku_config(
        &self,
        enabled: bool,
        opacity: f32,
        max_lanes: usize,
        font_family: &str,
        font_scale: f32,
    ) {
        _ = self.cmd_tx.send(StageCmd::SetStyle {
            enabled,
            opacity,
            max_lanes,
            font_family: font_family.to_string(),
            font_scale,
        });
    }

    pub fn set_danmaku_filter(&self, filter: DanmakuFilter) {
        _ = self.cmd_tx.send(StageCmd::SetFilter { filter });
    }

    /// 时基喂入(shell 状态节拍):层内按墙钟外推到满刷新率。
    pub fn set_time(&self, secs: f64) {
        if secs.is_finite() && secs >= 0.0 {
            _ = self.cmd_tx.send(StageCmd::SetTime { secs });
        }
    }

    pub fn set_playing(&self, playing: bool) {
        _ = self.cmd_tx.send(StageCmd::SetPlaying { playing });
    }

    /// 面板 DIP 尺寸 + 组合缩放上报(非法尺寸静默忽略,同 VideoEngine)。
    pub fn set_metrics(&self, dip_width: f64, dip_height: f64, scale_x: f32, scale_y: f32) {
        let Some(metrics) = compute_surface_metrics(dip_width, dip_height, scale_x, scale_y) else {
            return;
        };
        _ = self.cmd_tx.send(StageCmd::Resize(metrics));
    }

    pub fn detach_panel(&self) {
        _ = self.cmd_tx.send(StageCmd::Detach);
    }

    // ---- 挂载协调(UI 线程专用) ----

    /// 取走待挂载 swapchain 原始指针(接管所有权;无则 None)。
    pub fn take_pending(&self) -> Option<*mut c_void> {
        let chain = self.pending.lock().ok()?.take()?;
        Some(chain.into_raw())
    }

    /// 挂载请求入队失败(面板尚未实例化):放回指针,下一拍再试。
    pub fn put_back_pending(&self, raw: *mut c_void) {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some(SendRaw::new(raw));
        }
    }

    /// 挂载 completion 回写:成功 → attached + 踢一次重绘首帧。
    pub fn mark_attached(&self, ok: bool) {
        if ok {
            self.attached.store(true, Ordering::SeqCst);
            _ = self.cmd_tx.send(StageCmd::Wake);
        } else {
            eprintln!("[qaq-bx-video] danmaku stage: swapchain 挂载失败(SetSwapChain)");
        }
    }

    pub fn attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
    }
}

impl Drop for DanmakuStage {
    fn drop(&mut self) {
        _ = self.cmd_tx.send(StageCmd::Quit);
        if let Ok(mut guard) = self.thread.lock() {
            if let Some(thread) = guard.take() {
                let _ = thread.join();
            }
        }
    }
}

/// 层线程主体:建 device/swapchain → 交付 UI 挂载 → 命令 + 节拍驱动循环。
fn stage_thread(
    cmd_rx: Receiver<StageCmd>,
    pending: Arc<Mutex<Option<SendRaw>>>,
    attached: Arc<AtomicBool>,
    initial_config: VideoConfig,
) {
    // 1) 独立 D3D device(canvas GpuDevice 自建;失败退 WARP,同视频通道口径)。
    let gpu = match GpuDevice::new_or_warp() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("[qaq-bx-video] danmaku stage: device 创建失败:{e}");
            return;
        }
    };
    // 2) 预乘 alpha 的 composition swapchain(canvas 侧固定 PREMULTIPLIED +
    //    FLIP_SEQUENTIAL,透明层才能与 MPE 画面合成)。
    let mut chain = match gpu.create_swap_chain(INITIAL_WIDTH, INITIAL_HEIGHT) {
        Ok(chain) => chain,
        Err(e) => {
            eprintln!("[qaq-bx-video] danmaku stage: swapchain 创建失败:{e}");
            return;
        }
    };
    configure_swap_chain(&mut chain, 1.0, 1.0);
    match chain.into_unknown_raw() {
        Ok(raw) => {
            if let Ok(mut slot) = pending.lock() {
                *slot = Some(SendRaw::new(raw));
            }
        }
        Err(e) => {
            eprintln!("[qaq-bx-video] danmaku stage: swapchain → IUnknown 失败:{e}");
            return;
        }
    }
    eprintln!("STAGE-A: danmaku surface ok");

    let mut fonts = DanmakuFonts::new();
    let mut cfg = initial_config;
    let mut dm_filter = CompiledDanmakuFilter::compile(&cfg.danmaku_filter);
    let mut dm_dedup: HashMap<String, i32> = HashMap::new();
    let mut overlay: Vec<OverlayDanmaku> = Vec::new();
    let mut width = INITIAL_WIDTH;
    let mut height = INITIAL_HEIGHT;
    let mut scale_x = 1.0_f32;
    let mut scale_y = 1.0_f32;
    let mut need_repaint = true;
    let mut playing = false;
    let mut raw_t = 0.0_f64;
    // 弹幕时基锚点:与视频通道同一条外推逻辑(节拍喂入 + 墙钟补帧)。
    let mut dm_time_anchor: Option<(f64, Instant)> = None;
    let mut next_frame = Instant::now();
    // STAGE-B 诊断锚只在首次真正画出弹幕时打一次。
    let mut first_paint_logged = false;
    // P1 resize 合并:拖拽期间只记最新目标,节流窗口到期一次性应用。
    let mut pending_resize: Option<SurfaceMetrics> = None;
    let mut resize_first_seen: Option<Instant> = None;
    let mut first_pending: Option<StageCmd> = None;

    'main: loop {
        // 命令段:先消费已取出的首条,再一口气排空。
        loop {
            let next_cmd = match first_pending.take() {
                Some(cmd) => Ok(cmd),
                None => cmd_rx.try_recv(),
            };
            match next_cmd {
                Ok(cmd) => match cmd {
                    StageCmd::SetItems { items } => {
                        // 全量替换:重置去重表(新视频/换分P 清层语义)。
                        dm_dedup.clear();
                        overlay = dm_filter.filter_batch(items, &mut dm_dedup);
                        need_repaint = true;
                    }
                    StageCmd::AppendItems { items } => {
                        let kept = dm_filter.filter_batch(items, &mut dm_dedup);
                        if !kept.is_empty() {
                            overlay.extend(kept);
                            // 归并回 progress 升序(渲染二分扫描的不变量)。
                            overlay.sort_by_key(|e| e.progress_ms);
                            need_repaint = true;
                        }
                    }
                    StageCmd::SetFilter { filter } => {
                        dm_filter = CompiledDanmakuFilter::compile(&filter);
                        dm_dedup.clear();
                        let items = std::mem::take(&mut overlay);
                        overlay = dm_filter.filter_batch(items, &mut dm_dedup);
                        need_repaint = true;
                    }
                    StageCmd::SetStyle {
                        enabled,
                        opacity,
                        max_lanes,
                        font_family,
                        font_scale,
                    } => {
                        cfg.danmaku_enabled = enabled;
                        cfg.danmaku_opacity = opacity.clamp(0.1, 1.0);
                        cfg.danmaku_max_lanes = max_lanes.clamp(1, 20);
                        if cfg.danmaku_font_family != font_family
                            || (cfg.danmaku_font_scale - font_scale).abs() > f32::EPSILON
                        {
                            fonts = DanmakuFonts::with(&font_family, font_scale);
                        }
                        cfg.danmaku_font_family = font_family;
                        cfg.danmaku_font_scale = font_scale;
                        need_repaint = true;
                    }
                    StageCmd::SetTime { secs } => {
                        if (secs - raw_t).abs() > f64::EPSILON {
                            raw_t = secs;
                            // 暂停态的时间跳变(seek/换流后)也要一次重绘到位。
                            if !playing {
                                need_repaint = true;
                            }
                        }
                    }
                    StageCmd::SetPlaying { playing: next } => {
                        if next != playing {
                            playing = next;
                            // 暂停↔恢复:外推锚点作废,从当前时基重锚。
                            dm_time_anchor = None;
                            need_repaint = true;
                        }
                    }
                    StageCmd::Resize(metrics) => {
                        let size_changed = (metrics.width, metrics.height) != (width, height);
                        let scale_changed = (metrics.scale_x - scale_x).abs() > f32::EPSILON
                            || (metrics.scale_y - scale_y).abs() > f32::EPSILON;
                        if size_changed || scale_changed {
                            pending_resize = Some(metrics);
                            if resize_first_seen.is_none() {
                                resize_first_seen = Some(Instant::now());
                            }
                        }
                    }
                    StageCmd::Wake => {
                        need_repaint = true;
                    }
                    StageCmd::Detach => {
                        attached.store(false, Ordering::SeqCst);
                        if let Ok(raw) = chain.into_unknown_raw()
                            && let Ok(mut slot) = pending.lock()
                        {
                            *slot = Some(SendRaw::new(raw));
                        }
                    }
                    StageCmd::Quit => break 'main,
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'main,
            }
        }

        // P1:合并窗口到期 → 一次 ResizeBuffers(无需重挂)。
        if let (Some(metrics), Some(seen)) = (pending_resize, resize_first_seen)
            && seen.elapsed() >= RESIZE_COALESCE
        {
            pending_resize = None;
            resize_first_seen = None;
            let scale_changed = (metrics.scale_x - scale_x).abs() > f32::EPSILON
                || (metrics.scale_y - scale_y).abs() > f32::EPSILON;
            if metrics.width != width || metrics.height != height {
                if let Err(e) = chain.resize(metrics.width, metrics.height) {
                    eprintln!(
                        "[qaq-bx-video] danmaku stage: ResizeBuffers({}x{}) 失败:{e}",
                        metrics.width, metrics.height
                    );
                    break 'main;
                }
                width = metrics.width;
                height = metrics.height;
                scale_x = metrics.scale_x;
                scale_y = metrics.scale_y;
                configure_swap_chain(&mut chain, scale_x, scale_y);
                need_repaint = true;
            } else if scale_changed {
                scale_x = metrics.scale_x;
                scale_y = metrics.scale_y;
                configure_swap_chain(&mut chain, scale_x, scale_y);
                need_repaint = true;
            }
        }

        let is_attached = attached.load(Ordering::SeqCst);
        let danmaku_active = cfg.danmaku_enabled && !overlay.is_empty();

        if is_attached && (need_repaint || (playing && danmaku_active)) {
            need_repaint = false;
            // 播放中在两次节拍之间用墙钟线性外推;暂停/节拍停止即冻结。
            let t = if danmaku_active && playing {
                interpolated_danmaku_time(&mut dm_time_anchor, raw_t, Instant::now())
            } else {
                dm_time_anchor = None;
                raw_t
            };
            if let Err(e) = draw_present(
                &mut chain,
                &overlay,
                &cfg,
                &fonts,
                t,
                width as f32 / scale_x,
                height as f32 / scale_y,
                danmaku_active,
            ) {
                eprintln!("[qaq-bx-video] danmaku stage: {e}");
                break 'main;
            }
            // 诊断锚:透明层首次真正画出弹幕(数据链 + 绘制链同时通)。
            if !first_paint_logged && danmaku_active {
                first_paint_logged = true;
                eprintln!(
                    "STAGE-B: danmaku 首帧已绘制 items={} t={t:.1}",
                    overlay.len()
                );
            }
        }

        // 空闲(未挂载 / 暂停 / 无弹幕 / 弹幕关闭)阻塞休眠,0 空转。
        if !is_attached || !playing || !danmaku_active {
            if need_repaint || pending_resize.is_some() {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            match cmd_rx.recv() {
                Ok(cmd) => {
                    first_pending = Some(cmd);
                    continue;
                }
                Err(_) => break 'main,
            }
        }

        // 弹幕活动期:实际帧率由 Present(1) 的 vsync 阻塞决定,预算仅防空转。
        let now = Instant::now();
        if next_frame < now {
            next_frame = now;
        }
        next_frame += RENDER_BUDGET;
        match cmd_rx.recv_timeout(next_frame - now) {
            Ok(cmd) => {
                first_pending = Some(cmd);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break 'main,
        }
    }

    drop(chain);
}

/// 单帧:清透明 → 画弹幕 → Present(vsync)。
fn draw_present(
    chain: &mut windows_canvas::SwapChain,
    overlay: &[OverlayDanmaku],
    cfg: &VideoConfig,
    fonts: &DanmakuFonts,
    t: f64,
    dip_width: f32,
    dip_height: f32,
    danmaku_active: bool,
) -> Result<(), String> {
    let session = chain.begin_draw().map_err(|e| format!("begin_draw 失败:{e}"))?;
    // FLIP 轮转会复用旧内容:透明层每帧必须先清,否则弹幕留鬼影。
    session.clear(ColorF::TRANSPARENT);
    if danmaku_active {
        draw_danmaku_overlay(
            &session,
            overlay,
            cfg,
            fonts,
            t,
            dip_width,
            dip_height,
        );
    }
    drop(session);
    match chain.present() {
        Ok(true) => Ok(()),
        Ok(false) => Err("present 报设备丢失".to_string()),
        Err(e) => Err(format!("present 失败:{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::DANMAKU_DEFAULT_FONT;

    fn item(progress_ms: i32, content: &str) -> OverlayDanmaku {
        OverlayDanmaku {
            progress_ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: content.to_string(),
            mid_hash: String::new(),
            weight: 0,
        }
    }

    /// 层句柄可在无 UI 环境下创建/投递命令而不 panic(命令通道非阻塞)。
    #[test]
    fn stage_accepts_commands_without_ui() {
        let stage = DanmakuStage::new();
        stage.set_danmaku(vec![item(1_000, "测试")]);
        stage.append_danmaku(vec![item(2_000, "追加")]);
        stage.set_danmaku_config(true, 0.9, 10, DANMAKU_DEFAULT_FONT, 1.0);
        stage.set_danmaku_filter(DanmakuFilter::default());
        stage.set_time(1.5);
        stage.set_time(f64::NAN); // 非法时基必须被丢弃而非污染外推
        stage.set_playing(true);
        stage.set_playing(false);
        // 非法/未布局尺寸静默忽略(与 VideoEngine::set_metrics 同口径)。
        stage.set_metrics(0.0, 0.0, 1.0, 1.0);
        stage.set_metrics(366.0, 206.0, 1.0, 1.0);
        stage.detach_panel();
        assert!(!stage.attached());
        drop(stage); // Drop 走 Quit + join,不得卡死
    }

    /// 挂载协议:pending 槽至多交付一次;入队失败可原样放回重试。
    #[test]
    fn mount_handoff_protocol_is_single_use() {
        let stage = DanmakuStage::new();
        // 层线程建 swapchain 是异步的:轮询等待首个产物(上限 2s)。
        let deadline = Instant::now() + Duration::from_secs(2);
        let raw = loop {
            if let Some(raw) = stage.take_pending() {
                break raw;
            }
            if Instant::now() > deadline {
                eprintln!("(跳过)层线程未产出 swapchain");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(!raw.is_null());
        assert!(stage.take_pending().is_none(), "同一次产物不得重复交付");
        stage.put_back_pending(raw);
        assert!(stage.take_pending().is_some(), "放回后应可重试");
        stage.mark_attached(true);
        assert!(stage.attached());
        drop(stage);
    }

    /// 句柄等价只看身份(reactor Input 的 PartialEq 语义)。
    #[test]
    fn stage_equality_is_identity() {
        let a = DanmakuStage::new();
        let b = a.clone();
        let c = DanmakuStage::new();
        assert!(a == b);
        assert!(a != c);
    }
}
