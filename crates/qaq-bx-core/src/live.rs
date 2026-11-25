//! 直播客户端（U4 接线；guest 无 Cookie 可用的稳定旧版端点）。
//!
//! 契约锚（2026-11-23 实测，无 Cookie code=0）：
//! - 列表：`GET https://api.live.bilibili.com/room/v1/Area/getRoomList?area_id=0&order_type=online&page=<n>&page_size=20`
//!   → `data[]`：`roomid/uid/title/uname/online/user_cover/system_cover/area_v2_name/area_v2_parent_name`。
//!   （新版 `xlive/.../second/getList` 无 buvid3 风控 -352，不可用。）
//! - 房间流（v2 优先）：`GET https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo?room_id=<id>&protocol=0,1&format=0,1,2&codec=0&qn=0&platform=web&ptype=16`
//!   → `data.playurl_info.playurl.stream[]`，`protocol_name=http_hls` 携带
//!   `format=fmp4/ts` 的 m3u8 直链（URL = `url_info.host` + `codec.base_url` + `url_info.extra`）。
//!   系统引擎 `AdaptiveMediaSource` 原生支持 HLS（fMP4 v7 + EXT-X-MAP 实测 guest 可拉）。
//! - 房间流（v1 回落）：`GET https://api.live.bilibili.com/room/v1/Room/playUrl?cid=<roomid>&qn=0&platform=web`
//!   → `data.durl[]`（实测仅 FLV；系统引擎不支持，只能走 mpv）。

use std::sync::Arc;

use serde::Deserialize;

use crate::credentials::CredentialProvider;
use crate::error::{BiliError, Result};

const ROOM_LIST_URL: &str = "https://api.live.bilibili.com/room/v1/Area/getRoomList";
const PLAY_URL: &str = "https://api.live.bilibili.com/room/v1/Room/playUrl";
const PLAY_URL_V2: &str = "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo";

/// 房间流直链(协议已知,shell 据此分流播放引擎)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveStreamUrl {
    /// HLS(m3u8,fMP4/TS)——系统引擎 `AdaptiveMediaSource` 原生支持。
    Hls(String),
    /// FLV——系统引擎不支持,仅 mpv 可播(现状保留)。
    Flv(String),
}

/// 直播间展示模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRoom {
    pub roomid: u64,
    pub title: String,
    pub uname: String,
    /// 人气值（服务端口径,非真实观看人数）。
    pub online: u64,
    pub cover: String,
    pub area_name: String,
    pub parent_name: String,
}

/// 直播列表/房间流客户端。
pub struct LiveClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
}

impl LiveClient {
    pub fn new(http: reqwest::Client, creds: Arc<dyn CredentialProvider>) -> Self {
        Self { http, creds }
    }

    /// 直播房间列表（`page` 从 1 起；固定 `page_size=20`，实测上限）。
    pub async fn rooms(&self, page: u32) -> Result<Vec<LiveRoom>> {
        let page = page.max(1);
        let url = format!("{ROOM_LIST_URL}?area_id=0&order_type=online&page={page}&page_size=20");
        let raw: Vec<LooseRoom> =
            crate::httpx::get_json_with_ticket(&self.http, &self.creds, None, &url).await?;
        Ok(parse_live_rooms(raw))
    }

    /// 房间流直链(v2 HLS 优先,失败回落 v1 FLV;房间未开播返回 `InvalidData`)。
    pub async fn play_url(&self, roomid: u64) -> Result<LiveStreamUrl> {
        if roomid == 0 {
            return Err(BiliError::InvalidData("缺少房间号".into()));
        }
        match self.play_url_v2_hls(roomid).await {
            Ok(url) => Ok(LiveStreamUrl::Hls(url)),
            // v2 端点风控/无 hls 流/字段缺失 → 现状 v1 FLV(mpv 可播)。
            Err(_) => self.play_url_v1_flv(roomid).await.map(LiveStreamUrl::Flv),
        }
    }

    /// v2 xlive 端点:HLS(fMP4 优先)直链。失败即由调用方回落 v1。
    async fn play_url_v2_hls(&self, roomid: u64) -> Result<String> {
        let url = format!(
            "{PLAY_URL_V2}?room_id={roomid}&protocol=0,1&format=0,1,2&codec=0&qn=0&platform=web&ptype=16"
        );
        let data: RawV2Payload = crate::httpx::get_json_with_ticket(
            &self.http,
            &self.creds,
            None,
            &url,
        )
        .await?;
        parse_v2_hls_url(&data).map_err(|err| {
            BiliError::InvalidData(format!("v2 房间流无可用 HLS 直链:{err}"))
        })
    }

