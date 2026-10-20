// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzz the lenient Xtream JSON models, EPG decoding, URL building and redaction.
#![no_main]

use libfuzzer_sys::fuzz_target;
use opensmarters_core::de::{parse_list, parse_object};
use opensmarters_core::epg::{self, ServerTimeZone};
use opensmarters_core::model::{archive_programmes, parse_epg_listings};
use opensmarters_core::redact::{Redactor, redact_url};
use opensmarters_core::secrecy::SecretString;
use opensmarters_core::urls::{LiveContainer, StreamUrls, TimeshiftFormat};
use opensmarters_core::{
    Category, LiveStream, LoginInfo, Series, SeriesInfo, VodInfo, VodStream, XtreamAccount,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(l) = parse_list::<LiveStream>(data) {
        assert!(l.errors.len() <= opensmarters_core::de::MAX_ITEM_ERRORS);
    }
    let _ = parse_list::<VodStream>(data);
    let _ = parse_list::<Series>(data);
    let _ = parse_list::<Category>(data);
    let _ = parse_object::<VodInfo>(data);
    let _ = parse_object::<SeriesInfo>(data);

    let account = XtreamAccount::new("panel.example.com:8080", "user").expect("valid literal");
    let now = opensmarters_core::chrono::Utc::now();
    if let Ok(login) = LoginInfo::from_json(data) {
        let _ = login.check(now);
        let urls = StreamUrls::new(&login, &account, SecretString::from("pw".to_owned()));
        let _ = urls.live(1, LiveContainer::preferred(LiveContainer::Ts, &login.user_info));
        let tz = login.time_zone();
        if let Ok(l) = parse_epg_listings(data, &tz) {
            for p in archive_programmes(&l.items, &tz, now) {
                let _ = urls.timeshift(1, &p, TimeshiftFormat::Standard, LiveContainer::Ts);
            }
        }
    }
    let _ = parse_epg_listings(data, &ServerTimeZone::Utc);

    let text = String::from_utf8_lossy(data);
    let decoded = epg::decode_text(&text);
    assert!(decoded.len() <= epg::MAX_EPG_TEXT_LEN);
    let _ = epg::parse_server_datetime(&text);
    let _ = redact_url(&text);
    let r = Redactor::new();
    let prefix: String = text.chars().take(16).collect();
    r.add_secret(&prefix);
    let _ = r.redact(&text);
});
