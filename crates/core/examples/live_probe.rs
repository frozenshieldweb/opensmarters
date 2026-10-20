//! Real-panel compatibility probe.
//!
//! Exercises the Xtream client against a real provider and prints a **redacted** report
//! (no credentials, no full stream URLs). Credentials come only from the environment:
//!
//! ```text
//! OSM_SERVER=http://host[:port] OSM_USERNAME=… OSM_PASSWORD=… \
//!     cargo run -p opensmarters-core --example live_probe
//! ```
//!
//! `--emit-url live|movie|episode|catchup` prints exactly one **unredacted** stream URL
//! to stdout (and nothing else) so it can be captured into a variable and handed to the
//! player without ever being displayed.

use std::env;
use std::error::Error as StdError;
use std::time::Instant;

use opensmarters_core::chrono::Utc;
use opensmarters_core::model::{archive_programmes, group_by_server_date};
use opensmarters_core::secrecy::SecretString;
use opensmarters_core::urls::{LiveContainer, TimeshiftFormat};
use opensmarters_core::xtream::Client;
use opensmarters_core::{HttpConfig, XtreamAccount};

type Result<T> = std::result::Result<T, Box<dyn StdError>>;

fn var(name: &str) -> Result<String> {
    env::var(name).map_err(|_| format!("{name} is not set").into())
}

