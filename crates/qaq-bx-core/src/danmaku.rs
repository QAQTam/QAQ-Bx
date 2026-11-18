//! 弹幕客户端：dm/web/seg.so protobuf 分段拉取（WBI 签名）+ 手写 wire 解析。
//!
//! 契约锚 `docs/bilibili-api-inventory.md` §3：每段 6 分钟（360s），
//! `segment_index` 从 1 起；响应为 `DmSegMobileReply` protobuf（magic `0a…`
//! = len-delim field 1 = 弹幕元素数组）。不引入 prost——渲染只需要
//! `progress/mode/fontsize/color/content` 五个字段，手写 wire 解析
//! （单测锚定字节级契约），schema 差异由"未知字段跳过"兜底。
//!
//! 注:`dm/web/view` 元信息端点实测同样返回 protobuf（非 JSON,2024-11 实弹）,
//! 而分段数可由视频 duration 直接推导,故不请求该端点。

use std::sync::Arc;

use crate::account::NavClient;
use crate::credentials::CredentialProvider;
use crate::error::{BiliError, Result};
use crate::httpx::get_bytes_with_ticket;
use crate::ticket::TicketManager;
use crate::wbi::WbiSigner;

const SEG_URL: &str = "https://api.bilibili.com/x/v2/dm/wbi/web/seg.so";

/// 每段时长（毫秒；B 站 6 分钟/段实测）。
pub const SEGMENT_MS: u64 = 360_000;

/// 一条弹幕（渲染/过滤所需的子集；其余字段跳过）。
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuElem {
    /// 出现时刻（毫秒）。
    pub progress_ms: i32,
    /// 1=滚动 4=底部 5=顶部 7=高级（其余原样保留）。
    pub mode: i32,
    pub fontsize: i32,
    /// 文字颜色 0xRRGGBB（protobuf uint32；黑/白默认由 mode 端处理）。
    pub color: u32,
    pub content: String,
    /// 发送者 mid 的 CRC32 十六进制串（field 6；网页版用户屏蔽同源标识）。
    pub mid_hash: String,
    /// 云屏蔽权重（field 12;0 = 未下发。权重越低越疑似垃圾,网页版智能屏蔽同源）。
    pub weight: u32,
}

/// 弹幕客户端（WBI 签名复用凭据）。
pub struct DanmakuClient {
    http: reqwest::Client,
    creds: Arc<dyn CredentialProvider>,
    ticket: Option<Arc<TicketManager>>,
    wbi: WbiSigner,
}

impl DanmakuClient {
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

    /// 拉取一段弹幕并解析（segment_index 从 1 起）。
    pub async fn segment(
        &self,
        cid: u64,
        aid: u64,
        segment_index: u32,
    ) -> Result<Vec<DanmakuElem>> {
        let signed = self
            .wbi
            .sign_query(&[
                ("type", "1".into()),
                ("oid", cid.to_string()),
                ("pid", aid.to_string()),
                ("segment_index", segment_index.to_string()),
                ("pull_mode", "1".into()),
            ])
            .await?;
        let bytes = get_bytes_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &format!("{SEG_URL}?{signed}"),
        )
        .await?;
        parse_segment(&bytes)
    }

    /// 全片弹幕（duration_secs 为视频总长;段间顺序拉取,单请求轻量无需并发）。
    pub async fn all(&self, cid: u64, aid: u64, duration_secs: u64) -> Result<Vec<DanmakuElem>> {
        let segments = duration_secs.div_ceil((SEGMENT_MS / 1000) as u64).max(1) as u32;
        let mut batch = Vec::new();
        for index in 1..=segments {
            batch.extend(self.segment(cid, aid, index).await?);
        }
        // 出现时刻排序（渲染扫描用）。
        batch.sort_by_key(|e| e.progress_ms);
        Ok(batch)
    }
}

// ---------- protobuf wire 解析（手写） ----------

/// 解析 `DmSegMobileReply`：顶层反复出现 len-delim field 1（弹幕元素），
/// 其余字段未知则跳过。
pub fn parse_segment(bytes: &[u8]) -> Result<Vec<DanmakuElem>> {
    let mut elems = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let (field, wire_type) = read_tag(bytes, &mut pos)?;
        match (field, wire_type) {
            (1, 2) => {
                let len = read_varint(bytes, &mut pos)? as usize;
                let end = pos.checked_add(len).ok_or_else(|| bad_wire("长度溢出"))?;
                if end > bytes.len() {
                    return Err(bad_wire("子消息越界"));
                }
                if let Some(elem) = parse_elem(&bytes[pos..end]) {
                    elems.push(elem);
                }
                pos = end;
            }
            _ => skip_field(bytes, &mut pos, wire_type)?,
        }
    }
    Ok(elems)
}

/// 解析一条 `DanmakuElem`（2=progress 3=mode 4=fontsize 5=color 6=midHash
/// 7=content 12=weight;无 content 的记录丢弃）。
fn parse_elem(bytes: &[u8]) -> Option<DanmakuElem> {
    let mut elem = DanmakuElem {
        progress_ms: 0,
        mode: 1,
        fontsize: 25,
        color: 0xFF_FF_FF,
        content: String::new(),
        mid_hash: String::new(),
        weight: 0,
    };
    let mut has_content = false;
    let mut pos = 0usize;
    while pos < bytes.len() {
        let (field, wire_type) = read_tag(bytes, &mut pos).ok()?;
        match (field, wire_type) {
            (2, 0) => elem.progress_ms = read_varint(bytes, &mut pos).ok()? as i32,
            (3, 0) => elem.mode = read_varint(bytes, &mut pos).ok()? as i32,
            (4, 0) => elem.fontsize = read_varint(bytes, &mut pos).ok()? as i32,
            (5, 0) => elem.color = read_varint(bytes, &mut pos).ok()? as u32,
            (6, 2) => {
                let len = read_varint(bytes, &mut pos).ok()? as usize;
                let end = pos.checked_add(len)?;
                if end > bytes.len() {
                    return None;
                }
                elem.mid_hash = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
                pos = end;
            }
            (7, 2) => {
                let len = read_varint(bytes, &mut pos).ok()? as usize;
                let end = pos.checked_add(len)?;
                if end > bytes.len() {
                    return None;
                }
                elem.content = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
                has_content = true;
                pos = end;
            }
            (12, 0) => elem.weight = read_varint(bytes, &mut pos).ok()? as u32,
            _ => skip_field(bytes, &mut pos, wire_type).ok()?,
        }
    }
    has_content.then_some(elem)
}

