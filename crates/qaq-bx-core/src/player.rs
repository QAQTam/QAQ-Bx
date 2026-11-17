//! 播放地址客户端：view → cid；playurl（WBI 签名）→ DASH 分轨；流选择（纯函数）。
//!
//! 契约锚：docs/bilibili-api-inventory.md §2（fnval=16 纯 DASH 实测 code=0，
//! fnval=4044 被拒；m4s 流需 Referer 请求；URL 时效约 2.5h）。
//!
//! 流挑选策略（QAQ-Bx 硬件约束定制：RX560 无 AV1 硬解、Ivy Bridge 无 AVX2）：
//! codec 按优先级桶逐个尝试（默认 avc1 > hev1 > av01），桶内取 `id ≤ max_qn`
//! 中带宽最高者；音频取带宽最高档。默认上限 116 = 1080P60。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::account::NavClient;
use crate::credentials::CredentialProvider;
use crate::error::Result;
use crate::feed::{CardStat, Owner};
use crate::httpx::get_json_with_ticket;
use crate::loose::{f64_any, i32_any, i64_any, i64_any_or_zero};
use crate::ticket::TicketManager;
use crate::wbi::WbiSigner;

// B 站 playurl 的数字字段实测存在字符串形态——统一宽松解析移至
// `crate::loose`(player/danmaku 共用)。

const VIEW_URL: &str = "https://api.bilibili.com/x/web-interface/view";
const PLAYURL_URL: &str = "https://api.bilibili.com/x/player/wbi/playurl";

// ---------- view ----------

/// 视频元信息（view 响应，宽松契约：required 仅 bvid）。
#[derive(Debug, Clone, Deserialize)]
pub struct VideoMeta {
    pub bvid: String,
    #[serde(default)]
    pub aid: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default, deserialize_with = "i64_any_or_zero")]
    pub pubdate: i64,
    /// 默认分 P 的 cid（playurl/弹幕都靠它）。
    #[serde(default)]
    pub cid: u64,
    /// 总时长（秒;弹幕分段数计算用）。
    #[serde(default, deserialize_with = "i64_any")]
    pub duration: i64,
    #[serde(default)]
    pub owner: Option<Owner>,
    #[serde(default)]
    pub stat: Option<CardStat>,
    #[serde(default)]
    pub pages: Vec<VideoPage>,
}

/// 分 P（playurl/弹幕按页的 cid 取）。
#[derive(Debug, Clone, Deserialize)]
pub struct VideoPage {
    #[serde(default)]
    pub cid: u64,
    #[serde(default)]
    pub page: i64,
    #[serde(default)]
    pub part: String,
}

// ---------- playurl ----------

/// playurl 响应（fnval=16 路径；`durl` 旧 MP4 直链路径不做）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlayData {
    /// 实际下发档位代号。
    #[serde(default, deserialize_with = "i64_any")]
    pub quality: i64,
    /// 档位描述列表（UI 清晰度菜单用）。
    #[serde(default)]
    pub accept_description: Vec<String>,
    #[serde(default)]
    pub dash: Option<Dash>,
}

/// DASH 分轨集合。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dash {
    #[serde(default)]
    pub video: Vec<VideoStream>,
    #[serde(default)]
    pub audio: Vec<AudioStream>,
}

/// 视频流（m4s；`base_url` 为主，`backup_url` 为备）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VideoStream {
    /// 清晰度代号（80=1080p、116=1080P60 等）。
    #[serde(deserialize_with = "i64_any")]
    pub id: i64,
    #[serde(default)]
    pub codecs: String,
    #[serde(default, deserialize_with = "i64_any")]
    pub bandwidth: i64,
    #[serde(default, deserialize_with = "i32_any")]
    pub width: i32,
    #[serde(default, deserialize_with = "i32_any")]
    pub height: i32,
    #[serde(default, deserialize_with = "f64_any")]
    pub frame_rate: f64,
    pub base_url: String,
    #[serde(default)]
    pub backup_url: Vec<String>,
    /// 字节总量（预估磁盘/内存占用用）。
    #[serde(default, deserialize_with = "i64_any")]
    pub size: i64,
}

/// 音频流。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioStream {
    #[serde(deserialize_with = "i64_any")]
    pub id: i64,
    #[serde(default)]
    pub codecs: String,
    #[serde(default, deserialize_with = "i64_any")]
    pub bandwidth: i64,
    pub base_url: String,
    #[serde(default)]
    pub backup_url: Vec<String>,
}

/// 选中的一对流（loadfile 参数来源）。
#[derive(Debug, Clone)]
pub struct StreamChoice {
    pub video: VideoStream,
    pub audio: AudioStream,
}

