//! 弹幕客户端：dm/web/seg.so protobuf 分段拉取（WBI 签名）+ 手写 wire 解析。
//!
//! 契约锚 `docs/bilibili-api-inventory.md` §3：每段 6 分钟（360s），
//! `segment_index` 从 1 起；响应为 `DmSegMobileReply` protobuf（magic `0a…`
//! = len-delim field 1 = 弹幕元素数组）。不引入 prost——渲染只需要
//! `progress/mode/fontsize/color/content` 与屏蔽/豁免用的
//! `mid_hash/weight/pool`，手写 wire 解析（单测锚定字节级契约），
//! schema 差异由"未知字段跳过"兜底。
//!
//! 字段号以官方网页播放器内嵌 schema 为准（2026-11 抓取研究,
//! `docs/dev-notes/bilibili-danmaku-engine-research.md` §2.3）：weight=field 9
//! (varint)、dmid=field 12 (string)、pool=field 11 (varint)。
//!
//! 注:`dm/web/view` 元信息端点实测同样返回 protobuf（非 JSON,2024-11 实弹）,
//! 而分段数可由视频 duration 直接推导,故不请求该端点。

use std::sync::Arc;

use bytes::Bytes;

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
///
/// 零分配契约（#2）：文本不在此物化——`content_off/content_len` 指向所属
/// [`DanmakuSegment`] 原始 protobuf 缓冲,仅上屏/过滤命中时经
/// [`DanmakuSegment::content`] 解码；`mid_hash` 解析为 `Option<u32>`（8 位
/// hex,非法/缺失为 `None`,为 v0.7 弹幕屏蔽铺路）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DanmakuElem {
    /// 出现时刻（毫秒）。
    pub progress_ms: i32,
    /// 1=滚动 4=底部 5=顶部 7=高级（其余原样保留）。
    pub mode: i32,
    pub fontsize: i32,
    /// 文字颜色 0xRRGGBB（protobuf uint32；黑/白默认由 mode 端处理）。
    pub color: u32,
    /// content 在分段缓冲中的起始偏移。
    pub content_off: u32,
    /// content 字节长度（字段出现即视为有内容,空串保留与旧实现一致）。
    pub content_len: u32,
    /// 发送者 mid 的 CRC32（field 6,8 位 hex 串直解；非 hex/缺失 = None）。
    pub mid_hash: Option<u32>,
    /// 云屏蔽权重（field 9 varint;0 = 未下发。权重越低越疑似垃圾,
    /// 网页版智能屏蔽同源;官方 schema 佐证见研究文档 §2.3/§2.4）。
    pub weight: u32,
    /// 弹幕池（field 11 varint;0=普通 1=字幕 2=特殊/BAS;1 号池为官方
    /// 碰撞分配的「特权弹幕」豁免来源）。
    pub pool: u8,
}

/// 一个分段：原始 protobuf 缓冲（`Bytes`,网络层零拷贝直达）+ 元素偏移表。
/// 缓冲与元素共享同一引用计数,下游（引擎 overlay）按 `(off,len)` 取文本,
/// 被筛掉的弹幕全程零分配。
#[derive(Debug, Clone)]
pub struct DanmakuSegment {
    data: Bytes,
    elems: Vec<DanmakuElem>,
}

impl DanmakuSegment {
    /// 原始缓冲(下游构造零拷贝文本引用)。
    pub fn data(&self) -> &Bytes {
        &self.data
    }

    pub fn elems(&self) -> &[DanmakuElem] {
        &self.elems
    }

    pub fn len(&self) -> usize {
        self.elems.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elems.is_empty()
    }

    /// 取一条弹幕的文本(非法 UTF-8 按 lossy 替换,与旧解析行为一致)。
    pub fn content(&self, elem: &DanmakuElem) -> std::borrow::Cow<'_, str> {
        let start = (elem.content_off as usize).min(self.data.len());
        let end = start
            .saturating_add(elem.content_len as usize)
            .min(self.data.len());
        String::from_utf8_lossy(&self.data[start..end])
    }
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
    ) -> Result<DanmakuSegment> {
        let signed = self
            .wbi
            .sign_query(&[
                ("type", "1".into()),
                ("oid", cid.to_string()),
                ("pid", aid.to_string()),
                ("segment_index", segment_index.to_string()),
                // 实测(2026-11,probe_dm_pullmode,两视频交叉验证):pull_mode=1
                // 只让 segment_index=1 返回开头 ~1 分钟的弹幕(130/510 条),
                // 1 分钟后到段尾全空 → 播放开头「一窝蜂」且随后长期无弹幕。
                // pull_mode=0 与缺省均返回全段覆盖,取 0 显式锁定。
                ("pull_mode", "0".into()),
            ])
            .await?;
        let bytes = get_bytes_with_ticket(
            &self.http,
            &self.creds,
            self.ticket.as_deref(),
            &format!("{SEG_URL}?{signed}"),
        )
        .await?;
        parse_segment(bytes)
    }

    /// 全片弹幕（duration_secs 为视频总长;段间顺序拉取,单请求轻量无需并发）。
    /// 返回按 `segment_index` 升序的分段（段内 `progress_ms` 升序即时间序,
    /// 旧版跨段全量排序在段相对时间下无意义,去除）。
    pub async fn all(
        &self,
        cid: u64,
        aid: u64,
        duration_secs: u64,
    ) -> Result<Vec<DanmakuSegment>> {
        let segments = duration_secs.div_ceil((SEGMENT_MS / 1000) as u64).max(1) as u32;
        let mut batch = Vec::with_capacity(segments as usize);
        for index in 1..=segments {
            batch.push(self.segment(cid, aid, index).await?);
        }
        Ok(batch)
    }
}

