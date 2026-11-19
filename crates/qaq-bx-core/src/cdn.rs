//! B 站 DASH 流多 CDN 候选梳理、PCDN/MCDN 识别与节点健康追踪。
//!
//! B 站 playurl 在 `base_url` + `backup_url` 中同时下发官方 `upos-*` 节点与
//! `*.mcdn.bilivideo.cn` / 第三方边缘 PCDN 节点（且实测经常将 `mcdn` 放在首位
//! `base_url`）。本模块保持纯逻辑（无 UI/WinRT 耦合），负责：
//! 1. 将高卡顿风险的 `mcdn`/PCDN 节点识别并降权至兜底顺位，默认优先官方 `upos`；
//! 2. 按 `host:port` 维度记录失败冷却与延迟，发生超时/HTTP 错误时自动把坏节点后置。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_TRACKED_HOSTS: usize = 128;

/// CDN 节点等级（值越小优先级越高）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CdnTier {
    /// 官方商业云存储/CDN 镜像（`upos-sz-*` / `upos-hz-*` 等且非 `os=mcdn`）。
    OfficialUpos = 0,
    /// 普通商业 CDN / 通用边缘节点。
    NormalCdn = 1,
    /// PCDN / MCDN 节点（`os=mcdn`、`mcdn.bilivideo.cn`、第三方无线边缘等）。
    PcdnMcdn = 2,
}

fn split_authority_and_query(url: &str) -> Option<(&str, &str)> {
    let (_, rest) = url.trim().split_once("://")?;
    let (before_query, query) = rest.split_once('?').unwrap_or((rest, ""));
    let authority = before_query.split('/').next().unwrap_or("");
    if authority.is_empty() {
        None
    } else {
        Some((authority, query))
    }
}

/// 提取规范化 `host:port` 键（小写），供节点健康表统计使用。
pub fn host_port_key(url: &str) -> Option<String> {
    let (authority, _) = split_authority_and_query(url)?;
    let authority = authority.rsplit('@').next().unwrap_or(authority).trim();
    if authority.is_empty() {
        return None;
    }
    Some(authority.to_ascii_lowercase())
}

fn strip_port(authority: &str) -> String {
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if authority.starts_with('[') {
        authority
            .split(']')
            .next()
            .unwrap_or(authority)
            .trim_start_matches('[')
            .to_ascii_lowercase()
    } else {
        authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .to_ascii_lowercase()
    }
}

fn has_mcdn_query_marker(query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let k_lower = k.trim().to_ascii_lowercase();
            let v_lower = v.trim().to_ascii_lowercase();
            if k_lower == "os" && v_lower == "mcdn" {
                return true;
            }
            if k_lower == "mcdnid" && !v_lower.is_empty() && v_lower != "0" {
                return true;
            }
        }
    }
    false
}

/// 判定单个播放流 URL 的 CDN 等级。
pub fn classify_url_tier(url: &str) -> CdnTier {
    let Some((authority, query)) = split_authority_and_query(url) else {
        return CdnTier::NormalCdn;
    };
    let host = strip_port(authority);
    if has_mcdn_query_marker(query)
        || host.ends_with(".mcdn.bilivideo.cn")
        || host.ends_with(".mcdn.bilivideo.com")
        || host.starts_with("mcdn.bilivideo.")
        || host.contains(".mountaintoys.cn")
        || host.contains(".szbdyd.com")
        || host.contains(".pcdn.")
        || host.contains(".p2p.")
    {
        return CdnTier::PcdnMcdn;
    }
    if host.starts_with("upos-") || host.contains(".upos-") {
        return CdnTier::OfficialUpos;
    }
    CdnTier::NormalCdn
}

/// 合并并去重 `base_url` + `backup_url`；开启 `prefer_official_cdn` 时稳定重排为
/// `OfficialUpos` → `NormalCdn` → `PcdnMcdn`（PCDN 仍保留在末尾作为最后兜底）。
pub fn collect_cdn_candidates(
    base_url: &str,
    backup_url: &[String],
    prefer_official_cdn: bool,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(1 + backup_url.len());
    for raw in std::iter::once(base_url).chain(backup_url.iter().map(String::as_str)) {
        let trimmed = raw.trim();
        if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && seen.insert(trimmed.to_string())
        {
            out.push(trimmed.to_string());
        }
    }
    if prefer_official_cdn && out.len() > 1 {
        out.sort_by_key(|u| classify_url_tier(u));
    }
    out
}

