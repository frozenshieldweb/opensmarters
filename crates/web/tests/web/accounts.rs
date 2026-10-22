// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider accounts (Xtream and M3U), checks against the provider, secrets at rest,
//! isolation between two people.

use serde_json::json;

use crate::common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn xtream_accounts() {
    let panel = panel().await;
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    assert_eq!(c.get("/api/accounts").await.ok(), json!([]));

    let special = fake_panel::users::SPECIAL;
    let a = c
        .post(
            "/api/accounts",
            json!({"name": "My TV", "kind": "xtream", "server": format!("{}/player_api.php", panel.base_url()), "username": special.username, "password": special.password}),
        )
        .await
        .ok();
    assert_eq!(a["name"], "My TV");
    assert_eq!(a["kind"], "xtream");
    assert_eq!(a["host"], "127.0.0.1");
    assert_eq!(a["status"], "active");
    assert!(a["expires_at"].is_i64());
    assert!(a["max_connections"].is_i64());
    let text = a.to_string();
    assert!(
        !text.contains(special.password) && !text.contains("p%40ss"),
        "{text}"
    );
    let keys: Vec<&String> = a.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec![
            "active_connections",
            "expires_at",
            "host",
            "id",
            "kind",
            "max_connections",
            "name",
            "status"
        ]
    );
    let id = a["id"].as_str().unwrap().to_owned();
    assert_eq!(
        c.get("/api/accounts").await.ok().as_array().unwrap().len(),
        1
    );

    // Rejected by the provider.
    for (user, pw, needle) in [
        ("demo", "wrong", "didn't accept"),
        ("expired", "expired", "expired"),
        ("banned", "banned", ""),
        ("disabled", "disabled", "disabled"),
    ] {
        let r = c
            .post(
                "/api/accounts",
                json!({"kind": "xtream", "server": panel.base_url(), "username": user, "password": pw}),
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (400, "provider_rejected"),
            "{user}: {}",
            r.json
        );
        assert!(
            r.json["error"]["message"]
                .as_str()
                .unwrap()
                .contains(needle),
            "{}",
            r.json
        );
    }
    // Missing fields and bad addresses.
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "xtream", "server": panel.base_url(), "username": "demo"}),
        )
        .await;
    assert_eq!(r.status, 400);
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "ftp", "server": panel.base_url()}),
        )
        .await;
    assert_eq!(r.status, 400);
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "xtream", "server": "ftp://x", "username": "a", "password": "b"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "invalid_server"));

    // Rename; a wrong new password is refused and changes nothing.
    let r = c
        .patch(&format!("/api/accounts/{id}"), json!({"name": "Renamed"}))
        .await
        .ok();
    assert_eq!(r["name"], "Renamed");
    let r = c
        .patch(&format!("/api/accounts/{id}"), json!({"password": "nope"}))
        .await;
    assert_eq!(r.code(), "provider_rejected");
    let r = c
        .post_empty(&format!("/api/accounts/{id}/refresh"))
        .await
        .ok();
    assert_eq!(r["status"], "active");
    assert_eq!(r["name"], "Renamed");
    // Live lists still work with the stored (special-character) password.
    assert!(
        !c.get(&format!("/api/accounts/{id}/live/categories"))
            .await
            .ok()
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Secrets are never stored in clear.
    let bytes = all_bytes(app.dir.path());
    for needle in [special.password, "p%40ss", "w%23rd"] {
        assert!(
            !contains(&bytes, needle.as_bytes()),
            "{needle} found on disk"
        );
    }

    assert_eq!(c.delete(&format!("/api/accounts/{id}")).await.status, 200);
    assert_eq!(
        c.get(&format!("/api/accounts/{id}/live/categories"))
            .await
            .status,
        404
    );
    assert_eq!(c.delete(&format!("/api/accounts/{id}")).await.status, 404);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreachable_provider() {
    let closed = closed_port().await;
    let app = app(&[closed]).await;
    let c = admin(&app).await;
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "xtream", "server": format!("http://{closed}"), "username": "a", "password": "b"}),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (502, "provider_unreachable"),
        "{}",
        r.json
    );
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": format!("http://{closed}/list.m3u")}),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (502, "provider_unreachable"),
        "{}",
        r.json
    );
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3u_accounts() {
    let panel = panel().await;
    let app = app(&[panel.addr()]).await;
    let c = admin(&app).await;
    let playlist = panel.url("/get.php?username=demo&password=demo&type=m3u_plus&output=ts");
    let a = c
        .post(
            "/api/accounts",
            json!({"name": "Playlist", "kind": "m3u", "m3u_url": playlist, "epg_url": null}),
        )
        .await
        .ok();
    assert_eq!(a["kind"], "m3u");
    assert_eq!(a["host"], "127.0.0.1");
    assert!(!a.to_string().contains("get.php"));
    let id = a["id"].as_str().unwrap().to_owned();
    let bytes = all_bytes(app.dir.path());
    assert!(
        !contains(&bytes, b"get.php"),
        "the playlist address is stored in clear"
    );

    // Rejected: 404 and not a playlist; refused credentials.
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": panel.url("/nothing.m3u")}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "provider_rejected"));
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": panel.url("/player_api.php")}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "provider_rejected"));
    let r = c
        .post(
            "/api/accounts",
            json!({"kind": "m3u", "m3u_url": panel.url("/get.php?username=demo&password=wrong")}),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "provider_rejected"));

    // Set and remove an EPG address.
    let epg = panel.url("/xmltv.php?username=demo&password=demo");
    let r = c
        .patch(&format!("/api/accounts/{id}"), json!({"epg_url": epg}))
        .await
        .ok();
    assert_eq!(r["kind"], "m3u");
    let r = c
        .patch(&format!("/api/accounts/{id}"), json!({"epg_url": null}))
        .await
        .ok();
    assert_eq!(r["id"], id.as_str());
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_people_never_see_each_other() {
    let panel = panel().await;
    let app = app(&[panel.addr()]).await;
    let alice = admin(&app).await;
    let bob = invite(&app, &alice, "bob", "bob's password").await;
    let a = add_xtream(&alice, &panel).await;
    alice
        .put(&format!("/api/accounts/{a}/favourites/channel/1001"), None)
        .await
        .ok();
    alice
        .put(
            &format!("/api/accounts/{a}/progress"),
            Some(json!({"kind": "movie", "id": "200001", "position": 60, "duration": 6000})),
        )
        .await
        .ok();

    assert_eq!(bob.get("/api/accounts").await.ok(), json!([]));
    for path in [
        format!("/api/accounts/{a}/live/categories"),
        format!("/api/accounts/{a}/live/channels?category=all"),
        format!("/api/accounts/{a}/favourites"),
        format!("/api/accounts/{a}/continue"),
        format!("/api/accounts/{a}/movies/200001"),
        format!("/api/accounts/{a}/search?q=news"),
        format!("/api/accounts/{a}/epg/now?keys=1001"),
    ] {
        let r = bob.get(&path).await;
        assert_eq!((r.status, r.code()), (404, "not_found"), "{path}");
    }
    assert_eq!(
        bob.patch(&format!("/api/accounts/{a}"), json!({"name": "mine"}))
            .await
            .status,
        404
    );
    assert_eq!(bob.delete(&format!("/api/accounts/{a}")).await.status, 404);
    assert_eq!(
        bob.post_empty(&format!("/api/accounts/{a}/refresh"))
            .await
            .status,
        404
    );
    assert_eq!(
        bob.put(&format!("/api/accounts/{a}/favourites/channel/1002"), None)
            .await
            .status,
        404
    );
    assert_eq!(
        bob.delete(&format!("/api/accounts/{a}/favourites/channel/1001"))
            .await
            .status,
        404
    );
    let r = bob
        .put(
            &format!("/api/accounts/{a}/progress"),
            Some(json!({"kind": "movie", "id": "200001", "position": 1, "duration": 2})),
        )
        .await;
    assert_eq!(r.status, 404);
    let r = bob
        .post(
            "/api/play",
            json!({"account": a, "kind": "live", "id": "1001", "hevc": false}),
        )
        .await;
    assert_eq!(r.status, 404);

    // Alice's data is untouched.
    let fav = alice
        .get(&format!("/api/accounts/{a}/favourites"))
        .await
        .ok();
    assert_eq!(fav["channels"].as_array().unwrap().len(), 1);
    let cont = alice.get(&format!("/api/accounts/{a}/continue")).await.ok();
    assert_eq!(cont.as_array().unwrap().len(), 1);
    // Bob's own account with the same provider is separate.
    let b = add_xtream(&bob, &panel).await;
    assert_ne!(a, b);
    assert_eq!(
        bob.get(&format!("/api/accounts/{b}/favourites")).await.ok()["channels"],
        json!([])
    );
    app.shutdown().await;
}