// ---------- protobuf wire 解析（手写） ----------

/// 解析 `DmSegMobileReply`：顶层反复出现 len-delim field 1（弹幕元素），
/// 其余字段未知则跳过。
pub fn parse_segment(data: Bytes) -> Result<DanmakuSegment> {
    let mut elems = Vec::new();
    let mut pos = 0usize;
    let bytes = data.as_ref();
    while pos < bytes.len() {
        let (field, wire_type) = read_tag(bytes, &mut pos)?;
        match (field, wire_type) {
            (1, 2) => {
                let len = read_varint(bytes, &mut pos)? as usize;
                let end = pos.checked_add(len).ok_or_else(|| bad_wire("长度溢出"))?;
                if end > bytes.len() {
                    return Err(bad_wire("子消息越界"));
                }
                if let Some(elem) = parse_elem(bytes, pos, end) {
                    elems.push(elem);
                }
                pos = end;
            }
            _ => skip_field(bytes, &mut pos, wire_type)?,
        }
    }
    Ok(DanmakuSegment { data, elems })
}

/// 解析一条 `DanmakuElem`（2=progress 3=mode 4=fontsize 5=color 6=midHash
/// 7=content 9=weight 11=pool;12=dmid 字符串,只跳过不消费——历史上曾误把
/// field 12 当 weight 读,真实数据里它是 len-delim 字符串,(12,0) 永不命中,
/// 导致 weight 恒 0、智能屏蔽静默失效）。无 content 的记录丢弃。
/// 元素偏移为 `buf` 内绝对位置(子消息切片零拷贝,文本仅在
/// [`DanmakuSegment::content`] 调用时解码)。
fn parse_elem(buf: &[u8], start: usize, end: usize) -> Option<DanmakuElem> {
    let bytes = buf.get(start..end)?;
    let mut elem = DanmakuElem {
        progress_ms: 0,
        mode: 1,
        fontsize: 25,
        color: 0xFF_FF_FF,
        content_off: 0,
        content_len: 0,
        mid_hash: None,
        weight: 0,
        pool: 0,
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
                elem.mid_hash = parse_mid_hash(&bytes[pos..end]);
                pos = end;
            }
            (7, 2) => {
                let len = read_varint(bytes, &mut pos).ok()? as usize;
                let end = pos.checked_add(len)?;
                if end > bytes.len() {
                    return None;
                }
                elem.content_off = (start + pos) as u32;
                elem.content_len = len as u32;
                has_content = true;
                pos = end;
            }
            (9, 0) => elem.weight = read_varint(bytes, &mut pos).ok()? as u32,
            (11, 0) => elem.pool = read_varint(bytes, &mut pos).ok()? as u8,
            _ => skip_field(bytes, &mut pos, wire_type).ok()?,
        }
    }
    has_content.then_some(elem)
}