#[tokio::main]
async fn main() -> Result<()> {
    let emit = env::args().skip_while(|a| a != "--emit-url").nth(1);
    let ua_smarters = env::args().any(|a| a == "--smarters-ua");

    let account = XtreamAccount::new(&var("OSM_SERVER")?, var("OSM_USERNAME")?)?;
    let password = SecretString::from(var("OSM_PASSWORD")?);
    let config = if ua_smarters {
        HttpConfig::smarters_compatible()
    } else {
        HttpConfig::default()
    };
    let client = Client::new(account, password, &config)?;
    let r = client.redactor().clone();
    let quiet = emit.is_some();
    macro_rules! say { ($($t:tt)*) => { if !quiet { println!($($t)*); } } }

    // --- login -------------------------------------------------------------------------
    let t = Instant::now();
    let login = client.login().await?;
    let ui = &login.user_info;
    let si = &login.server_info;
    say!("LOGIN ok in {:?}", t.elapsed());
    say!(
        "  status={:?} exp={:?} trial={:?} max_conn={:?} active={:?}",
        ui.status,
        ui.exp_date,
        ui.is_trial,
        ui.max_connections,
        ui.active_cons
    );
    say!("  allowed_output_formats={:?}", ui.allowed_output_formats);
    say!(
        "  server_info: host_same_as_login={} protocol={:?} port={:?} https_port={:?} tz={:?}",
        si.url.as_deref() == Some(client.account().server().host_str()),
        si.server_protocol,
        si.port,
        si.https_port,
        si.timezone
    );
    if env::args().any(|a| a == "--login-only") {
        return Ok(());
    }
    let urls = client.stream_urls(&login);
    say!("  stream base: {}", r.redact(urls.base().as_str()));
    let container = LiveContainer::preferred(LiveContainer::Ts, ui);

    // --- live --------------------------------------------------------------------------
    let t = Instant::now();
    let cats = client.live_categories().await?;
    let live = client.live_streams(None).await?;
    say!(
        "LIVE categories={} (skipped {}), channels={} (skipped {}) in {:?}",
        cats.items.len(),
        cats.skipped,
        live.items.len(),
        live.skipped,
        t.elapsed()
    );
    for c in cats.items.iter().take(5) {
        say!("  cat {:>6} {}", c.id.to_string(), c.name);
    }
    let archive_count = live.items.iter().filter(|s| s.tv_archive).count();
    let epg_count = live
        .items
        .iter()
        .filter(|s| s.epg_channel_id.is_some())
        .count();
    say!("  with epg_channel_id={epg_count}, with tv_archive={archive_count}");

    let with_epg = live.items.iter().find(|s| s.epg_channel_id.is_some());
    if let Some(ch) = with_epg {
        match client.short_epg(ch.stream_id, Some(4)).await {
            Ok(epg) => {
                say!(
                    "SHORT EPG for #{} '{}': {} listings (skipped {})",
                    ch.stream_id,
                    ch.name,
                    epg.items.len(),
                    epg.skipped
                );
                for l in epg.items.iter().take(3) {
                    say!("  {} → {}  {}", l.start, l.stop, l.title);
                }
            }
            Err(e) => say!("SHORT EPG error: {}", r.redact(&e.to_string())),
        }
    }

    // --- catch-up ----------------------------------------------------------------------
    let mut catchup_url = None;
    if let Some(ch) = live.items.iter().find(|s| s.tv_archive) {
        match client.simple_data_table(ch.stream_id).await {
            Ok(table) => {
                let progs = archive_programmes(&table.items, &login.time_zone(), Utc::now());
                let days = group_by_server_date(progs.clone());
                say!(
                    "CATCH-UP #{} '{}': listings={} archived={} days={} (archive_duration={:?})",
                    ch.stream_id,
                    ch.name,
                    table.items.len(),
                    progs.len(),
                    days.len(),
                    ch.tv_archive_duration
                );
                // An older programme is more likely to be fully archived.
                let pick = progs.len().saturating_sub(6);
                if let Some(p) = progs.get(pick) {
                    let fmt = if env::args().any(|a| a == "--smarters-timeshift") {
                        TimeshiftFormat::SmartersCompat
                    } else {
                        TimeshiftFormat::Standard
                    };
                    let url = urls.timeshift(ch.stream_id, p, fmt, container);
                    say!(
                        "  programme '{}' {} ({} min)",
                        p.title,
                        p.start_local,
                        p.duration_minutes()
                    );
                    say!("  timeshift ({fmt:?}): {}", r.redact(url.as_str()));
                    catchup_url = Some(url);
                    if env::args().any(|a| a == "--probe-timeshift") {
                        probe_timeshift(&urls, ch.stream_id, p, container, &r).await;
                    }
                }
            }
            Err(e) => say!("CATCH-UP error: {}", r.redact(&e.to_string())),
        }
    } else {
        say!("CATCH-UP: no channel advertises tv_archive");
    }

    // --- VOD ---------------------------------------------------------------------------
    let t = Instant::now();
    let vcats = client.vod_categories().await?;
    let vods = client.vod_streams(None).await?;
    say!(
        "VOD categories={}, movies={} (skipped {}) in {:?}",
        vcats.items.len(),
        vods.items.len(),
        vods.skipped,
        t.elapsed()
    );
    let mut movie_url = None;
    if let Some(m) = vods.items.first() {
        match client.vod_info(m.stream_id).await {
            Ok(info) => {
                say!(
                    "  vod_info #{} '{}': ext={:?} tmdb={:?}",
                    m.stream_id,
                    m.name,
                    m.container_extension,
                    info.info.tmdb_id
                );
            }
            Err(e) => say!("  vod_info error: {}", r.redact(&e.to_string())),
        }
        movie_url = Some(urls.movie(m.stream_id, m.container_extension.as_deref()));
    }

    // --- series ------------------------------------------------------------------------
    let t = Instant::now();
    let scats = client.series_categories().await?;
    let series = client.series(None).await?;
    say!(
        "SERIES categories={}, series={} (skipped {}) in {:?}",
        scats.items.len(),
        series.items.len(),
        series.skipped,
        t.elapsed()
    );
    let mut episode_url = None;
    if let Some(s) = series.items.first() {
        match client.series_info(s.series_id).await {
            Ok(info) => {
                let eps: usize = info.episodes.values().map(Vec::len).sum();
                say!(
                    "  series_info #{} '{}': seasons={} episodes={}",
                    s.series_id,
                    s.name,
                    info.episodes.len(),
                    eps
                );
                if let Some(ep) = info.episodes.values().flatten().next() {
                    episode_url = Some(urls.episode(ep.id, ep.container_extension.as_deref()));
                }
            }
            Err(e) => say!("  series_info error: {}", r.redact(&e.to_string())),
        }
    }

    // --- stream URLs -------------------------------------------------------------------
    let live_url = live
        .items
        .first()
        .map(|s| urls.live(s.stream_id, container));
    say!("URLS (redacted):");
    for (k, u) in [
        ("live", &live_url),
        ("movie", &movie_url),
        ("episode", &episode_url),
    ] {
        if let Some(u) = u {
            say!("  {k}: {}", r.redact(u.as_str()));
        }
    }

    if let Some(kind) = emit {
        let url = match kind.as_str() {
            "live" => live_url,
            "movie" => movie_url,
            "episode" => episode_url,
            "catchup" => catchup_url,
            other => return Err(format!("unknown --emit-url kind {other}").into()),
        };
        println!("{}", url.ok_or("no such stream")?);
    }
    Ok(())
}

