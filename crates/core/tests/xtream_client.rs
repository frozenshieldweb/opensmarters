// SPDX-License-Identifier: GPL-3.0-or-later
//! Xtream client against a mock HTTP server (synthetic data only).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::time::Duration;

use opensmarters_core::epg::ServerTimeZone;
use opensmarters_core::model::archive_programmes;
use opensmarters_core::net::HttpClient;
use opensmarters_core::secrecy::SecretString;
use opensmarters_core::urls::{LiveContainer, TimeshiftFormat};
use opensmarters_core::xtream::Client;
use opensmarters_core::{
    AccountStatus, AuthFailure, CategoryId, Error, HttpConfig, IPTV_SMARTERS_UA, Redactor,
    XtreamAccount, m3u, xmltv,
};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A password with every character that breaks naive URL building.
const PASSWORD: &str = "p@ss w/rd&x=1#?%é";
const USERNAME: &str = "demo user";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn client_with(server: &MockServer, config: &HttpConfig) -> Client {
    let account = XtreamAccount::new(&server.uri(), USERNAME).unwrap();
    Client::new(account, SecretString::from(PASSWORD.to_owned()), config).unwrap()
}

fn client(server: &MockServer) -> Client {
    client_with(server, &HttpConfig::default())
}

fn api() -> wiremock::MockBuilder {
    Mock::given(method("GET"))
        .and(path("/player_api.php"))
        .and(query_param("username", USERNAME))
        .and(query_param("password", PASSWORD))
}

