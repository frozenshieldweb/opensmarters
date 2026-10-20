//! Shared helpers for fake-panel integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use fake_panel::{Config, Handle, Quirks};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("reqwest client")
}

/// Percent-encode everything except ASCII alphanumerics.
pub fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

pub async fn spawn_api(quirks: &str) -> Handle {
    let q: Quirks = quirks.parse().expect("quirks");
    fake_panel::spawn(Config::api_only().with_quirks(q))
        .await
        .expect("spawn")
}

pub async fn get(url: &str) -> reqwest::Response {
    client().get(url).send().await.expect("request")
}

pub async fn get_json(url: &str) -> Value {
    let text = get(url).await.text().await.expect("body");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("bad JSON from {url}: {e}: {text:.300}"))
}

pub fn api_url(h: &Handle, user: &str, pass: &str, extra: &str) -> String {
    format!(
        "{}/player_api.php?username={}&password={}{extra}",
        h.base_url(),
        enc(user),
        enc(pass)
    )
}

pub async fn api(h: &Handle, extra: &str) -> Value {
    get_json(&api_url(h, "demo", "demo", extra)).await
}

/// Numeric value that may be a JSON number or a numeric string.
pub fn num(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
