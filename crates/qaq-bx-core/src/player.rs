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

/// playurl 响应（fnval=16 路径；`durl` 为未解锁稿件的试看降级形态，只记录不消费）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlayData {
    /// 实际下发档位代号。
    #[serde(default, deserialize_with = "i64_any")]
    pub quality: i64,
    /// 档位描述列表（UI 清晰度菜单用）。
    #[serde(default)]
    pub accept_description: Vec<String>,
    /// 全片时长（毫秒；dash 形态即视频时长，试看判定基准）。
    #[serde(default, deserialize_with = "i64_any")]
    pub timelength: i64,
    #[serde(default)]
    pub dash: Option<Dash>,
    /// 旧 MP4 直链降级段：充电专稿/付费未解锁时 `dash=null` 且 `durl=[试看片段]`
    /// （真机行为锚：BV1zonn67E6W 试看 180s vs 全片 1223s，`result="suee"`）。
    #[serde(default)]
    pub durl: Vec<DurlSegment>,
}

/// `durl` 降级段（URL 不消费，仅长度判试看）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DurlSegment {
    /// 分段时长（毫秒）。
    #[serde(default, deserialize_with = "i64_any")]
    pub length: i64,
}

/// 试看降级文案的稳定标记（UI 端据此识别未解锁稿件并打标，勿改动文案中的该子串）。
pub const TRIAL_DOWNGRADE_MARK: &str = "充电专稿/付费专稿";

/// 判定一条解析错误是否为未解锁稿件的试看降级（UI 打标过滤用）。
pub fn is_trial_downgrade_reason(reason: &str) -> bool {
    reason.contains(TRIAL_DOWNGRADE_MARK)
}

/// `dash` 缺失的原因判定（纯函数）：试看降级 vs 旧格式 durl vs 真异常。
pub fn dash_missing_reason(play: &PlayData) -> String {
    let trial_ms: i64 = play.durl.iter().map(|d| d.length).sum();
    if play.timelength > 0 && trial_ms > 0 && trial_ms < play.timelength {
        format!(
            "该稿件未解锁（{TRIAL_DOWNGRADE_MARK}），仅提供 {} 秒试看片段，不支持播放",
            trial_ms / 1000
        )
    } else if !play.durl.is_empty() {
        "服务端降级为 durl（MP4 直链）形态，不支持播放".into()
    } else {
        "playurl 无 dash（fnval 或权限）".into()
    }
}

/// DASH 分轨集合。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dash {
    #[serde(default)]
    pub video: Vec<VideoStream>,
    #[serde(default)]
    pub audio: Vec<AudioStream>,
}

/// m4s 的 DASH SegmentBase(B 站 JSON 原样范围串,如 initialization="0-976" /
/// indexRange="977-1721";MPD 生成直接内插)。
///
/// 实测(BV1GJ411x7h7 playurl fnval=16):B 站对同一 dash 条目同时下发
/// PascalCase `SegmentBase` 与 snake_case `segment_base` 两种键形,内部键
/// 同样混用,故全部收进 alias。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct SegmentBase {
    #[serde(
        default,
        rename = "initialization",
        alias = "Initialization",
        deserialize_with = "loose_str"
    )]
    pub initialization: String,
    #[serde(
        default,
        rename = "indexRange",
        alias = "IndexRange",
        alias = "index_range",
        deserialize_with = "loose_str"
    )]
    pub index_range: String,
}

/// 宽松字符串:JSON 里范围串可能是数字形态。
fn loose_str<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    })
}

/// SegmentBase 宽容反序列化:B 站同键位可能下发 null/字符串/对象多形态
/// (snake `segment_base` 实测非对象),非法形态一律视为缺失。
fn seg_base_loose<'de, D>(deserializer: D) -> std::result::Result<Option<SegmentBase>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => None,
        other => SegmentBase::deserialize(other).ok().filter(|s| {
            !s.initialization.is_empty() || !s.index_range.is_empty()
        }),
    })
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
    /// MIME（"video/mp4";系统 DASH 管线按它区分资源类型）。
    #[serde(default)]
    pub mime_type: String,
    #[serde(default, deserialize_with = "i32_any")]
    pub start_with_sap: i32,
    #[serde(
        default,
        rename = "SegmentBase",
        alias = "segmentBase",
        deserialize_with = "seg_base_loose"
    )]
    pub segment_base: Option<SegmentBase>,
    /// 捕获未知键名(B 站 dash 同键双形取证/前向兼容;不参与渲染路径)。
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
    /// MIME（"audio/mp4"）。
    #[serde(default)]
    pub mime_type: String,
    #[serde(default, deserialize_with = "i32_any")]
    pub start_with_sap: i32,
    #[serde(
        default,
        rename = "SegmentBase",
        alias = "segmentBase",
        deserialize_with = "seg_base_loose"
    )]
    pub segment_base: Option<SegmentBase>,
    /// 捕获未知键名(B 站 dash 同键双形取证/前向兼容;不参与渲染路径)。
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// 选中的一对流（loadfile 参数来源）。
#[derive(Debug, Clone)]
pub struct StreamChoice {
    pub video: VideoStream,
    pub audio: AudioStream,
}