#[derive(Debug, Clone)]
struct HostHealth {
    consecutive_failures: u32,
    cooldown_until: Option<Instant>,
    ewma_rtt_ms: Option<f64>,
    last_seen: Instant,
}

impl HostHealth {
    fn new(now: Instant) -> Self {
        Self {
            consecutive_failures: 0,
            cooldown_until: None,
            ewma_rtt_ms: None,
            last_seen: now,
        }
    }
}

/// 进程/会话级 CDN 节点健康表（有界容量，严格适配 4c8T 8G）。
pub struct CdnHealthTracker {
    hosts: Mutex<HashMap<String, HostHealth>>,
}

impl Default for CdnHealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CdnHealthTracker {
    pub fn new() -> Self {
        Self {
            hosts: Mutex::new(HashMap::new()),
        }
    }

    pub fn report_success(&self, url: &str, elapsed: Duration) {
        self.report_success_at(url, elapsed, Instant::now());
    }

    pub fn report_failure(&self, url: &str) {
        self.report_failure_at(url, Instant::now());
    }

    /// 结合健康状态、CDN 等级和已测延迟对候选 URL 排序：
    /// 健康节点永远优先于冷却中节点；同为健康节点保持 `OfficialUpos` 等级优先。
    pub fn rank_candidates(&self, urls: &[String]) -> Vec<String> {
        self.rank_candidates_at(urls, Instant::now())
    }

    fn report_success_at(&self, url: &str, elapsed: Duration, now: Instant) {
        let Some(key) = host_port_key(url) else {
            return;
        };
        let sample_ms = elapsed.as_secs_f64() * 1000.0;
        let Ok(mut map) = self.hosts.lock() else {
            return;
        };
        if !map.contains_key(&key) && map.len() >= MAX_TRACKED_HOSTS {
            evict_oldest(&mut map);
        }
        let entry = map.entry(key).or_insert_with(|| HostHealth::new(now));
        entry.consecutive_failures = 0;
        entry.cooldown_until = None;
        entry.last_seen = now;
        entry.ewma_rtt_ms = Some(match entry.ewma_rtt_ms {
            Some(prev) => prev * 0.7 + sample_ms * 0.3,
            None => sample_ms,
        });
    }

    fn report_failure_at(&self, url: &str, now: Instant) {
        let Some(key) = host_port_key(url) else {
            return;
        };
        let Ok(mut map) = self.hosts.lock() else {
            return;
        };
        if !map.contains_key(&key) && map.len() >= MAX_TRACKED_HOSTS {
            evict_oldest(&mut map);
        }
        let entry = map.entry(key).or_insert_with(|| HostHealth::new(now));
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.last_seen = now;
        let secs = match entry.consecutive_failures {
            1 => 15,
            2 => 45,
            _ => 120,
        };
        entry.cooldown_until = Some(now + Duration::from_secs(secs));
    }

    fn rank_candidates_at(&self, urls: &[String], now: Instant) -> Vec<String> {
        if urls.len() <= 1 {
            return urls.to_vec();
        }
        let Ok(map) = self.hosts.lock() else {
            return urls.to_vec();
        };
        let mut seen_hosts: HashSet<String> = HashSet::new();
        let mut indexed: Vec<(usize, &String, u8, u8, CdnTier, u64)> = urls
            .iter()
            .enumerate()
            .map(|(idx, url)| {
                let host = host_port_key(url);
                let health = host.as_ref().and_then(|k| map.get(k));
                let penalty_group = match health.and_then(|h| h.cooldown_until) {
                    Some(until) if until > now => 1u8,
                    _ => 0u8,
                };
                // 同一轮候选中同域名多签名 URL（如 orderid=1,3 与 2,3）将次重复项后置，
                // 故障转移优先切到真正不同的 CDN 节点。
                let dup_host_rank = if let Some(h) = host {
                    if seen_hosts.insert(h) { 0u8 } else { 1u8 }
                } else {
                    0u8
                };
                let tier = classify_url_tier(url);
                let latency_score = health
                    .and_then(|h| h.ewma_rtt_ms)
                    .map(|ms| (ms / 50.0).round() as u64)
                    .unwrap_or(2);
                (idx, url, penalty_group, dup_host_rank, tier, latency_score)
            })
            .collect();
        indexed.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| a.3.cmp(&b.3))
                .then_with(|| a.4.cmp(&b.4))
                .then_with(|| a.5.cmp(&b.5))
                .then_with(|| a.0.cmp(&b.0))
        });
        indexed.into_iter().map(|(_, u, _, _, _, _)| u.clone()).collect()
    }
}

