// SPDX-License-Identifier: GPL-3.0-or-later
//! Command-line options, including the dev/test harness (see `docs/APP_ARCHITECTURE.md`
//! → Harness).
//!
//! **SR-03:** the harness flags (`--route`, `--keys`, `--screenshot-after`,
//! `--screenshot-out`, `--exit-after`, `--ppp`, `--window-size`) are only parsed in builds
//! with the `harness` cargo feature. Without it the fields still exist (always `None`) so
//! the rest of the app compiles unchanged, but the parser rejects the flags: a shortcut
//! or another program can't script key presses, write screenshots to arbitrary paths or
//! jump past the profile picker in a release build.

use std::path::PathBuf;

use clap::Parser;

/// opensmarters: a secure, open-source IPTV player.
#[derive(Debug, Clone, Parser, Default)]
#[command(name = "opensmarters", version, about)]
pub struct Cli {
    /// Keep the database, image cache and logs in this directory instead of the per-user
    /// data directory.
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,

    /// Use an in-memory database and an in-memory keychain: nothing is written to the real
    /// database or the OS keychain (logs/images go to `--data-dir` only if given).
    #[arg(long)]
    pub ephemeral: bool,

    /// Run the built-in fake provider and sign in to it (needs the `demo` cargo feature).
    #[arg(long)]
    pub demo: bool,

    /// Open this route once signed in, e.g. `home`, `live`, `live:category=3`,
    /// `player:live=1001`, `settings:about`, `onboarding`, `profiles` (harness builds).
    #[cfg_attr(feature = "harness", arg(long, value_name = "ROUTE"))]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub route: Option<String>,

    /// Synthetic key presses, comma-separated: key names (`Down`, `Enter`, `Esc`,
    /// `Backspace`, `PageUp`, `5`, `Shift+Tab`…), `wait:<secs>`, `text:<chars>`,
    /// `shot:<png>` (screenshot), `quit` (harness builds).
    #[cfg_attr(feature = "harness", arg(long, value_name = "SCRIPT"))]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub keys: Option<String>,

    /// Take a screenshot this many seconds after the first frame, then exit (harness
    /// builds).
    #[cfg_attr(
        feature = "harness",
        arg(long, value_name = "SECS", requires = "screenshot_out")
    )]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub screenshot_after: Option<f64>,

    /// Where to write the `--screenshot-after` PNG (harness builds).
    #[cfg_attr(
        feature = "harness",
        arg(long, value_name = "PNG", requires = "screenshot_after")
    )]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub screenshot_out: Option<PathBuf>,

    /// Window inner size in physical pixels, e.g. `1920x1080` (harness builds).
    #[cfg_attr(
        feature = "harness",
        arg(long, value_name = "WxH", value_parser = parse_size)
    )]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub window_size: Option<(u32, u32)>,

    /// Force this many physical pixels per point (e.g. `1` for 1:1 screenshots on a
    /// scaled display). Ignored in TV mode (harness builds).
    #[cfg_attr(feature = "harness", arg(long, value_name = "PPP"))]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub ppp: Option<f32>,

    /// TV mode for this run (zoom so the window is ~1100 pt wide).
    #[arg(long)]
    pub tv: bool,

    /// Also log to stderr (always on in debug builds).
    #[arg(long)]
    pub log_stderr: bool,

    /// mpv log verbosity forwarded to the (redacted) log: error, warn, info, v, debug.
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    pub mpv_log: String,

    /// Exit this many seconds after the first frame (harness builds).
    #[cfg_attr(feature = "harness", arg(long, value_name = "SECS"))]
    #[cfg_attr(not(feature = "harness"), arg(skip))]
    pub exit_after: Option<f64>,
}

#[cfg_attr(not(feature = "harness"), allow(dead_code))]
fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| "expected WxH, e.g. 1920x1080".to_owned())?;
    let w: u32 = w.trim().parse().map_err(|_| "bad width".to_owned())?;
    let h: u32 = h.trim().parse().map_err(|_| "bad height".to_owned())?;
    if !(320..=8192).contains(&w) || !(240..=8192).contains(&h) {
        return Err("size out of range".into());
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "harness")]
    #[test]
    fn parses_harness_flags() {
        let cli = Cli::try_parse_from([
            "opensmarters",
            "--ephemeral",
            "--route",
            "live",
            "--keys",
            "wait:1,Down,Enter",
            "--screenshot-after",
            "3",
            "--screenshot-out",
            "out.png",
            "--window-size",
            "1920x1080",
            "--tv",
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(cli.ephemeral && cli.tv);
        assert_eq!(cli.window_size, Some((1920, 1080)));
        assert_eq!(cli.route.as_deref(), Some("live"));
        assert!(Cli::try_parse_from(["opensmarters", "--screenshot-after", "3"]).is_err());
        assert!(Cli::try_parse_from(["opensmarters", "--window-size", "big"]).is_err());
    }

    #[cfg(not(feature = "harness"))]
    #[test]
    fn harness_flags_are_not_accepted_without_the_feature() {
        for args in [
            &["opensmarters", "--route", "onboarding"][..],
            &["opensmarters", "--keys", "Enter"],
            &[
                "opensmarters",
                "--screenshot-after",
                "1",
                "--screenshot-out",
                "x.png",
            ],
            &["opensmarters", "--exit-after", "1"],
            &["opensmarters", "--ppp", "1"],
            &["opensmarters", "--window-size", "1920x1080"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
        let cli = Cli::try_parse_from(["opensmarters", "--tv", "--ephemeral"])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(cli.tv && cli.route.is_none() && cli.keys.is_none());
    }

    #[test]
    fn window_sizes_are_validated() {
        assert_eq!(parse_size("1280x720"), Ok((1280, 720)));
        assert!(parse_size("10x10").is_err());
        assert!(parse_size("big").is_err());
    }
}