/// 清晰度代号 → 展示名（B 站公开档位；未知代号回退 `<qn>P`）。
pub fn quality_label(qn: i64) -> String {
    match qn {
        6 => "240P 极速".into(),
        16 => "360P 流畅".into(),
        32 => "480P 清晰".into(),
        64 => "720P 高清".into(),
        74 => "720P60 高帧率".into(),
        80 => "1080P 高清".into(),
        112 => "1080P+ 高码率".into(),
        116 => "1080P60 高帧率".into(),
        120 => "4K 超清".into(),
        125 => "HDR 真彩".into(),
        126 => "杜比视界".into(),
        127 => "8K 超清".into(),
        other => format!("{other}P"),
    }
}

/// 画质菜单数据：同一清晰度可能下发多 codec 流，按 `id` 去重并降序排列；
/// `max_qn` 过滤超出账号权限的档位（0 = 不过滤）。
pub fn quality_menu(video_streams: &[VideoStream], max_qn: i64) -> Vec<i64> {
    let mut qns: Vec<i64> = video_streams
        .iter()
        .map(|v| v.id)
        .filter(|id| *id > 0 && (max_qn <= 0 || *id <= max_qn))
        .collect();
    qns.sort_unstable();
    qns.dedup();
    qns.reverse();
    qns
}

