// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzz the HLS playlist parser (recording HLS-only channels): no panics, the caps hold,
//! and every URI it returns is an http(s) web address.
#![no_main]

use libfuzzer_sys::fuzz_target;
use opensmarters_core::hls::{self, HlsLimits, Playlist};
use opensmarters_core::url::Url;

fuzz_target!(|data: &[u8]| {
    let limits = HlsLimits {
        max_bytes: 1 << 20,
        max_segments: 500,
        max_variants: 32,
        ..HlsLimits::default()
    };
    let Ok(base) = Url::parse("http://cdn.example.com/live/u/p/1/index.m3u8") else {
        return;
    };
    let web = |u: &Url| matches!(u.scheme(), "http" | "https") && u.host_str().is_some();
    let _ = hls::looks_like_playlist(data);
    match hls::parse(data, &base, &limits) {
        Ok(Playlist::Master(m)) => {
            assert!(!m.variants.is_empty() && m.variants.len() <= limits.max_variants);
            assert!(m.variants.iter().all(|v| web(&v.uri)));
            let _ = m.best();
            let _ = format!("{m:?}");
        }
        Ok(Playlist::Media(m)) => {
            assert!(m.segments.len() <= limits.max_segments);
            for s in &m.segments {
                assert!(web(&s.uri));
                assert!(s.duration.is_finite() && s.duration >= 0.0);
                assert!(s.init.as_ref().is_none_or(web));
            }
            assert!(m.segments.windows(2).all(|w| w[0].sequence < w[1].sequence || w[1].sequence == u64::MAX));
            let _ = m.reload_after();
            let _ = format!("{m:?}");
        }
        Err(e) => {
            let _ = e.to_string();
        }
    }
});
