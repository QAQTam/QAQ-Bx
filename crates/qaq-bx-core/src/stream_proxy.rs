//! 本地轻量并发 HTTP Range 流式代理（兼容 Native `AdaptiveMediaSource` 与 `libmpv`）。
//!
//! 设计目标（贴合 4c8T / 8G 内存约束与 B 站 DASH `.m4s` 特征）：
//! 1. **统一收口防盗链与多 CDN 容灾**：集中注入 `Referer` / `User-Agent`、`CdnHealthTracker`
//!    排名与 `backup_url` 故障切换（超时、403、5xx 或截断自动切下一节点，对播放器完全透明）；
//! 2. **多连接并发滑窗预取**：对当前读取位置之后 `PREFETCH_AHEAD` 个 512KiB 分块并行发起
//!    Range 请求（受全局信号量与单流信号量双重限流），并按 `chunk_idx` 做 single-flight 去重；
//! 3. **严格有界内存**：每条流最多缓存 `MAX_CACHED_CHUNKS`（10 MiB）分块、最多保留 `MAX_TRACKS`
//!    条活动流，换视频/停播即刻取消在途任务并释放缓存。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, watch};

use crate::cdn::CdnHealthTracker;
use crate::wbi::BROWSER_UA;

const REFERER: &str = "https://www.bilibili.com/";
pub const CHUNK_SIZE: usize = 512 * 1024;
pub const MAX_CACHED_CHUNKS: usize = 20;
const DEFAULT_PREFETCH_AHEAD: u64 = 6;
const MAX_ACTIVE_TRACKS: usize = 6;
const MAX_GLOBAL_FETCHES: usize = 6;
const MAX_TRACK_FETCHES: usize = 3;
const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct StreamProxyConfig {
    pub chunk_size: usize,
    pub max_cached_chunks: usize,
    pub prefetch_ahead: u64,
    pub connect_timeout: Duration,
    pub chunk_timeout: Duration,
}

impl Default for StreamProxyConfig {
    fn default() -> Self {
        Self {
            chunk_size: CHUNK_SIZE,
            max_cached_chunks: MAX_CACHED_CHUNKS,
            prefetch_ahead: DEFAULT_PREFETCH_AHEAD,
            connect_timeout: Duration::from_secs(4),
            chunk_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyStreamPair {
    pub session_id: u64,
    pub video_url: String,
    pub audio_url: String,
}

pub struct StreamProxy {
    addr: SocketAddr,
    runtime: Handle,
    http: reqwest::Client,
    cdn_health: Arc<CdnHealthTracker>,
    cfg: StreamProxyConfig,
    global_sem: Arc<Semaphore>,
    tracks: Arc<Mutex<HashMap<u64, Arc<TrackState>>>>,
    next_id: AtomicU64,
    next_session: AtomicU64,
    shutdown: Arc<AtomicBool>,
}

struct ChunkCache {
    chunks: HashMap<u64, Bytes>,
    cursor: u64,
    last_used: HashMap<u64, u64>,
    tick: u64,
}

impl ChunkCache {
    fn new() -> Self {
        Self {
            chunks: HashMap::new(),
            cursor: 0,
            last_used: HashMap::new(),
            tick: 0,
        }
    }

    fn get(&mut self, idx: u64) -> Option<Bytes> {
        self.cursor = idx;
        if let Some(data) = self.chunks.get(&idx).cloned() {
            self.tick += 1;
            self.last_used.insert(idx, self.tick);
            Some(data)
        } else {
            None
        }
    }

    /// 不刷新 LRU/游标的只读命中探测(供 join 前缓存短路)
    fn peek(&self, idx: u64) -> Option<Bytes> {
        self.chunks.get(&idx).cloned()
    }

    fn insert(&mut self, idx: u64, data: Bytes, max_chunks: usize, prefetch_ahead: u64) {
        self.tick += 1;
        self.chunks.insert(idx, data);
        self.last_used.insert(idx, self.tick);
        let max_chunks = max_chunks.max(2);
        while self.chunks.len() > max_chunks {
            let win_lo = self.cursor.saturating_sub(2);
            let win_hi = self.cursor.saturating_add(prefetch_ahead);
            // 优先淘汰已播完（cursor 之前）最远的分块，其次窗口外，最后按 LRU 兜底。
            let victim = self
                .chunks
                .keys()
                .copied()
                .max_by_key(|&k| {
                    let behind_score = if k < win_lo {
                        (2u8, win_lo - k)
                    } else if k > win_hi {
                        (1u8, k - win_hi)
                    } else {
                        (0u8, 0)
                    };
                    let stale_score = u64::MAX - self.last_used.get(&k).copied().unwrap_or(0);
                    (behind_score, stale_score)
                });
            if let Some(key) = victim {
                self.chunks.remove(&key);
                self.last_used.remove(&key);
            } else {
                break;
            }
        }
    }
}

/// 分块在途状态。`Streaming` 为「边收边转发」的连续前缀快照(每片到达发布一次,
/// 快照 clone 仅按片数引用计数,无字节拷贝);`Done` 为终态。跨候选 failover 时
/// 新连接从已发布缺口续拉,已发布字节位置恒定有效,无需 epoch 回退。
enum ChunkUpdate {
    Streaming(Arc<ChunkBuf>),
    Done(Result<Bytes, String>),
}

/// 已接收分片的连续前缀(绝对偏移 = 块起点 + 前缀累计长)。
struct ChunkBuf {
    pieces: Vec<Bytes>,
    len: usize,
}

impl ChunkBuf {
    fn new() -> Self {
        Self {
            pieces: Vec::new(),
            len: 0,
        }
    }

    fn push(&mut self, piece: Bytes) {
        self.len += piece.len();
        self.pieces.push(piece);
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }

    /// 广播用快照:片列表按引用计数 clone,无字节拷贝。
    fn snapshot(&self) -> Arc<ChunkBuf> {
        Arc::new(ChunkBuf {
            pieces: self.pieces.clone(),
            len: self.len,
        })
    }

    /// 终态单一缓存块:单片直接透出(全链路 0 拷贝),多片仅一次合并拷贝
    /// (旧路径为 reqwest 内部拼接 + `to_vec` 二次拷贝)。
    fn finalize(&mut self) -> Bytes {
        if self.pieces.len() == 1 {
            return self.pieces.pop().unwrap();
        }
        let mut out = BytesMut::with_capacity(self.len);
        for p in self.pieces.drain(..) {
            out.extend_from_slice(&p);
        }
        out.freeze()
    }

    /// 把块内区间 `[from, to)` 按已收片写出,返回已写到的偏移(可能 `< to`,
    /// 等待后续片再续写)。与旧 `write_all(&chunk[a..b])` 语义等价,仅拆分点不同。
    async fn write_range<W>(
        &self,
        w: &mut W,
        mut from: usize,
        to: usize,
    ) -> std::io::Result<usize>
    where
        W: tokio::io::AsyncWrite + Unpin + ?Sized,
    {
        let mut pos = 0usize;
        for p in &self.pieces {
            let next = pos + p.len();
            if pos >= to {
                break;
            }
            if from < next {
                let a = from.max(pos);
                let b = to.min(next);
                w.write_all(&p[a - pos..b - pos]).await?;
                from = b;
            }
            pos = next;
        }
        Ok(from)
    }
}

type ChunkWatchRx = watch::Receiver<ChunkUpdate>;
type ChunkWatchTx = watch::Sender<ChunkUpdate>;

/// join 句柄:缓存/空块直接就绪,否则挂在在途快照通道上。
enum ChunkJoin {
    Ready(Bytes),
    Waiting(ChunkWatchRx),
}

/// join 侧立即终结的 rx(空块等场景)。
fn ready_rx(data: Bytes) -> ChunkWatchRx {
    let (tx, rx) = watch::channel(ChunkUpdate::Done(Ok(data)));
    drop(tx);
    rx
}

struct TrackState {
    id: u64,
    session_id: u64,
    urls: Vec<String>,
    content_type: &'static str,
    prefetch_ahead: u64,
    chunk_size: usize,
    max_cached_chunks: usize,
    total_len: AtomicU64,
    has_total: AtomicBool,
    cancelled: AtomicBool,
    cache: Mutex<ChunkCache>,
    inflight: Mutex<HashMap<u64, ChunkWatchRx>>,
    track_sem: Arc<Semaphore>,
}

impl TrackState {
    fn total_len_opt(&self) -> Option<u64> {
        if self.has_total.load(Ordering::Acquire) {
            Some(self.total_len.load(Ordering::Acquire))
        } else {
            None
        }
    }

    fn set_total_len(&self, total: u64) {
        self.total_len.store(total, Ordering::Release);
        self.has_total.store(true, Ordering::Release);
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut guard) = self.cache.lock() {
            guard.chunks.clear();
            guard.last_used.clear();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn cached_len(&self) -> usize {
        self.cache.lock().map(|c| c.chunks.len()).unwrap_or(0)
    }
}

struct InflightCleanup<'a> {
    track: &'a TrackState,
    chunk_idx: u64,
}

impl Drop for InflightCleanup<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.track.inflight.lock() {
            map.remove(&self.chunk_idx);
        }
    }
}

impl StreamProxy {
    /// 启动本地流式代理，绑定 `127.0.0.1:0` 并由传入 tokio runtime 驱动。
    pub fn start(runtime: Handle, cdn_health: Arc<CdnHealthTracker>) -> Result<Arc<Self>, String> {
        Self::start_with_config(runtime, cdn_health, StreamProxyConfig::default())
    }

