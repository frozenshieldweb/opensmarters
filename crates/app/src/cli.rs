// SPDX-License-Identifier: GPL-3.0-or-later
//! Command-line options, including the dev/test harness (see `docs/APP_ARCHITECTURE.md`
//! → Harness).

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
    /// `player:live=1001`, `settings:about`, `onboarding`, `profiles`.
    #[arg(long, value_name = "ROUTE")]
    pub route: Option<String>,

    /// Synthetic key presses, comma-separated: key names (`Down`, `Enter`, `Esc`,
    /// `Backspace`, `PageUp`, `5`, `Shift+Tab`…), `wait:<secs>`, `text:<chars>`,
    /// `shot:<png>` (screenshot), `quit`.
    #[arg(long, value_name = "SCRIPT")]
    pub keys: Option<String>,

    /// Take a screenshot this many seconds after the first frame, then exit.
    #[arg(long, value_name = "SECS", requires = "screenshot_out")]
    pub screenshot_after: Option<f64>,

    /// Where to write the `--screenshot-after` PNG.
    #[arg(long, value_name = "PNG", requires = "screenshot_after")]
    pub screenshot_out: Option<PathBuf>,

    /// Window inner size in physical pixels, e.g. `1920x1080`.
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    pub window_size: Option<(u32, u32)>,

    /// Force this many physical pixels per point (e.g. `1` for 1:1 screenshots on a
    /// scaled display). Ignored in TV mode.
    #[arg(long, value_name = "PPP")]
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

    /// Exit this many seconds after the first frame (harness).
    #[arg(long, value_name = "SECS")]
    pub exit_after: Option<f64>,
}

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
}