    /// v1 旧端点:FLV 直链(现状路径,系统引擎不支持)。
    async fn play_url_v1_flv(&self, roomid: u64) -> Result<String> {
        #[derive(Deserialize)]
        struct RawPlay {
            #[serde(default)]
            durl: Vec<RawDurl>,
        }
        #[derive(Deserialize)]
        struct RawDurl {
            #[serde(default)]
            url: String,
        }
        let url = format!("{PLAY_URL}?cid={roomid}&qn=0&platform=web");
        let data: RawPlay = crate::httpx::get_json_with_ticket(
            &self.http,
            &self.creds,
            None,
            &url,
        )
        .await?;
        let first = data
            .durl
            .into_iter()
            .find(|d| !d.url.trim().is_empty())
            .ok_or(BiliError::InvalidData("房间未开播或无可用流".into()))?;
        Ok(first.url)
    }
}

/// v2 `getRoomPlayInfo` 载荷的宽松解析结构(`data` 解包后即 `RawV2Payload`;
/// 字段缺省安全,数字/枚举形态以实测样本为准)。
#[derive(Deserialize)]
struct RawV2Payload {
    #[serde(default)]
    playurl_info: Option<RawV2Info>,
}

#[derive(Deserialize)]
struct RawV2Info {
    #[serde(default)]
    playurl: Option<RawV2Play>,
}

#[derive(Deserialize)]
struct RawV2Play {
    #[serde(default)]
    stream: Vec<RawV2Stream>,
}

#[derive(Deserialize)]
struct RawV2Stream {
    #[serde(default)]
    protocol_name: String,
    #[serde(default)]
    format: Vec<RawV2Format>,
}

#[derive(Deserialize)]
struct RawV2Format {
    #[serde(default)]
    format_name: String,
    #[serde(default)]
    codec: Vec<RawV2Codec>,
}

#[derive(Deserialize)]
struct RawV2Codec {
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    url_info: Vec<RawV2UrlInfo>,
}

#[derive(Deserialize)]
struct RawV2UrlInfo {
    #[serde(default)]
    host: String,
    #[serde(default)]
    extra: String,
}

/// 从 v2 载荷抽取 HLS 直链(纯函数,单测锁定 URL 拼装口径):
/// 取 `protocol_name=http_hls` 的流,fMP4 格式优先(EXT-X-MAP 分段,系统引擎
/// 支持最好);URL = `url_info[0].host` + `codec[0].base_url` + `url_info[0].extra`
/// (host 带协议,base_url 是带尾问号的路径,extra 是查询串)。
fn parse_v2_hls_url(payload: &RawV2Payload) -> Result<String> {
    let stream = payload
        .playurl_info
        .as_ref()
        .and_then(|info| info.playurl.as_ref())
        .map(|play| play.stream.as_slice())
        .unwrap_or_default();
    let hls = stream
        .iter()
        .find(|s| s.protocol_name == "http_hls")
        .ok_or(BiliError::InvalidData("无 http_hls 协议流".into()))?;
    let format = hls
        .format
        .iter()
        .find(|f| f.format_name == "fmp4")
        .or_else(|| hls.format.first())
        .ok_or(BiliError::InvalidData("http_hls 流无 format".into()))?;
    let codec = format
        .codec
        .first()
        .ok_or(BiliError::InvalidData("format 无 codec".into()))?;
    let url_info = codec
        .url_info
        .first()
        .ok_or(BiliError::InvalidData("codec 无 url_info".into()))?;
    let url = format!("{}{}{}", url_info.host, codec.base_url, url_info.extra);
    if !url.starts_with("http") || !url.contains(".m3u8") {
        return Err(BiliError::InvalidData(format!("HLS 直链形态异常:{url}")));
    }
    Ok(url)
}

// Envelope 在 feed.rs 定义为 {code, message, data};live 端点 message 字段为
// `message`/`msg` 双形态,复用同一解包(code 判定一致)。

/// 宽松房间行(数字字段实测存在字符串形态;全字段缺省安全)。
#[derive(Deserialize)]
pub(crate) struct LooseRoom {
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    roomid: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    uname: String,
    #[serde(default, deserialize_with = "crate::loose::u64_any_or_zero")]
    online: u64,
    #[serde(default)]
    user_cover: String,
    #[serde(default)]
    cover: String,
    #[serde(default)]
    area_v2_name: String,
    #[serde(default)]
    area_v2_parent_name: String,
}