fn evict_oldest(map: &mut HashMap<String, HostHealth>) {
    if let Some(oldest_key) = map
        .iter()
        .min_by_key(|(_, v)| v.last_seen)
        .map(|(k, _)| k.clone())
    {
        map.remove(&oldest_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_upos_normal_and_pcdn_urls() {
        let upos = "https://upos-sz-estgoss.bilivideo.com/upgcxcode/99/91/137.m4s?os=estgoss";
        let mcdn = "https://xy118x212xy.mcdn.bilivideo.cn:8082/v1/resource/137.m4s?os=mcdn&mcdnid=5006";
        let edge_mcdn = "https://r6iv589a.edge.mountaintoys.cn:4483/upgcxcode/137.m4s?os=mcdn";
        let normal = "https://cn-sh-fx-01-02.bilivideo.com/upgcxcode/137.m4s?os=bcache";

        assert_eq!(classify_url_tier(upos), CdnTier::OfficialUpos);
        assert_eq!(classify_url_tier(mcdn), CdnTier::PcdnMcdn);
        assert_eq!(classify_url_tier(edge_mcdn), CdnTier::PcdnMcdn);
        assert_eq!(classify_url_tier(normal), CdnTier::NormalCdn);
    }

    #[test]
    fn collect_candidates_prefers_upos_while_keeping_mcdn_as_fallback() {
        let mcdn = "https://xy118.mcdn.bilivideo.cn:8082/a.m4s?os=mcdn".to_string();
        let edge = "https://edge.mountaintoys.cn:4483/a.m4s?os=mcdn".to_string();
        let normal = "https://cn-bj.bilivideo.com/a.m4s".to_string();
        let upos1 = "https://upos-sz-estgoss.bilivideo.com/a.m4s?os=estgoss".to_string();
        let upos2 = "https://upos-sz-mirrorcoso1.bilivideo.com/a.m4s?os=coso1bv".to_string();

        let sorted = collect_cdn_candidates(
            &mcdn,
            &[edge.clone(), normal.clone(), upos1.clone(), upos2.clone(), upos1.clone()],
            true,
        );
        assert_eq!(sorted, vec![upos1.clone(), upos2.clone(), normal.clone(), mcdn.clone(), edge.clone()]);

        // 关闭优先官方 CDN 时保持原始顺序（仅去重）
        let raw = collect_cdn_candidates(&mcdn, &[edge.clone(), upos1.clone(), mcdn.clone()], false);
        assert_eq!(raw, vec![mcdn, edge, upos1]);
    }

    #[test]
    fn health_tracker_demotes_failing_host_and_recovers_after_cooldown() {
        let tracker = CdnHealthTracker::new();
        let u1 = "https://upos-sz-a.bilivideo.com/v.m4s".to_string();
        let u2 = "https://upos-sz-b.bilivideo.com/v.m4s".to_string();
        let m1 = "https://xy.mcdn.bilivideo.cn:8082/v.m4s?os=mcdn".to_string();
        let urls = vec![u1.clone(), u2.clone(), m1.clone()];

        let t0 = Instant::now();
        tracker.report_failure_at(&u1, t0);
        let ranked = tracker.rank_candidates_at(&urls, t0 + Duration::from_secs(2));
        // u1 进入冷却 → u2（健康 upos）第一，m1（健康 mcdn）也在冷却中的 u1 之前
        assert_eq!(ranked, vec![u2.clone(), m1.clone(), u1.clone()]);

        // 冷却到期（>15s）后，u1 恢复到 OfficialUpos 队列
        let recovered = tracker.rank_candidates_at(&urls, t0 + Duration::from_secs(20));
        assert_eq!(recovered, vec![u1.clone(), u2.clone(), m1]);

        // 同域名多签名 URL 将重复 host 后置，让故障切换首跳直接命中不同节点
        let u1_dup = "https://upos-sz-a.bilivideo.com/v.m4s?orderid=2,3".to_string();
        let ranked_dup = tracker.rank_candidates_at(
            &[u1.clone(), u1_dup.clone(), u2.clone()],
            t0 + Duration::from_secs(25),
        );
        assert_eq!(ranked_dup, vec![u1, u2, u1_dup]);
    }
}