/// 按目标清晰度精确重选（播放页画质切换）：`id == qn` 中按 codec 优先级桶、
/// 桶内带宽最高；音频取带宽最高档。无匹配返回 `None`（菜单只列 dash 存在的档）。
pub fn select_stream_by_qn(dash: &Dash, qn: i64, codec_priority: &[&str]) -> Option<StreamChoice> {
    let mut best: Option<&VideoStream> = None;
    for codec in codec_priority {
        for v in &dash.video {
            if v.id != qn || !v.codecs.starts_with(codec) {
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

/// 解码失败回退（F4）：与 [`select_stream`] 同策略，但跳过 `failed_prefixes`
/// 命中的 codec 桶（前缀匹配 `codecs` 字段），直到找到可尝试的下一桶。
pub fn select_stream_excluding(
    dash: &Dash,
    max_qn: i64,
    codec_priority: &[&str],
    failed_prefixes: &[&str],
) -> Option<StreamChoice> {
    let mut best: Option<&VideoStream> = None;
    for codec in codec_priority {
        if failed_prefixes
            .iter()
            .any(|f| codec.starts_with(f) || f.starts_with(codec))
        {
            continue;
        }
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

/// 音轨回退（F4）：跳过失败 URL 取带宽次高的音轨；无可选返回 `None`。
pub fn select_audio_excluding(dash: &Dash, failed_url: &str) -> Option<AudioStream> {
    dash.audio
        .iter()
        .filter(|a| a.base_url != failed_url)
        .max_by_key(|a| a.bandwidth)
        .cloned()
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
        Self {
            wbi: WbiSigner::with_nav(nav),
            http,
            creds,
            ticket,
        }
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
        self.resolve_full(bvid, max_qn, codec_priority)
            .await
            .map(|media| (media.meta, media.choice))
    }

    /// 完整解析：额外携带全量 DASH 分轨（播放页画质菜单/切换的数据源）。
    pub async fn resolve_full(
        &self,
        bvid: &str,
        max_qn: i64,
        codec_priority: &[&str],
    ) -> Result<ResolvedMedia> {
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
        self.resolve_full_with_cid(bvid, aid, cid, title, duration, max_qn, codec_priority)
            .await
            .map(|media| (media.meta, media.choice))
    }

    /// [`resolve_with_cid`] 的完整版：额外返回全量 DASH 分轨。
    #[allow(clippy::too_many_arguments)]
    pub async fn resolve_full_with_cid(
        &self,
        bvid: &str,
        aid: u64,
        cid: u64,
        title: &str,
        duration: i64,
        max_qn: i64,
        codec_priority: &[&str],
    ) -> Result<ResolvedMedia> {
        if cid == 0 {
            return self.resolve_full(bvid, max_qn, codec_priority).await;
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
    ) -> Result<ResolvedMedia> {
        let play = self.playurl(&meta.bvid, meta.cid).await?;
        let dash = match play.dash {
            Some(dash) => dash,
            None => {
                return Err(crate::error::BiliError::InvalidData(dash_missing_reason(&play)))
            }
        };
        let choice = select_stream(&dash, max_qn, codec_priority).ok_or(
            crate::error::BiliError::InvalidData("无可选流（档位上限过高或 codec 全缺）".into()),
        )?;
        Ok(ResolvedMedia { meta, choice, dash })
    }
}

/// 完整解析结果：含全量 DASH 分轨（画质菜单数据源 + 切换时重选）。
#[derive(Debug, Clone)]
pub struct ResolvedMedia {
    pub meta: VideoMeta,
    pub choice: StreamChoice,
    pub dash: Dash,
}

// ---------- 流选择（纯函数） ----------

/// 从 DASH 选流：codec 按优先级桶尝试（`codecs` 字段前缀匹配，avc1/hev1/av01），
/// 桶内取 `id ≤ max_qn` 中带宽最高者；音频取带宽最高档。全部无果返回 `None`。
pub fn select_stream(dash: &Dash, max_qn: i64, codec_priority: &[&str]) -> Option<StreamChoice> {
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
            mime_type: "video/mp4".into(),
            start_with_sap: 1,
            segment_base: None,
            extra: Default::default(),
        }
    }

    fn audio(id: i64, bandwidth: i64) -> AudioStream {
        AudioStream {
            id,
            codecs: "mp4a.40.2".into(),
            bandwidth,
            base_url: format!("https://cdn/audio-{id}.m4s"),
            backup_url: vec![],
            mime_type: "audio/mp4".into(),
            start_with_sap: 1,
            segment_base: None,
            extra: Default::default(),
        }
    }

    fn dash(videos: Vec<VideoStream>, audios: Vec<AudioStream>) -> Dash {
        Dash {
            video: videos,
            audio: audios,
        }
    }

    #[test]
    fn selects_highest_bandwidth_within_qn_cap() {
        let d = dash(
            vec![
                stream(64, "avc1.640028", 1_000),
                stream(80, "avc1.640032", 2_000),
                stream(116, "avc1.640034", 3_000),
            ],
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
            vec![
                stream(80, "hev1.1.6.L120.90", 1_500),
                stream(112, "hev1.1.6.L150", 4_000),
            ],
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
    fn quality_menu_dedupes_descends_and_respects_cap() {
        let d = dash(
            vec![
                stream(80, "avc1", 2_000),
                stream(80, "hev1", 1_500), // 同档第二 codec → 去重
                stream(16, "avc1", 300),
                stream(116, "avc1", 3_000),
            ],
            vec![audio(30280, 192)],
        );
        assert_eq!(quality_menu(&d.video, 0), vec![116, 80, 16]);
        assert_eq!(quality_menu(&d.video, 80), vec![80, 16]);
        assert!(quality_menu(&d.video, 8).is_empty());
    }

    #[test]
    fn quality_label_covers_known_tiers_and_falls_back() {
        assert_eq!(quality_label(80), "1080P 高清");
        assert_eq!(quality_label(116), "1080P60 高帧率");
        assert_eq!(quality_label(120), "4K 超清");
        assert_eq!(quality_label(999), "999P");
    }

    #[test]
    fn select_stream_by_qn_matches_exact_tier_and_codec_priority() {
        let d = dash(
            vec![
                stream(80, "avc1.640032", 2_000),
                stream(80, "hev1.1.6.L120.90", 2_500), // 同档 hevc 带宽更高
                stream(64, "avc1", 1_000),
            ],
            vec![audio(30280, 192)],
        );
        // avc1 桶优先，即便同档 hevc 带宽更高。
        let c = select_stream_by_qn(&d, 80, &["avc1", "hev1", "av01"]).unwrap();
        assert_eq!(c.video.codecs, "avc1.640032");
        // 空桶降级：avc1 无 80 档时取 hevc。
        let d2 = dash(
            vec![stream(80, "hev1.1.6.L120.90", 2_500)],
            vec![audio(30280, 192)],
        );
        let c2 = select_stream_by_qn(&d2, 80, &["avc1", "hev1", "av01"]).unwrap();
        assert_eq!(c2.video.codecs, "hev1.1.6.L120.90");
        // 目标档不存在 → None。
        assert!(select_stream_by_qn(&d, 120, &["avc1"]).is_none());
    }

    #[test]
    fn select_stream_excluding_skips_failed_codec_buckets() {
        let d = dash(
            vec![
                stream(80, "avc1.640032", 2_000),
                stream(80, "hev1.1.6.L120.90", 1_500),
                stream(80, "av01.0.08M.08", 1_000),
            ],
            vec![audio(30280, 192)],
        );
        // avc1 失败 → 降级 hev1 桶。
        let c = select_stream_excluding(&d, 80, &["avc1", "hev1", "av01"], &["avc1"]).unwrap();
        assert_eq!(c.video.codecs, "hev1.1.6.L120.90");
        // avc1+hev1 全失败 → av01；全失败 → None。
        let c2 =
            select_stream_excluding(&d, 80, &["avc1", "hev1", "av01"], &["avc1", "hev1"]).unwrap();
        assert_eq!(c2.video.codecs, "av01.0.08M.08");
        let all: Vec<&str> = vec!["avc1", "hev1", "av01"];
        assert!(select_stream_excluding(&d, 80, &all, &all).is_none());
    }

    #[test]
    fn select_audio_excluding_falls_back_to_next_bandwidth() {
        let d = dash(
            vec![stream(80, "avc1", 1_000)],
            vec![audio(30280, 192), audio(30216, 64), audio(30232, 132)],
        );
        let top_url = d.audio[0].base_url.clone();
        // 排除最高带宽档 → 取次高(132)。
        let next = select_audio_excluding(&d, &top_url).unwrap();
        assert_eq!(next.id, 30232);
        // URL 不在列表 → 照常返回最高档。
        assert_eq!(
            select_audio_excluding(&d, "https://other").unwrap().id,
            30280
        );
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
        assert_eq!(
            owner.face.as_deref(),
            Some("https://i2.hdslb.com/bfs/face/test.jpg")
        );

        let minimal = serde_json::json!({ "bvid": "BV1x", "desc": null, "pubdate": null });
        let min_meta: VideoMeta = serde_json::from_value(minimal).unwrap();
        assert_eq!(min_meta.bvid, "BV1x");
        assert!(min_meta.owner.is_none());
        assert_eq!(min_meta.pubdate, 0);
    }

    #[test]
    fn playdata_parses_trial_downgrade_shape() {
        // 真机行为锚(BV1zonn67E6W 充电专稿):code=0 + dash=null + durl 试看段。
        let json = serde_json::json!({
            "quality": 64,
            "accept_description": ["超清 4K", "高清 1080P60"],
            "timelength": 1222771,
            "dash": null,
            "durl": [{ "length": 179990, "size": 5847787, "url": "https://cdn/preview.mp4" }]
        });
        let p: PlayData = serde_json::from_value(json).unwrap();
        assert!(p.dash.is_none());
        assert_eq!(p.timelength, 1222771);
        assert_eq!(p.durl.len(), 1);
        assert_eq!(p.durl[0].length, 179990);
    }

    #[test]
    fn dash_missing_reason_distinguishes_trial_downgrade() {
        // 试看降级:durl 片段短于全片 → 明确「未解锁」文案。
        let trial = PlayData {
            quality: 64,
            accept_description: vec![],
            timelength: 1_222_771,
            dash: None,
            durl: vec![DurlSegment { length: 179_990 }],
        };
        let msg = dash_missing_reason(&trial);
        assert!(msg.contains("未解锁"), "{msg}");
        assert!(msg.contains("试看"), "{msg}");
        // 稳定标记:UI 端靠它识别未解锁稿件(过滤打标)。
        assert!(is_trial_downgrade_reason(&msg), "{msg}");
        assert!(!is_trial_downgrade_reason("playurl 无 dash（fnval 或权限）"));
        assert!(!is_trial_downgrade_reason("API 错误 -404: 啥都木有"));

        // 全片 durl(无试看证据) → 旧格式降级文案。
        let full = PlayData {
            quality: 64,
            accept_description: vec![],
            timelength: 179_990,
            dash: None,
            durl: vec![DurlSegment { length: 179_990 }],
        };
        assert!(dash_missing_reason(&full).contains("durl"));

        // 既无 dash 也无 durl → 保留原异常文案。
        let bare = PlayData {
            quality: 0,
            accept_description: vec![],
            timelength: 0,
            dash: None,
            durl: vec![],
        };
        assert_eq!(
            dash_missing_reason(&bare),
            "playurl 无 dash（fnval 或权限）"
        );
    }
}
