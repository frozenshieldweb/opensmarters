// SPDX-License-Identifier: GPL-3.0-or-later
//! Setup, sign-in, sessions, invites, disabling, rate limit, CSRF, headers.

use reqwest::Method;
use serde_json::json;

use crate::common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_run_setup_is_single_use() {
    let app = app(&[]).await;
    let c = app.client();
    let s = c.get("/api/status").await.ok();
    assert_eq!(s["setup_required"], true);
    assert!(s["user"].is_null());
    assert!(s["version"].is_string());

    let wrong = c
        .post(
            "/api/setup",
            json!({"setup_code": "nope-nope", "name": "admin", "password": ADMIN_PASSWORD}),
        )
        .await;
    assert_eq!((wrong.status, wrong.code()), (403, "bad_setup_code"));
    let weak = c
        .post(
            "/api/setup",
            json!({"setup_code": SETUP_CODE, "name": "admin", "password": "short"}),
        )
        .await;
    assert_eq!((weak.status, weak.code()), (400, "weak_password"));
    let bad_name = c
        .post(
            "/api/setup",
            json!({"setup_code": SETUP_CODE, "name": "a b", "password": ADMIN_PASSWORD}),
        )
        .await;
    assert_eq!((bad_name.status, bad_name.code()), (400, "invalid_name"));

    let ok = c
        .post("/api/setup", json!({"setup_code": SETUP_CODE.to_lowercase(), "name": "admin", "password": ADMIN_PASSWORD}))
        .await;
    assert_eq!(ok.status, 200, "{}", ok.json);
    assert_eq!(ok.json["user"]["name"], "admin");
    assert_eq!(ok.json["user"]["is_admin"], true);
    let cookie = ok.header("set-cookie");
    for part in [
        "opensmarters_session=",
        "HttpOnly",
        "Secure",
        "SameSite=Strict",
        "Path=/",
    ] {
        assert!(cookie.contains(part), "{cookie}");
    }
    let s = c.get("/api/status").await.ok();
    assert_eq!(s["setup_required"], false);
    assert_eq!(s["user"]["name"], "admin");

    let again = app
        .client()
        .post(
            "/api/setup",
            json!({"setup_code": SETUP_CODE, "name": "other", "password": ADMIN_PASSWORD}),
        )
        .await;
    assert_eq!((again.status, again.code()), (409, "already_set_up"));
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_logout_and_session_rotation() {
    let app = app(&[]).await;
    let admin = admin(&app).await;
    let first = admin.session_token().unwrap();

    let c = app.client();
    for (name, pw) in [("admin", "wrong password!"), ("nobody", "wrong password!")] {
        let r = c
            .post("/api/login", json!({"name": name, "password": pw}))
            .await;
        assert_eq!((r.status, r.code()), (401, "bad_credentials"));
        assert_eq!(
            r.json["error"]["message"],
            "That name and password don't match an account here."
        );
    }
    // Sign in again in the same browser: a new token, the old one ends.
    let r = admin
        .post(
            "/api/login",
            json!({"name": "ADMIN", "password": ADMIN_PASSWORD}),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.json);
    let second = admin.session_token().unwrap();
    assert_ne!(first, second);
    let old = app.client();
    *old.cookie.lock().unwrap() = Some(first);
    assert_eq!(old.get("/api/accounts").await.status, 401);
    assert_eq!(admin.get("/api/accounts").await.status, 200);

    // Sign out.
    let out = admin.post_empty("/api/logout").await;
    assert_eq!(out.status, 200);
    assert!(out.header("set-cookie").contains("Max-Age=0"));
    let stale = app.client();
    *stale.cookie.lock().unwrap() = Some(second);
    let r = stale.get("/api/accounts").await;
    assert_eq!((r.status, r.code()), (401, "not_signed_in"));
    assert!(app.client().get("/api/status").await.ok()["user"].is_null());
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_is_rate_limited() {
    let app = app(&[]).await;
    let _admin = admin(&app).await;
    let c = app.client();
    for _ in 0..5 {
        let r = c
            .post(
                "/api/login",
                json!({"name": "admin", "password": "not the password"}),
            )
            .await;
        assert_eq!(r.status, 401);
    }
    // Locked out now, even with the right password.
    let r = c
        .post(
            "/api/login",
            json!({"name": "admin", "password": ADMIN_PASSWORD}),
        )
        .await;
    assert_eq!((r.status, r.code()), (429, "rate_limited"));
    assert!(r.header("retry-after").parse::<u64>().unwrap() >= 1);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn csrf_header_is_required() {
    let app = app(&[]).await;
    let admin = admin(&app).await;
    let r = admin
        .raw(Method::POST, "/api/admin/invites", Some(json!({})), false)
        .await;
    assert_eq!((r.status, r.code()), (403, "csrf"));
    let r = admin.raw(Method::POST, "/api/logout", None, false).await;
    assert_eq!((r.status, r.code()), (403, "csrf"));
    let r = app
        .client()
        .raw(
            Method::POST,
            "/api/login",
            Some(json!({"name": "admin", "password": ADMIN_PASSWORD})),
            false,
        )
        .await;
    assert_eq!((r.status, r.code()), (403, "csrf"));
    // GETs don't need it.
    assert_eq!(
        admin
            .raw(Method::GET, "/api/accounts", None, false)
            .await
            .status,
        200
    );
    // JSON only (a form post can't reach a handler).
    let form = reqwest::Client::new()
        .post(format!("{}/api/login", app.base))
        .header("X-Opensmarters", "1")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("name=admin&password=x")
        .send()
        .await
        .unwrap();
    assert_eq!(form.status().as_u16(), 400);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invites_users_and_disabling() {
    let app = app(&[]).await;
    let admin = admin(&app).await;
    let inv = admin.post("/api/admin/invites", json!({})).await.ok();
    let url = inv["url"].as_str().unwrap();
    assert!(url.contains("/invite#"), "{url}");
    let now = chrono::Utc::now().timestamp();
    let exp = inv["expires_at"].as_i64().unwrap();
    assert!((exp - now - 72 * 3600).abs() < 60);
    let token = url.split_once("/invite#").unwrap().1.to_owned();

    let bob = app.client();
    let taken = bob
        .post(
            "/api/invites/accept",
            json!({"token": token, "name": "Admin", "password": "bob's password"}),
        )
        .await;
    assert_eq!((taken.status, taken.code()), (409, "name_taken"));
    let ok = bob
        .post(
            "/api/invites/accept",
            json!({"token": token, "name": "bob", "password": "bob's password"}),
        )
        .await;
    assert_eq!(ok.status, 200, "{}", ok.json);
    assert_eq!(ok.json["user"]["is_admin"], false);
    let reuse = app
        .client()
        .post(
            "/api/invites/accept",
            json!({"token": token, "name": "eve", "password": "eve's password"}),
        )
        .await;
    assert_eq!((reuse.status, reuse.code()), (400, "invalid_invite"));
    let forged = app
        .client()
        .post("/api/invites/accept", json!({"token": "forged-token-forged-token", "name": "eve", "password": "eve's password"}))
        .await;
    assert_eq!(forged.code(), "invalid_invite");

    // Not an admin.
    assert_eq!(bob.get("/api/admin/users").await.status, 403);
    assert_eq!(bob.post("/api/admin/invites", json!({})).await.status, 403);

    let users = admin.get("/api/admin/users").await.ok();
    let list = users.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let bob_row = list.iter().find(|u| u["name"] == "bob").unwrap();
    let bob_id = bob_row["id"].as_str().unwrap().to_owned();
    for k in [
        "id",
        "name",
        "is_admin",
        "disabled",
        "created_at",
        "last_seen_at",
    ] {
        assert!(bob_row.get(k).is_some(), "{k}");
    }
    let admin_id = list.iter().find(|u| u["name"] == "admin").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The admin can't disable or remove themselves.
    let r = admin
        .post_empty(&format!("/api/admin/users/{admin_id}/disable"))
        .await;
    assert_eq!(r.status, 409);
    let r = admin.delete(&format!("/api/admin/users/{admin_id}")).await;
    assert_eq!(r.status, 409);

    // Disable: bob's session ends and he can't sign in.
    admin
        .post_empty(&format!("/api/admin/users/{bob_id}/disable"))
        .await
        .ok();
    assert_eq!(bob.get("/api/accounts").await.status, 401);
    let r = bob
        .post(
            "/api/login",
            json!({"name": "bob", "password": "bob's password"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (401, "bad_credentials"));
    let users = admin.get("/api/admin/users").await.ok();
    assert_eq!(
        users
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["name"] == "bob")
            .unwrap()["disabled"],
        true
    );
    admin
        .post_empty(&format!("/api/admin/users/{bob_id}/enable"))
        .await
        .ok();
    bob.post(
        "/api/login",
        json!({"name": "bob", "password": "bob's password"}),
    )
    .await
    .ok();
    assert_eq!(bob.get("/api/accounts").await.status, 200);

    // Remove.
    admin
        .delete(&format!("/api/admin/users/{bob_id}"))
        .await
        .ok();
    assert_eq!(bob.get("/api/accounts").await.status, 401);
    assert_eq!(
        admin
            .delete(&format!("/api/admin/users/{bob_id}"))
            .await
            .status,
        404
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_change_ends_other_sessions() {
    let app = app(&[]).await;
    let admin = admin(&app).await;
    let other = app.client();
    other
        .post(
            "/api/login",
            json!({"name": "admin", "password": ADMIN_PASSWORD}),
        )
        .await
        .ok();

    let wrong = admin
        .post(
            "/api/password",
            json!({"current": "wrong one!!", "new": "brand new password"}),
        )
        .await;
    assert_eq!((wrong.status, wrong.code()), (403, "wrong_password"));
    let weak = admin
        .post(
            "/api/password",
            json!({"current": ADMIN_PASSWORD, "new": "short"}),
        )
        .await;
    assert_eq!(weak.status, 400);
    let before = admin.session_token();
    admin
        .post(
            "/api/password",
            json!({"current": ADMIN_PASSWORD, "new": "brand new password"}),
        )
        .await
        .ok();
    assert_ne!(
        admin.session_token(),
        before,
        "this browser gets a fresh session"
    );
    assert_eq!(admin.get("/api/accounts").await.status, 200);
    assert_eq!(other.get("/api/accounts").await.status, 401);
    let r = app
        .client()
        .post(
            "/api/login",
            json!({"name": "admin", "password": ADMIN_PASSWORD}),
        )
        .await;
    assert_eq!(r.status, 401);
    app.client()
        .post(
            "/api/login",
            json!({"name": "admin", "password": "brand new password"}),
        )
        .await
        .ok();
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pages_and_api_headers() {
    let app = app(&[]).await;
    let c = app.client();
    let index = c.get("/").await;
    assert_eq!(index.status, 200);
    assert!(index.header("content-type").starts_with("text/html"));
    assert_eq!(index.header("cache-control"), "no-store");
    let csp = index.header("content-security-policy");
    for d in [
        "default-src 'self'",
        "script-src 'self'",
        "style-src 'self'",
        "frame-ancestors 'none'",
        "base-uri 'none'",
        "form-action 'self'",
        "img-src 'self' data: blob:",
        "media-src 'self' blob:",
        "connect-src 'self'",
    ] {
        assert!(csp.contains(d), "{csp}");
    }
    assert_eq!(index.header("x-content-type-options"), "nosniff");
    assert_eq!(index.header("referrer-policy"), "no-referrer");
    // SPA fallback for routes; 404 for missing files; JSON 404 under /api.
    let route = c.get("/app/live/123").await;
    assert_eq!(route.status, 200);
    assert!(route.header("content-type").starts_with("text/html"));
    assert_eq!(c.get("/assets/missing-abc123.js").await.status, 404);
    let api404 = c.get("/api/nope").await;
    assert_eq!((api404.status, api404.code()), (404, "not_found"));
    assert!(api404.json["error"]["message"].is_string());
    let status = c.get("/api/status").await;
    assert_eq!(status.header("cache-control"), "no-store");
    assert_eq!(status.header("x-content-type-options"), "nosniff");
    // Signed-out access to everything else is refused.
    for path in [
        "/api/accounts",
        "/api/settings",
        "/api/about",
        "/api/admin/users",
    ] {
        assert_eq!(c.get(path).await.status, 401, "{path}");
    }
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_and_about() {
    let app = app(&[]).await;
    let admin = admin(&app).await;
    let s = admin.get("/api/settings").await.ok();
    assert_eq!(
        s,
        json!({"time_format": "24h", "live_format": "ts", "autoplay_next": true})
    );
    let s = admin
        .put(
            "/api/settings",
            Some(json!({"time_format": "12h", "live_format": "m3u8", "autoplay_next": false})),
        )
        .await
        .ok();
    assert_eq!(
        s,
        json!({"time_format": "12h", "live_format": "m3u8", "autoplay_next": false})
    );
    assert_eq!(admin.get("/api/settings").await.ok()["time_format"], "12h");
    let bad = admin
        .put("/api/settings", Some(json!({"time_format": "13h"})))
        .await;
    assert_eq!(bad.status, 400);
    let about = admin.get("/api/about").await.ok();
    assert_eq!(about["transcode_video"], "never");
    assert_eq!(about["streams_per_user"], 1);
    assert_eq!(about["max_streams"], 8);
    assert!(
        about["source_url"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/")
    );
    assert!(about["version"].is_string());
    app.shutdown().await;
}