    pub fn start_with_config(
        runtime: Handle,
        cdn_health: Arc<CdnHealthTracker>,
        cfg: StreamProxyConfig,
    ) -> Result<Arc<Self>, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|e| format!("StreamProxy reqwest 初始化失败: {e}"))?;

        let std_listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("StreamProxy 绑定端口失败: {e}"))?;
        std_listener
            .set_nonblocking(true)
            .map_err(|e| format!("StreamProxy set_nonblocking 失败: {e}"))?;
        let addr = std_listener
            .local_addr()
            .map_err(|e| format!("StreamProxy local_addr 失败: {e}"))?;
        let listener = {
            let _guard = runtime.enter();
            TcpListener::from_std(std_listener)
                .map_err(|e| format!("StreamProxy TcpListener 接管失败: {e}"))?
        };

        let proxy = Arc::new(Self {
            addr,
            runtime: runtime.clone(),
            http,
            cdn_health,
            cfg,
            global_sem: Arc::new(Semaphore::new(MAX_GLOBAL_FETCHES)),
            tracks: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            next_session: AtomicU64::new(1),
            shutdown: Arc::new(AtomicBool::new(false)),
        });

        let accept_proxy = proxy.clone();
        runtime.spawn(async move {
            accept_loop(accept_proxy, listener).await;
        });
        eprintln!("[stream-proxy] listening on http://{addr}");
        Ok(proxy)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 登记一对 DASH 音视频轨，自动挤出超额旧会话并支持按需并发预取。
    pub fn register_dash_pair(
        self: &Arc<Self>,
        video_urls: Vec<String>,
        audio_urls: Vec<String>,
        multi_thread_preload: bool,
    ) -> ProxyStreamPair {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let prefetch = if multi_thread_preload {
            self.cfg.prefetch_ahead
        } else {
            0
        };
        let vid_id = self.insert_track(session_id, video_urls, "video/mp4", prefetch);
        let aud_id = self.insert_track(session_id, audio_urls, "audio/mp4", prefetch.min(3));
        ProxyStreamPair {
            session_id,
            video_url: format!("http://{}/track/{vid_id}/video.m4s", self.addr),
            audio_url: format!("http://{}/track/{aud_id}/audio.m4s", self.addr),
        }
    }

    /// 注销指定会话所属的流轨道，立刻停止预取并释放缓存。
    pub fn unregister_session(&self, session_id: u64) {
        let removed: Vec<Arc<TrackState>> = {
            let Ok(mut map) = self.tracks.lock() else {
                return;
            };
            let keys: Vec<u64> = map
                .iter()
                .filter(|(_, t)| t.session_id == session_id)
                .map(|(k, _)| *k)
                .collect();
            keys.into_iter().filter_map(|k| map.remove(&k)).collect()
        };
        for track in removed {
            track.cancel();
        }
    }

    /// 清空全部已登记轨道（切新片 / 停播）。
    pub fn clear_all(&self) {
        let removed: Vec<Arc<TrackState>> = {
            let Ok(mut map) = self.tracks.lock() else {
                return;
            };
            map.drain().map(|(_, v)| v).collect()
        };
        for track in removed {
            track.cancel();
        }
    }

    fn insert_track(
        &self,
        session_id: u64,
        urls: Vec<String>,
        content_type: &'static str,
        prefetch_ahead: u64,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(TrackState {
            id,
            session_id,
            urls,
            content_type,
            prefetch_ahead,
            chunk_size: self.cfg.chunk_size,
            max_cached_chunks: self.cfg.max_cached_chunks,
            total_len: AtomicU64::new(0),
            has_total: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            cache: Mutex::new(ChunkCache::new()),
            inflight: Mutex::new(HashMap::new()),
            track_sem: Arc::new(Semaphore::new(MAX_TRACK_FETCHES)),
        });

        let mut evicted: Vec<Arc<TrackState>> = Vec::new();
        if let Ok(mut map) = self.tracks.lock() {
            map.insert(id, state);
            while map.len() > MAX_ACTIVE_TRACKS {
                if let Some(oldest_id) = map.keys().min().copied() {
                    if let Some(old) = map.remove(&oldest_id) {
                        evicted.push(old);
                    }
                } else {
                    break;
                }
            }
        }
        for old in evicted {
            old.cancel();
        }
        id
    }

    fn get_track(&self, id: u64) -> Option<Arc<TrackState>> {
        self.tracks
            .lock()
            .ok()
            .and_then(|map| map.get(&id).cloned())
            .filter(|t| !t.is_cancelled())
    }

    #[cfg(test)]
    fn track_cached_chunks(&self, id: u64) -> usize {
        self.get_track(id)
            .map(|t| t.cached_len())
            .unwrap_or(0)
    }

    /// 拉取整块终态(预取/HEAD 兼容路径):缓存命中直接返回,否则并入在途
    /// leader(single-flight 合并)。
    async fn fetch_chunk(
        self: &Arc<Self>,
        track: &Arc<TrackState>,
        chunk_idx: u64,
        from_reader: bool,
    ) -> Result<Bytes, String> {
        match self.join_chunk(track, chunk_idx, from_reader) {
            ChunkJoin::Ready(data) => Ok(data),
            ChunkJoin::Waiting(rx) => await_done(rx).await,
        }
    }

    /// join 在途分块:缓存命中(`from_reader` 时刷新 LRU/游标)即终结;否则复用
    /// 或拉起 leader 任务。leader 由 runtime spawn,读取者断开不影响下载完成与
    /// 入缓存(与旧「预取者持 leader」行为一致)。
    fn join_chunk(
        self: &Arc<Self>,
        track: &Arc<TrackState>,
        chunk_idx: u64,
        from_reader: bool,
    ) -> ChunkJoin {
        if track.is_cancelled() {
            return ChunkJoin::Ready(Bytes::new());
        }
        if let Ok(mut cache) = track.cache.lock() {
            if from_reader {
                if let Some(hit) = cache.get(chunk_idx) {
                    return ChunkJoin::Ready(hit);
                }
            } else if let Some(hit) = cache.peek(chunk_idx) {
                return ChunkJoin::Ready(hit);
            }
        }
        let (tx, rx) = {
            let mut inflight = match track.inflight.lock() {
                Ok(g) => g,
                Err(_) => return ChunkJoin::Ready(Bytes::new()),
            };
            if let Some(existing) = inflight.get(&chunk_idx) {
                let rx = existing.clone();
                let alive = matches!(&*rx.borrow(), ChunkUpdate::Streaming(_)) && !rx.is_closed();
                if alive {
                    return ChunkJoin::Waiting(rx);
                }
                // 陈旧终态(leader panic 后事务对象清理前的窗口):视为无在途。
            }
            let (tx, rx) = watch::channel(ChunkUpdate::Streaming(Arc::new(ChunkBuf::new())));
            inflight.insert(chunk_idx, rx.clone());
            (tx, rx)
        };
        let proxy = Arc::clone(self);
        let tr = Arc::clone(track);
        self.runtime.spawn(async move {
            proxy
                .run_chunk_leader(tr, chunk_idx, tx, from_reader)
                .await;
        });
        ChunkJoin::Waiting(rx)
    }

    async fn run_chunk_leader(
        self: Arc<Self>,
        track: Arc<TrackState>,
        chunk_idx: u64,
        tx: ChunkWatchTx,
        from_reader: bool,
    ) {
        let _guard = InflightCleanup {
            track: &track,
            chunk_idx,
        };
        let result = self.stream_chunk(&track, chunk_idx, &tx).await;
        if let Ok(data) = &result {
            if !data.is_empty() && !track.is_cancelled() {
                if let Ok(mut cache) = track.cache.lock() {
                    if from_reader {
                        cache.cursor = chunk_idx;
                    }
                    cache.insert(
                        chunk_idx,
                        data.clone(),
                        track.max_cached_chunks,
                        track.prefetch_ahead,
                    );
                }
            }
        }
        let _ = tx.send(ChunkUpdate::Done(result));
    }

    /// 「边收边转发」整块下载:候选逐个尝试,每次尝试从已收缺口续拉,每片到达
    /// 即广播连续前缀快照。候选中途失败切备线续拉尾段——已发布字节绝对位置
    /// 不变,天然免回退;首试 416 视为空块(旧 `Eof` 语义)。终态拼接为单一
    /// `Bytes` 入缓存(单片 0 拷贝直转)。
    async fn stream_chunk(
        &self,
        track: &TrackState,
        chunk_idx: u64,
        tx: &ChunkWatchTx,
    ) -> Result<Bytes, String> {
        let chunk_size = track.chunk_size as u64;
        let start = chunk_idx.saturating_mul(chunk_size);
        let mut end = start.saturating_add(chunk_size).saturating_sub(1);
        if let Some(total) = track.total_len_opt() {
            if start >= total {
                return Ok(Bytes::new());
            }
            end = end.min(total - 1);
        }

        let _global_permit = self
            .global_sem
            .acquire()
            .await
            .map_err(|_| "proxy global semaphore closed".to_string())?;
        let _track_permit = track
            .track_sem
            .acquire()
            .await
            .map_err(|_| "proxy track semaphore closed".to_string())?;

        let candidates = self.cdn_health.rank_candidates(&track.urls);
        if candidates.is_empty() {
            return Err("无可用流候选地址".to_string());
        }

        let mut buf = ChunkBuf::new();
        let mut errors = Vec::new();
        let mut failed_hosts = std::collections::HashSet::new();
        for candidate in candidates {
            if track.is_cancelled() || self.shutdown.load(Ordering::Acquire) {
                return Err("流已取消".to_string());
            }
            if let Some(total) = track.total_len_opt() {
                if start >= total {
                    if buf.is_empty() {
                        return Ok(Bytes::new());
                    }
                }
                if total > 0 {
                    end = end.min(total - 1);
                }
            }
            if start + buf.len() as u64 > end {
                break;
            }
            let host_key = crate::cdn::host_port_key(&candidate);
            if let Some(ref hk) = host_key && failed_hosts.contains(hk) {
                continue;
            }
            let from = start + buf.len() as u64;
            let started = Instant::now();
            let attempt = tokio::time::timeout(
                self.cfg.chunk_timeout,
                self.stream_candidate_range(&candidate, track, from, end, chunk_size, &mut buf, tx),
            )
            .await;
            match attempt {
                Ok(Ok(truncate)) => {
                    self.cdn_health.report_success(&candidate, started.elapsed());
                    if truncate.is_some() {
                        // 上游声明到此为尽(416 空块 / 无 Content-Range 短尾):
                        // 按已收字节收敛块尾,避免向后续候选重复拉取。
                        if buf.is_empty() {
                            return Ok(Bytes::new());
                        }
                        end = end.min(start + buf.len() as u64 - 1);
                    }
                }
                Ok(Err(err)) => {
                    self.cdn_health.report_failure(&candidate);
                    if let Some(hk) = host_key.clone() {
                        failed_hosts.insert(hk);
                    }
                    eprintln!(
                        "[stream-proxy] track={} chunk={} host={:?} 失败，切备线(已收 {}B): {}",
                        track.id,
                        chunk_idx,
                        host_key.unwrap_or_default(),
                        buf.len(),
                        err
                    );
                    errors.push(format!("{err}(已收 {}B)", buf.len()));
                }
                Err(_) => {
                    self.cdn_health.report_failure(&candidate);
                    if let Some(hk) = host_key.clone() {
                        failed_hosts.insert(hk);
                    }
                    let msg = format!("读取超时({:?})", self.cfg.chunk_timeout);
                    eprintln!(
                        "[stream-proxy] track={} chunk={} host={:?} 超时，切备线",
                        track.id,
                        chunk_idx,
                        host_key.unwrap_or_default(),
                    );
                    errors.push(msg);
                }
            }
        }
        if start + buf.len() as u64 > end {
            return Ok(buf.finalize());
        }
        Err(format!("全部候选 CDN 拉片失败: {}", errors.join("; ")))
    }

    /// 单候选拉 `[from, end]`:逐片广播。206 按 Content-Range 校验起点与声明
    /// 区间(对齐旧校验);返回 `Some(())` 表示上游声明数据到此为尽。
    async fn stream_candidate_range(
        &self,
        url: &str,
        track: &TrackState,
        from: u64,
        end: u64,
        chunk_size: u64,
        buf: &mut ChunkBuf,
        tx: &ChunkWatchTx,
    ) -> Result<Option<()>, String> {
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::REFERER, REFERER)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::RANGE, format!("bytes={from}-{end}"))
            .send()
            .await
            .map_err(|e| format!("HTTP 请求错误: {e}"))?;

        let status = resp.status();
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let total_len = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range_total);
            if let Some(total) = total_len {
                track.set_total_len(total);
            }
            if !buf.is_empty() {
                return Err("续拉缺口时上游返回 416".to_string());
            }
            return Ok(Some(()));
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let cr = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_content_range);
            if let Some((r_start, _r_end, total)) = cr {
                if r_start != from {
                    return Err(format!("Content-Range 起点不匹配: {r_start} != {from}"));
                }
                if let Some(total) = total {
                    track.set_total_len(total);
                }
            }
            let declared = cr.map(|(_, r_end, _)| r_end.saturating_sub(from) + 1);
            let needed = end - from + 1;
            let mut stream = resp.bytes_stream();
            let mut this_got: u64 = 0;
            loop {
                let item = match tokio::time::timeout(self.cfg.chunk_timeout, stream.next()).await
                {
                    Ok(Some(Ok(chunk))) => chunk,
                    Ok(Some(Err(e))) => return Err(format!("读取响应体失败: {e}")),
                    Ok(None) => break,
                    Err(_) => {
                        return Err(format!("读取超时({:?})", self.cfg.chunk_timeout));
                    }
                };
                if item.is_empty() {
                    continue;
                }
                let cap = declared.unwrap_or(needed);
                if this_got + item.len() as u64 > cap {
                    return Err(format!(
                        "206 载荷超出声明区间: {} > {cap}",
                        this_got + item.len() as u64
                    ));
                }
                this_got += item.len() as u64;
                buf.push(item);
                if tx.receiver_count() > 0 {
                    let _ = tx.send(ChunkUpdate::Streaming(buf.snapshot()));
                }
            }
            if this_got == 0 {
                return Err("206 响应体为空".to_string());
            }
            if let Some(declared) = declared {
                if this_got != declared {
                    return Err(format!("206 分片截断: {this_got} != {declared}"));
                }
                return Ok(None);
            }
            // 无 Content-Range:足量即完成;短 Body 视数据到此为尽(旧路径同样
            // 接受短块并交由收敛/上层判定)。
            if this_got < needed {
                return Ok(Some(()));
            }
            return Ok(None);
        }
        if status == reqwest::StatusCode::OK {
            if from != 0 {
                return Err("上游忽略 Range 返回 200 OK".to_string());
            }
            if let Some(len) = resp.content_length() && len > chunk_size {
                return Err(format!("上游未分片返回全量 200 ({len}B)"));
            }
            let body = resp
                .bytes()
                .await
                .map_err(|e| format!("读取 200 响应体失败: {e}"))?;
            if body.len() as u64 > chunk_size {
                return Err("上游返回超出分块上限的 200 载荷".to_string());
            }
            track.set_total_len(body.len() as u64);
            buf.push(body);
            if tx.receiver_count() > 0 {
                let _ = tx.send(ChunkUpdate::Streaming(buf.snapshot()));
            }
            return Ok(Some(()));
        }
        Err(format!("HTTP 状态码 {status}"))
    }

    fn schedule_prefetch(self: &Arc<Self>, track: &Arc<TrackState>, from_chunk: u64) {
        if track.prefetch_ahead == 0 || track.is_cancelled() {
            return;
        }
        let Some(total) = track.total_len_opt() else {
            return;
        };
        let chunk_size = track.chunk_size as u64;
        if total == 0 {
            return;
        }
        let max_chunk = (total - 1) / chunk_size;
        let end_chunk = from_chunk
            .saturating_add(track.prefetch_ahead.saturating_sub(1))
            .min(max_chunk);
        for c in from_chunk..=end_chunk {
            if track.is_cancelled() {
                break;
            }
            let already_ready = track
                .cache
                .lock()
                .map(|cache| cache.chunks.contains_key(&c))
                .unwrap_or(false)
                || track
                    .inflight
                    .lock()
                    .map(|inf| inf.contains_key(&c))
                    .unwrap_or(false);
            if already_ready {
                continue;
            }
            let proxy = Arc::clone(self);
            let tr = Arc::clone(track);
            self.runtime.spawn(async move {
                let _ = proxy.fetch_chunk(&tr, c, false).await;
            });
        }
    }
}