fn bad_wire(msg: &str) -> BiliError {
    BiliError::InvalidData(format!("弹幕 protobuf 解析失败: {msg}"))
}

/// 读 tag varint → (field_number, wire_type)。
fn read_tag(bytes: &[u8], pos: &mut usize) -> Result<(u32, u8)> {
    let tag = read_varint(bytes, pos)?;
    if tag == 0 {
        return Err(bad_wire("tag 为 0"));
    }
    Ok(((tag >> 3) as u32, (tag & 7) as u8))
}

/// 读 varint（每字节低 7 位，最高位续传;上限 10 字节）。
fn read_varint(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for i in 0..10 {
        let Some(&b) = bytes.get(*pos) else {
            return Err(bad_wire("varint 截断"));
        };
        *pos += 1;
        value |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(bad_wire("varint 过长"))
}

/// 跳过一个未知字段（按 wire type 定长/长度前缀）。
fn skip_field(bytes: &[u8], pos: &mut usize, wire_type: u8) -> Result<()> {
    match wire_type {
        0 => {
            read_varint(bytes, pos)?;
        }
        1 => {
            *pos = pos.checked_add(8).ok_or_else(|| bad_wire("fixed64 越界"))?;
            if *pos > bytes.len() {
                return Err(bad_wire("fixed64 越界"));
            }
        }
        2 => {
            let len = read_varint(bytes, pos)? as usize;
            let end = pos.checked_add(len).ok_or_else(|| bad_wire("长度溢出"))?;
            if end > bytes.len() {
                return Err(bad_wire("len-delim 越界"));
            }
            *pos = end;
        }
        5 => {
            *pos = pos.checked_add(4).ok_or_else(|| bad_wire("fixed32 越界"))?;
            if *pos > bytes.len() {
                return Err(bad_wire("fixed32 越界"));
            }
        }
        other => return Err(bad_wire(&format!("未知 wire type {other}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手工编码 varint（测试助手,与实现分离以锚定字节格式）。
    fn enc_varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out
    }

    fn enc_tag(field: u32, wire_type: u8) -> Vec<u8> {
        enc_varint((u64::from(field) << 3) | u64::from(wire_type))
    }

    fn enc_bytes_field(field: u32, data: &[u8]) -> Vec<u8> {
        let mut out = enc_tag(field, 2);
        out.extend(enc_varint(data.len() as u64));
        out.extend_from_slice(data);
        out
    }

    fn enc_varint_field(field: u32, v: u64) -> Vec<u8> {
        let mut out = enc_tag(field, 0);
        out.extend(enc_varint(v));
        out
    }

    #[test]
    fn parses_minimal_segment() {
        // DanmakuElem { progress:1234, mode:1, fontsize:25, color:0xFFFFFF,
        //              midHash:"ab12", content:"你好", weight:9 } —— 手工字节编码。
        let mut elem = Vec::new();
        elem.extend(enc_varint_field(2, 1234));
        elem.extend(enc_varint_field(3, 1));
        elem.extend(enc_varint_field(4, 25));
        elem.extend(enc_varint_field(5, 0xFF_FF_FF));
        elem.extend(enc_bytes_field(6, b"ab12"));
        elem.extend(enc_bytes_field(7, "你好".as_bytes()));
        elem.extend(enc_varint_field(12, 9));
        // DmSegMobileReply { elems: [elem] } + 一个未知字段(field 9 varint)。
        let mut reply = enc_bytes_field(1, &elem);
        reply.extend(enc_varint_field(9, 7));
        let parsed = parse_segment(&reply).expect("解析失败");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].progress_ms, 1234);
        assert_eq!(parsed[0].mode, 1);
        assert_eq!(parsed[0].fontsize, 25);
        assert_eq!(parsed[0].color, 0xFF_FF_FF);
        assert_eq!(parsed[0].content, "你好");
        assert_eq!(parsed[0].mid_hash, "ab12");
        assert_eq!(parsed[0].weight, 9);
    }

    #[test]
    fn skips_unknown_and_drops_empty() {
        // 无 content 的 elem → 丢弃;两个正常 elem → 保留并按序。
        let mut with_content = enc_varint_field(2, 5_000);
        with_content.extend(enc_bytes_field(7, b"hi"));
        let no_content = enc_varint_field(2, 6_000);
        let mut reply = enc_bytes_field(1, &with_content);
        reply.extend(enc_bytes_field(1, &no_content));
        reply.extend(enc_bytes_field(1, &with_content));
        let parsed = parse_segment(&reply).expect("解析失败");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].progress_ms, 5_000);
        assert_eq!(parsed[1].progress_ms, 5_000);
    }

    #[test]
    fn truncated_wire_is_error() {
        // tag 之后截断 → 错误而非 panic。
        assert!(parse_segment(&[0x0A, 0x10, 0x08]).is_err());
    }
}