/// 宽松解析房间列表（单测与实网共用;`roomid/online` 双形态数字由 `LooseRoom`
/// derive 直读,无 `Value` 中转;封面 `user_cover` 优先,空则 `cover`;roomid
/// 无效的行丢弃）。
pub(crate) fn parse_live_rooms(raw: Vec<LooseRoom>) -> Vec<LiveRoom> {
    raw.into_iter()
        .filter(|room| room.roomid != 0)
        .map(|room| {
            let cover = if room.user_cover.trim().is_empty() {
                room.cover
            } else {
                room.user_cover
            };
            LiveRoom {
                roomid: room.roomid,
                title: room.title,
                uname: room.uname,
                online: room.online,
                cover,
                area_name: room.area_v2_name,
                parent_name: room.area_v2_parent_name,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rooms(json: &str) -> Vec<LiveRoom> {
        let raw: Vec<LooseRoom> = serde_json::from_str(json).unwrap();
        parse_live_rooms(raw)
    }

    /// v2 抓包形态(2026-11-23,room 24158116 裁剪):http_hls 含 fmp4/ts 两档,
    /// URL 拼装 = host + codec.base_url + url_info.extra,且 fMP4 优先。
    #[test]
    fn parses_v2_hls_url_prefers_fmp4_and_concats_host_base_extra() {
        let payload: RawV2Payload = serde_json::from_str(
            r#"{"playurl_info":{"playurl":{"stream":[
                {"protocol_name":"http_stream","format":[
                    {"format_name":"flv","codec":[{"codec_name":"avc",
                     "base_url":"/live-bvc/415391/live_x.flv?",
                     "url_info":[{"host":"https://cn-jstz-cu-01-04.bilivideo.com",
                                  "extra":"expires=1&oi=2"}]}]}]},
                {"protocol_name":"http_hls","format":[
                    {"format_name":"ts","codec":[{"codec_name":"avc",
                     "base_url":"/live-bvc/713191/live_x.m3u8?",
                     "url_info":[{"host":"https://d1--cn-gotcha104.bilivideo.com",
                                  "extra":"expires=1"}]}]},
                    {"format_name":"fmp4","codec":[{"codec_name":"avc",
                     "base_url":"/live-bvc/255164/live_x/index.m3u8?",
                     "url_info":[{"host":"https://cn-jstz-cu-01-03.bilivideo.com",
                                  "extra":"expires=1795372228&oi=2"}]}]}]}
            ]}}}"#,
        )
        .unwrap();
        let url = parse_v2_hls_url(&payload).expect("应解析出 fMP4 HLS 直链");
        assert_eq!(
            url,
            "https://cn-jstz-cu-01-03.bilivideo.com/live-bvc/255164/live_x/index.m3u8?expires=1795372228&oi=2"
        );
    }

    /// v2 载荷无 http_hls(仅 FLV)→ Err,调用方回落 v1。
    #[test]
    fn parses_v2_hls_url_errors_without_hls_stream() {
        let payload: RawV2Payload = serde_json::from_str(
            r#"{"playurl_info":{"playurl":{"stream":[
                {"protocol_name":"http_stream","format":[{"format_name":"flv",
                 "codec":[{"codec_name":"avc","base_url":"/x.flv?",
                 "url_info":[{"host":"https://a.bilivideo.com","extra":"e=1"}]}]}]}
            ]}}}"#,
        )
        .unwrap();
        assert!(parse_v2_hls_url(&payload).is_err());
    }

    /// 实测响应样本(2026-11-18)宽松解析:字段缺省/字符串数字不 panic。
    #[test]
    fn parses_live_rooms_from_captured_shape() {
        let rooms = parse_rooms(
            r#"[{"roomid":1939978136,"title":"三角洲跑刀主播","uname":"卡卡罗特mxa","online":1000371,
                 "user_cover":"https://i0.hdslb.com/cover.jpg","cover":"https://i0.hdslb.com/cover.jpg",
                 "area_v2_name":"三角洲行动","area_v2_parent_name":"网游"},
                {"roomid":"23929781","title":"我吃好了","uname":"罗太","online":"739602",
                 "user_cover":"","cover":"https://i0.hdslb.com/b.jpg","area_v2_name":"","area_v2_parent_name":"单机游戏"}]"#,
        );
        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].roomid, 1939978136);
        assert_eq!(rooms[0].area_name, "三角洲行动");
        assert_eq!(rooms[1].cover, "https://i0.hdslb.com/b.jpg");
    }

    /// 需真实网络:直播列表 + 房间流直链冒烟(无 Cookie)。
    #[test]
    #[ignore = "实网冒烟:cargo test -p qaq-bx-core -- --ignored live_smoke"]
    fn live_list_smoke() {
        struct Anonymous;
        #[async_trait::async_trait]
        impl CredentialProvider for Anonymous {
            async fn get(&self) -> Result<crate::credentials::Credentials> {
                Err(BiliError::NotLoggedIn)
            }
            fn cached(&self) -> Option<crate::credentials::Credentials> {
                None
            }
        }
        let client = LiveClient::new(reqwest::Client::new(), Arc::new(Anonymous));
        let rooms = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { client.rooms(1).await });
        let rooms = rooms.expect("直播列表应 code=0");
        assert!(!rooms.is_empty(), "首页应有直播间");
        let first = rooms.first().unwrap().clone();
        assert!(first.roomid > 0);
        // 房间流直链:开播房间应能取到(v2 HLS 优先;风控/未开播回落 v1 FLV)。
        let url = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { client.play_url(first.roomid).await });
        let stream = url.expect("首房房间流应可解析");
        match stream {
            LiveStreamUrl::Hls(u) => assert!(u.contains(".m3u8"), "HLS 直链应为 m3u8:{u}"),
            LiveStreamUrl::Flv(u) => assert!(u.contains(".flv"), "FLV 直链应为 flv:{u}"),
        }
    }
}