impl Drop for StreamProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.clear_all();
    }
}

async fn accept_loop(proxy: Arc<StreamProxy>, listener: TcpListener) {
    while !proxy.shutdown.load(Ordering::Acquire) {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let _ = stream.set_nodelay(true);
        let p = Arc::clone(&proxy);
        proxy.runtime.spawn(async move {
            handle_connection(p, stream).await;
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeSpec {
    Full,
    From(u64),
    Bounded(u64, u64),
    Invalid,
}

async fn handle_connection(proxy: Arc<StreamProxy>, stream: TcpStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::with_capacity(8192, read_half);

    loop {
        let mut req_line = String::new();
        match reader.read_line(&mut req_line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed_line = req_line.trim_end_matches(['\r', '\n']);
        if trimmed_line.is_empty() {
            continue;
        }
        let mut parts = trimmed_line.split_whitespace();
        let Some(method) = parts.next() else {
            let _ = write_status_close(&mut write_half, "400 Bad Request", "text/plain", b"").await;
            break;
        };
        let Some(target) = parts.next() else {
            let _ = write_status_close(&mut write_half, "400 Bad Request", "text/plain", b"").await;
            break;
        };
        let version = parts.next().unwrap_or("HTTP/1.1");

        let mut header_bytes = req_line.len();
        let mut range_hdr: Option<String> = None;
        let mut conn_close = version.eq_ignore_ascii_case("HTTP/1.0");
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(n) => header_bytes += n,
            }
            if header_bytes > MAX_HEADER_BYTES {
                let _ = write_status_close(&mut write_half, "431 Request Header Fields Too Large", "text/plain", b"").await;
                return;
            }
            let h = line.trim_end_matches(['\r', '\n']);
            if h.is_empty() {
                break;
            }
            if let Some((name, val)) = h.split_once(':') {
                let name = name.trim();
                let val = val.trim();
                if name.eq_ignore_ascii_case("range") {
                    range_hdr = Some(val.to_string());
                } else if name.eq_ignore_ascii_case("connection") {
                    if val.eq_ignore_ascii_case("close") {
                        conn_close = true;
                    } else if val.eq_ignore_ascii_case("keep-alive") {
                        conn_close = false;
                    }
                }
            }
        }

        let is_head = method.eq_ignore_ascii_case("HEAD");
        if !is_head && !method.eq_ignore_ascii_case("GET") {
            let _ = write_status_close(&mut write_half, "405 Method Not Allowed", "text/plain", b"").await;
            break;
        }

        let Some(track_id) = parse_track_id(target) else {
            let _ = write_status_close(&mut write_half, "404 Not Found", "text/plain", b"").await;
            break;
        };
        let Some(track) = proxy.get_track(track_id) else {
            let _ = write_status_close(&mut write_half, "404 Not Found", "text/plain", b"").await;
            break;
        };

        let range = match range_hdr.as_deref() {
            None => RangeSpec::Full,
            Some(r) => parse_range_header(r),
        };
        if range == RangeSpec::Invalid {
            let total_hdr = track
                .total_len_opt()
                .map(|t| format!("bytes */{t}"))
                .unwrap_or_else(|| "bytes */*".to_string());
            let hdr = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: {total_hdr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = write_half.write_all(hdr.as_bytes()).await;
            break;
        }

        if !serve_request(&proxy, &track, &mut write_half, range, is_head, conn_close).await {
            break;
        }
        if conn_close {
            break;
        }
    }
}

/// 等待者句柄:终态缓存块直写,或在途快照增量写(边收边转发)。
enum ChunkHandle {
    Cached(Bytes),
    Live(ChunkWatchRx),
}

async fn await_done(mut rx: ChunkWatchRx) -> Result<Bytes, String> {
    loop {
        {
            let guard = rx.borrow_and_update();
            if let ChunkUpdate::Done(res) = &*guard {
                return match res {
                    Ok(data) => Ok(data.clone()),
                    Err(e) => Err(e.clone()),
                };
            }
        }
        if rx.changed().await.is_err() {
            return Err("上游等待中止".to_string());
        }
    }
}

/// 终态块写出 `[written, to)`;空块返回 `false`(旧实现 `chunk_end_off <= start`
/// 校验的等价路径)。
async fn write_done_range<W>(
    w: &mut W,
    data: &Bytes,
    written: &mut usize,
    to: usize,
) -> Result<bool, ()>
where
    W: tokio::io::AsyncWrite + Unpin + ?Sized,
{
    if data.is_empty() {
        return Ok(false);
    }
    let to = to.min(data.len());
    if *written < to && w.write_all(&data[*written..to]).await.is_err() {
        return Err(());
    }
    *written = (*written).max(to);
    Ok(true)
}

async fn serve_request(
    proxy: &Arc<StreamProxy>,
    track: &Arc<TrackState>,
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    range: RangeSpec,
    is_head: bool,
    conn_close: bool,
) -> bool {
    let chunk_size = track.chunk_size as u64;
    let probe_chunk_idx = match range {
        RangeSpec::Full => 0,
        RangeSpec::From(s) | RangeSpec::Bounded(s, _) => s / chunk_size,
        RangeSpec::Invalid => return false,
    };

    // total 收敛:首块响应头(Content-Range / 200 / 416)一到即可写响应头,
    // 不再等待整块下载完成——首帧延迟只受首片约束。
    let mut probe_rx: Option<ChunkWatchRx> = None;
    loop {
        if track.total_len_opt().is_some() {
            break;
        }
        if probe_rx.is_none() {
            probe_rx = Some(match proxy.join_chunk(track, probe_chunk_idx, true) {
                ChunkJoin::Ready(data) => ready_rx(data),
                ChunkJoin::Waiting(rx) => rx,
            });
        }
        let rx = probe_rx.as_mut().unwrap();
        if matches!(&*rx.borrow_and_update(), ChunkUpdate::Done(_)) {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    let Some(total_len) = track.total_len_opt() else {
        // 首块已终态仍无 total:拉取失败,或上游 206 不带 Content-Range
        // (旧实现同样以 502 收口)。
        let _ = write_status_close(write_half, "502 Bad Gateway", "text/plain", b"").await;
        return false;
    };

    if total_len == 0 {
        let conn = if conn_close { "close" } else { "keep-alive" };
        let hdr = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: 0\r\nAccept-Ranges: bytes\r\nConnection: {conn}\r\n\r\n",
            track.content_type
        );
        return write_half.write_all(hdr.as_bytes()).await.is_ok();
    }

    let (status_line, start, end, send_range_header) = match range {
        RangeSpec::Full => ("200 OK", 0, total_len - 1, false),
        RangeSpec::From(s) => {
            if s >= total_len {
                let hdr = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total_len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = write_half.write_all(hdr.as_bytes()).await;
                return false;
            }
            ("206 Partial Content", s, total_len - 1, true)
        }
        RangeSpec::Bounded(s, e) => {
            if s >= total_len || e < s {
                let hdr = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total_len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = write_half.write_all(hdr.as_bytes()).await;
                return false;
            }
            ("206 Partial Content", s, e.min(total_len - 1), true)
        }
        RangeSpec::Invalid => return false,
    };

    let content_len = end - start + 1;
    let conn = if conn_close { "close" } else { "keep-alive" };
    let mut hdr = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {}\r\nContent-Length: {content_len}\r\nAccept-Ranges: bytes\r\nConnection: {conn}\r\n",
        track.content_type
    );
    if send_range_header {
        hdr.push_str(&format!("Content-Range: bytes {start}-{end}/{total_len}\r\n"));
    }
    hdr.push_str("\r\n");
    if write_half.write_all(hdr.as_bytes()).await.is_err() {
        return false;
    }

    if is_head {
        proxy.schedule_prefetch(track, 0);
        return write_half.flush().await.is_ok();
    }

    let start_chunk = start / chunk_size;
    let end_chunk = end / chunk_size;
    proxy.schedule_prefetch(track, start_chunk + 1);

    for chunk_idx in start_chunk..=end_chunk {
        if track.is_cancelled() {
            return false;
        }
        if chunk_idx > start_chunk {
            proxy.schedule_prefetch(track, chunk_idx + 1);
        }
        let chunk_start_off = chunk_idx * chunk_size;
        let local_start = start.saturating_sub(chunk_start_off) as usize;
        let chunk_span = chunk_size.min(total_len.saturating_sub(chunk_start_off)) as usize;
        let local_end =
            ((end + 1).min(chunk_start_off + chunk_span as u64) - chunk_start_off) as usize;
        if local_start >= local_end {
            return false;
        }

        let mut handle = if chunk_idx == probe_chunk_idx {
            match probe_rx.take() {
                Some(rx) => ChunkHandle::Live(rx),
                None => match proxy.join_chunk(track, chunk_idx, true) {
                    ChunkJoin::Ready(data) => ChunkHandle::Cached(data),
                    ChunkJoin::Waiting(rx) => ChunkHandle::Live(rx),
                },
            }
        } else {
            match proxy.join_chunk(track, chunk_idx, true) {
                ChunkJoin::Ready(data) => ChunkHandle::Cached(data),
                ChunkJoin::Waiting(rx) => ChunkHandle::Live(rx),
            }
        };

        let mut written = local_start;
        loop {
            let rx = match &mut handle {
                ChunkHandle::Cached(data) => {
                    if !write_done_range(write_half, data, &mut written, local_end)
                        .await
                        .unwrap_or(false)
                    {
                        return false;
                    }
                    break;
                }
                ChunkHandle::Live(rx) => rx,
            };
            let snapshot = match &*rx.borrow_and_update() {
                ChunkUpdate::Streaming(buf) => Ok(buf.clone()),
                ChunkUpdate::Done(Ok(data)) => {
                    let data = data.clone();
                    Err(data)
                }
                ChunkUpdate::Done(Err(_)) => return false,
            };
            match snapshot {
                Ok(buf) => {
                    match buf.write_range(write_half, written, local_end).await {
                        Ok(next) => written = next,
                        Err(_) => return false,
                    }
                    if written >= local_end {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        return false;
                    }
                }
                Err(data) => {
                    if !write_done_range(write_half, &data, &mut written, local_end)
                        .await
                        .unwrap_or(false)
                    {
                        return false;
                    }
                    break;
                }
            }
        }
    }
    write_half.flush().await.is_ok()
}

async fn write_status_close(
    write_half: &mut tokio::net::tcp::OwnedWriteHalf,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let hdr = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    write_half.write_all(hdr.as_bytes()).await?;
    if !body.is_empty() {
        write_half.write_all(body).await?;
    }
    write_half.flush().await
}

fn parse_track_id(target: &str) -> Option<u64> {
    let path = target.split('?').next().unwrap_or(target);
    let mut segs = path.trim_start_matches('/').split('/');
    match (segs.next(), segs.next()) {
        (Some("track"), Some(id)) => id.parse::<u64>().ok(),
        _ => None,
    }
}

fn parse_range_header(value: &str) -> RangeSpec {
    let trimmed = value.trim();
    let Some(spec) = trimmed.strip_prefix("bytes=") else {
        return RangeSpec::Invalid;
    };
    if spec.contains(',') {
        return RangeSpec::Invalid;
    }
    let Some((s, e)) = spec.split_once('-') else {
        return RangeSpec::Invalid;
    };
    if s.is_empty() {
        return RangeSpec::Invalid;
    }
    let Ok(start) = s.trim().parse::<u64>() else {
        return RangeSpec::Invalid;
    };
    if e.trim().is_empty() {
        RangeSpec::From(start)
    } else if let Ok(end) = e.trim().parse::<u64>() {
        if end < start {
            RangeSpec::Invalid
        } else {
            RangeSpec::Bounded(start, end)
        }
    } else {
        RangeSpec::Invalid
    }
}

fn parse_content_range(header: &str) -> Option<(u64, u64, Option<u64>)> {
    let spec = header.trim().strip_prefix("bytes ")?;
    let (range_part, total_part) = spec.split_once('/')?;
    let (s, e) = range_part.split_once('-')?;
    let start = s.trim().parse::<u64>().ok()?;
    let end = e.trim().parse::<u64>().ok()?;
    let total = if total_part.trim() == "*" {
        None
    } else {
        total_part.trim().parse::<u64>().ok()
    };
    Some((start, end, total))
}

fn parse_content_range_total(header: &str) -> Option<u64> {
    let (_, total_part) = header.trim().split_once('/')?;
    total_part.trim().parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct MockHitCounter {
        bad_hits: AtomicU64,
        good_hits: AtomicU64,
        good_range_requests: Mutex<HashSet<u64>>,
    }

    async fn spawn_mock_range_server(
        payload: Arc<Vec<u8>>,
        fail_requests: bool,
        counter: Arc<MockHitCounter>,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = payload.clone();
                let ctr = counter.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    let req = String::from_utf8_lossy(&buf[..n]);
                    if fail_requests {
                        ctr.bad_hits.fetch_add(1, Ordering::Relaxed);
                        let _ = sock
                            .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                        return;
                    }
                    ctr.good_hits.fetch_add(1, Ordering::Relaxed);
                    let mut range_val = None;
                    for line in req.lines() {
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("range")
                        {
                            range_val = Some(v.trim().to_string());
                        }
                    }
                    let total = data.len() as u64;
                    let (start, end) = match range_val.as_deref().map(parse_range_header) {
                        Some(RangeSpec::Bounded(s, e)) => (s, e.min(total - 1)),
                        Some(RangeSpec::From(s)) => (s, total - 1),
                        _ => (0, total - 1),
                    };
                    if let Ok(mut set) = ctr.good_range_requests.lock() {
                        set.insert(start);
                    }
                    let slice = &data[start as usize..=end as usize];
                    let hdr = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        slice.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    let _ = sock.write_all(slice).await;
                });
            }
        });
        addr
    }

    fn build_pattern_payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    #[tokio::test]
    async fn proxy_handles_range_failover_prefetch_and_bounded_cache() {
        // 300 KiB 载荷，配置分块 32 KiB → 共 10 块；缓存上限 4 块（测试边界驱逐）。
        let payload = Arc::new(build_pattern_payload(300 * 1024));
        let counter = Arc::new(MockHitCounter {
            bad_hits: AtomicU64::new(0),
            good_hits: AtomicU64::new(0),
            good_range_requests: Mutex::new(HashSet::new()),
        });
        let bad_addr = spawn_mock_range_server(payload.clone(), true, counter.clone()).await;
        let good_addr = spawn_mock_range_server(payload.clone(), false, counter.clone()).await;

        let health = Arc::new(CdnHealthTracker::new());
        let cfg = StreamProxyConfig {
            chunk_size: 32 * 1024,
            max_cached_chunks: 4,
            prefetch_ahead: 3,
            connect_timeout: Duration::from_secs(2),
            chunk_timeout: Duration::from_secs(2),
        };
        let proxy = StreamProxy::start_with_config(Handle::current(), health, cfg).unwrap();

        // 同等级候选：主链返回 503，验证代理自动回退到 backup 并记主节点冷却。
        let video_urls = crate::cdn::collect_cdn_candidates(
            &format!("http://{bad_addr}/primary.m4s"),
            &[format!("http://{good_addr}/backup.m4s")],
            false,
        );
        let audio_urls = vec![format!("http://{good_addr}/audio.m4s")];
        let pair = proxy.register_dash_pair(video_urls, audio_urls, true);
        let track_id = parse_track_id(
            pair.video_url
                .strip_prefix(&format!("http://{}", proxy.addr()))
                .unwrap(),
        )
        .unwrap();

        let client = reqwest::Client::new();
        // 1) MPD SegmentBase 风格：先读 0-976，再读 977-1721（第二笔应 0ms 命中分块 0 缓存）
        let r1 = client
            .get(&pair.video_url)
            .header(reqwest::header::RANGE, "bytes=0-976")
            .send()
            .await
            .unwrap();
        assert_eq!(r1.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            r1.headers()
                .get(reqwest::header::CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            format!("bytes 0-976/{}", payload.len())
        );
        let b1 = r1.bytes().await.unwrap();
        assert_eq!(&b1[..], &payload[0..=976]);
        assert_eq!(counter.bad_hits.load(Ordering::Relaxed), 1, "bad 节点应被尝试并记失败冷却");

        // 2) 跨多块开区间读取（libmpv 风格 Range: bytes=40000-）
        let r2 = client
            .get(&pair.video_url)
            .header(reqwest::header::RANGE, "bytes=40000-")
            .send()
            .await
            .unwrap();
        assert_eq!(r2.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        let b2 = r2.bytes().await.unwrap();
        assert_eq!(&b2[..], &payload[40000..]);

        // 3) 缓存严格受限于 max_cached_chunks=4，不无限膨胀
        assert!(
            proxy.track_cached_chunks(track_id) <= 4,
            "分块缓存不得突破上限: {}",
            proxy.track_cached_chunks(track_id)
        );

        // 4) HEAD 与 416 越界检查
        let head = client.head(&pair.video_url).send().await.unwrap();
        assert_eq!(head.status(), reqwest::StatusCode::OK);
        assert_eq!(
            head.headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            payload.len().to_string()
        );
        let oob = client
            .get(&pair.video_url)
            .header(reqwest::header::RANGE, format!("bytes={}-", payload.len() + 10))
            .send()
            .await
            .unwrap();
        assert_eq!(oob.status(), reqwest::StatusCode::RANGE_NOT_SATISFIABLE);

        // 5) 注销会话即清理轨道与缓存
        proxy.unregister_session(pair.session_id);
        let gone = client.get(&pair.video_url).send().await.unwrap();
        assert_eq!(gone.status(), reqwest::StatusCode::NOT_FOUND);
    }

    /// 涓流 mock:按 `piece` 片间隔 `gap` 滴流发完;`cut_after` 为 Some 时写到
    /// 该字节数即掐断连接(模拟 206 中途截断)。
    async fn spawn_trickle_server(
        payload: Arc<Vec<u8>>,
        piece: usize,
        gap: Duration,
        cut_after: Option<usize>,
    ) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = payload.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let total = data.len() as u64;
                    let mut range_val = None;
                    for line in req.lines() {
                        if let Some((k, v)) = line.split_once(':')
                            && k.trim().eq_ignore_ascii_case("range")
                        {
                            range_val = Some(v.trim().to_string());
                        }
                    }
                    let (start, end) = match range_val.as_deref().map(parse_range_header) {
                        Some(RangeSpec::Bounded(s, e)) => (s, e.min(total - 1)),
                        Some(RangeSpec::From(s)) => (s, total - 1),
                        _ => (0, total - 1),
                    };
                    let slice = &data[start as usize..=end as usize];
                    let hdr = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        slice.len()
                    );
                    if sock.write_all(hdr.as_bytes()).await.is_err() {
                        return;
                    }
                    let limit = cut_after.map_or(slice.len(), |c| c.min(slice.len()));
                    let mut sent = 0usize;
                    while sent < limit {
                        let n = piece.min(limit - sent);
                        if sock.write_all(&slice[sent..sent + n]).await.is_err() {
                            return;
                        }
                        sent += n;
                        if sent < slice.len() {
                            tokio::time::sleep(gap).await;
                        }
                    }
                });
            }
        });
        addr
    }

    /// #1 验收:分块未下完,读取端已能拿到前缀字节,且整体字节序列正确。
    #[tokio::test]
    async fn reader_receives_bytes_before_chunk_completes() {
        // 64 KiB 载荷 / 32 KiB 分块 / 8 KiB×100ms 滴流:首块完整需 ~300ms,
        // 「边收边转发」下首 8 KiB 应在首个滴流间隔内到达。
        let payload = Arc::new(build_pattern_payload(64 * 1024));
        let addr =
            spawn_trickle_server(payload.clone(), 8 * 1024, Duration::from_millis(100), None).await;
        let health = Arc::new(CdnHealthTracker::new());
        let cfg = StreamProxyConfig {
            chunk_size: 32 * 1024,
            max_cached_chunks: 8,
            prefetch_ahead: 0,
            connect_timeout: Duration::from_secs(2),
            chunk_timeout: Duration::from_secs(5),
        };
        let proxy = StreamProxy::start_with_config(Handle::current(), health, cfg).unwrap();
        let pair = proxy.register_dash_pair(
            vec![format!("http://{addr}/trickle.m4s")],
            vec![format!("http://{addr}/a.m4s")],
            false,
        );

        let path = pair
            .video_url
            .strip_prefix(&format!("http://{}", proxy.addr()))
            .unwrap();
        let t0 = Instant::now();
        let mut sock = TcpStream::connect(proxy.addr()).await.unwrap();
        sock.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: x\r\nRange: bytes=0-\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        // 读 HTTP 头直到空行。
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            sock.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
            assert!(head.len() < 4096, "响应头异常: {}", String::from_utf8_lossy(&head));
        }
        let head_s = String::from_utf8_lossy(&head).into_owned();
        assert!(head_s.starts_with("HTTP/1.1 206 Partial Content"), "{head_s}");
        assert!(head_s.contains("Content-Length: 65536"), "{head_s}");

        let mut first = vec![0u8; 8 * 1024];
        sock.read_exact(&mut first).await.unwrap();
        let first_in = t0.elapsed();
        assert!(
            first_in < Duration::from_millis(200),
            "首 8 KiB 到达耗时 {first_in:?},未按片边收边转发(整块下完约需 300ms)"
        );
        assert_eq!(first, payload[..8 * 1024]);

        let mut rest = vec![0u8; 64 * 1024 - 8 * 1024];
        sock.read_exact(&mut rest).await.unwrap();
        assert_eq!(
            [&first[..], &rest[..]].concat(),
            **payload,
            "分块未下完即开始读,整体字节序列必须一致"
        );
    }

    /// #1 回归:候选中途截断后,备线从已发布缺口续拉(而非整块重下载),
    /// 交付字节仍完整正确。
    #[tokio::test]
    async fn midstream_truncation_resumes_from_gap_on_backup() {
        let payload = Arc::new(build_pattern_payload(64 * 1024));
        let counter = Arc::new(MockHitCounter {
            bad_hits: AtomicU64::new(0),
            good_hits: AtomicU64::new(0),
            good_range_requests: Mutex::new(HashSet::new()),
        });
        // 主链:每请求写到 16 KiB 即掐断;备线:正常 Range mock。
        let cut_addr =
            spawn_trickle_server(payload.clone(), 16 * 1024, Duration::from_millis(0), Some(16 * 1024))
                .await;
        let good_addr =
            spawn_mock_range_server(payload.clone(), false, counter.clone()).await;
        let health = Arc::new(CdnHealthTracker::new());
        let cfg = StreamProxyConfig {
            chunk_size: 32 * 1024,
            max_cached_chunks: 8,
            prefetch_ahead: 0,
            connect_timeout: Duration::from_secs(2),
            chunk_timeout: Duration::from_secs(5),
        };
        let proxy = StreamProxy::start_with_config(Handle::current(), health, cfg).unwrap();
        let video_urls = crate::cdn::collect_cdn_candidates(
            &format!("http://{cut_addr}/cut.m4s"),
            &[format!("http://{good_addr}/backup.m4s")],
            false,
        );
        let pair = proxy.register_dash_pair(video_urls, vec![], false);

        let got = reqwest::Client::new()
            .get(&pair.video_url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(&got[..], &payload[..], "截断续拉后字节必须完整正确");
        let starts = counter.good_range_requests.lock().unwrap();
        assert!(
            starts.contains(&16384),
            "备线应从缺口 16384 续拉而非整块重拉,实际请求起点: {starts:?}"
        );
    }
}
