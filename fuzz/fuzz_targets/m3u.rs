// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzz the M3U parser and the get.php detector.
#![no_main]

use libfuzzer_sys::fuzz_target;
use opensmarters_core::catalog::{self, SortOrder};
use opensmarters_core::m3u;

fuzz_target!(|data: &[u8]| {
    let limits = m3u::M3uLimits {
        max_total_bytes: 1 << 20,
        max_line_len: 64 * 1024,
        max_entries: 100_000,
        ..m3u::M3uLimits::default()
    };
    if let Ok(pl) = m3u::parse(data, &limits) {
        assert!(pl.entries.len() <= limits.max_entries);
        assert!(pl.tvg_urls.len() <= limits.max_tvg_urls);
        for e in &pl.entries {
            assert!(e.attributes.len() <= limits.max_attributes_per_line);
            assert!(!e.url.is_empty());
            assert!(!e.url.starts_with('#'));
            assert!(!e.title.contains(['\r', '\n']));
            let _ = m3u::classify_url(&e.url);
        }
        let _ = pl.categories();
        let _ = catalog::sorted_indices(&pl.entries, SortOrder::NameAsc);
        let _ = format!("{pl:?}");
    }
    let text = String::from_utf8_lossy(data);
    let _ = m3u::detect_xtream(&text);
});
