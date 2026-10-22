// SPDX-License-Identifier: GPL-3.0-or-later
//! Test helpers: a server per test (temporary data folder, the fake panel's exact
//! address allowed through the SSRF policy), a cookie-keeping API client, small HTTP
//! servers that play hostile hosts or victims, and a TCP proxy that counts open
//! connections (the "provider connection" a stream holds).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use axum::Router;
use opensmarters_core::Redactor;
use opensmarters_web::Server;
use opensmarters_web::config::Config;
use opensmarters_web::crypto::SecretKey;
use reqwest::Method;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};

pub const SETUP_CODE: &str = "TEST-SETUP-CODE-0001";
pub const ADMIN: &str = "admin";
pub const ADMIN_PASSWORD: &str = "correct horse battery";

pub fn init_tracing() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let level = match std::env::var("OPENSMARTERS_TEST_LOG").as_deref() {
            Ok("debug") => tracing::Level::DEBUG,
            Ok("info") => tracing::Level::INFO,
            _ => tracing::Level::WARN,
        };
        let _ = tracing_subscriber::fmt()
            .with_max_level(level)
            .with_test_writer()
            .try_init();
    });
}

pub struct TestApp {
    pub server: Server,
    pub dir: tempfile::TempDir,
    pub base: String,
}

impl TestApp {
    pub fn client(&self) -> Client {
        Client::new(&self.base)
    }

    pub async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

/// A server allowing exactly `allow` on loopback.
pub async fn app_with(allow: &[SocketAddr], tweak: impl FnOnce(&mut Config)) -> TestApp {
    init_tracing();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::new(dir.path().to_path_buf(), SecretKey::generate().unwrap());
    config.listen = "127.0.0.1:0".parse().unwrap();
    config.setup_code = Some(SETUP_CODE.to_owned());
    config.dev_loopback_allow = allow.to_vec();
    tweak(&mut config);
    let server = opensmarters_web::start(config, Redactor::new())
        .await
        .unwrap();
    let base = server.url();
    TestApp { server, dir, base }
}

pub async fn app(allow: &[SocketAddr]) -> TestApp {
    app_with(allow, |_| {}).await
}

/// The fake panel without generated media (fast).
pub async fn panel() -> fake_panel::Handle {
    fake_panel::spawn(fake_panel::Config::api_only())
        .await
        .unwrap()
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub json: Value,
    pub bytes: Vec<u8>,
}

impl Resp {
    pub fn code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }

    pub fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned()
    }

    #[track_caller]
    pub fn ok(self) -> Value {
        assert_eq!(
            self.status, 200,
            "unexpected answer: {} {}",
            self.status, self.json
        );
        self.json
    }
}

/// An API client that keeps the session cookie like a browser.
pub struct Client {
    http: reqwest::Client,
    base: String,
    pub cookie: Mutex<Option<String>>,
}

impl Client {
    pub fn new(base: &str) -> Self {
        Client {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(90))
                .build()
                .unwrap(),
            base: base.to_owned(),
            cookie: Mutex::new(None),
        }
    }

    pub fn session_token(&self) -> Option<String> {
        self.cookie.lock().unwrap().clone()
    }

    pub async fn raw(&self, method: Method, path: &str, body: Option<Value>, csrf: bool) -> Resp {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.base));
        if let Some(c) = self.cookie.lock().unwrap().clone() {
            req = req.header("Cookie", format!("opensmarters_session={c}"));
        }
        if csrf && method != Method::GET {
            req = req.header("X-Opensmarters", "1");
        }
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&b).unwrap());
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        for v in headers.get_all("set-cookie") {
            let s = v.to_str().unwrap();
            if let Some(rest) = s.strip_prefix("opensmarters_session=") {
                let value = rest.split(';').next().unwrap_or("");
                *self.cookie.lock().unwrap() = (!value.is_empty()).then(|| value.to_owned());
            }
        }
        let bytes = res.bytes().await.unwrap().to_vec();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Resp {
            status,
            headers,
            json,
            bytes,
        }
    }

    pub async fn get(&self, path: &str) -> Resp {
        self.raw(Method::GET, path, None, true).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Resp {
        self.raw(Method::POST, path, Some(body), true).await
    }

    pub async fn post_empty(&self, path: &str) -> Resp {
        self.raw(Method::POST, path, None, true).await
    }

    pub async fn put(&self, path: &str, body: Option<Value>) -> Resp {
        self.raw(Method::PUT, path, body, true).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Resp {
        self.raw(Method::PATCH, path, Some(body), true).await
    }

    pub async fn delete(&self, path: &str) -> Resp {
        self.raw(Method::DELETE, path, None, true).await
    }
}

/// Runs the first-run setup and returns the administrator's client.
pub async fn admin(app: &TestApp) -> Client {
    let c = app.client();
    c.post(
        "/api/setup",
        json!({"setup_code": SETUP_CODE, "name": ADMIN, "password": ADMIN_PASSWORD}),
    )
    .await
    .ok();
    c
}

/// Invites and signs in a second person.
pub async fn invite(app: &TestApp, admin: &Client, name: &str, password: &str) -> Client {
    let inv = admin.post("/api/admin/invites", json!({})).await.ok();
    let url = inv["url"].as_str().unwrap();
    let token = url.split_once("/invite#").unwrap().1;
    let c = app.client();
    c.post(
        "/api/invites/accept",
        json!({"token": token, "name": name, "password": password}),
    )
    .await
    .ok();
    c
}

/// Adds the fake panel's `demo` Xtream account; returns its id.
pub async fn add_xtream(c: &Client, panel: &fake_panel::Handle) -> String {
    let a = c
        .post(
            "/api/accounts",
            json!({"name": "Fake", "kind": "xtream", "server": panel.base_url(), "username": "demo", "password": "demo"}),
        )
        .await
        .ok();
    a["id"].as_str().unwrap().to_owned()
}

/// Serves `router` on 127.0.0.1 (a "host on the internet" in tests once allowed, or a
/// victim when not).
pub async fn serve(router: Router) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// A victim server counting every request it receives.
pub async fn victim() -> (SocketAddr, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    let router = Router::new().fallback(move || {
        let h = Arc::clone(&h);
        async move {
            h.fetch_add(1, Ordering::SeqCst);
            "internal secret"
        }
    });
    (serve(router).await, hits)
}

/// A port with nothing listening.
pub async fn closed_port() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    drop(l);
    a
}

/// A TCP proxy that counts its open connections.
pub struct Proxy {
    pub addr: SocketAddr,
    pub active: Arc<AtomicUsize>,
    pub total: Arc<AtomicUsize>,
}

struct Decrement(Arc<AtomicUsize>);
impl Drop for Decrement {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub async fn proxy(target: SocketAddr) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(0));
    let (a, t) = (Arc::clone(&active), Arc::clone(&total));
    tokio::spawn(async move {
        while let Ok((mut inbound, _)) = listener.accept().await {
            t.fetch_add(1, Ordering::SeqCst);
            a.fetch_add(1, Ordering::SeqCst);
            let guard = Decrement(Arc::clone(&a));
            tokio::spawn(async move {
                let _guard = guard;
                if let Ok(mut outbound) = TcpStream::connect(target).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    Proxy {
        addr,
        active,
        total,
    }
}

/// Polls `cond` for up to `secs` seconds.
pub async fn eventually(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cond()
}

/// Every file under `dir` as one byte vector (for "never stored in clear" checks).
pub fn all_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(b) = std::fs::read(&p) {
                out.extend_from_slice(&b);
            }
        }
    }
    out
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
