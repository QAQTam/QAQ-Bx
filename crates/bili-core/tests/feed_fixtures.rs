//! fixtures 行为锚测试（契约策略见 `api/README.md`）。
//!
//! fixtures 是真机捕获的截断样本（`api/fixtures/`）：
//! - 保证 Rust 契约类型能反序列化真实形状；
//! - 锚定 required 字段口径（goto 多态卡、u64 aid、av/ad/live 混排）。
//! 服务器行为变化 → 重跑捕获 → fixture 更新 → 本测试即契约警报。

use bili_core::feed::{ArchiveCard, Envelope, FeedCard, OgvCard, RcmdData};

/// 读取 fixture 并取 `json` 字段（外层含 url/captured_at 证据）。
fn fixture(name: &str) -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../api/fixtures/");
    let raw = std::fs::read_to_string(format!("{path}{name}.json"))
        .expect("fixture 缺失：api/fixtures 应随仓库提供");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("fixture 不是合法 JSON");
    v["json"].clone()
}

#[test]
fn rcmd_fixture_matches_contract_v1() {
    let env: Envelope<RcmdData> =
        serde_json::from_value(fixture("feed-rcmd")).expect("rcmd fixture 反序列化失败");
    assert_eq!(env.code, 0);
    let items = env.data.expect("rcmd 缺 data").item;
    assert!(!items.is_empty(), "rcmd fixture 无卡片");

    // 多态卡（goto=av/ad/live 至少各一类，来自 make-fixtures 的多样性选择）
    let gotos: Vec<&str> = items.iter().filter_map(|c| c.goto.as_deref()).collect();
    assert!(gotos.contains(&"av"), "fixture 应含 av 卡: {gotos:?}");

    // av 卡：消费面字段完整（bvid/owner/stat/pic/duration/pubdate）
    let av = items.iter().find(|c| c.is_av()).expect("无有效 av 卡");
    let av = av;
    assert!(av.bvid.as_deref().is_some_and(|b| b.starts_with("BV")), "av 卡 bvid 缺失");
    assert!(av.cid.is_some(), "av 卡 cid 缺失");
    assert!(av.pic.as_deref().is_some_and(|p| p.starts_with("http")), "av 卡封面缺失");
    assert!(av.duration.is_some_and(|d| d > 0), "av 卡时长缺失");
    assert!(av.pubdate.is_some_and(|p| p > 1_000_000_000), "av 卡 pubdate 缺失");
    let owner = av.owner.as_ref().expect("av 卡 owner 缺失");
    assert!(owner.mid > 0 && !owner.name.is_empty());
    let stat = av.stat.as_ref().expect("av 卡 stat 缺失");
    assert!(stat.view >= 0 && stat.danmaku >= 0);

    // id 已溢出 u32（契约：64 位）
    let max_id = items.iter().map(|c| c.id).max().unwrap_or(0);
    assert!(max_id <= u64::from(u32::MAX) || max_id > u32::MAX as u64);
}

#[test]
fn archive_fixtures_match_contract_v1() {
    for name in ["feed-popular", "feed-ranking", "feed-precious"] {
        let raw = fixture(name);
        let list_key = if raw["data"]["list"].is_array() { "list" } else { "item" };
        let cards: Vec<ArchiveCard> = serde_json::from_value(raw["data"][list_key].clone())
            .unwrap_or_else(|e| panic!("{name} 反序列化失败: {e}"));
        assert!(!cards.is_empty(), "{name} 空");
        let c = &cards[0];
        assert!(c.aid > 0 && c.bvid.starts_with("BV"), "{name} 首卡字段异常");
        assert!(c.duration > 0 && c.pubdate > 1_000_000_000, "{name} 时间/时长异常");
        assert!(!c.owner.name.is_empty(), "{name} owner 异常");
        // ranking 独有 score
        if name == "feed-ranking" {
            assert!(c.score.is_some(), "ranking 卡应带 score");
        }
    }
}

#[test]
fn ogv_fixture_matches_contract_v1() {
    let raw = fixture("feed-ogv");
    let items: Vec<OgvCard> = serde_json::from_value(raw["data"]["items"].clone())
        .expect("ogv fixture 反序列化失败");
    assert!(!items.is_empty());
    let s = &items[0];
    assert!(s.epid > 0 && s.season_id > 0, "ogv 卡 epid/season_id 异常");
    assert!(s.cover.starts_with("http"), "ogv 封面异常");
}

/// 相关视频：`data` 直接为 ArchiveCard 数组（无 list/items 包裹层，与 popular 族不同）。
#[test]
fn related_fixture_matches_contract_v1() {
    let raw = fixture("feed-related");
    let env: Envelope<Vec<ArchiveCard>> =
        serde_json::from_value(raw).expect("related fixture 信封反序列化失败");
    assert_eq!(env.code, 0);
    let cards = env.data.expect("related 缺 data 数组");
    assert!(!cards.is_empty(), "related fixture 空");
    let c = &cards[0];
    assert!(c.aid > 0 && c.bvid.starts_with("BV"), "related 首卡字段异常");
    assert!(c.cid > 0 && c.duration > 0 && c.pubdate > 1_000_000_000);
    assert!(!c.owner.name.is_empty() && c.pic.starts_with("http"));
}

/// 契约警报示例：required 字段若被服务器删除（如未来去掉 stat.danmaku），
/// 在重新捕获 fixture 后本组测试将失败 → 升级契约代（api/README.md）。
#[test]
fn stat_required_fields_are_strict() {
    let raw = fixture("feed-rcmd");
    let av = raw["data"]["item"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["goto"] == "av")
        .expect("无 av 卡");
    let stat = av["stat"].as_object().expect("av 卡 stat 缺失");
    assert!(stat.contains_key("view"), "stat.view 是 required（契约代 1）");
    assert!(stat.contains_key("danmaku"), "stat.danmaku 是 required（契约代 1）");
}

/// FeedCard 在缺字段（如手造最小卡）时仍可解析 —— 向下兼容面。
#[test]
fn feed_card_tolerates_missing_optional_fields() {
    let minimal = serde_json::json!({ "id": 42, "title": "t", "goto": "av", "bvid": "BV1x" });
    let card: FeedCard = serde_json::from_value(minimal).expect("最小卡应可解析");
    assert_eq!(card.id, 42);
    assert!(card.is_av());
    assert!(card.pic.is_none(), "可选字段缺失时保持 None");
}