// ---------- 客户端 ----------

/// 播放地址客户端（WBI 签名 + 登录态决定档位上限）。
pub struct PlayerClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<TicketManager>>,
    wbi: WbiSigner,
}

impl PlayerClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        let nav = Arc::new(NavClient::new(http.clone(), creds.clone()));
        Self::with_nav(http, creds, nav)
    }

    /// 复用共享 NavClient（WBI 密钥 + 用户资料共用一份 nav 缓存）。
    pub fn with_nav(
        http: reqwest::Client,
        creds: Arc<dyn CredentialProvider>,
        nav: Arc<NavClient>,
    ) -> Self {
        let ticket = nav.ticket().cloned();
        Self { wbi: WbiSigner::with_nav(nav), http, creds, ticket }
    }

    /// 视频元信息（无签名）：cid/aid/title/分 P 列表。
    pub async fn video_meta(&self, bvid: &str) -> Result<VideoMeta> {
        get_json_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &format!("{VIEW_URL}?bvid={bvid}"),
        )
        .await
    }

    /// DASH 播放地址（WBI 签名；`qn=0` + `fnval=16` 全档下发，客户端自选）。
    pub async fn playurl(&self, bvid: &str, cid: u64) -> Result<PlayData> {
        let signed = self
            .wbi
            .sign_query(&[
                ("bvid", bvid.to_string()),
                ("cid", cid.to_string()),
                ("qn", "0".into()),
                ("fnval", "16".into()),
                ("fnver", "0".into()),
            ])
            .await?;
        get_json_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &format!("{PLAYURL_URL}?{signed}"),
        )
        .await
    }

    /// 一站式：meta + playurl + 按策略选流。
    pub async fn resolve(
        &self,
        bvid: &str,
        max_qn: i64,
        codec_priority: &[&str],
    ) -> Result<(VideoMeta, StreamChoice)> {
        let meta = self.video_meta(bvid).await?;
        self.resolve_for_meta(meta, max_qn, codec_priority).await
    }

    /// 已知卡片直出字段时的快路径：`cid > 0` 直接 1 跳请求 `playurl`，省去前置 `view` RTT；
    /// 若 `cid == 0` 自动退回完整 `resolve()`。
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_with_cid(
        &self,
        bvid: &str,
        aid: u64,
        cid: u64,
        title: &str,
        duration: i64,
        max_qn: i64,
        codec_priority: &[&str],
    ) -> Result<(VideoMeta, StreamChoice)> {
        if cid == 0 {
            return self.resolve(bvid, max_qn, codec_priority).await;
        }
        let meta = VideoMeta {
            bvid: bvid.to_string(),
            aid,
            title: title.to_string(),
            desc: None,
            pubdate: 0,
            cid,
            duration,
            owner: None,
            stat: None,
            pages: Vec::new(),
        };
        self.resolve_for_meta(meta, max_qn, codec_priority).await
    }

    async fn resolve_for_meta(
        &self,
        meta: VideoMeta,
        max_qn: i64,
        codec_priority: &[&str],
    ) -> Result<(VideoMeta, StreamChoice)> {
        let play = self.playurl(&meta.bvid, meta.cid).await?;
        let dash = play
            .dash
            .ok_or(crate::error::BiliError::InvalidData("playurl 无 dash（fnval 或权限）".into()))?;
        let choice = select_stream(&dash, max_qn, codec_priority)
            .ok_or(crate::error::BiliError::InvalidData("无可选流（档位上限过高或 codec 全缺）".into()))?;
        Ok((meta, choice))
    }
}

// ---------- 流选择（纯函数） ----------

