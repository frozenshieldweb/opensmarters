//! M3U output: `get.php` (m3u / m3u_plus) and the `/nasty.m3u` torture playlist.

use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::api::{cover_url, logo_url, poster_url};
use crate::server::{AppState, Params, State as PanelState};
use crate::users::{self, TestUser};

/// Everything except RFC 3986 unreserved characters.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode a query value or path segment.
pub(crate) fn enc(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// Attribute values cannot contain `"` in m3u_plus; panels differ, we swap in `'`.
fn attr(s: &str) -> String {
    s.replace('"', "'").replace(['\r', '\n'], " ")
}

fn title(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

const M3U_TYPE: &str = "audio/x-mpegurl; charset=utf-8";

/// `GET /get.php?username=&password=&type=m3u_plus|m3u&output=ts|m3u8`.
pub(crate) async fn get_php(
    State(st): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let p = Params::from_pairs(pairs);
    let Some(user) = users::find_streamable(p.get("username"), p.get("password")) else {
        return (StatusCode::UNAUTHORIZED, "").into_response();
    };
    let ext = match p.get("output").trim() {
        "m3u8" | "hls" => "m3u8",
        _ => "ts",
    };
    let plus = p.get("type").trim() != "m3u";
    let body = build(&st, user, plus, ext);
    (
        [
            (header::CONTENT_TYPE, M3U_TYPE),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"playlist.m3u\"",
            ),
        ],
        body,
    )
        .into_response()
}

fn build(st: &PanelState, user: &TestUser, plus: bool, ext: &str) -> String {
    let cat = &st.catalogue;
    let (u, p) = (enc(user.username), enc(user.password));
    let stream = st.stream_base();
    let mut out = String::with_capacity(cat.live.len() * 260 + 200_000);
    let epg = format!("{}/xmltv.php?username={u}&password={p}", st.api_base);
    if plus {
        let _ = writeln!(out, "#EXTM3U url-tvg=\"{epg}\" x-tvg-url=\"{epg}\"");
    } else {
        out.push_str("#EXTM3U\n");
    }
    let entry = |out: &mut String, tvg_id: &str, name: &str, logo: &str, group: &str| {
        if plus {
            let _ = writeln!(
                out,
                "#EXTINF:-1 tvg-id=\"{}\" tvg-name=\"{}\" tvg-logo=\"{}\" group-title=\"{}\",{}",
                attr(tvg_id),
                attr(name),
                attr(logo),
                attr(group),
                title(name)
            );
        } else {
            let _ = writeln!(out, "#EXTINF:-1,{}", title(name));
        }
    };
    for ch in &cat.live {
        entry(
            &mut out,
            ch.epg_channel_id.as_deref().unwrap_or_default(),
            &ch.name,
            &logo_url(st, ch),
            cat.live_category_name(ch.category_id),
        );
        let _ = writeln!(out, "{stream}/live/{u}/{p}/{}.{ext}", ch.stream_id);
    }
    for m in &cat.movies {
        entry(
            &mut out,
            "",
            &m.name,
            &poster_url(st, m),
            cat.vod_category_name(m.category_id),
        );
        let _ = writeln!(
            out,
            "{stream}/movie/{u}/{p}/{}.{}",
            m.stream_id, m.container_extension
        );
    }
    for (s, e) in cat.episodes() {
        let name = format!("{} S{:02} E{:02}", s.name, e.season, e.episode_num);
        entry(
            &mut out,
            "",
            &name,
            &cover_url(st, s),
            cat.series_category_name(s.category_id),
        );
        let _ = writeln!(
            out,
            "{stream}/series/{u}/{p}/{}.{}",
            e.id, e.container_extension
        );
    }
    out
}

/// `GET /nasty.m3u`: a torture playlist exercising every M3U parser edge case.
///
/// Contents: UTF-8 BOM, CRLF (plus a few bare-LF lines), `#EXTINF:0`, `-1` and fractional
/// durations, attribute values containing commas, quotes and key names, `#EXTGRP`,
/// `#EXTVLCOPT` headers, `#KODIPROP`, an http logo with an https stream, rtmp/udp/rtsp
/// URLs, an entry with no URL followed by a normal one, blank lines, a very long name,
/// lowercase `#extinf`, padded whitespace, duplicates, catch-up attributes, and URLs a
/// client must refuse to play (`file://`, `lavfi://`, `av://`, `edl://`, `memory://`, UNC).
/// It ends without a trailing newline.
pub(crate) async fn nasty(State(st): State<AppState>) -> Response {
    ([(header::CONTENT_TYPE, M3U_TYPE)], nasty_body(&st)).into_response()
}

pub(crate) fn nasty_body(st: &PanelState) -> String {
    let b = &st.api_base;
    let live = |i: usize| -> String {
        st.catalogue
            .live
            .get(i)
            .map_or(1001, |c| c.stream_id)
            .to_string()
    };
    let movie = |i: usize, fallback: &str| -> String {
        st.catalogue.movies.get(i).map_or_else(
            || format!("200001.{fallback}"),
            |m| format!("{}.{}", m.stream_id, m.container_extension),
        )
    };
    let long_name = "Very Long Channel Name ".repeat(28);
    let archive = st
        .catalogue
        .live
        .iter()
        .find(|c| c.tv_archive)
        .map_or(1001, |c| c.stream_id);
    let mut s = String::new();
    s.push('\u{FEFF}');
    let mut crlf = |line: &str| {
        s.push_str(line);
        s.push_str("\r\n");
    };
    crlf(&format!(
        "#EXTM3U x-tvg-url=\"{b}/xmltv.php?username=demo&password=demo\" url-tvg=\"{b}/xmltv.xml.gz\" tvg-shift=\"0\""
    ));
    crlf("");
    crlf(&format!(
        "#EXTINF:-1 tvg-id=\"plain.fp\" tvg-name=\"Plain Channel\" tvg-logo=\"{b}/images/logo/{}.png\" group-title=\"Basics\",Plain Channel",
        live(0)
    ));
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(0)));
    crlf("#EXTINF:0 tvg-id=\"duration.zero\" group-title=\"Durations\",Duration Zero");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(1)));
    crlf("#EXTINF:123.456 tvg-id=\"fractional\" group-title=\"Durations\",Fractional Duration VOD");
    crlf(&format!("{b}/movie/demo/demo/{}", movie(1, "mp4")));
    crlf(
        "#EXTINF:-1 tvg-id=\"comma.test\" tvg-name=\"Comma, Separated, Name\" group-title=\"News, Weather & Traffic\",Comma, Separated, Name",
    );
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(2)));
    crlf(
        "#EXTINF:-1 tvg-id=\"quote.test\" tvg-name=\"The \\\"Quoted\\\" Channel\" tvg-logo=\"\" group-title=\"Quotes 'n' Things\",The \"Quoted\" Channel",
    );
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(3)));
    crlf(&format!(
        "#EXTINF:-1 tvg-name=\"tvg-logo=fake.png\" tvg-id=\"group-title=\" tvg-logo=\"{b}/images/logo/{}.png\" group-title=\"tvg-id=x, tvg-name=y\",Key Names Inside Values",
        live(4)
    ));
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(4)));
    crlf("#EXTINF:-1 tvg-id=\"extgrp.test\",Grouped Only By EXTGRP");
    crlf("#EXTGRP:Extgrp Group");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(5)));
    crlf("#EXTINF:-1 tvg-id=\"ua.test\" group-title=\"Headers\",Custom User Agent And Referrer");
    crlf("#EXTVLCOPT:http-user-agent=FakePanelAgent/1.0 (compatible; test)");
    crlf(&format!("#EXTVLCOPT:http-referrer={b}/"));
    crlf("#KODIPROP:inputstream.adaptive.manifest_type=hls");
    crlf(&format!("{b}/live/demo/demo/{}.m3u8", live(6)));
    crlf(&format!(
        "#EXTINF:-1 tvg-id=\"mixed.schemes\" tvg-logo=\"{b}/images/logo/{}.png\" group-title=\"Mixed Schemes\",HTTP Logo With HTTPS Stream",
        live(7)
    ));
    crlf(&format!(
        "https://secure.stream.invalid/live/demo/demo/{}.ts",
        live(7)
    ));
    crlf("#EXTINF:-1 group-title=\"Mixed Schemes\",RTMP Stream");
    crlf("rtmp://rtmp.stream.invalid:1935/live/channel1");
    crlf("#EXTINF:-1 group-title=\"Mixed Schemes\",UDP Multicast");
    crlf("udp://@239.255.0.1:1234");
    crlf("#EXTINF:-1 group-title=\"Mixed Schemes\",RTSP Camera");
    crlf("rtsp://camera.stream.invalid:554/stream");
    crlf("#EXTINF:-1 tvg-id=\"no.url\" group-title=\"Broken\",Entry Without URL");
    crlf("#EXTINF:-1 tvg-id=\"after.no.url\" group-title=\"Broken\",Entry After Missing URL");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(8)));
    crlf("");
    crlf("");
    crlf(&format!(
        "#EXTINF:-1 tvg-id=\"long.name\" group-title=\"Long\",{}",
        long_name.trim_end()
    ));
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(9)));
    // Two bare-LF lines in an otherwise CRLF file.
    s.push_str(
        "#EXTINF:-1 tvg-id=\"unicode.test\" tvg-name=\"Κύμα 🌊\" group-title=\"Ελληνικά\",Κύμα TV 🌊 日本語テスト\n",
    );
    s.push_str(&format!("{b}/live/demo/demo/{}.ts\n", live(10)));
    let mut crlf = |line: &str| {
        s.push_str(line);
        s.push_str("\r\n");
    };
    crlf(&format!(
        "#EXTINF:-1 tvg-id=\"catchup.test\" tvg-chno=\"42\" catchup=\"default\" catchup-days=\"3\" catchup-source=\"{b}/timeshift/demo/demo/{{duration}}/{{Y}}-{{m}}-{{d}}:{{H}}-{{M}}/{archive}.ts\" group-title=\"Catchup\",Catch-up Attributes"
    ));
    crlf(&format!("{b}/live/demo/demo/{archive}.ts"));
    crlf("#EXTINF:-1,Title Without Attributes");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(11)));
    crlf("#EXTINF:-1 tvg-id=\"no.title\" group-title=\"Broken\"");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(12)));
    crlf("#extinf:-1 tvg-id=\"lowercase\" group-title=\"Case\",Lowercase Directive");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(13)));
    crlf("#EXTINF:-1 tvg-id=\"padded\" group-title=\"Whitespace\"  ,   Padded Title   ");
    crlf(&format!("   {b}/live/demo/demo/{}.ts   ", live(14)));
    crlf("#EXTINF:-1 tvg-id=\"dup\" group-title=\"Duplicates\",Duplicate Entry");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(15)));
    crlf("#EXTINF:-1 tvg-id=\"dup\" group-title=\"Duplicates\",Duplicate Entry");
    crlf(&format!("{b}/live/demo/demo/{}.ts", live(15)));
    crlf("#EXTINF:-1 group-title=\"Movies\",VOD By Path");
    crlf(&format!("{b}/movie/demo/demo/{}", movie(0, "mkv")));
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",Local File (Unix)");
    crlf("file:///etc/passwd");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",Local File (Windows)");
    crlf("file:///C:/Windows/win.ini");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",Lavfi Source");
    crlf("lavfi://sine=frequency=1000");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",mpv av lavfi");
    crlf("av://lavfi:testsrc");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",mpv EDL");
    crlf("edl://file:///etc/passwd,0,10");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",mpv memory");
    crlf("memory://not-a-real-stream");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",Windows UNC Path");
    crlf("\\\\fileserver.invalid\\share\\video.ts");
    crlf("#EXTINF:-1 group-title=\"Must Not Play\",Relative Path");
    crlf("../../secrets/video.ts");
    s.push_str(
        "#EXTINF:-1 tvg-id=\"last\" group-title=\"End\",Last Entry Without Trailing Newline\r\n",
    );
    s.push_str(&format!("{b}/live/demo/demo/{}.ts", live(16)));
    s
}
