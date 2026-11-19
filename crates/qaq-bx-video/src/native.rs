//! 系统媒体引擎(Windows.Media.Playback + AdaptiveMediaSource,v0.3 正式实现)。
//!
//! 方案实证自 Richasy/Bili.Uwp(NativePlayerViewModel):内存 MPD
//! (qaq-bx-core::mpd 生成)→ `AdaptiveMediaSource::CreateFromStreamWithDownloaderAsync`
//! (带 Referer 头的 HttpClient)→ `MediaSource::CreateFromAdaptiveMediaSource`
//! → 注入 **MediaPlayerElement 自持的 MediaPlayer**(渲染由 MPE 托管)。
//!
//! 协议(WinUI3 MPE 语义决定):MPE 内部 `MediaPlayer` 属性**只读**,播放面必须
//! 用元素自己的 player。引擎不造 player:由 UI(`NativeSurface`)经
//! `request_set_media_source_get_player`(UI 线程 imperative)注入引擎产出的
//! MediaSource 并读回 player,再交 `attach_player(raw, seq)` 接管。此后
//! 控制/状态(play/pause/seek/volume/position)全部作用于该 player,与 mpv 引擎的
//! `PlaybackStatus` 快照语义对齐。
//!
//! 音频重定向约定(与 Bili.Uwp 同款):MPD 内两个 Representation 的
//! `<BaseURL>` 留空,manifest URI 用视频流 base_url;`DownloadRequested` 里把
//! `ResourceContentType == "audio/*"` 的请求重定向到音频 base_url。
//!
//! 防盗链约定(与 mpv 通道同口径):管线用的 `HttpClient` 必须同时带
//! `Referer: https://www.bilibili.com` **和浏览器 `User-Agent`**——只给 Referer 时
//! B 站 CDN 会间歇 403(`MediaFailed(SourceNotSupported)`,
//! `ExtendedErrorCode=0x80190193`),日志里看起来像"源格式不支持",实为下载被拒。
//!
//! 线程模型(v0.3 正式版):`load` **不阻塞调用线程**(UI 线程)。MPD → MediaSource
//! 的网络+解析在专用工作线程(COM MTA)执行,产物按「装载世代」落 pending 槽,
//! UI 的 250ms 挂载节拍取出注入。**首次装载 / 换画质 / 换分P 走同一条路径**:
//! 引擎产出 → UI 注入 → attach 回放该世代的 autoplay/start 语义。世代时序集中在
//! [`LoadLedger`](纯逻辑、不碰 COM,`mod tests` 覆盖过期结果丢弃与注入推进)。
//! 装载均由用户操作触发(打开视频/选画质/选分P),不做并发闸;快速连点时旧世代
//! 结果直接丢弃(见 `LoadLedger::finish` → `Finish::Stale`)。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows::Win32::combaseapi::{CoInitializeEx, CoUninitialize};
use windows::core::{HSTRING, Interface as _};
use windows::Foundation::Uri;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{MediaPlayer, MediaPlaybackSession, MediaPlaybackState};
use windows::Media::Streaming::Adaptive::AdaptiveMediaSourceCreationStatus;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Web::Http::HttpClient;
use windows_time::TimeSpan;

const REFERER: &str = "https://www.bilibili.com";
/// 与 `engine.rs::BROWSER_UA` / `qaq-bx-core::wbi::BROWSER_UA` 同一口径(本 crate
/// 不依赖 qaq-bx-core)。系统管线只用 MF 默认 UA 会被 B 站 CDN 间歇 403
/// (实测 `MediaFailed(SourceNotSupported) + ExtendedErrorCode=0x80190193`=HTTP 403),
/// 分段下载必须带浏览器 UA + Referer 两条头,与 mpv 路径一致。
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
const MPD_CONTENT_TYPE: &str = "application/dash+xml";
/// 面板重建换源的最小间隔:快速伸缩窗口会连续拆树/重建,每个节拍都重建既浪费
/// 线程,也让管线反复开局。节流期内断点保持未消费,由后续节拍继续。
const REBUILD_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1_500);
/// `AdaptiveMediaSource::CreateFromStreamWithDownloaderAsync` 建源超时阈值。
const BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// `CoInitializeEx` 的 COINIT_MULTITHREADED(与 `codec.rs` WIC 探测同一口径)。
const COINIT_MULTITHREADED: u32 = 0;

/// 播放状态快照(shell 250ms 节拍轮询;与 mpv 侧 PlaybackStatus 字段对齐)。
#[derive(Debug, Clone, Default)]
pub struct NativeStatus {
    pub playing: bool,
    pub opening: bool,
    pub buffering: bool,
    pub paused: bool,
    pub wants_play: bool,
    pub media_opened: bool,
    pub buffering_progress: f64,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub finished: bool,
    /// 装载在途(工作线程正在建 MediaSource):UI 据此保持「加载中」遮罩。
    pub loading: bool,
    pub error: Option<String>,
    /// 装载世代(`LoadLedger::begin` 同步自增;load() 返回即已落账)。UI 用
    /// 「本次 load 的世代 && !loading」识别新会话已构建完成——loading 单独
    /// 不可靠:快速建源时它在首个 UI 节拍前就可能翻回 false。
    pub request_seq: u64,
    /// 已被 UI 注入并接管的最近世代。
    pub injected_seq: Option<u64>,
    /// 当前视频流解码后的固有像素尺寸(0x0 表示尚未就绪或无视频轨)。
    pub natural_video_width: u32,
    pub natural_video_height: u32,
}