/// Tries the timeshift URL variants seen in the wild and reports HTTP status + the first
/// bytes' signature (MPEG-TS sync byte / `#EXTM3U`) for each. Reads at most 4 KiB.
async fn probe_timeshift(
    urls: &opensmarters_core::urls::StreamUrls,
    stream_id: u64,
    p: &opensmarters_core::ArchiveProgramme,
    container: LiveContainer,
    r: &opensmarters_core::Redactor,
) {
    let mins = p.duration_minutes();
    let mut variants: Vec<(String, opensmarters_core::url::Url)> = Vec::new();
    for fmt in [TimeshiftFormat::Standard, TimeshiftFormat::SmartersCompat] {
        for c in [LiveContainer::Ts, LiveContainer::M3u8] {
            variants.push((
                format!("path {fmt:?} {c:?}"),
                urls.timeshift(stream_id, p, fmt, c),
            ));
        }
    }
    // `/streaming/timeshift.php?username=&password=&stream=&start=&duration=` variant,
    // built from the path variant so credentials stay percent-encoded.
    let path_url = urls.timeshift(stream_id, p, TimeshiftFormat::Standard, container);
    let segs: Vec<String> = path_url
        .path_segments()
        .map(|s| s.map(str::to_owned).collect())
        .unwrap_or_default();
    if let [_, user, pass, ..] = segs.as_slice() {
        let mut q = urls.base().clone();
        q.set_path("/streaming/timeshift.php");
        let dec = |s: &str| {
            opensmarters_core::url::form_urlencoded::parse(format!("x={s}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        let start = TimeshiftFormat::Standard.format_start(p.start_local);
        q.query_pairs_mut()
            .append_pair("username", &dec(user))
            .append_pair("password", &dec(pass))
            .append_pair("stream", &stream_id.to_string())
            .append_pair("start", &start)
            .append_pair("duration", &mins.to_string());
        variants.push(("timeshift.php Standard".into(), q));
    }

    // Control: the live URL of the same channel (known to play).
    variants.insert(0, ("control: live".into(), urls.live(stream_id, container)));
    for ua in [
        opensmarters_core::DEFAULT_USER_AGENT,
        opensmarters_core::IPTV_SMARTERS_UA,
        "VLC/3.0.21 LibVLC/3.0.21",
    ] {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(ua)
            .build();
        let Ok(http) = http else { return };
        println!("TIMESHIFT variants (UA {ua}):");
        for (label, url) in variants.clone() {
            let res = http
                .get(url.as_str())
                .header("Range", "bytes=0-4095")
                .send()
                .await;
            match res {
                Ok(mut resp) => {
                    let status = resp.status();
                    let ctype = resp
                        .headers()
                        .get("content-type")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-")
                        .to_owned();
                    let first = resp.chunk().await.ok().flatten().unwrap_or_default();
                    let sig = if first.first() == Some(&0x47) {
                        "MPEG-TS"
                    } else if first.starts_with(b"#EXTM3U") {
                        "HLS playlist"
                    } else {
                        "other"
                    };
                    println!("  {label:<26} {status} {ctype} [{sig}, {} B]", first.len());
                }
                Err(e) => println!("  {label:<26} error: {}", r.redact(&e.to_string())),
            }
        }
    }
}
