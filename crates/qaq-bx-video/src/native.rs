//! 系统媒体引擎 MVP(Windows.Media.Playback + AdaptiveMediaSource,v0.3 spike)。
//!
//! 方案实证自 Richasy/Bili.Uwp(NativePlayerViewModel):内存 MPD
//! (qaq-bx-core::mpd 生成)→ `AdaptiveMediaSource::CreateFromStreamWithDownloaderAsync`
//! (带 Referer 头的 HttpClient)→ `MediaSource::CreateFromAdaptiveMediaSource`
//! → 注入 **MediaPlayerElement 自持的 MediaPlayer**(渲染由 MPE 托管)。
//!
//! 协议(WinUI3 MPE 语义决定):MPE 内部 `MediaPlayer` 属性**只读**,播放面必须
//! 用元素自己的 player。因此引擎不造 player,而是等 UI(`NativeSurface`)经
//! `request_get_media_player` 把 player 交回 `attach_player`;attach 之前的
//! `load` 进入 pending,attach 时重放。控制/状态(pause/seek/volume/position)
//! 全部走该 player,与 mpv 引擎的 `PlaybackStatus` 快照语义对齐。
//!
//! 音频重定向约定(与 Bili.Uwp 同款):MPD 内两个 Representation 的
//! `<BaseURL>` 留空,manifest URI 用视频流 base_url;`DownloadRequested` 里把
//! `ResourceContentType == "audio/*"` 的请求重定向到音频 base_url。
//!
//! 线程模型(spike 版):`load` 阻塞 join 异步创建(WinRT Agile,调用线程
//! 不限;attach 在 UI 线程 completion 上执行,可能短卡 UI)——正式版把
//! load 移到工作线程,见 docs/dev-notes/media-engine-comparison.md §5。

use windows::core::{HSTRING, Interface as _};
use windows::Foundation::Uri;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{MediaPlayer, MediaPlaybackSession, MediaPlaybackState};
use windows::Media::Streaming::Adaptive::AdaptiveMediaSourceCreationStatus;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Web::Http::HttpClient;
use windows_time::TimeSpan;

const REFERER: &str = "https://www.bilibili.com";
const MPD_CONTENT_TYPE: &str = "application/dash+xml";

/// 播放状态快照(shell 250ms 节拍轮询;与 mpv 侧 PlaybackStatus 字段对齐)。
#[derive(Debug, Clone, Default)]
pub struct NativeStatus {
    pub playing: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub finished: bool,
    pub error: Option<String>,
}