/// 事件 revoker 存活盒:WinRT 事件 revoker 内部 COM 指针 Agile 但未标 Send,
/// 统一擦除(仅保活,不跨线程解引用)。
struct AgileBox(#[allow(dead_code)] Box<dyn std::any::Any>);
unsafe impl Send for AgileBox {}
unsafe impl Sync for AgileBox {}

/// attach 前挂起的装载产物(MediaSource 等 UI 注入 MPE 后接管 player)。
struct PendingSource {
    /// 该产物所属装载世代(与 `LoadLedger.built_seq` 一致)。
    seq: u64,
    source: windows::core::IUnknown,
    keep: Vec<AgileBox>,
}

/// 一次装载的收尾结论(工作线程 → 引擎槽位)。
#[derive(Debug, PartialEq)]
enum Finish {
    /// 已被更新的装载请求取代:结果丢弃。
    Stale,
    /// 本世代构建失败(错误已入 ledger)。
    Failed,
    /// 本世代产物可用,携带该次装载的时序语义。
    Ready { autoplay: bool, start_secs: f64 },
}

/// 装载世代账本:纯时序状态机,不触碰任何 COM 对象(可离线单测)。
#[derive(Debug, Default, PartialEq)]
struct LoadLedger {
    /// 最近一次 `load` 请求的世代(单调自增)。
    request_seq: u64,
    /// 最近一次请求的时序语义(autoplay, start_secs)。
    intent: Option<(bool, f64)>,
    /// 已构建完成、待 UI 注入的世代。
    built_seq: Option<u64>,
    /// 已被 UI 注入的世代。
    injected_seq: Option<u64>,
    /// 注入在途(UI 已领取注入权,尚未回调):同一份 MediaSource 全局只允许
    /// 一次绑定(MPE 元素重建时旧绑定尚未收尾就再注入,管线报 SourceNotSupported)。
    inflight_seq: Option<u64>,
    /// 有构建在途。
    loading: bool,
    /// 面板已拆走、等重建后续播:此期间旧产物不得注入新元素(必须换新产物)。
    awaiting_rebuild: bool,
    /// 最近一次装载失败(下一次 `begin` 清除)。
    load_error: Option<String>,
    /// MediaFailed 上报的运行时错误(下一次 `begin` / 成功注入清除)。
    media_error: Option<String>,
    /// MediaEnded 到达。
    finished: bool,
}

impl LoadLedger {
    /// 发起一次装载:世代自增、在途置位、清历史错误;返回新世代。
    fn begin(&mut self, autoplay: bool, start_secs: f64) -> u64 {
        self.request_seq += 1;
        self.intent = Some((autoplay, start_secs));
        self.built_seq = None;
        self.inflight_seq = None;
        self.loading = true;
        self.awaiting_rebuild = false;
        self.load_error = None;
        self.media_error = None;
        self.finished = false;
        self.request_seq
    }

    /// 工作线程收尾:只有最新世代的结果生效;过期结果不影响在途状态。
    fn finish(&mut self, seq: u64, error: Option<String>) -> Finish {
        if seq != self.request_seq {
            return Finish::Stale;
        }
        self.loading = false;
        match error {
            Some(message) => {
                self.load_error = Some(message);
                Finish::Failed
            }
            None => {
                // 全新 MediaSource 尚未放入 pending 也未被任何 MediaPlayer 绑定,
                // 若此前面板卸载落在在途建源期间,可直接由新面板认领该新产物。
                self.awaiting_rebuild = false;
                self.built_seq = Some(seq);
                self.load_error = None;
                let (autoplay, start_secs) = self.intent.unwrap_or((false, 0.0));
                Finish::Ready {
                    autoplay,
                    start_secs,
                }
            }
        }
    }

    /// 可注入的世代(已构建、尚未注入、无注入在途,且面板没在等重建换源)。
    fn pending_seq(&self) -> Option<u64> {
        match self.built_seq {
            Some(seq)
                if self.injected_seq != Some(seq)
                    && self.inflight_seq.is_none()
                    && !self.awaiting_rebuild =>
            {
                Some(seq)
            }
            _ => None,
        }
    }

    /// UI 领取一次注入权(全局单飞:同一份 MediaSource 不得同时交给两个元素
    /// ——WinRT 的 MediaSource 只能绑一个 MediaPlayer,重复绑定管线会报
    /// SourceNotSupported)。
    fn claim(&mut self) -> Option<u64> {
        let seq = self.pending_seq()?;
        self.inflight_seq = Some(seq);
        Some(seq)
    }

    /// 注入请求失败(面板未就绪):放回注入权,下一拍同世代重试。
    fn release(&mut self, seq: u64) {
        if self.inflight_seq == Some(seq) {
            self.inflight_seq = None;
        }
    }

    /// UI 注入完成:推进注入世代(单调),清运行时错误/结束标志;
    /// 仅当注入的是最新世代时返回其时序语义。
    fn inject(&mut self, seq: u64) -> Option<(bool, f64)> {
        if self.inflight_seq == Some(seq) {
            self.inflight_seq = None;
        }
        if self.injected_seq.is_none_or(|done| seq > done) {
            self.injected_seq = Some(seq);
        }
        self.media_error = None;
        self.load_error = None;
        self.finished = false;
        if seq == self.request_seq { self.intent } else { None }
    }

    /// 快照视图:在途 / 结束 / 生效错误(装载错误优先) / 请求世代 / 已注入世代。
    fn view(&self) -> (bool, bool, Option<String>, u64, Option<u64>) {
        (
            self.loading,
            self.finished,
            self.load_error.clone().or_else(|| self.media_error.clone()),
            self.request_seq,
            self.injected_seq,
        )
    }

    /// 显式停播/切离 native 时复位账本并推进请求世代，令所有在途建源自动落为 Stale。
    fn reset_stopped(&mut self) {
        self.request_seq += 1;
        self.intent = None;
        self.built_seq = None;
        self.injected_seq = None;
        self.inflight_seq = None;
        self.loading = false;
        self.awaiting_rebuild = false;
        self.load_error = None;
        self.media_error = None;
        self.finished = false;
    }

    /// 面板拆走:旧元素自持的 player 作废,标记「等重建换源」并撤回注入权。
    /// 旧 MediaSource 已被那个 player 绑走,绝不能给新元素用。
    fn note_rebuild(&mut self) {
        self.awaiting_rebuild = true;
        self.inflight_seq = None;
    }

    fn note_media_ended(&mut self) {
        self.finished = true;
    }

    fn note_media_failed(&mut self, message: String) {
        self.media_error = Some(message);
    }
}

/// 最近一次装载请求(面板重建的兜底原料:仅当没有可复用的 MediaSource 产物时
/// 才认为可复用;面板重建一律走 `remount_pending` 重新构建新产物)。
#[derive(Clone)]
struct LoadRequest {
    mpd: String,
    video_base_url: String,
    audio_base_url: Option<String>,
}

/// 系统媒体引擎(attach 模式;字段以 Mutex/RwLock 保护,Arc 共享合法)。
pub struct NativePlayer {
    /// UI 交回的 MPE 内部 player(attach 后 Some;面板卸载时清空)。
    player: std::sync::Mutex<Option<MediaPlayer>>,
    session: std::sync::Mutex<Option<MediaPlaybackSession>>,
    /// 待注入 MPE 的 MediaSource(引擎工作线程产出;UI 注入后仍留作最新产物)。
    pending: std::sync::Mutex<Option<PendingSource>>,
    /// 装载世代账本(时序 + 错误 + 在途;MediaEnded/MediaFailed 也写这里)。
    ledger: Arc<std::sync::Mutex<LoadLedger>>,
    /// 最近一次装载原料(面板重建时原位重载)。
    last_request: std::sync::Mutex<Option<LoadRequest>>,
    /// 面板卸载时记住的断点(autoplay, start_secs);新面板挂载时消费。
    resume: std::sync::Mutex<Option<(bool, f64)>>,
    /// 上一次「面板重建换源」的时刻:重建风暴(快速伸缩窗口)下按此节流,
    /// 避免每个挂载节拍都起一个装载线程。
    last_rebuild_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// 期望音量(0..=100 bits;attach 前设定也不丢,接管时统一施加)。
    volume: AtomicU64,
    /// 最近接管世代(供 MediaOpened 异步回调识别是否对应当前 pending_seek)。
    attached_seq: Arc<AtomicU64>,
    /// 即将注入/已触发 MediaOpened 的世代（早于 attach_player 前写入，防首次回调竞态）。
    opened_seq: Arc<AtomicU64>,
    /// 正在领取等待 SetMediaSource 的世代。
    claiming_seq: Arc<AtomicU64>,
    /// 待在 MediaOpened 时兜底施加的断点位置(attach 时 session 可能尚未 Opened)。
    pending_seek: Arc<std::sync::Mutex<Option<(u64, f64)>>>,
    /// 当前生效 AdaptiveMediaSource 及 DownloadRequested revoker（切源/停播即释，O(1) 内存）。
    active_keep: std::sync::Mutex<Vec<AgileBox>>,
    /// player 事件 revoker(面板卸载即释放 = 退订旧元素自持的 player)。
    event_keep: std::sync::Mutex<Vec<AgileBox>>,
    /// 用户侧播放意愿(Play/Pause 调用与装载意图写;面板重建续播以此为准,
    /// 不受「刚装载还没开播」的瞬时状态影响)。
    want_play: Arc<AtomicBool>,
    /// 至少成功注入过一次(= 有画面可看)。
    loaded: AtomicBool,
}

fn seek_session(session: &MediaPlaybackSession, secs: f64) -> Result<(), String> {
    session
        .SetPosition(TimeSpan {
            duration: (secs.max(0.0) * 10_000_000.0) as i64,
        })
        .map_err(|e| format!("Seek 失败:{e}"))
}

// fork 对 MediaPlayer/会话对象 unsafe impl Send/Sync(Media_Core feature),
// Mutex 包装后整组共享合法;工作线程只经 ledger + pending 槽交互。
unsafe impl Send for NativePlayer {}
unsafe impl Sync for NativePlayer {}

impl Default for NativePlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl NativePlayer {
    pub fn new() -> Self {
        Self {
            player: std::sync::Mutex::new(None),
            session: std::sync::Mutex::new(None),
            pending: std::sync::Mutex::new(None),
            ledger: Arc::new(std::sync::Mutex::new(LoadLedger::default())),
            last_request: std::sync::Mutex::new(None),
            resume: std::sync::Mutex::new(None),
            last_rebuild_at: std::sync::Mutex::new(None),
            volume: AtomicU64::new(100.0f64.to_bits()),
            attached_seq: Arc::new(AtomicU64::new(0)),
            opened_seq: Arc::new(AtomicU64::new(0)),
            claiming_seq: Arc::new(AtomicU64::new(0)),
            pending_seek: Arc::new(std::sync::Mutex::new(None)),
            active_keep: std::sync::Mutex::new(Vec::new()),
            event_keep: std::sync::Mutex::new(Vec::new()),
            want_play: Arc::new(AtomicBool::new(false)),
            loaded: AtomicBool::new(false),
        }
    }

    /// UI(UI 线程 imperative 的 completion)交回 MPE 内部 player:接管控制,
    /// 并按 `seq` 回放该次装载的 autoplay/start 语义。可重复调用(换画质/换分P
    /// 的每次注入都会读回同一个 player),事件仅订阅一次。
    pub fn attach_player(&self, player: windows::core::IUnknown, seq: u64) -> Result<(), String> {
        let player: MediaPlayer = player.cast().map_err(|e| format!("player cast 失败:{e}"))?;
        eprintln!("[qaq-bx-video] native attach: MPE 内部 player 已接管(seq={seq})");
        // MPE 默认 AutoPlay=true;起播时机收归引擎(shell settings.autoplay)。
        let _ = player.SetAutoPlay(false);
        let session = player
            .PlaybackSession()
            .map_err(|e| format!("PlaybackSession 获取失败:{e}"))?;
        self.attached_seq.store(seq, Ordering::SeqCst);
        // MediaEnded / MediaFailed → 世代账本(仅订阅一次)。
        let subscribed = self
            .player
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false);
        if !subscribed {
            let ledger_ended = self.ledger.clone();
            let opened_player = player.clone();
            let opened_session = session.clone();
            let opened_seq = self.attached_seq.clone();
            let opened_claiming_seq = self.claiming_seq.clone();
            let opened_seq_ack = self.opened_seq.clone();
            let opened_seek = self.pending_seek.clone();
            let opened_want_play = self.want_play.clone();
            let opened = player
                .MediaOpened(move |_, _| {
                    let cur_seq = opened_seq
                        .load(Ordering::SeqCst)
                        .max(opened_claiming_seq.load(Ordering::SeqCst));
                    if cur_seq > 0 {
                        opened_seq_ack.store(cur_seq, Ordering::SeqCst);
                    }
                    let nw = opened_session.NaturalVideoWidth().unwrap_or(0);
                    let nh = opened_session.NaturalVideoHeight().unwrap_or(0);
                    eprintln!("[qaq-bx-video] native media opened (nat={nw}x{nh})");
                    if let Ok(mut slot) = opened_seek.lock()
                        && let Some((target_seq, target_secs)) = *slot
                        && target_seq == opened_seq.load(Ordering::SeqCst)
                    {
                        *slot = None;
                        let cur = opened_session
                            .Position()
                            .ok()
                            .map(|t| t.duration as f64 / 10_000_000.0)
                            .unwrap_or(0.0);
                        if target_secs > 0.0 && cur < (target_secs - 1.0).max(0.5) {
                            let _ = seek_session(&opened_session, target_secs);
                        }
                    }
                    if opened_want_play.load(Ordering::SeqCst) {
                        let _ = opened_player.Play();
                    }
                })
                .map(|r| AgileBox(Box::new(r)));
            let ended = player
                .MediaEnded(move |_, _| {
                    if let Ok(mut ledger) = ledger_ended.lock() {
                        ledger.note_media_ended();
                    }
                })
                .map(|r| AgileBox(Box::new(r)));
            let ledger_failed = self.ledger.clone();
            let failed = player
                .MediaFailed(move |_, args| {
                    let Some(args) = args.as_ref() else { return };
                    let message = args
                        .ErrorMessage()
                        .map(|m| m.to_string_lossy())
                        .unwrap_or_default();
                    let kind = args.Error();
                    // ExtendedErrorCode = MF/管线真正的 HRESULT,只有它能区分
                    // 「源不支持」与「管线被中途拆毁/网络拒绝」。
                    let extended = args
                        .ExtendedErrorCode()
                        .map(|e| format!("{:#010X}", e.0 as u32))
                        .unwrap_or_else(|_| "无".to_string());
                    eprintln!(
                        "[qaq-bx-video] native media failed: error={kind:?} extended={extended} message={message}"
                    );
                    if let Ok(mut ledger) = ledger_failed.lock() {
                        ledger.note_media_failed(format!(
                            "媒体失败(error={kind:?},extended={extended}):{message}"
                        ));
                    }
                })
                .map(|r| AgileBox(Box::new(r)));
            // revoker 单独存放:面板卸载时释放 = 退订旧元素自持的 player。
            let mut keep = self.event_keep.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(r) = opened {
                keep.push(r);
            }
            if let Ok(r) = ended {
                keep.push(r);
            }
            if let Ok(r) = failed {
                keep.push(r);
            }
        }
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = Some(player.clone());
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.clone());
        self.apply_volume(&player);

        // 注入的是最新产物 → 回放该世代的时序语义;过期世代的注入只做接管。
        let intent = {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.inject(seq)
        };
        if let Some((autoplay, start_secs)) = intent {
            if start_secs > 0.0 {
                *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((seq, start_secs));
                let _ = seek_session(&session, start_secs);
            } else {
                *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            if autoplay {
                let _ = player.Play();
            }
            self.loaded.store(true, Ordering::SeqCst);
        }
        // 新 MediaSource 已完成绑定：将其保活句柄切为当前 active_keep，淘汰上一条流的 AMS/revoker。
        if let Some(pending_keep) = self
            .pending
            .lock()
            .ok()
            .and_then(|mut slot| {
                if slot.as_ref().is_some_and(|p| p.seq == seq) {
                    slot.take().map(|p| p.keep)
                } else {
                    None
                }
            })
        {
            *self.active_keep.lock().unwrap_or_else(|e| e.into_inner()) = pending_keep;
        }
        Ok(())
    }

    /// UI(NativeSurface)领取一次注入权:返回待注入 MPE 的 MediaSource。
    ///
    /// 单飞语义:同一份 MediaSource 只能绑一个 MediaPlayer(MPE 元素重建时若让
    /// 两个元素同时绑定同一份源,管线报 SourceNotSupported),所以领取后其它
    /// 面板实例一律拿到 `None`,直到本次注入收尾(`release_injection` 或
    /// `attach_player`)。
    pub fn claim_pending_source(&self) -> Option<(u64, windows::core::IUnknown)> {
        let seq = {
            let mut ledger = self.ledger.lock().ok()?;
            ledger.claim()?
        };
        self.claiming_seq.store(seq, Ordering::SeqCst);
        let slot = self.pending.lock().ok()?;
        let source = slot
            .as_ref()
            .filter(|pending| pending.seq == seq)
            .map(|pending| pending.source.clone());
        match source {
            Some(source) => Some((seq, source)),
            None => {
                // 槽位与世代不一致(理论上不该发生):放回注入权,别把它卡死。
                self.claiming_seq.store(0, Ordering::SeqCst);
                if let Ok(mut ledger) = self.ledger.lock() {
                    ledger.release(seq);
                }
                None
            }
        }
    }

    /// 注入请求未执行(面板未实例化 / imperative 入队失败):放回注入权,
    /// 下一拍按同世代重试。
    pub fn release_injection(&self, seq: u64) {
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.release(seq);
        }
    }

    /// 当前接管的 MPE player(UI 线程读取,供系统媒体控件 SMTC 绑定)。
    /// 面板重建后返回的是新元素的 player,调用方需据此重新绑定。
    pub fn current_player(&self) -> Option<MediaPlayer> {
        self.player
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 提交 MPD 装载请求:**立即返回**(不阻塞调用线程)。工作线程建好
    /// MediaSource 后落 pending 槽,由 UI 挂载节拍注入。返回 Err 仅代表请求
    /// 本身不合法(空参数 / 工作线程创建失败)。
    pub fn load(
        self: &Arc<Self>,
        mpd: &str,
        video_base_url: &str,
        audio_base_url: Option<&str>,
        autoplay: bool,
        start_secs: f64,
    ) -> Result<(), String> {
        if mpd.is_empty() || video_base_url.is_empty() {
            return Err("MPD 或视频地址为空".to_string());
        }
        self.start_build(
            LoadRequest {
                mpd: mpd.to_string(),
                video_base_url: video_base_url.to_string(),
                audio_base_url: audio_base_url.map(str::to_string),
            },
            autoplay,
            start_secs,
        );
        Ok(())
    }

    /// 装载请求公共入口:起世代 → 作废续播计划 → 留原料 → 静音旧媒体 →
    /// 起工作线程。起播时机统一由注入完成后的世代语义(`attach_player` 回放
    /// 本次装载的 autoplay/start)决定。
    fn start_build(self: &Arc<Self>, request: LoadRequest, autoplay: bool, start_secs: f64) {
        let seq = {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.begin(autoplay, start_secs)
        };
        self.opened_seq.store(0, Ordering::SeqCst);
        self.claiming_seq.store(0, Ordering::SeqCst);
        // 用户给了新的播放意图,旧的「面板重建续播」及待补 seek 计划作废。
        if let Ok(mut slot) = self.resume.lock() {
            *slot = None;
        }
        *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.last_request.lock().unwrap_or_else(|e| e.into_inner()) = Some(request.clone());
        // 旧媒体先静音:新产物注入前 MPE 仍在放上一首(换视频/换画质都会听到
        // 残留声轨)。
        if self
            .player
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
        {
            let _ = self.pause();
        }
        // 静音会把 want_play 写成 false,新意图必须在其后落定(面板重建续播
        // 以此为准,不能因为「刚静音过」就把用户的播放意愿丢掉)。
        self.want_play.store(autoplay, Ordering::SeqCst);
        self.spawn_build(request.mpd, request.video_base_url, request.audio_base_url, seq);
    }

    /// 渲染面板卸载(NativeSurface 随视图树拆走):暂停并作废旧元素自持的
    /// player/session(并退订其事件),记住断点(播放意愿 + 位置)等新面板挂载续播。
    ///
    /// 关键约束:旧 MediaSource 已被那个作废的 player 绑走,**不能**给新元素复用
    /// (否则管线报 SourceNotSupported),所以这里只置「待续播」+「等换源」标记,
    /// 由 `remount_pending` 重新构建一份新产物。
    pub fn note_surface_detached(&self) {
        let old_player = self.player.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(player) = old_player {
            // WinUI 3 MPE 离树不会自动 Pause MediaPlayer，必须显式暂停避免残留声音。
            let _ = player.Pause();
        }
        let session_pos = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .and_then(|session| session.Position().ok())
            .map(|t| t.duration as f64 / 10_000_000.0);
        self.attached_seq.store(0, Ordering::SeqCst);
        *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // 账本:旧产物作废(不得再注入新元素),撤回在途注入权。
        let intent_start = if let Ok(mut ledger) = self.ledger.lock() {
            ledger.note_rebuild();
            ledger.intent.map(|(_, s)| s).unwrap_or(0.0)
        } else {
            0.0
        };
        // 退订旧 player 事件(revoker Drop 即 remove;新元素会重新订阅)。
        if let Ok(mut keep) = self.event_keep.lock() {
            keep.clear();
        }
        let resumable = self
            .last_request
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false);
        if resumable {
            let autoplay = self.want_play.load(Ordering::SeqCst);
            let mut slot = self.resume.lock().unwrap_or_else(|e| e.into_inner());
            let prev_pos = slot.map(|(_, p)| p).unwrap_or(0.0);
            let position = match session_pos {
                Some(p) if p > 0.0 => p,
                Some(_) if prev_pos > 0.0 => prev_pos,
                Some(p) => p,
                None if prev_pos > 0.0 => prev_pos,
                None => intent_start,
            };
            *slot = Some((autoplay, position));
            eprintln!(
                "[qaq-bx-video] native surface detached: resume(autoplay={autoplay},pos={position:.1})"
            );
        }
    }

    /// 显式停播/切离 native 通路:暂停并释放全部原生对象、断点与待装载请求。
    pub fn stop(&self) {
        self.want_play.store(false, Ordering::SeqCst);
        self.loaded.store(false, Ordering::SeqCst);
        self.attached_seq.store(0, Ordering::SeqCst);
        self.opened_seq.store(0, Ordering::SeqCst);
        self.claiming_seq.store(0, Ordering::SeqCst);
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = player.Pause();
        }
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.resume.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.last_request.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.active_keep
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.event_keep
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.reset_stopped();
        }
    }

    /// 新渲染面板挂载:存在断点则**重建一份新 MediaSource** 续播。
    ///
    /// 快速伸缩窗口会连续拆树/重建(`docs/dev-notes/resize-black-screen.md` 同源
    /// 现象),所以按 `REBUILD_MIN_INTERVAL` 节流:窗口内的重复挂载不另起装载线程,
    /// 断点保持未消费,由下一个挂载节拍继续(旧产物已被作废 player 绑走,这里
    /// 绝不能拿它去注入新元素)。
    pub fn remount_pending(self: &Arc<Self>) -> bool {
        let resume = self.resume.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some((autoplay, start_secs)) = resume else {
            return false;
        };
        let now = std::time::Instant::now();
        {
            let mut slot = self.last_rebuild_at.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_some_and(|at| now.duration_since(at) < REBUILD_MIN_INTERVAL) {
                return false; // 节流:断点未消费,下一拍再来
            }
            *slot = Some(now);
        }
        // 续播成功消费断点(此后显式装载会重新记账)。
        if let Ok(mut slot) = self.resume.lock() {
            *slot = None;
        }
        let request = self
            .last_request
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(request) = request else {
            return false;
        };
        eprintln!(
            "[qaq-bx-video] native surface remount: 换源续播(autoplay={autoplay},start={start_secs:.1})"
        );
        self.start_build(request, autoplay, start_secs);
        true
    }

    /// 显式布局切换（如进入/退出全屏）清除重挂节流冷却，确保立即换源续播，
    /// 同时保留快速缩放风暴期 1500ms 节流防护。
    pub fn allow_immediate_remount(&self) {
        if let Ok(mut slot) = self.last_rebuild_at.lock() {
            *slot = None;
        }
    }

    pub fn play(&self) -> Result<(), String> {
        self.want_play.store(true, Ordering::SeqCst);
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            player.Play().map_err(|e| format!("Play 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        self.want_play.store(false, Ordering::SeqCst);
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            player.Pause().map_err(|e| format!("Pause 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn seek(&self, secs: f64) -> Result<(), String> {
        *self.pending_seek.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(session) = session {
            seek_session(&session, secs)?;
        }
        Ok(())
    }

    /// 设定期望音量并即时施加(未 attach 时只记账,attach 时统一施加)。
    pub fn set_volume(&self, pct: f64) -> Result<(), String> {
        let pct = pct.clamp(0.0, 100.0);
        self.volume.store(pct.to_bits(), Ordering::SeqCst);
        let player = self.player.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(player) = player {
            player
                .SetVolume(pct / 100.0)
                .map_err(|e| format!("Volume 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn status(&self) -> NativeStatus {
        let (loading, finished, error, request_seq, injected_seq) = {
            let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.view()
        };
        let wants_play = self.want_play.load(Ordering::SeqCst);
        let attached = self.attached_seq.load(Ordering::SeqCst);
        let opened = self.opened_seq.load(Ordering::SeqCst);
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(session) = session else {
            return NativeStatus {
                wants_play,
                loading,
                finished,
                error,
                request_seq,
                injected_seq,
                ..NativeStatus::default()
            };
        };
        let state = session.PlaybackState().ok();
        let position = session
            .Position()
            .ok()
            .map(|t| t.duration as f64 / 10_000_000.0)
            .unwrap_or(0.0);
        let duration = session
            .NaturalDuration()
            .ok()
            .map(|t| t.duration as f64 / 10_000_000.0)
            .unwrap_or(0.0);
        let natural_video_width = session.NaturalVideoWidth().unwrap_or(0);
        let natural_video_height = session.NaturalVideoHeight().unwrap_or(0);
        let buffering_progress = session.BufferingProgress().unwrap_or(0.0);
        let playing = state == Some(MediaPlaybackState::Playing);
        let opening = state == Some(MediaPlaybackState::Opening);
        let buffering = state == Some(MediaPlaybackState::Buffering);
        let paused = state == Some(MediaPlaybackState::Paused);
        let media_opened = (attached > 0 && opened == attached && attached == request_seq)
            || playing
            || paused
            || natural_video_width > 0
            || duration > 0.0;
        NativeStatus {
            playing,
            opening,
            buffering,
            paused,
            wants_play,
            media_opened,
            buffering_progress,
            position_secs: position,
            duration_secs: duration,
            finished,
            loading,
            error,
            request_seq,
            injected_seq,
            natural_video_width,
            natural_video_height,
        }
    }

    /// 期望音量施加(0..=100 bits)。
    fn apply_volume(&self, player: &MediaPlayer) {
        let bits = self.volume.load(Ordering::SeqCst);
        let pct = f64::from_bits(bits).clamp(0.0, 100.0);
        let _ = player.SetVolume(pct / 100.0);
    }

    /// 工作线程装载:COM MTA 初始化 → `build_source` → 按世代回收结果。
    fn spawn_build(
        self: &Arc<Self>,
        mpd: String,
        video_base_url: String,
        audio_base_url: Option<String>,
        seq: u64,
    ) {
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name(format!("qaq-bx-native-build-{seq}"))
            .spawn(move || {
                // S_OK = 本线程首次初始化(需配对 CoUninitialize);S_FALSE(已初始化)/
                // RPC_E_CHANGED_MODE(套间模式不同)等一律沿用现状,不释放。
                let uninit = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.0 == 0;
                let Some(engine) = weak.upgrade() else {
                    if uninit {
                        unsafe { CoUninitialize() };
                    }
                    return;
                };
                let outcome = engine.build_source(&mpd, &video_base_url, audio_base_url.as_deref());
                engine.collect_build(seq, outcome);
                if uninit {
                    unsafe { CoUninitialize() };
                }
            });
        if let Err(e) = spawned {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.finish(seq, Some(format!("装载线程创建失败:{e}")));
            eprintln!("[qaq-bx-video] native build_source failed: 装载线程创建失败:{e}");
        }
    }

    /// 工作线程产物回收:仅最新世代入 pending 槽;过期结果直接丢弃,
    /// 失败写入 ledger(经 `status()` 上报 shell)。
    fn collect_build(&self, seq: u64, outcome: Result<(MediaSource, Vec<AgileBox>), String>) {
        let error = outcome.as_ref().err().cloned();
        let finish = {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            ledger.finish(seq, error)
        };
        match finish {
            Finish::Stale => {
                eprintln!("[qaq-bx-video] native build_source stale(seq={seq}) discarded");
            }
            Finish::Failed => {
                // 错误文本已由 build_source / ledger 上报,此处不重复打印。
            }
            Finish::Ready { autoplay, start_secs } => {
                let Ok((source, keep)) = outcome else { return };
                *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingSource {
                    seq,
                    source: source.into(),
                    keep,
                });
                eprintln!(
                    "[qaq-bx-video] native build_source ok(seq={seq},autoplay={autoplay},start={start_secs:.1})"
                );
            }
        }
    }

    /// MPD → MediaSource 的产出体(带超时与保活列表;网络+解析在装载工作线程,不在 UI)。
    fn build_source(
        &self,
        mpd: &str,
        video_base_url: &str,
        audio_base_url: Option<&str>,
    ) -> Result<(MediaSource, Vec<AgileBox>), String> {
        let mut keep = Vec::with_capacity(2);
        // 1) 带 Referer 的 HttpClient(系统管线拉分段全走它)。
        let client = HttpClient::new().map_err(|e| format!("HttpClient 创建失败:{e}"))?;
        let headers = client
            .DefaultRequestHeaders()
            .map_err(|e| format!("请求头获取失败:{e}"))?;
        let referer = Uri::CreateUri(&HSTRING::from(REFERER))
            .map_err(|e| format!("Referer Uri 构造失败:{e}"))?;
        headers
            .SetReferer(&referer)
            .map_err(|e| format!("Referer 头设置失败:{e}"))?;
        headers
            .Insert(&HSTRING::from("User-Agent"), &HSTRING::from(BROWSER_UA))
            .map_err(|e| format!("User-Agent 头设置失败:{e}"))?;

        // 2) MPD 文本 → InMemoryRandomAccessStream。
        let stream =
            InMemoryRandomAccessStream::new().map_err(|e| format!("内存流创建失败:{e}"))?;
        let writer =
            DataWriter::CreateDataWriter(&stream).map_err(|e| format!("DataWriter 创建失败:{e}"))?;
        writer
            .WriteBytes(mpd.as_bytes())
            .map_err(|e| format!("MPD 写入失败:{e}"))?;
        writer
            .StoreAsync()
            .and_then(|op| op.join())
            .map_err(|e| format!("MPD 流提交失败:{e}"))?;
        stream
            .Seek(0)
            .map_err(|e| format!("MPD 流回卷失败:{e}"))?;

        // 3) AdaptiveMediaSource(manifest URI = 视频 base_url,分段相对解析;超时取消防止死等)。
        let manifest_uri = Uri::CreateUri(&HSTRING::from(video_base_url))
            .map_err(|e| format!("manifest Uri 构造失败:{e}"))?;
        let op =
            windows::Media::Streaming::Adaptive::AdaptiveMediaSource::CreateFromStreamWithDownloaderAsync(
                &stream,
                &manifest_uri,
                &HSTRING::from(MPD_CONTENT_TYPE),
                &client,
            )
            .map_err(|e| format!("AdaptiveMediaSource 创建失败:{e}"))?;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        op.when(move |res| {
            let _ = tx.send(res);
        })
        .map_err(|e| format!("AdaptiveMediaSource 回调注册失败:{e}"))?;
        let creation = match rx.recv_timeout(BUILD_TIMEOUT) {
            Ok(res) => res.map_err(|e| format!("AdaptiveMediaSource 创建失败:{e}"))?,
            Err(_) => {
                let _ = op.Cancel();
                return Err("AdaptiveMediaSource 构建超时".to_string());
            }
        };
        if creation
            .Status()
            .map_err(|e| format!("AMS status 读取失败:{e}"))?
            != AdaptiveMediaSourceCreationStatus::Success
        {
            return Err(format!(
                "MPD 解析失败(status={:?},extended={:?})",
                creation.Status(),
                creation.ExtendedError().map(|e| e.0)
            ));
        }
        let ams = creation.MediaSource().map_err(|e| format!("AMS 获取失败:{e}"))?;
        if let Ok(advanced) = ams.AdvancedSettings() {
            let _ = advanced.SetAllSegmentsIndependent(true);
        }

        // 4) 音频重定向(Bili.Uwp 同款:按 ResourceContentType 改写 URI)。
        if let Some(audio_url) = audio_base_url {
            let audio_uri =
                Uri::CreateUri(&HSTRING::from(audio_url)).map_err(|e| format!("音频 Uri 构造失败:{e}"))?;
            let revoker = ams.DownloadRequested(move |_, args| {
                let Some(args) = args.as_ref() else { return };
                if let Ok(result) = args.Result()
                    && let Ok(content_type) = args.ResourceContentType()
                    && content_type.to_string_lossy().starts_with("audio/")
                {
                    let _ = result.SetResourceUri(&audio_uri);
                }
            });
            keep.push(AgileBox(Box::new(revoker)));
        }

        // 5) 产出 MediaSource(注入路径统一:UI 挂载节拍经 imperative 交给 MPE)。
        let media_source =
            MediaSource::CreateFromAdaptiveMediaSource(&ams).map_err(|e| format!("MediaSource 包装失败:{e}"))?;
        keep.push(AgileBox(Box::new(ams)));
        Ok((media_source, keep))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> std::sync::Mutex<LoadLedger> {
        std::sync::Mutex::new(LoadLedger::default())
    }

    /// 请求 → 构建 → 注入的完整时序:世代逐段推进。
    #[test]
    fn load_lifecycle_advances_generations() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        assert_eq!(l.begin(true, 0.0), 1);
        assert_eq!(l.pending_seq(), None, "尚未产出不应可注入");
        assert_eq!(
            l.finish(1, None),
            Finish::Ready {
                autoplay: true,
                start_secs: 0.0
            }
        );
        assert_eq!(l.pending_seq(), Some(1));
        assert_eq!(l.inject(1), Some((true, 0.0)));
        assert_eq!(l.pending_seq(), None, "注入后不应重复注入同世代");
        let (loading, _, error, req_seq, injected_seq) = l.view();
        assert!(!loading, "注入完成不应仍在途");
        assert_eq!(error, None);
        assert_eq!(req_seq, 1);
        assert_eq!(injected_seq, Some(1));
    }
    /// 装载世代暴露:view().3 必须与 begin() 返回值一致——UI 以
    /// 「世代命中 && !loading」识别新会话构建完成并解除装载闸门
    /// (loading 单独不可靠:快速建源时首个 UI 节拍前就可能翻回 false)。
    #[test]
    fn view_exposes_request_seq_for_load_gate() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let seq = l.begin(true, 0.0);
        assert_eq!(l.view().3, seq, "在途世代即当前世代");
        assert!(l.view().0, "在途时 loading 必须为真");
        let _ = l.finish(seq, None);
        // 建源完成:世代不变、loading 翻 false——UI 闸门在此解除。
        assert_eq!(l.view().3, seq);
        assert!(!l.view().0);
        let next = l.begin(false, 5.0);
        assert_eq!(next, seq + 1, "下一次装载世代必须自增");
        assert_eq!(l.view().3, next);
    }
    /// 快速连点换画质:旧世代结果晚到必须丢弃,不能覆盖新请求,也不能清在途。
    #[test]
    fn stale_build_result_is_discarded_and_loading_kept() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let old = l.begin(true, 30.0);
        let new = l.begin(true, 30.0);
        assert_eq!(old, 1);
        assert_eq!(new, 2);
        // 旧世代结果晚到:丢弃,在途保持(新请求仍在工作线程)。
        assert_eq!(l.finish(old, None), Finish::Stale);
        assert!(l.view().0);
        assert_eq!(l.pending_seq(), None);
        // 新世代结果:生效,携带新请求的意图。
        assert_eq!(
            l.finish(new, None),
            Finish::Ready {
                autoplay: true,
                start_secs: 30.0
            }
        );
        assert!(!l.view().0);
        assert_eq!(l.pending_seq(), Some(new));
    }

    /// 构建失败:在途清除 + 错误可被 status 视图读到;下一次请求清错误。
    #[test]
    fn build_failure_surfaces_and_is_cleared_by_next_request() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let seq = l.begin(false, 0.0);
        assert_eq!(
            l.finish(seq, Some("MPD 解析失败".to_string())),
            Finish::Failed
        );
        let (loading, _, error, _, _) = l.view();
        assert!(!loading);
        assert_eq!(error.as_deref(), Some("MPD 解析失败"));
        assert_eq!(l.pending_seq(), None);
        // 新请求清历史错误(避免旧错误一直挂在遮罩上)。
        l.begin(true, 0.0);
        assert_eq!(l.view().2, None);
        assert!(l.view().0);
    }

    /// 过期世代的注入只做接管,不回放新请求的时序语义(否则会按旧意图起播)。
    #[test]
    fn injecting_stale_generation_does_not_replay_latest_intent() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let old = l.begin(true, 12.0);
        let new = l.begin(false, 0.0);
        l.finish(old, None);
        l.finish(new, None);
        // UI 先完成 old 的注入(请求在途时新产物已落槽)。
        assert_eq!(l.inject(old), None);
        assert_eq!(l.pending_seq(), Some(new), "新世代仍需一次注入");
        assert_eq!(l.inject(new), Some((false, 0.0)));
        assert_eq!(l.pending_seq(), None);
    }

    /// MediaFailed 优先由注入清除;MediaEnded 置 finished(与 mpv eof 语义对齐)。
    #[test]
    fn media_events_feed_the_ledger_view() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let seq = l.begin(true, 0.0);
        l.finish(seq, None);
        l.note_media_failed("0xc00d36c4".to_string());
        assert_eq!(l.view().2.as_deref(), Some("0xc00d36c4"));
        assert_eq!(l.inject(seq), Some((true, 0.0)));
        assert_eq!(l.view().2, None, "成功注入后运行时错误应清除");
        l.note_media_ended();
        assert!(l.view().1);
    }

    /// 工作线程只在建好源后落槽:未 attach 也能取到待注入产物(协议不依赖 player)。
    #[test]
    fn pending_source_is_empty_before_build() {
        let engine = Arc::new(NativePlayer::new());
        assert!(engine.claim_pending_source().is_none());
        let status = engine.status();
        assert!(!status.loading);
        assert!(status.error.is_none());
        // 空参数请求同步失败(不占世代、不起线程)。
        assert!(engine.load("", "https://x", None, true, 0.0).is_err());
        assert!(!engine.status().loading);
    }

    /// 装载请求非法参数不污染世代账本;合法请求进入在途(线程可能极短已完成)。
    #[test]
    fn load_marks_loading_and_rejects_empty_urls() {
        let engine = Arc::new(NativePlayer::new());
        assert!(
            engine
                .load("<MPD/>", "", None, true, 0.0)
                .is_err()
        );
        let ledger = engine.ledger.lock().unwrap();
        assert_eq!(ledger.request_seq, 0, "非法请求不应推进世代");
        drop(ledger);
        // 受理即返回(非法 MPD + 保留域主机:工作线程必然快速失败,不触网),
        // 不断言终态——只验证调用者没被阻塞、世代已推进。
        let started = std::time::Instant::now();
        assert!(
            engine
                .load("<not-dash>", "https://invalid.invalid/x.mpd", None, false, 0.0)
                .is_ok()
        );
        let elapsed = started.elapsed();
        let ledger = engine.ledger.lock().unwrap();
        assert_eq!(ledger.request_seq, 1);
        assert!(
            ledger.loading || ledger.built_seq.is_some() || ledger.load_error.is_some()
        );
        drop(ledger);
        // 本项的核心承诺:装载耗时全在工作线程,调用者(UI 线程)即刻返回。
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "load 阻塞了调用线程:{elapsed:?}"
        );
    }

    /// 面板重建续播:旧产物已被作废 player 绑走,`note_rebuild` 后一律不得再注入,
    /// 直到新的装载世代落地。
    #[test]
    fn rebuild_flag_blocks_stale_source_until_new_generation_lands() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let old = l.begin(true, 10.0);
        l.finish(old, None);
        assert_eq!(l.pending_seq(), Some(old));
        assert_eq!(l.inject(old), Some((true, 10.0)));
        // 面板拆走:旧产物作废,禁止注入下一个元素。
        l.note_rebuild();
        assert_eq!(l.pending_seq(), None, "重建期不得复用被绑走的旧源");
        // 重建续播发起新装载并产出 → 才可注入。
        let fresh = l.begin(true, 10.0);
        assert_ne!(fresh, old);
        assert_eq!(l.pending_seq(), None, "新世代还没产出");
        l.finish(fresh, None);
        assert_eq!(l.pending_seq(), Some(fresh));
    }

    /// 注入权单飞:同一份 MediaSource 同时只允许一次绑定(两个 MPE 实例并发
    /// 绑定同一份源会触发管线 SourceNotSupported);失败可放回重试。
    #[test]
    fn injection_claim_is_single_flight_and_releasable() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let seq = l.begin(true, 0.0);
        l.finish(seq, None);
        assert_eq!(l.claim(), Some(seq));
        assert_eq!(l.claim(), None, "注入在途时其它实例必须拿到 None");
        l.release(seq);
        assert_eq!(l.claim(), Some(seq), "放回后可重新领取");
        assert_eq!(l.inject(seq), Some((true, 0.0)));
        assert_eq!(l.claim(), None, "已注入的世代不再重复注入");
    }

    /// 未装载过的面板卸载/挂载不起线程、不记断点(首屏路由切换的常见路径)。
    #[test]
    fn detach_without_load_records_nothing() {
        let engine = Arc::new(NativePlayer::new());
        engine.note_surface_detached();
        assert!(engine.resume.lock().unwrap().is_none());
        assert!(!engine.remount_pending());
        assert!(engine.claim_pending_source().is_none());
        assert!(!engine.status().loading);
    }

    /// 显式装载作废待续播计划(用户切视频时不能被旧的 resume 语义劫持)。
    #[test]
    fn explicit_load_cancels_pending_resume() {
        let engine = Arc::new(NativePlayer::new());
        {
            let mut slot = engine.resume.lock().unwrap();
            *slot = Some((true, 88.0));
        }
        // 非法参数不消耗 resume(请求根本没被受理)。
        assert!(engine.load("", "", None, false, 0.0).is_err());
        assert!(engine.resume.lock().unwrap().is_some());
        // 合法请求:作废续播 + 记原料;工作线程随后自行失败(保留域主机)。
        assert!(
            engine
                .load("<not-dash>", "https://invalid.invalid/x.mpd", None, true, 3.0)
                .is_ok()
        );
        assert!(engine.resume.lock().unwrap().is_none());
        assert!(engine.last_request.lock().unwrap().is_some());
        assert!(engine.want_play.load(Ordering::SeqCst));
    }

    /// 播放意愿记账:暂停写 false,恢复写 true(面板重建续播只看意愿)。
    #[test]
    fn want_play_tracks_explicit_control_calls() {
        let engine = NativePlayer::new();
        assert!(!engine.want_play.load(Ordering::SeqCst));
        let _ = engine.play();
        assert!(engine.want_play.load(Ordering::SeqCst));
        let _ = engine.pause();
        assert!(!engine.want_play.load(Ordering::SeqCst));
    }

    /// 未接管前控制调用不报错(UI 节拍可能先于 attach 到达)。
    #[test]
    fn controls_are_noops_before_attach() {
        let engine = NativePlayer::new();
        assert!(engine.play().is_ok());
        assert!(engine.pause().is_ok());
        assert!(engine.seek(3.0).is_ok());
        assert!(engine.set_volume(42.0).is_ok());
        let bits = engine.volume.load(Ordering::SeqCst);
        assert_eq!(f64::from_bits(bits), 42.0, "音量应先记账");
        let status = engine.status();
        assert!(!status.playing);
        assert_eq!(status.position_secs, 0.0);
    }

    /// 连续二次 detach(如快速进出全屏/双屏重建,第二次时 session 已 None)
    /// 不得用 0.0 覆盖第一次记下的非零断点。
    #[test]
    fn double_detach_preserves_nonzero_resume_position() {
        let engine = Arc::new(NativePlayer::new());
        *engine.last_request.lock().unwrap() = Some(LoadRequest {
            mpd: "<MPD/>".into(),
            video_base_url: "https://invalid.invalid/v.mpd".into(),
            audio_base_url: None,
        });
        *engine.resume.lock().unwrap() = Some((true, 42.5));
        engine.want_play.store(true, Ordering::SeqCst);
        engine.note_surface_detached();
        assert_eq!(*engine.resume.lock().unwrap(), Some((true, 42.5)));
    }

    /// 1500ms 节流期后再次调用 remount_pending 能消费断点并清除 resume。
    #[test]
    fn remount_pending_consumes_resume_after_throttle_window() {
        let engine = Arc::new(NativePlayer::new());
        *engine.last_request.lock().unwrap() = Some(LoadRequest {
            mpd: "<not-dash>".into(),
            video_base_url: "https://invalid.invalid/v.mpd".into(),
            audio_base_url: None,
        });
        *engine.resume.lock().unwrap() = Some((true, 12.0));
        *engine.last_rebuild_at.lock().unwrap() = Some(std::time::Instant::now());
        assert!(!engine.remount_pending(), "节流窗口内不得重装载");
        assert!(engine.resume.lock().unwrap().is_some(), "节流期内断点应保留");
        *engine.last_rebuild_at.lock().unwrap() =
            Some(std::time::Instant::now() - REBUILD_MIN_INTERVAL - std::time::Duration::from_millis(50));
        assert!(engine.remount_pending(), "冷却到期后必须消费断点重试");
        assert!(engine.resume.lock().unwrap().is_none());
    }

    /// stop() 推进世代作废在途建源并清空断点/请求/待补 seek。
    #[test]
    fn stop_clears_resume_last_request_and_advances_ledger() {
        let engine = Arc::new(NativePlayer::new());
        let _ = engine.load("<not-dash>", "https://invalid.invalid/v.mpd", None, true, 5.0);
        *engine.resume.lock().unwrap() = Some((true, 5.0));
        *engine.pending_seek.lock().unwrap() = Some((1, 5.0));
        let prev_seq = engine.status().request_seq;
        engine.stop();
        assert!(engine.resume.lock().unwrap().is_none());
        assert!(engine.last_request.lock().unwrap().is_none());
        assert!(engine.pending_seek.lock().unwrap().is_none());
        assert!(!engine.want_play.load(Ordering::SeqCst));
        let st = engine.status();
        assert!(!st.loading);
        assert!(st.request_seq > prev_seq);
        assert_eq!(st.injected_seq, None);
        engine.note_surface_detached();
        assert!(engine.resume.lock().unwrap().is_none(), "停播后再卸载不得写回幽灵断点");
    }

    /// 全屏等显式布局切换清除重挂节流后，remount_pending 必须在冷却窗口内立即生效。
    #[test]
    fn allow_immediate_remount_bypasses_throttle_window() {
        let engine = Arc::new(NativePlayer::new());
        *engine.last_request.lock().unwrap() = Some(LoadRequest {
            mpd: "<not-dash>".into(),
            video_base_url: "https://invalid.invalid/v.mpd".into(),
            audio_base_url: None,
        });
        *engine.resume.lock().unwrap() = Some((true, 21.0));
        *engine.last_rebuild_at.lock().unwrap() = Some(std::time::Instant::now());
        assert!(!engine.remount_pending(), "正常风暴期应受 1500ms 节流");

        engine.allow_immediate_remount();
        assert!(
            engine.remount_pending(),
            "allow_immediate_remount 后应立即放行续播"
        );
        assert!(engine.resume.lock().unwrap().is_none());
    }

    /// 建源在途时面板发生 detach：全新产出的 MediaSource 从未被旧元素绑定，
    /// `finish` 必须允许新面板立即认领，而不是被 `awaiting_rebuild` 白白阻塞 1500ms。
    #[test]
    fn finish_clears_awaiting_rebuild_for_unclaimed_fresh_build() {
        let ledger = ledger();
        let mut l = ledger.lock().unwrap();
        let seq = l.begin(true, 0.0);
        // 面板在 build_source 仍在飞时卸载（此时 built_seq=None）
        l.note_rebuild();
        assert!(l.awaiting_rebuild);
        assert_eq!(
            l.finish(seq, None),
            Finish::Ready {
                autoplay: true,
                start_secs: 0.0
            }
        );
        assert!(
            !l.awaiting_rebuild,
            "刚完成且未绑定过任何 player 的全新 MediaSource 应可立即注入"
        );
        assert_eq!(l.pending_seq(), Some(seq));
    }
}
