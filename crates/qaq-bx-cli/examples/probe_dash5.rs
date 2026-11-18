//! 批量 view 对比:popular 全量卡 + 充电专稿样本,验证可判定字段组合。
//! 用法: cargo run --example probe_dash5
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_plugin_login::{CREDENTIALS_FILE, QrAuthPlugin};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
const VIEW_URL: &str = "https://api.bilibili.com/x/web-interface/view";
const POPULAR_URL: &str = "https://api.bilibili.com/x/web-interface/popular?ps=20&pn=";

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

async fn raw_view(
    http: &reqwest::Client,
    cookie: Option<&str>,
    bvid: &str,
) -> serde_json::Value {
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

/// (bvid, upower_exclusive, upower_preview, copyright, rights.ugc_pay, rights.free_watch, title前20)
fn row(v: &serde_json::Value) -> String {
    let d = &v["data"];
    format!(
        "{} excl={} prev={} ©{} ugc_pay={} free_watch={} | {}",
        d["bvid"].as_str().unwrap_or("?"),
        d["is_upower_exclusive"],
        d["is_upower_preview"],
        d["copyright"],
        d["rights"]["ugc_pay"],
        d["rights"]["free_watch"],
        d["title"].as_str().unwrap_or("?").chars().take(24).collect::<String>()
    )
}

async fn async_main() {
    let data_dir = qaq_bx_core::resolve_data_dir();
    let store = Arc::new(FileCredentialStore::new(data_dir.join(CREDENTIALS_FILE)));
    let auth = Arc::new(QrAuthPlugin::new(store));
    let mut registry = PluginRegistry::new();
    registry.register(auth.clone());
    let http = reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    registry
        .init_all(&PluginContext { http: http.clone(), data_dir })
        .await
        .unwrap();
    let cookie = auth.credentials().get().await.ok().map(|c| c.cookie_header());

    // 收集 popular 两页 + 充电专稿样本 + 普通样本。
    let mut targets: Vec<String> = vec![
        "BV1zonn67E6W".into(), // 充电专稿(基准)
        "BV1GJ411x7h7".into(), // 普通视频(基准)
    ];
    for pn in 1..=2 {
        let mut b = http
            .get(format!("{POPULAR_URL}{pn}"))
            .header("User-Agent", UA)
            .header("Referer", "https://www.bilibili.com/");
        if let Some(c) = cookie.as_deref() {
            b = b.header("Cookie", c);
        }
        let body = b.send().await.unwrap().text().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        for card in v["data"]["list"].as_array().cloned().unwrap_or_default() {
            if let Some(bv) = card["bvid"].as_str() {
                targets.push(bv.to_string());
            }
        }
    }
    targets.dedup();

    println!("bvid | is_upower_exclusive | is_upower_preview | copyright | ugc_pay | free_watch | title");
    let mut excl_count = 0;
    for bv in &targets {
        let v = raw_view(&http, cookie.as_deref(), bv).await;
        if v["code"].as_i64() != Some(0) {
            println!("{bv} → view code={} (受限/失效?)", v["code"]);
            continue;
        }
        let line = row(&v);
        if v["data"]["is_upower_exclusive"].as_bool() == Some(true) {
            excl_count += 1;
            println!("⚠ {line}");
        } else {
            println!("  {line}");
        }
    }
    println!("\npopular 40 卡 + 2 基准中充电专稿(is_upower_exclusive)计数: {excl_count}");
}
