//! 直播客户端（U4 接线；guest 无 Cookie 可用的稳定旧版端点）。
//!
//! 契约锚（2026-11-18 实测，无 Cookie code=0）：
//! - 列表：`GET https://api.live.bilibili.com/room/v1/Area/getRoomList?area_id=0&order_type=online&page=<n>&page_size=20`
//!   → `data[]`：`roomid/uid/title/uname/online/user_cover/system_cover/area_v2_name/area_v2_parent_name`。
//!   （新版 `xlive/.../second/getList` 无 buvid3 风控 -352，不可用。）
//! - 房间流：`GET https://api.live.bilibili.com/room/v1/Room/playUrl?cid=<roomid>&qn=0&platform=web`
//!   → `data.durl[]`（FLV/HLS 直链；mpv 可播）。

use std::sync::Arc;

use serde::Deserialize;

use crate::credentials::CredentialProvider;
use crate::error::{BiliError, Result};

const ROOM_LIST_URL: &str = "https://api.live.bilibili.com/room/v1/Area/getRoomList";
const PLAY_URL: &str = "https://api.live.bilibili.com/room/v1/Room/playUrl";

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
        let raw: Vec<serde_json::Value> =
            crate::httpx::get_json_with_ticket(&self.http, &self.creds, None, &url).await?;
        Ok(parse_live_rooms(&raw))
    }

    /// 房间 FLV/HLS 播放地址（取 `durl[0].url`；房间未开播返回 `InvalidData`）。
    pub async fn play_url(&self, roomid: u64) -> Result<String> {
        if roomid == 0 {
            return Err(BiliError::InvalidData("缺少房间号".into()));
        }
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

// Envelope 在 feed.rs 定义为 {code, message, data};live 端点 message 字段为
// `message`/`msg` 双形态,复用同一解包(code 判定一致)。

/// 宽松房间行(数字字段实测存在字符串形态;全字段缺省安全)。
#[derive(Deserialize)]
struct LooseRoom {
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

/// 宽松解析房间列表（单测与实网共用;`roomid/online` 双形态数字;封面
/// `user_cover` 优先,空则 `cover`;roomid 无效的行丢弃）。
pub(crate) fn parse_live_rooms(raw: &[serde_json::Value]) -> Vec<LiveRoom> {
    raw.iter()
        .filter_map(|v| {
            let room: LooseRoom = serde_json::from_value(v.clone()).ok()?;
            if room.roomid == 0 {
                return None;
            }
            let cover = if room.user_cover.trim().is_empty() {
                room.cover
            } else {
                room.user_cover
            };
            Some(LiveRoom {
                roomid: room.roomid,
                title: room.title,
                uname: room.uname,
                online: room.online,
                cover,
                area_name: room.area_v2_name,
                parent_name: room.area_v2_parent_name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rooms(json: &str) -> Vec<LiveRoom> {
        let raw: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        parse_live_rooms(&raw)
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
        // 房间流直链:开播房间应能取到 durl。
        let url = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { client.play_url(first.roomid).await });
        assert!(url.is_ok(), "首房房间流应可解析: {:?}", url.err());
    }
}
