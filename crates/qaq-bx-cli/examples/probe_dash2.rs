//! 对指定 bvid 做 playurl「无 dash」形态定点复现:抓原始响应体。
//! 用法: cargo run --example probe_dash2 -- <bvid>
use std::{sync::Arc, time::Duration};

use qaq_bx_core::auth::AuthPlugin as _;
use qaq_bx_core::credentials::FileCredentialStore;
use qaq_bx_core::plugin::{PluginContext, PluginRegistry};
use qaq_bx_core::ticket::TicketManager;
use qaq_bx_core::wbi::WbiSigner;
use qaq_bx_plugin_login::{CREDENTIALS_FILE, QrAuthPlugin};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";
const VIEW_URL: &str = "https://api.bilibili.com/x/web-interface/view";
const PLAYURL_URL: &str = "https://api.bilibili.com/x/player/wbi/playurl";

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

/// 复刻 httpx::send_get 的头体系(UA/Referer/Cookie∪bili_ticket)。
async fn raw_get(
    http: &reqwest::Client,
    creds: &Arc<dyn qaq_bx_core::credentials::CredentialProvider>,
    ticket: Option<&TicketManager>,
    url: &str,
) -> serde_json::Value {
    let mut builder = http
        .get(url)
        .header("User-Agent", UA)
        .header("Referer", "https://www.bilibili.com/");
    let mut cookie = creds.get().await.ok().map(|c| c.cookie_header());
    if let Some(frag) = ticket.and_then(TicketManager::cookie_header_fragment) {
        match &mut cookie {
            Some(c) if !c.contains("bili_ticket=") => {
                if !c.is_empty() {
                    c.push_str("; ");
                }
                c.push_str(&frag);
            }
            _ => cookie = Some(frag),
        }
    }
    if let Some(c) = cookie {
        builder = builder.header("Cookie", c);
    }
    let body = builder
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({"parse_fail": body}))
}

/// 摘要 envelope:code/message + data 的关键形态字段。
fn summarize(env: &serde_json::Value, label: &str) {
    let code = env["code"].as_i64().unwrap_or(-999);
    let msg = env["message"].as_str().unwrap_or("");
    println!("── {label} → code={code} message={msg:?}");
    if code != 0 {
        return;
    }
    let data = &env["data"];
    match data {
        serde_json::Value::Null => println!("    data: null"),
        serde_json::Value::Object(map) => {
            let keys: Vec<&String> = map.keys().collect();
            println!("    data keys = {keys:?}");
            println!("    dash = {}", data["dash"]);
            let durl = &data["durl"];
            println!(
                "    durl 类型={} 内容首200字={}",
                durl.is_null(),
                serde_json::to_string(durl).unwrap_or_default().chars().take(200).collect::<String>()
            );
            println!("    quality={} accept_description={}", data["quality"], data["accept_description"]);
        }
        _ => println!("    data(其他形态) = {data}"),
    }
}

async fn async_main() {
    let bvid = std::env::args().nth(1).unwrap_or_else(|| "BV1zonn67E6W".into());
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
    let creds = auth.credentials();

    // nav/ticket/wbi(与 PlayerClient::with_nav 同构)。
    let nav = Arc::new(qaq_bx_core::account::NavClient::new(http.clone(), creds.clone()));
    let ticket = nav.ticket().cloned();
    let wbi = WbiSigner::with_nav(nav.clone());

    println!("══ view 原始形态({bvid}) ══");
    let view = raw_get(&http, &creds, ticket.as_deref(), &format!("{VIEW_URL}?bvid={bvid}")).await;
    let vcode = view["code"].as_i64().unwrap_or(-999);
    println!("code={vcode} message={:?}", view["message"].as_str().unwrap_or(""));
    if vcode == 0 {
        let data = &view["data"];
        println!("  title = {}", data["title"].as_str().unwrap_or("?"));
        println!("  cid(P1) = {}", data["cid"]);
        println!("  rights = {}", serde_json::to_string(&data["rights"]).unwrap_or_default());
        println!("  state = {} argue_info={}", data["state"], data["argue_info"]);
        let cid = data["cid"].as_u64().unwrap_or(0);

        println!("\n══ playurl(WBI, fnval=16) 原始形态 ══");
        let signed = wbi
            .sign_query(&[
                ("bvid", bvid.clone()),
                ("cid", cid.to_string()),
                ("qn", "0".into()),
                ("fnval", "16".into()),
                ("fnver", "0".into()),
            ])
            .await
            .unwrap();
        let play = raw_get(&http, &creds, ticket.as_deref(), &format!("{PLAYURL_URL}?{signed}")).await;
        summarize(&play, "playurl");

        // 存档完整响应供行为锚。
        let path = ".qaqh/research/samples/playurl-nodash.json";
        std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).ok();
        std::fs::write(path, serde_json::to_string_pretty(&play).unwrap()).ok();
        println!("\n原始响应已存档 → {path}");
    }
}