/// 事件 revoker 存活盒:WinRT 事件 revoker 内部 COM 指针 Agile 但未标 Send,
/// 统一擦除(仅保活,不跨线程解引用)。
struct AgileBox(#[allow(dead_code)] Box<dyn std::any::Any>);
unsafe impl Send for AgileBox {}
unsafe impl Sync for AgileBox {}

/// attach 前挂起的装载产物(MediaSource 等 UI 注入 MPE 后接管 player)。
struct PendingSource {
    source: windows::core::IUnknown,
    autoplay: bool,
    start_secs: f64,
}

/// 系统媒体引擎(attach 模式;字段以 Mutex/RwLock 保护,Arc 共享合法)。
pub struct NativePlayer {
    /// UI 交回的 MPE 内部 player(attach 后 Some)。
    player: std::sync::Mutex<Option<MediaPlayer>>,
    session: std::sync::Mutex<Option<MediaPlaybackSession>>,
    /// attach 前挂起的 MediaSource(UI 注入 MPE 的原料)。
    pending_source: std::sync::Mutex<Option<PendingSource>>,
    /// 事件 revoker / 已注入 MediaSource 保活。
    _keep: std::sync::Mutex<Vec<AgileBox>>,
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// load 过至少一次成功(= 有画面可看)。
    loaded: std::sync::atomic::AtomicBool,
}

// fork 对 MediaPlayer/会话对象 unsafe impl Send/Sync(Media_Core feature),
// Mutex 包装后整组共享合法。
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
            pending_source: std::sync::Mutex::new(None),
            _keep: std::sync::Mutex::new(Vec::new()),
            finished: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            error: std::sync::Arc::new(std::sync::Mutex::new(None)),
            loaded: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// UI(NativeSurface)交回 MPE 内部 player(已由 UI 的
    /// `request_set_media_source_get_player` 设好 Source):接管控制,应用
    /// pending 的 autoplay/start 语义。
    pub fn attach_player(&self, player: windows::core::IUnknown) -> Result<(), String> {
        let player: MediaPlayer = player.cast().map_err(|e| format!("player cast 失败:{e}"))?;
        eprintln!("[qaq-bx-video] native attach: MPE 内部 player 已接管");
        // MPE 默认 AutoPlay=true;起播时机收归引擎(shell settings.autoplay)。
        let _ = player.SetAutoPlay(false);
        let session = player
            .PlaybackSession()
            .map_err(|e| format!("PlaybackSession 获取失败:{e}"))?;
        // MediaEnded / MediaFailed → 状态快照(仅订阅一次)。
        let already = self.player.lock().map(|slot| slot.is_some()).unwrap_or(false);
        if !already {
            let finished_flag = self.finished.clone();
            let ended = player
                .MediaEnded(move |_, _| {
                    finished_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                })
                .map(|r| AgileBox(Box::new(r)));
            let error_slot = self.error.clone();
            let failed = player
                .MediaFailed(move |_, args| {
                    let Some(args) = args.as_ref() else { return };
                    let message = args
                        .ErrorMessage()
                        .map(|m| m.to_string_lossy())
                        .unwrap_or_default();
                    if let Ok(mut slot) = error_slot.lock() {
                        *slot = Some(format!("媒体失败(error={:?}):{message}", args.Error()));
                    }
                })
                .map(|r| AgileBox(Box::new(r)));
            let mut keep = self._keep.lock().unwrap_or_else(|e| e.into_inner());
            if let Ok(r) = ended {
                keep.push(r);
            }
            if let Ok(r) = failed {
                keep.push(r);
            }
        }
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = Some(player.clone());
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);

        // UI 注入请求已把 pending source 设进 player;这里只补时序语义。
        let pending = self.pending_source.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(PendingSource { autoplay, start_secs, .. }) = pending {
            if start_secs > 0.0 {
                let _ = self.seek(start_secs);
            }
            if autoplay {
                let _ = player.Play();
            }
        }
        Ok(())
    }

    /// UI 取走待注入 MPE 的 MediaSource(引擎 load 产出;无则 None,下一拍再试)。
    pub fn take_pending_source_raw(&self) -> Option<windows::core::IUnknown> {
        let mut slot = self.pending_source.lock().ok()?;
        slot.as_ref().map(|p| p.source.clone())
    }

    /// 加载 MPD:同步产出 MediaSource。player 已 attach → 直接注入;
    /// 未 attach → 存 pending 槽等 UI 注入 MPE(下一拍)。
    pub fn load(
        &self,
        mpd: &str,
        video_base_url: &str,
        audio_base_url: Option<&str>,
        autoplay: bool,
        start_secs: f64,
    ) -> Result<(), String> {
        let media_source = self.build_source(mpd, video_base_url, audio_base_url)?;
        let player = self.player.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match player {
            Some(player) => {
                player
                    .SetSource(&media_source)
                    .map_err(|e| format!("player.SetSource 失败:{e}"))?;
                if start_secs > 0.0 {
                    let _ = self.seek(start_secs);
                }
                if autoplay {
                    let _ = player.Play();
                }
                self.finished
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut slot) = self.error.lock() {
                    *slot = None;
                }
                self.loaded
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                if let Ok(mut keep) = self._keep.lock() {
                    keep.push(AgileBox(Box::new(media_source)));
                }
                Ok(())
            }
            None => {
                *self.pending_source.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(PendingSource {
                        source: media_source.into(),
                        autoplay,
                        start_secs,
                    });
                Ok(())
            }
        }
    }

    pub fn play(&self) -> Result<(), String> {
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            player.Play().map_err(|e| format!("Play 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            player.Pause().map_err(|e| format!("Pause 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn seek(&self, secs: f64) -> Result<(), String> {
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(session) = session {
            session
                .SetPosition(TimeSpan {
                    duration: (secs * 10_000_000.0) as i64,
                })
                .map_err(|e| format!("Seek 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn set_volume(&self, pct: f64) -> Result<(), String> {
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            player
                .SetVolume((pct / 100.0).clamp(0.0, 1.0))
                .map_err(|e| format!("Volume 失败:{e}"))?;
        }
        Ok(())
    }

    pub fn status(&self) -> NativeStatus {
        let player = self.player.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let session = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(session) = session else {
            return NativeStatus::default();
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
        let finished = self.finished.load(std::sync::atomic::Ordering::SeqCst);
        let _ = player;
        NativeStatus {
            playing: state == Some(MediaPlaybackState::Playing),
            position_secs: position,
            duration_secs: duration,
            finished,
            error: self.error.lock().ok().and_then(|s| s.clone()),
        }
    }

    /// MPD → MediaSource 的产出体(阻塞 join;网络+解析在调用线程)。
    fn build_source(
        &self,
        mpd: &str,
        video_base_url: &str,
        audio_base_url: Option<&str>,
    ) -> Result<MediaSource, String> {
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

        // 2) MPD 文本 → InMemoryRandomAccessStream。
        let stream =
            InMemoryRandomAccessStream::new().map_err(|e| format!("内存流创建失败:{e}"))?;
        let writer =
            DataWriter::CreateDataWriter(&stream).map_err(|e| format!("DataWriter 创建失败:{e}"))?;
        let bytes: Vec<u8> = mpd.bytes().collect();
        writer
            .WriteBytes(&bytes)
            .map_err(|e| format!("MPD 写入失败:{e}"))?;
        writer
            .StoreAsync()
            .and_then(|op| op.join())
            .map_err(|e| format!("MPD 流提交失败:{e}"))?;
        stream
            .Seek(0)
            .map_err(|e| format!("MPD 流回卷失败:{e}"))?;

        // 3) AdaptiveMediaSource(manifest URI = 视频 base_url,分段相对解析)。
        let manifest_uri = Uri::CreateUri(&HSTRING::from(video_base_url))
            .map_err(|e| format!("manifest Uri 构造失败:{e}"))?;
        let creation =
            windows::Media::Streaming::Adaptive::AdaptiveMediaSource::CreateFromStreamWithDownloaderAsync(
                &stream,
                &manifest_uri,
                &HSTRING::from(MPD_CONTENT_TYPE),
                &client,
            )
            .and_then(|op| op.join())
            .map_err(|e| format!("AdaptiveMediaSource 创建失败:{e}"))?;
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
            if let Ok(mut keep) = self._keep.lock() {
                keep.push(AgileBox(Box::new(revoker)));
            }
        }

        // 5) 产出 MediaSource(注入路径由调用方决定:player 已 attach 直接
        // SetSource;未 attach 存 pending 槽等 UI 注入 MPE)。
        let media_source =
            MediaSource::CreateFromAdaptiveMediaSource(&ams).map_err(|e| format!("MediaSource 包装失败:{e}"))?;
        if let Ok(mut keep) = self._keep.lock() {
            keep.push(AgileBox(Box::new(ams)));
        }
        eprintln!("[qaq-bx-video] native build_source ok");
        Ok(media_source)
    }
}