/// 从 DASH 选流：codec 按优先级桶尝试（`codecs` 字段前缀匹配，avc1/hev1/av01），
/// 桶内取 `id ≤ max_qn` 中带宽最高者；音频取带宽最高档。全部无果返回 `None`。
pub fn select_stream(
    dash: &Dash,
    max_qn: i64,
    codec_priority: &[&str],
) -> Option<StreamChoice> {
    let mut best: Option<&VideoStream> = None;
    for codec in codec_priority {
        for v in &dash.video {
            if !v.codecs.starts_with(codec) || v.id > max_qn {
                continue;
            }
            best = match best {
                Some(b) if b.bandwidth >= v.bandwidth => Some(b),
                _ => Some(v),
            };
        }
        if best.is_some() {
            break;
        }
    }
    let video = best?.clone();
    let audio = dash.audio.iter().max_by_key(|a| a.bandwidth)?.clone();
    Some(StreamChoice { video, audio })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(id: i64, codecs: &str, bandwidth: i64) -> VideoStream {
        VideoStream {
            id,
            codecs: codecs.into(),
            bandwidth,
            width: 1920,
            height: 1080,
            frame_rate: 30.0,
            base_url: format!("https://cdn/{id}-{codecs}.m4s"),
            backup_url: vec![],
            size: 0,
        }
    }

    fn audio(id: i64, bandwidth: i64) -> AudioStream {
        AudioStream {
            id,
            codecs: "mp4a.40.2".into(),
            bandwidth,
            base_url: format!("https://cdn/audio-{id}.m4s"),
            backup_url: vec![],
        }
    }

    fn dash(videos: Vec<VideoStream>, audios: Vec<AudioStream>) -> Dash {
        Dash { video: videos, audio: audios }
    }

    #[test]
    fn selects_highest_bandwidth_within_qn_cap() {
        let d = dash(
            vec![stream(64, "avc1.640028", 1_000), stream(80, "avc1.640032", 2_000), stream(116, "avc1.640034", 3_000)],
            vec![audio(30216, 64), audio(30280, 192)],
        );
        // 上限 80：116 被过滤，80 胜出。
        let c = select_stream(&d, 80, &["avc1", "hev1", "av01"]).unwrap();
        assert_eq!(c.video.id, 80);
        assert_eq!(c.audio.id, 30280); // 音频取带宽最高
    }

    #[test]
    fn codec_priority_prefers_avc1_and_skips_empty_buckets() {
        let d = dash(
            vec![
                stream(80, "avc1.640032", 2_000),
                stream(80, "hev1.1.6.L120.90", 1_500), // 同档 hevc 带宽更低
                stream(112, "hev1.1.6.L150", 4_000),
            ],
            vec![audio(30232, 132)],
        );
        // avc1 桶非空 → 直接用 avc1(80)，不落 hevc。
        let c = select_stream(&d, 116, &["avc1", "hev1", "av01"]).unwrap();
        assert_eq!(c.video.codecs, "avc1.640032");
        assert_eq!(c.video.id, 80);

        // avc1 全缺 → 降级 hevc 桶（max_qn=80 滤掉 112，取 80 hevc）。
        let d2 = dash(
            vec![stream(80, "hev1.1.6.L120.90", 1_500), stream(112, "hev1.1.6.L150", 4_000)],
            vec![audio(30232, 132)],
        );
        let c2 = select_stream(&d2, 80, &["avc1", "hev1", "av01"]).unwrap();
        assert_eq!(c2.video.codecs, "hev1.1.6.L120.90");
    }

    #[test]
    fn qn_cap_filters_and_empty_dash_returns_none() {
        let d = dash(vec![stream(116, "avc1", 3_000)], vec![audio(30280, 192)]);
        assert!(select_stream(&d, 80, &["avc1"]).is_none()); // 只剩超上限档
        let empty = dash(vec![], vec![]);
        assert!(select_stream(&empty, 116, &["avc1"]).is_none());
    }

    #[test]
    fn video_meta_parses_owner_desc_stat_and_minimal_shape() {
        let full = serde_json::json!({
            "bvid": "BV1GJ411x7h7",
            "aid": 80433022,
            "title": "Never Gonna Give You Up",
            "desc": "Official MV",
            "pubdate": 1577835803,
            "cid": 137649199,
            "duration": 213,
            "owner": {
                "mid": 486906719,
                "name": "索尼音乐中国",
                "face": "https://i2.hdslb.com/bfs/face/test.jpg"
            },
            "stat": {
                "view": 1000,
                "danmaku": 50,
                "reply": 200
            },
            "pages": [{ "cid": 137649199, "page": 1, "part": "P1" }]
        });
        let meta: VideoMeta = serde_json::from_value(full).unwrap();
        assert_eq!(meta.aid, 80433022);
        assert_eq!(meta.desc.as_deref(), Some("Official MV"));
        let owner = meta.owner.expect("owner 应存在");
        assert_eq!(owner.mid, 486906719);
        assert_eq!(owner.name, "索尼音乐中国");
        assert_eq!(owner.face.as_deref(), Some("https://i2.hdslb.com/bfs/face/test.jpg"));

        let minimal = serde_json::json!({ "bvid": "BV1x", "desc": null, "pubdate": null });
        let min_meta: VideoMeta = serde_json::from_value(minimal).unwrap();
        assert_eq!(min_meta.bvid, "BV1x");
        assert!(min_meta.owner.is_none());
        assert_eq!(min_meta.pubdate, 0);
    }
}