/// midHash 8 位十六进制 → `u32`(零分配;长度/字符非法回退 `None` = 未下发)。
fn parse_mid_hash(raw: &[u8]) -> Option<u32> {
    if raw.len() != 8 {
        return None;
    }
    let s = std::str::from_utf8(raw).ok()?;
    u32::from_str_radix(s, 16).ok()
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
        //              midHash:"abcd1234", content:"你好", weight:9, pool:1,
        //              dmid:"8f2c…" } —— 手工字节编码(字段号=官方网页 schema:
        // weight=9 varint / pool=11 varint / dmid=12 string)。
        let mut elem = Vec::new();
        elem.extend(enc_varint_field(2, 1234));
        elem.extend(enc_varint_field(3, 1));
        elem.extend(enc_varint_field(4, 25));
        elem.extend(enc_varint_field(5, 0xFF_FF_FF));
        elem.extend(enc_bytes_field(6, b"abcd1234"));
        elem.extend(enc_bytes_field(7, "你好".as_bytes()));
        elem.extend(enc_varint_field(9, 9));
        elem.extend(enc_varint_field(11, 1));
        elem.extend(enc_bytes_field(12, b"8f2c1d3e"));
        // DmSegMobileReply { elems: [elem] } + 一个未知字段(field 9 varint)。
        let mut reply = enc_bytes_field(1, &elem);
        reply.extend(enc_varint_field(9, 7));
        let seg = parse_segment(reply.into()).expect("解析失败");
        assert_eq!(seg.len(), 1);
        let e = &seg.elems()[0];
        assert_eq!(e.progress_ms, 1234);
        assert_eq!(e.mode, 1);
        assert_eq!(e.fontsize, 25);
        assert_eq!(e.color, 0xFF_FF_FF);
        assert_eq!(&*seg.content(e), "你好");
        assert_eq!(e.mid_hash, Some(0xabcd1234));
        assert_eq!(e.weight, 9);
        assert_eq!(e.pool, 1);
    }

    /// 官方 schema 交叉验证(抓取研究 §2.4):field 12 是 dmid 字符串(wire 2),
    /// 不是 weight。若解析器仍按 (12,0) 读权重,真实数据下 weight 恒 0、
    /// 智能屏蔽静默失效——此用例锚定修复。
    #[test]
    fn weight_comes_from_field9_not_field12() {
        let mut elem = enc_bytes_field(7, b"x");
        elem.extend(enc_varint_field(9, 7)); // weight=7(真实 wire:331 条全部如此)
        elem.extend(enc_bytes_field(12, b"dmid-string")); // dmid 字符串,须跳过
        let reply = enc_bytes_field(1, &elem);
        let seg = parse_segment(reply.into()).expect("解析失败");
        let e = &seg.elems()[0];
        assert_eq!(e.weight, 7, "weight 必须来自 field 9");
    }

    #[test]
    fn pool_defaults_to_zero_when_absent() {
        let mut elem = enc_bytes_field(7, "普通池".as_bytes());
        elem.extend(enc_varint_field(2, 100));
        let reply = enc_bytes_field(1, &elem);
        let seg = parse_segment(reply.into()).expect("解析失败");
        assert_eq!(seg.elems()[0].pool, 0);
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
        let seg = parse_segment(reply.into()).expect("解析失败");
        assert_eq!(seg.len(), 2);
        assert_eq!(seg.elems()[0].progress_ms, 5_000);
        assert_eq!(seg.elems()[1].progress_ms, 5_000);
        assert_eq!(&*seg.content(&seg.elems()[0]), "hi");
    }

    #[test]
    fn truncated_wire_is_error() {
        // tag 之后截断 → 错误而非 panic。
        assert!(parse_segment(Bytes::from(vec![0x0A, 0x10, 0x08])).is_err());
    }

    /// #2 验收:content 子消息在缓冲内截断(声明长度越界)→ 整条丢弃不 panic。
    #[test]
    fn elem_with_out_of_bounds_content_is_dropped() {
        let mut elem = enc_varint_field(2, 100);
        elem.extend(enc_tag(7, 2));
        elem.extend(enc_varint(9_999)); // 声明长度远超实际
        elem.extend(b"short");
        let reply = enc_bytes_field(1, &elem);
        let seg = parse_segment(reply.into()).expect("顶层合法");
        assert!(seg.is_empty(), "越界 content 的 elem 必须丢弃");
    }

    /// #2 验收:content 字段出现即为空串保留;非法 UTF-8 在取文本时 lossy 替换
    /// (与旧 `from_utf8_lossy` 解析期行为一致,仅推迟到访问时)。
    #[test]
    fn empty_and_invalid_utf8_content_survive_until_access() {
        let mut empty = enc_bytes_field(7, b"");
        empty.extend(enc_varint_field(2, 1));
        let mut broken = enc_bytes_field(7, &[0xE4, 0xBD, 0xA0, 0xFF]);
        broken.extend(enc_varint_field(2, 2));
        let mut reply = enc_bytes_field(1, &empty);
        reply.extend(enc_bytes_field(1, &broken));
        let seg = parse_segment(reply.into()).expect("零分配延迟解码不应在解析期失败");
        assert_eq!(seg.len(), 2);
        assert_eq!(&*seg.content(&seg.elems()[0]), "");
        assert_eq!(&*seg.content(&seg.elems()[1]), "\u{4F60}\u{FFFD}");
    }

    /// #2 验收:midHash 非 8 位 hex(短/长/非 hex)回退 None,不再存字符串。
    #[test]
    fn mid_hash_non_hex_falls_back_to_none() {
        for bad in [b"ab12".as_slice(), b"abcdefgh", b"abcd12345", b"12345-xy"] {
            let mut elem = enc_bytes_field(6, bad);
            elem.extend(enc_bytes_field(7, b"hi"));
            let reply = enc_bytes_field(1, &elem);
            let seg = parse_segment(reply.into()).unwrap();
            assert_eq!(seg.elems()[0].mid_hash, None, "非法 midHash {bad:?} 应为 None");
        }
        let mut elem = enc_bytes_field(6, b"ABCD1234");
        elem.extend(enc_bytes_field(7, b"hi"));
        let seg = parse_segment(enc_bytes_field(1, &elem).into()).unwrap();
        assert_eq!(seg.elems()[0].mid_hash, Some(0xabcd1234), "大小写 hex 均可解析");
    }
}
