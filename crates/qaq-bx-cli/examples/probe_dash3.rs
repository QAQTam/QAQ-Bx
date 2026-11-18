//! 付费/充电专稿卡片级信号探测:
//! 1) view 全字段对比(充电专稿 BV1zonn67E6W vs 普通视频)找可判定字段;
//! 2) rcmd 原始卡全字段(找 badge/rights 类信号)+ 是否混入充电专稿。
//! 用法: cargo run --example probe_dash3 -- [charging_bvid]
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::feed::{FeedKind, FeedSession};
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_plugin_login::{CREDENTIALS_FILE, QrAuthPlugin};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
const VIEW_URL: &str = "https://api.bilibili.com/x/web-interface/view";
const RCMD_URL: &str = "https://api.bilibili.com/x/web-interface/wbi/index/top/feed/rcmd";

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

async fn raw_view(http: &reqwest::Client, cookie: Option<&str>, bvid: &str) -> serde_json::Value {
    let mut b = http
        .get(format!("{VIEW_URL}?bvid={bvid}"))
        .header("User-Agent", UA)
        .header("Referer", "https://www.bilibili.com/");
    if let Some(c) = cookie {
        b = b.header("Cookie", c);
    }
    let body = b.send().await.unwrap().text().await.unwrap();
    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null)
}

/// 截断值的全字段打印(找可判定信号)。
fn dump_keys(v: &serde_json::Value, label: &str) {
    let Some(map) = v.as_object() else {
        println!("  {label}: (非对象)");
        return;
    };
    println!("  {label} 全字段:");
    for (k, val) in map {
        let s = serde_json::to_string(val).unwrap_or_default();
        let shown = if s.chars().count() > 90 {
            format!("{}…", s.chars().take(90).collect::<String>())
        } else {
            s
        };
        println!("    {k} = {shown}");
    }
}

async fn async_main() {
    let charging_bvid = std::env::args().nth(1).unwrap_or_else(|| "BV1zonn67E6W".into());
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
    let creds = auth.credentials();
    let cookie = creds.get().await.ok().map(|c| c.cookie_header());

    println!("══ 1) view 全字段对比 ══");
    let chg = raw_view(&http, cookie.as_deref(), &charging_bvid).await;
    println!("[充电专稿 {charging_bvid}] code={}", chg["code"]);
    dump_keys(&chg["data"], "充电专稿 data");

    let base = raw_view(&http, cookie.as_deref(), "BV1GJ411x7h7").await;
    println!("\n[普通视频 BV1GJ411x7h7] code={}", base["code"]);
    if base["code"].as_i64() == Some(0) {
        let keys_diff: Vec<&String> = chg["data"]
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        println!("  普通视频 data keys = {keys_diff:?}");
        println!("  普通视频 rights = {}", serde_json::to_string(&base["data"]["rights"]).unwrap_or_default());
        // 逐字段对比 rights(普通 vs 充电专稿)。
        println!("  ── rights 逐字段对比(充电专稿 vs 普通) ──");
        if let (Some(a), Some(b)) =
            (chg["data"]["rights"].as_object(), base["data"]["rights"].as_object())
        {
            for (k, av) in a {
                let bv = b.get(k);
                let mark = if bv == Some(av) { "" } else { "  ← 差异!" };
                println!("    {k}: {av} vs {}{}", bv.map(|v| v.to_string()).unwrap_or_else(|| "(缺)".into()), mark);
            }
        }
    }

    println!("\n══ 2) rcmd 原始卡信号 ══");
    let mut session = FeedSession::new();
    let mut found_charging = false;
    let mut dumped_one = false;
    for round in 0..3 {
        let query = session.plan_with_ps(FeedKind::Next, 30);
        let mut b = http
            .get(format!(
                "{RCMD_URL}?ps={}&fresh_type={}&fresh_idx={}&fresh_idx_1h={}&brush={}&fetch_row={}&y_num=3&device=win&homepage_ver=1&feed_version=V8&uniq_id={}&w_rid=x&wts=0",
                query.ps.unwrap_or(30),
                query.fresh_type.unwrap_or(4),
                query.fresh_idx.unwrap_or(0),
                query.fresh_idx_1h.unwrap_or(0),
                query.brush.unwrap_or(0),
                query.fetch_row.unwrap_or(0),
                query.uniq_id.unwrap_or_default(),
            ))
            .header("User-Agent", UA)
            .header("Referer", "https://www.bilibili.com/");
        if let Some(c) = cookie.as_deref() {
            b = b.header("Cookie", c);
        }
        // 无签名大概率 -403,失败即止(仅试探原始卡字段形态,拿不到就用 view 探针替代)。
        let Ok(resp) = b.send().await else { break };
        let Ok(body): Result<serde_json::Value, _> = resp.json().await else { break };
        let code = body["code"].as_i64().unwrap_or(-1);
        println!("  round {round}: code={code}");
        if code != 0 {
            println!("  message={:?}(无签名被拒,符合契约)", body["message"].as_str().unwrap_or(""));
            break;
        }
        let items = body["data"]["item"].as_array().cloned().unwrap_or_default();
        for card in &items {
            let bv = card["bvid"].as_str().unwrap_or("");
            if bv == charging_bvid {
                found_charging = true;
                println!("  ⚠ 充电专稿出现在 rcmd!原始卡:");
                dump_keys(card, "充电专稿卡");
            }
            if !dumped_one && card["goto"].as_str() == Some("av") && !bv.is_empty() {
                dumped_one = true;
                dump_keys(card, "普通 av 卡样本");
            }
        }
    }
    println!(
        "  rcmd 3 轮扫描:充电专稿出现={found_charging}(未出现不代表 rcmd 不含此类,仅本样本未命中)"
    );
}
