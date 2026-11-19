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
    chunks: HashMap<u64, Arc<Vec<u8>>>,
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

    fn get(&mut self, idx: u64) -> Option<Arc<Vec<u8>>> {
        self.cursor = idx;
        if let Some(data) = self.chunks.get(&idx).cloned() {
            self.tick += 1;
            self.last_used.insert(idx, self.tick);
            Some(data)
        } else {
            None
        }
    }

    fn insert(&mut self, idx: u64, data: Arc<Vec<u8>>, max_chunks: usize, prefetch_ahead: u64) {
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

type ChunkWatchRx = watch::Receiver<Option<Result<Arc<Vec<u8>>, String>>>;
type ChunkWatchTx = watch::Sender<Option<Result<Arc<Vec<u8>>, String>>>;

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
    tx: Option<ChunkWatchTx>,
}

impl Drop for InflightCleanup<'_> {
    fn drop(&mut self) {
        if let Ok(mut map) = self.track.inflight.lock() {
            map.remove(&self.chunk_idx);
        }
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Some(Err("请求已取消".to_string())));
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

    async fn fetch_chunk(
        self: &Arc<Self>,
        track: &Arc<TrackState>,
        chunk_idx: u64,
        from_reader: bool,
    ) -> Result<Arc<Vec<u8>>, String> {
        if track.is_cancelled() {
            return Err("流已释放".to_string());
        }
        if let Ok(mut cache) = track.cache.lock() {
            if from_reader {
                if let Some(hit) = cache.get(chunk_idx) {
                    return Ok(hit);
                }
            } else if let Some(hit) = cache.chunks.get(&chunk_idx).cloned() {
                return Ok(hit);
            }
        }

        // single-flight 合并：同一分块并发请求共享一条上游连接。
        let (wait_rx, leader_tx) = {
            let mut inflight = track
                .inflight
                .lock()
                .map_err(|_| "inflight 锁中毒".to_string())?;
            if let Some(rx) = inflight.get(&chunk_idx) {
                (Some(rx.clone()), None)
            } else {
                let (tx, rx) = watch::channel(None);
                inflight.insert(chunk_idx, rx);
                (None, Some(tx))
            }
        };

        if let Some(mut rx) = wait_rx {
            let _ = rx.wait_for(|v| v.is_some()).await;
            return rx
                .borrow()
                .clone()
                .unwrap_or_else(|| Err("上游等待中止".to_string()));
        }

        let mut cleanup = InflightCleanup {
            track,
            chunk_idx,
            tx: leader_tx,
        };

        let result = self.fetch_chunk_upstream(track, chunk_idx).await;
        if let Ok(ref data) = result
            && !data.is_empty()
            && !track.is_cancelled()
            && let Ok(mut cache) = track.cache.lock()
        {
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
        if let Some(tx) = cleanup.tx.take() {
            let _ = tx.send(Some(result.clone()));
        }
        result
    }

    async fn fetch_chunk_upstream(
        &self,
        track: &TrackState,
        chunk_idx: u64,
    ) -> Result<Arc<Vec<u8>>, String> {
        let chunk_size = track.chunk_size as u64;
        let start = chunk_idx.saturating_mul(chunk_size);
        let mut end = start.saturating_add(chunk_size).saturating_sub(1);
        if let Some(total) = track.total_len_opt() {
            if start >= total {
                return Ok(Arc::new(Vec::new()));
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

        let mut errors = Vec::new();
        let mut failed_hosts = std::collections::HashSet::new();
        for candidate in candidates {
            if track.is_cancelled() || self.shutdown.load(Ordering::Acquire) {
                return Err("流已取消".to_string());
            }
            let host_key = crate::cdn::host_port_key(&candidate);
            if let Some(ref hk) = host_key
                && failed_hosts.contains(hk)
            {
                continue;
            }
            let started = Instant::now();
            match tokio::time::timeout(
                self.cfg.chunk_timeout,
                self.try_fetch_range(&candidate, start, end, chunk_size),
            )
            .await
            {
                Ok(Ok(FetchOutcome::Chunk { data, total_len })) => {
                    if let Some(total) = total_len {
                        track.set_total_len(total);
                    }
                    self.cdn_health.report_success(&candidate, started.elapsed());
                    return Ok(Arc::new(data));
                }
                Ok(Ok(FetchOutcome::Eof { total_len })) => {
                    if let Some(total) = total_len {
                        track.set_total_len(total);
                    }
                    return Ok(Arc::new(Vec::new()));
                }
                Ok(Err(err)) => {
                    self.cdn_health.report_failure(&candidate);
                    if let Some(hk) = host_key.clone() {
                        failed_hosts.insert(hk);
                    }
                    eprintln!(
                        "[stream-proxy] track={} chunk={} host={:?} 失败，切备线: {}",
                        track.id,
                        chunk_idx,
                        host_key.unwrap_or_default(),
                        err
                    );
                    errors.push(err);
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

        Err(format!("全部候选 CDN 拉片失败: {}", errors.join("; ")))
    }

    async fn try_fetch_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
        chunk_size: u64,
    ) -> Result<FetchOutcome, String> {
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::REFERER, REFERER)
            .header(reqwest::header::USER_AGENT, BROWSER_UA)
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
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
            return Ok(FetchOutcome::Eof { total_len });
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let cr_header = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let parsed_cr = cr_header.as_deref().and_then(parse_content_range);
            let body = resp
                .bytes()
                .await
                .map_err(|e| format!("读取响应体失败: {e}"))?;
            if body.is_empty() {
                return Err("206 响应体为空".to_string());
            }
            if let Some((r_start, r_end, total)) = parsed_cr {
                if r_start != start {
                    return Err(format!("Content-Range 起点不匹配: {r_start} != {start}"));
                }
                let expected = (r_end - r_start + 1) as usize;
                if body.len() != expected {
                    return Err(format!("206 分片截断: {} != {}", body.len(), expected));
                }
                return Ok(FetchOutcome::Chunk {
                    data: body.to_vec(),
                    total_len: total,
                });
            }
            return Ok(FetchOutcome::Chunk {
                data: body.to_vec(),
                total_len: None,
            });
        }
        if status == reqwest::StatusCode::OK {
            if start != 0 {
                return Err("上游忽略 Range 返回 200 OK".to_string());
            }
            if let Some(len) = resp.content_length()
                && len > chunk_size
            {
                return Err(format!("上游未分片返回全量 200 ({len}B)"));
            }
            let body = resp
                .bytes()
                .await
                .map_err(|e| format!("读取 200 响应体失败: {e}"))?;
            if body.len() as u64 > chunk_size {
                return Err("上游返回超出分块上限的 200 载荷".to_string());
            }
            let total = body.len() as u64;
            return Ok(FetchOutcome::Chunk {
                data: body.to_vec(),
                total_len: Some(total),
            });
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

enum FetchOutcome {
    Chunk {
        data: Vec<u8>,
        total_len: Option<u64>,
    },
    Eof {
        total_len: Option<u64>,
    },
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

    let first_chunk = match proxy.fetch_chunk(track, probe_chunk_idx, true).await {
        Ok(data) => data,
        Err(_) => {
            let _ = write_status_close(write_half, "502 Bad Gateway", "text/plain", b"").await;
            return false;
        }
    };

    let Some(total_len) = track.total_len_opt() else {
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
        let chunk = if chunk_idx == probe_chunk_idx {
            first_chunk.clone()
        } else {
            match proxy.fetch_chunk(track, chunk_idx, true).await {
                Ok(c) => c,
                Err(_) => return false,
            }
        };
        let chunk_start_off = chunk_idx * chunk_size;
        let local_start = start.saturating_sub(chunk_start_off) as usize;
        let chunk_end_off = chunk_start_off + chunk.len() as u64;
        if chunk_end_off <= start {
            return false;
        }
        let wanted_until = (end + 1).min(chunk_end_off);
        let local_end = (wanted_until - chunk_start_off) as usize;
        if local_start >= local_end || local_end > chunk.len() {
            return false;
        }
        if write_half
            .write_all(&chunk[local_start..local_end])
            .await
            .is_err()
        {
            return false;
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
}