fn json_as_html(body: Vec<u8>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=UTF-8")
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn assert_no_secrets(text: &str) {
    for needle in [
        "p@ss",
        "w/rd",
        "p%40ss",
        "demo user",
        "demo+user",
        "demo%20user",
    ] {
        assert!(!text.contains(needle), "{needle:?} leaked in {text:?}");
    }
}

#[tokio::test]
async fn login_accepts_json_served_as_text_html() {
    let server = MockServer::start().await;
    api()
        .and(query_param_is_missing("action"))
        .and(header("user-agent", opensmarters_core::DEFAULT_USER_AGENT))
        .respond_with(json_as_html(fixture("xtream/login_active.json")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    assert_eq!(c.server_time_zone(), ServerTimeZone::Utc);
    let login = c.login().await.unwrap();
    assert_eq!(login.user_info.status, Some(AccountStatus::Active));
    assert_eq!(login.user_info.max_connections, Some(2));
    assert_eq!(
        c.server_time_zone(),
        ServerTimeZone::from_name("Europe/Paris").unwrap()
    );
    // Stream URLs come from server_info, not from the mock's host.
    let urls = c.stream_urls(&login);
    assert_eq!(
        urls.live(101, LiveContainer::Ts).as_str(),
        "http://stream.example.net:8080/live/demo%20user/p%40ss%20w%2Frd%26x%3D1%23%3F%25%C3%A9/101.ts"
    );
}

#[tokio::test]
async fn login_rejects_invalid_credentials() {
    let server = MockServer::start().await;
    api()
        .respond_with(json_as_html(fixture("xtream/login_auth_failed.json")))
        .mount(&server)
        .await;
    let err = client(&server).login().await.unwrap_err();
    assert_eq!(
        err,
        Error::Auth {
            status: AuthFailure::InvalidCredentials
        }
    );
    assert_eq!(
        err.to_string(),
        "authentication failed: invalid username or password"
    );
}

#[tokio::test]
async fn login_reports_expired_status_but_account_info_still_works() {
    let server = MockServer::start().await;
    api()
        .respond_with(json_as_html(fixture("xtream/login_expired.json")))
        .mount(&server)
        .await;
    let c = client(&server);
    let err = c.login().await.unwrap_err();
    assert_eq!(
        err,
        Error::Auth {
            status: AuthFailure::Status(AccountStatus::Expired)
        }
    );
    assert_eq!(
        err.to_string(),
        "authentication failed: account status is Expired"
    );
    let info = c.account_info().await.unwrap();
    assert_eq!(info.user_info.status, Some(AccountStatus::Expired));
}

#[tokio::test]
async fn login_checks_expiry_against_server_clock() {
    let server = MockServer::start().await;
    // Status says Active, but exp_date is before the server's timestamp_now.
    let body = br#"{"user_info":{"auth":1,"status":"Active","exp_date":"1700000000"},
                   "server_info":{"timestamp_now":1704067200,"url":"","port":""}}"#;
    api()
        .respond_with(json_as_html(body.to_vec()))
        .mount(&server)
        .await;
    let err = client(&server).login().await.unwrap_err();
    match err {
        Error::Auth {
            status: AuthFailure::Expired { exp_date },
        } => {
            assert_eq!(exp_date.timestamp(), 1_700_000_000);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn http_errors_are_typed() {
    let server = MockServer::start().await;
    api()
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let err = client(&server).live_categories().await.unwrap_err();
    assert_eq!(err, Error::Http { status: 500 });
    assert_eq!(err.to_string(), "the server returned HTTP status 500");
}

#[tokio::test]
async fn html_error_page_is_a_parse_error_without_body_text() {
    let server = MockServer::start().await;
    api()
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body>Secret internal error p@ss</body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;
    let err = client(&server).login().await.unwrap_err();
    assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    assert!(!err.to_string().contains("internal error"));
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let server = MockServer::start().await;
    let big = format!(
        "[{}]",
        vec![r#"{"stream_id":1,"name":"x"}"#; 1000].join(",")
    );
    api()
        .and(query_param("action", "get_live_streams"))
        .respond_with(json_as_html(big.clone().into_bytes()))
        .mount(&server)
        .await;
    let config = HttpConfig {
        max_response_bytes: 1024,
        ..HttpConfig::default()
    };
    let err = client_with(&server, &config)
        .live_streams(None)
        .await
        .unwrap_err();
    assert_eq!(err, Error::TooLarge { limit: 1024 });
    // Within the default cap the same body is fine.
    assert_eq!(
        client(&server).live_streams(None).await.unwrap().len(),
        1000
    );
}

#[tokio::test]
async fn gzip_bodies_are_decoded_and_capped_after_decompression() {
    let server = MockServer::start().await;
    let big = format!(
        "[{}]",
        vec![r#"{"stream_id":1,"name":"xxxxxxxxxxxxxxxx"}"#; 5000].join(",")
    );
    let compressed = gzip(big.as_bytes());
    assert!(compressed.len() < 4096, "{}", compressed.len());
    api()
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-encoding", "gzip")
                .set_body_raw(compressed, "application/json"),
        )
        .mount(&server)
        .await;
    // Small on the wire, large once inflated: the cap applies to the inflated size.
    let config = HttpConfig {
        max_response_bytes: 64 * 1024,
        ..HttpConfig::default()
    };
    let err = client_with(&server, &config)
        .live_streams(None)
        .await
        .unwrap_err();
    assert_eq!(err, Error::TooLarge { limit: 64 * 1024 });
    let ok = client(&server).live_streams(None).await.unwrap();
    assert_eq!(ok.len(), 5000);
}

#[tokio::test]
async fn overall_timeout() {
    let server = MockServer::start().await;
    api()
        .respond_with(json_as_html(b"[]".to_vec()).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let config = HttpConfig {
        request_timeout: Duration::from_millis(300),
        ..HttpConfig::default()
    };
    let started = std::time::Instant::now();
    let err = client_with(&server, &config)
        .live_categories()
        .await
        .unwrap_err();
    assert_eq!(err, Error::Timeout);
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[tokio::test]
async fn read_timeout_while_waiting_for_headers() {
    let server = MockServer::start().await;
    api()
        .respond_with(json_as_html(b"[]".to_vec()).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let config = HttpConfig {
        read_timeout: Duration::from_millis(300),
        ..HttpConfig::default()
    };
    let started = std::time::Instant::now();
    let err = client_with(&server, &config)
        .live_categories()
        .await
        .unwrap_err();
    assert_eq!(err, Error::Timeout);
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[tokio::test]
async fn malformed_list_items_are_skipped_and_reported() {
    let server = MockServer::start().await;
    api()
        .and(query_param("action", "get_live_streams"))
        .respond_with(json_as_html(fixture("xtream/live_streams.json")))
        .mount(&server)
        .await;
    let listing = client(&server).live_streams(None).await.unwrap();
    assert_eq!(listing.len(), 4);
    assert_eq!(listing.skipped, 2);
    assert_eq!(
        listing.errors.iter().map(|e| e.index).collect::<Vec<_>>(),
        vec![2, 4]
    );
}

#[tokio::test]
async fn auth_failure_object_instead_of_list() {
    let server = MockServer::start().await;
    api()
        .and(query_param("action", "get_vod_streams"))
        .respond_with(json_as_html(br#"{"user_info":{"auth":0}}"#.to_vec()))
        .mount(&server)
        .await;
    let err = client(&server).vod_streams(None).await.unwrap_err();
    assert_eq!(
        err,
        Error::Auth {
            status: AuthFailure::InvalidCredentials
        }
    );
}

#[tokio::test]
async fn credentials_are_percent_encoded_on_the_wire() {
    let server = MockServer::start().await;
    api()
        .and(query_param("action", "get_live_categories"))
        .respond_with(json_as_html(fixture("xtream/live_categories.json")))
        .expect(1)
        .mount(&server)
        .await;
    let cats = client(&server).live_categories().await.unwrap();
    assert_eq!(cats.len(), 3);
    let requests = server.received_requests().await.unwrap();
    let query = requests[0].url.query().unwrap().to_owned();
    assert_eq!(
        query,
        "username=demo+user&password=p%40ss+w%2Frd%26x%3D1%23%3F%25%C3%A9&action=get_live_categories"
    );
    assert!(!query.contains("&x=1"));
}

#[tokio::test]
async fn all_omits_category_id_and_categories_are_sent() {
    let server = MockServer::start().await;
    api()
        .and(query_param("action", "get_vod_streams"))
        .and(query_param_is_missing("category_id"))
        .respond_with(json_as_html(fixture("xtream/vod_streams.json")))
        .expect(1)
        .mount(&server)
        .await;
    api()
        .and(query_param("action", "get_series"))
        .and(query_param("category_id", "20"))
        .respond_with(json_as_html(fixture("xtream/series.json")))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    assert_eq!(c.vod_streams(None).await.unwrap().len(), 5);
    assert_eq!(
        c.series(Some(&CategoryId::new("20"))).await.unwrap().len(),
        3
    );
}

#[tokio::test]
async fn smarters_user_agent_preset() {
    let server = MockServer::start().await;
    api()
        .and(header("user-agent", IPTV_SMARTERS_UA))
        .respond_with(json_as_html(b"[]".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let c = client_with(&server, &HttpConfig::smarters_compatible());
    assert!(c.series_categories().await.unwrap().is_empty());
}

#[tokio::test]
async fn empty_and_bom_bodies() {
    let server = MockServer::start().await;
    api()
        .and(query_param("action", "get_live_categories"))
        .respond_with(json_as_html(Vec::new()))
        .mount(&server)
        .await;
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend(fixture("xtream/live_categories.json"));
    api()
        .and(query_param("action", "get_vod_categories"))
        .respond_with(json_as_html(bom))
        .mount(&server)
        .await;
    let c = client(&server);
    assert!(c.live_categories().await.unwrap().is_empty());
    assert_eq!(c.vod_categories().await.unwrap().len(), 3);
}

#[tokio::test]
async fn details_endpoints() {
    let server = MockServer::start().await;
    api()
        .and(query_param("action", "get_vod_info"))
        .and(query_param("vod_id", "201"))
        .respond_with(json_as_html(fixture("xtream/vod_info.json")))
        .mount(&server)
        .await;
    api()
        .and(query_param("action", "get_vod_info"))
        .and(query_param("vod_id", "999"))
        .respond_with(json_as_html(b"[]".to_vec()))
        .mount(&server)
        .await;
    api()
        .and(query_param("action", "get_series_info"))
        .and(query_param("series_id", "501"))
        .respond_with(json_as_html(fixture("xtream/series_info_object.json")))
        .mount(&server)
        .await;
    let c = client(&server);
    let vod = c.vod_info(201).await.unwrap();
    assert_eq!(vod.info.tmdb_id.as_deref(), Some("12345"));
    assert_eq!(c.vod_info(999).await.unwrap(), Default::default());
    let series = c.series_info(501).await.unwrap();
    assert_eq!(
        series.episodes.keys().copied().collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[tokio::test]
async fn epg_endpoints_and_catch_up_flow() {
    let server = MockServer::start().await;
    api()
        .and(query_param_is_missing("action"))
        .respond_with(json_as_html(fixture("xtream/login_active.json")))
        .mount(&server)
        .await;
    api()
        .and(query_param("action", "get_short_epg"))
        .and(query_param("stream_id", "101"))
        .and(query_param("limit", "4"))
        .respond_with(json_as_html(fixture("xtream/short_epg.json")))
        .expect(1)
        .mount(&server)
        .await;
    api()
        .and(query_param("action", "get_short_epg"))
        .and(query_param_is_missing("limit"))
        .respond_with(json_as_html(br#"{"epg_listings":[]}"#.to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    api()
        .and(query_param("action", "get_simple_data_table"))
        .and(query_param("stream_id", "101"))
        .respond_with(json_as_html(fixture("xtream/simple_data_table.json")))
        .mount(&server)
        .await;
    let c = client(&server);
    let login = c.login().await.unwrap();
    let short = c.short_epg(101, Some(4)).await.unwrap();
    assert_eq!(short.items[0].title, "Morning News");
    // Local-only times converted with the server zone learnt at login (Europe/Paris).
    assert_eq!(short.items[2].start.timestamp(), 1_704_096_000);
    assert!(c.short_epg(101, None).await.unwrap().is_empty());

    let table = c.simple_data_table(101).await.unwrap();
    let now = chrono::DateTime::from_timestamp(1_704_110_400, 0).unwrap();
    let progs = archive_programmes(&table.items, &c.server_time_zone(), now);
    let url = c.stream_urls(&login).timeshift(
        101,
        &progs[0],
        TimeshiftFormat::Standard,
        LiveContainer::Ts,
    );
    assert_eq!(
        url.as_str(),
        "http://stream.example.net:8080/timeshift/demo%20user/p%40ss%20w%2Frd%26x%3D1%23%3F%25%C3%A9/90/2023-12-31:23-30/101.ts"
    );
    assert_eq!(
        c.redactor().redact(url.as_str()),
        "http://stream.example.net:8080/timeshift/***/***/90/2023-12-31:23-30/101.ts"
    );
}

#[tokio::test]
async fn network_errors_never_leak_credentials() {
    // A port with nothing listening.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let account = XtreamAccount::new(&format!("127.0.0.1:{port}"), USERNAME).unwrap();
    let config = HttpConfig {
        connect_timeout: Duration::from_secs(2),
        ..HttpConfig::default()
    };
    let c = Client::new(account, SecretString::from(PASSWORD.to_owned()), &config).unwrap();
    let err = c.login().await.unwrap_err();
    assert!(
        matches!(err, Error::Network { .. } | Error::Timeout),
        "{err:?}"
    );
    assert_no_secrets(&err.to_string());
    assert_no_secrets(&format!("{err:?}"));
    // `Debug` of the client shows the username (not a secret by itself) but never the
    // password.
    let debug = format!("{c:?}");
    assert!(!debug.contains("p@ss") && !debug.contains("w/rd") && !debug.contains("p%40ss"));
}

#[tokio::test]
async fn xmltv_download_and_parse() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xmltv.php"))
        .and(query_param("username", USERNAME))
        .and(query_param("password", PASSWORD))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            gzip(&fixture("xmltv/basic.xml")),
            "application/octet-stream",
        ))
        .mount(&server)
        .await;
    let c = client(&server);
    let redactor = Redactor::new();
    let http = HttpClient::new(&HttpConfig::default(), redactor).unwrap();
    let bytes = http.get_bytes(&c.xmltv_url()).await.unwrap();
    let epg = xmltv::parse_bytes(&bytes, &xmltv::XmltvLimits::default()).unwrap();
    assert_eq!(epg.programmes_for("news.synthetic").len(), 3);
}

#[tokio::test]
async fn m3u_download_detect_and_parse() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/get.php"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("m3u/basic.m3u"), "audio/x-mpegurl"),
        )
        .mount(&server)
        .await;
    let m3u_url = format!(
        "{}/get.php?username=demo&password=demo_pass&type=m3u_plus&output=ts",
        server.uri()
    );
    let link = m3u::detect_xtream(&m3u_url).unwrap();
    assert_eq!(link.username, "demo");
    assert_eq!(link.server.to_string(), server.uri());

    let redactor = Redactor::new();
    redactor.add_secret(&m3u_url);
    let http = HttpClient::new(&HttpConfig::default(), redactor.clone()).unwrap();
    let bytes = http
        .get_bytes(&url::Url::parse(&m3u_url).unwrap())
        .await
        .unwrap();
    let pl = m3u::parse(&bytes, &m3u::M3uLimits::default()).unwrap();
    assert_eq!(pl.entries.len(), 2);
    assert_eq!(pl.entries[1].catchup.as_deref(), Some("xc"));
    assert_eq!(pl.tvg_urls, vec!["http://epg.example.org/basic.xml"]);
    assert_eq!(
        redactor.redact(&format!("loading {m3u_url}")),
        "loading ***"
    );

    // Non-HTTP schemes are refused by the downloader.
    let err = http
        .get_bytes(&url::Url::parse("file:///etc/hosts").unwrap())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::InvalidUrl { .. }));
}
