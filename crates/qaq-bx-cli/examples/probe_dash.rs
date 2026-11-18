//! playurl「code=0 无 dash」根因探测：
//! A) 正确 cid 基线；B) 人为 bvid+错误 cid（模拟快路径卡片 cid 过期/错位）；
//! C) 真实推荐流卡片逐条走 resolve_with_cid 同款请求（在野扫描 card.cid 与 view P1 cid 漂移）；
//! D) 多 P 视频逐页 cid。
//!
//! 用法: cargo run --example probe_dash -- <bvid>（缺省 BV1GJ411x7h7）
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::feed::{FeedClient, FeedKind, FeedSession, RcmdQuery};
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_core::player::PlayerClient;
use qaq_bx_plugin_login::{CREDENTIALS_FILE, QrAuthPlugin};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

fn build_http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}

/// 探测一次 playurl,统一打印 dash 有无与错误形态。
async fn probe(client: &PlayerClient, tag: &str, bvid: &str, cid: u64) {
    match client.playurl(bvid, cid).await {
        Ok(p) => println!(
            "  [{tag}] bvid={bvid} cid={cid} → code=0 dash={} quality={} accept={}",
            p.dash.is_some(),
            p.quality,
            p.accept_description.len()
        ),
        Err(e) => println!("  [{tag}] bvid={bvid} cid={cid} → ERR {e}"),
    }
}

async fn async_main() {
    let bvid = std::env::args().nth(1).unwrap_or_else(|| "BV1GJ411x7h7".into());
    let data_dir = qaq_bx_core::resolve_data_dir();
    let store = Arc::new(FileCredentialStore::new(data_dir.join(CREDENTIALS_FILE)));
    let auth = Arc::new(QrAuthPlugin::new(store));
    let mut registry = PluginRegistry::new();
    registry.register(auth.clone());
    let http = build_http();
    registry
        .init_all(&PluginContext { http: http.clone(), data_dir })
        .await
        .unwrap();
    let client = PlayerClient::new(http.clone(), auth.credentials());
    let feed = FeedClient::new(http.clone(), auth.credentials());

    println!("══ A) 基线:正确 cid ══");
    let meta = match client.video_meta(&bvid).await {
        Ok(m) => m,
        Err(e) => {
            println!("  view 失败: {e}");
            return;
        }
    };
    println!(
        "  view: {} (aid={}) cid(P1)={} pages={}",
        meta.title,
        meta.aid,
        meta.cid,
        meta.pages.len().max(1)
    );
    probe(&client, "A 正确cid", &bvid, meta.cid).await;

    println!("══ B) 人为错误 cid(模拟快路径卡片 cid 过期/错位) ══");
    let bogus = meta.cid + 1;
    probe(&client, "B 错误cid+1", &bvid, bogus).await;
    probe(&client, "B 错误cid+9999", &bvid, meta.cid + 9999).await;

    println!("══ D) 多 P 逐页(若基线视频是多 P) ══");
    for page in meta.pages.iter().skip(1).take(3) {
        probe(&client, "D 非P1页", &bvid, page.cid).await;
    }

    println!("══ C) 真实推荐流快路径在野扫描(card.cid vs view P1 cid) ══");
    let mut session = FeedSession::new();
    let _ = session.plan(FeedKind::First);
    let query = RcmdQuery {
        ps: Some(30),
        fresh_idx: Some(1),
        fresh_idx_1h: Some(1),
        ..Default::default()
    };
    let cards = match feed.rcmd(&query).await {
        Ok(c) => c,
        Err(e) => {
            println!("  rcmd 失败: {e}");
            return;
        }
    };
    let av: Vec<_> = cards.iter().filter(|c| c.is_av()).take(20).collect();
    println!("  rcmd 卡 {} 条(av {})", cards.len(), av.len());
    let mut drift = 0;
    let mut multi_p = 0;
    for card in &av {
        let Some(bv) = card.bvid.as_deref() else { continue };
        let card_cid = card.cid.unwrap_or(0);
        let Ok(m) = client.video_meta(bv).await else {
            println!("  ▶ {bv} view 失败(稿件不可见/充电专稿?)");
            continue;
        };
        let mismatch = card_cid != 0 && card_cid != m.cid;
        if mismatch {
            drift += 1;
        }
        if m.pages.len() > 1 {
            multi_p += 1;
        }
        println!(
            "  ▶ {bv} card_cid={card_cid} view_P1_cid={} pages={} {}",
            m.cid,
            m.pages.len(),
            if mismatch { "⚠ 漂移!" } else { "" }
        );
        // 快路径同款请求:卡片 cid 直打 playurl。
        if mismatch {
            probe(&client, "C 卡片cid快路径", bv, card_cid).await;
            probe(&client, "C view_cid对照", bv, m.cid).await;
        } else if card_cid == 0 {
            // UI 中 cid=0 会退回 view 全路径,略过。
        }
        // 多 P 视频抽查 P2(快路径在播放页切 P 用非 P1 cid)。
        if let Some(p2) = m.pages.get(1) {
            probe(&client, "D P2页", bv, p2.cid).await;
        }
    }
    println!(
        "  扫描汇总:漂移 {} / 多P {} / 样本 {}",
        drift,
        multi_p,
        av.len()
    );
}
