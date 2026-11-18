//! popular/related 原始卡字段探测:找卡片级付费/充电专稿标志。
//! 用法: cargo run --example probe_dash4 -- [aid_for_related]
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_plugin_login::{CREDENTIALS_FILE, QrAuthPlugin};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
const POPULAR_URL: &str = "https://api.bilibili.com/x/web-interface/popular?ps=20&pn=";
const RELATED_URL: &str = "https://api.bilibili.com/x/web-interface/archive/related?aid=";

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

async fn raw_get(http: &reqwest::Client, cookie: Option<&str>, url: &str) -> serde_json::Value {
    let mut b = http
        .get(url)
        .header("User-Agent", UA)
        .header("Referer", "https://www.bilibili.com/");
    if let Some(c) = cookie {
        b = b.header("Cookie", c);
    }
    let body = b.send().await.unwrap().text().await.unwrap();
    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null)
}

fn dump_card(card: &serde_json::Value, label: &str) {
    println!("  {label} 全字段:");
    if let Some(map) = card.as_object() {
        for (k, val) in map {
            let s = serde_json::to_string(val).unwrap_or_default();
            let shown = if s.chars().count() > 80 {
                format!("{}…", s.chars().take(80).collect::<String>())
            } else {
                s
            };
            println!("    {k} = {shown}");
        }
    }
}

/// 扫描数组里所有卡的字符串化形态,找 upower/pay/badge 痕迹。
fn scan_signals(items: &[serde_json::Value], label: &str) {
    let mut hits = std::collections::BTreeMap::new();
    for card in items {
        let s = serde_json::to_string(card).unwrap_or_default();
        for kw in ["upower", "ugc_pay", "is_paid", "badge", "充电", "付费"] {
            if s.contains(kw) {
                let bv = card["bvid"].as_str().unwrap_or("?").to_string();
                hits.entry(kw).or_insert_with(Vec::new).push(bv);
            }
        }
    }
    println!(
        "  {label} 信号扫描: {} 张卡 → {}",
        items.len(),
        if hits.is_empty() {
            "无 upower/pay/badge 痕迹".into()
        } else {
            format!("{hits:?}")
        }
    );
}

async fn async_main() {
    let aid = std::env::args().nth(1).unwrap_or_else(|| "80433022".into());
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
    let cookie = auth.credentials().get().await.ok().map(|c| c.cookie_header());

    println!("══ popular 原始卡(2 页) ══");
    for pn in 1..=2 {
        let v = raw_get(&http, cookie.as_deref(), &format!("{POPULAR_URL}{pn}")).await;
        let list = v["data"]["list"].as_array().cloned().unwrap_or_default();
        scan_signals(&list, &format!("popular pn={pn}"));
        if pn == 1 && let Some(c) = list.first() {
            dump_card(c, "popular 卡样本");
        }
    }

    println!("\n══ related 原始卡(aid={aid}) ══");
    let v = raw_get(&http, cookie.as_deref(), &format!("{RELATED_URL}{aid}")).await;
    let list = v["data"].as_array().cloned().unwrap_or_default();
    scan_signals(&list, "related");
    if let Some(c) = list.first() {
        dump_card(c, "related 卡样本");
    }
}
