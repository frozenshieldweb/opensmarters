// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzz the XMLTV parser (plain and gzip input).
#![no_main]

use libfuzzer_sys::fuzz_target;
use opensmarters_core::xmltv;

fuzz_target!(|data: &[u8]| {
    let limits = xmltv::XmltvLimits {
        max_decompressed_bytes: 16 << 20,
        max_channels: 10_000,
        max_programmes: 100_000,
        max_text_len: 4096,
        max_values_per_field: 8,
        max_token_bytes: 1 << 20,
    };
    if let Ok(epg) = xmltv::parse_bytes(data, &limits) {
        for (channel, progs) in &epg.programmes {
            assert!(progs.windows(2).all(|w| w[0].start <= w[1].start));
            for p in progs {
                assert_eq!(&p.channel, channel);
                assert!(p.stop.is_none_or(|s| s >= p.start));
            }
            let _ = epg.now_and_next(channel, opensmarters_core::chrono::Utc::now());
        }
    }
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = xmltv::parse_xmltv_time(s);
    }
});

