// SPDX-License-Identifier: GPL-3.0-or-later
//! [`PlayerConfig`] and the hardened mpv option set derived from it.

use std::path::PathBuf;
use std::time::Duration;

use crate::playable::{UrlPolicy, UserAgent};

/// mpv log verbosity forwarded as [`crate::PlayerEvent::Log`].
///
/// Ordered from least to most verbose, so `level <= LogLevel::Warn` selects
/// fatal, error and warning lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum LogLevel {
    /// No log messages.
    None,
    /// Fatal errors only.
    Fatal,
    /// Errors.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational messages (mpv's default terminal level). Includes lines
    /// that echo stream URLs, so the app **must** redact before logging.
    #[default]
    Info,
    /// Verbose.
    Verbose,
    /// Debug (very noisy).
    Debug,
    /// Trace (extremely noisy).
    Trace,
}

impl LogLevel {
    /// mpv's name for this level (`mpv_request_log_messages` argument).
    pub fn as_mpv_str(self) -> &'static str {
        match self {
            Self::None => "no",
            Self::Fatal => "fatal",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Verbose => "v",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// Parses mpv's level name, as found in a log event.
    pub fn from_mpv_str(s: &str) -> Option<Self> {
        Some(match s {
            "no" => Self::None,
            "fatal" => Self::Fatal,
            "error" => Self::Error,
            "warn" => Self::Warn,
            "info" => Self::Info,
            "v" => Self::Verbose,
            "debug" => Self::Debug,
            "trace" => Self::Trace,
            _ => return None,
        })
    }
}

/// Hardware decoding mode (`hwdec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Hwdec {
    /// `auto-safe`: use a hardware decoder known to work, else software.
    #[default]
    AutoSafe,
    /// `no`: software decoding only.
    Disabled,
}

impl Hwdec {
    fn as_mpv_str(self) -> &'static str {
        match self {
            Self::AutoSafe => "auto-safe",
            Self::Disabled => "no",
        }
    }
}

/// Configuration for [`crate::Player::new`]. Build with the chained setters:
///
/// ```
/// use std::time::Duration;
/// use opensmarters_player::{PlayerConfig, UrlPolicy, UserAgent};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let cfg = PlayerConfig::default()
///     .user_agent(UserAgent::new("opensmarters/0.1.0")?)
///     .network_timeout(Duration::from_secs(20))
///     .url_policy(UrlPolicy { allow_rtmp: true, ..UrlPolicy::HTTP_ONLY });
/// # let _ = cfg; Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    pub(crate) url_policy: UrlPolicy,
    pub(crate) allow_local_files: bool,
    pub(crate) user_agent: Option<UserAgent>,
    pub(crate) network_timeout: Duration,
    pub(crate) hwdec: Hwdec,
    pub(crate) log_level: LogLevel,
    pub(crate) cache_mib: u32,
    pub(crate) back_cache_mib: u32,
    pub(crate) initial_volume: u8,
    pub(crate) libmpv_path: Option<PathBuf>,
    pub(crate) audio_output: bool,
    pub(crate) video_output: bool,
    pub(crate) seekable_cache: bool,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            url_policy: UrlPolicy::HTTP_ONLY,
            allow_local_files: false,
            user_agent: None,
            network_timeout: Duration::from_secs(15),
            hwdec: Hwdec::AutoSafe,
            log_level: LogLevel::Info,
            cache_mib: 64,
            back_cache_mib: 16,
            initial_volume: 100,
            libmpv_path: None,
            audio_output: true,
            video_output: true,
            seekable_cache: false,
        }
    }
}

impl PlayerConfig {
    /// Extra streaming schemes to accept (default: http/https only). Also
    /// sets the ffmpeg protocol whitelist.
    #[must_use]
    pub fn url_policy(mut self, policy: UrlPolicy) -> Self {
        self.url_policy = policy;
        self
    }

    /// Allow [`crate::PlayableUrl::from_local_file`] URLs. **Development and
    /// tests only**; also adds `file` to the ffmpeg protocol whitelist.
    #[must_use]
    pub fn allow_local_files(mut self, allow: bool) -> Self {
        self.allow_local_files = allow;
        self
    }

    /// Default HTTP User-Agent (mpv's own default is used if unset). A
    /// per-load value in [`crate::LoadOptions`] overrides it for that load.
    #[must_use]
    pub fn user_agent(mut self, ua: UserAgent) -> Self {
        self.user_agent = Some(ua);
        self
    }

    /// Network connect/read timeout (`network-timeout`). Clamped to 1..=600 s.
    #[must_use]
    pub fn network_timeout(mut self, timeout: Duration) -> Self {
        self.network_timeout = timeout.clamp(Duration::from_secs(1), Duration::from_secs(600));
        self
    }

    /// Hardware decoding mode.
    #[must_use]
    pub fn hwdec(mut self, hwdec: Hwdec) -> Self {
        self.hwdec = hwdec;
        self
    }

    /// Initial log level for [`crate::PlayerEvent::Log`] (changeable later).
    #[must_use]
    pub fn log_level(mut self, level: LogLevel) -> Self {
        self.log_level = level;
        self
    }

    /// Forward/backward demuxer cache caps in MiB (`demuxer-max-bytes`,
    /// `demuxer-max-back-bytes`). Clamped to 1..=2048.
    #[must_use]
    pub fn cache_mib(mut self, forward: u32, back: u32) -> Self {
        self.cache_mib = forward.clamp(1, 2048);
        self.back_cache_mib = back.clamp(1, 2048);
        self
    }

    /// Initial volume, 0..=100 (clamped).
    #[must_use]
    pub fn initial_volume(mut self, volume: u8) -> Self {
        self.initial_volume = volume.min(100);
        self
    }

    /// Load libmpv from this exact path first (e.g. a bundled copy).
    #[must_use]
    pub fn libmpv_path(mut self, path: PathBuf) -> Self {
        self.libmpv_path = Some(path);
        self
    }

    /// Disable audio output (`ao=null`), e.g. for tests on machines without
    /// an audio device or for muted preview tiles.
    #[must_use]
    pub fn audio_output(mut self, enabled: bool) -> Self {
        self.audio_output = enabled;
        self
    }

    /// Video output through a render context (default `true`). With `false`
    /// the player is audio-only (`vid=no`) and loads never wait for a
    /// [`crate::VideoView`]; use it for headless tests and audio previews.
    #[must_use]
    pub fn video_output(mut self, enabled: bool) -> Self {
        self.video_output = enabled;
        self
    }

    /// Keep the demuxer cache seekable even for streams the server cannot seek (live TS,
    /// HTTP without range support): `demuxer-seekable-cache=yes` + `force-seekable=yes`.
    /// Needed to seek back inside what was received (pause / rewind live TV) and for
    /// [`crate::Player::dump_cache`]. Seeks outside the cached range fail on such streams.
    /// Default `false`.
    #[must_use]
    pub fn seekable_cache(mut self, enabled: bool) -> Self {
        self.seekable_cache = enabled;
        self
    }

    /// The URL policy in effect.
    pub fn policy(&self) -> UrlPolicy {
        self.url_policy
    }

    /// Whether local files are allowed.
    pub fn local_files_allowed(&self) -> bool {
        self.allow_local_files
    }

    /// The full list of options applied before `mpv_initialize`.
    pub(crate) fn mpv_options(&self) -> Vec<MpvOption> {
        use OptValue::{Str, Whitelist};
        let m = |name: &'static str, value: OptValue| MpvOption {
            name,
            value,
            mandatory: true,
        };
        let b = |name: &'static str, value: OptValue| MpvOption {
            name,
            value,
            mandatory: false,
        };
        let s = |v: &str| Str(v.to_owned());
        let protocols = self.url_policy.ffmpeg_protocols(self.allow_local_files);

        let mut v = vec![
            // --- Security hardening (BUILD_PROMPT §3 rule 6). Mandatory: the
            // player refuses to start if any of these cannot be set.
            // No mpv.conf / input.conf / watch_later / script-opts files.
            m("config", s("no")),
            m("load-scripts", s("no")),
            // No youtube-dl/yt-dlp hook (would run an external program on URLs).
            m("ytdl", s("no")),
            m("osc", s("no")),
            m("input-default-bindings", s("no")),
            m("input-vo-keyboard", s("no")),
            m("input-terminal", s("no")),
            m("terminal", s("no")),
            m("input-ipc-server", s("")),
            // Playlists fetched from the network may only reference network
            // URLs (mpv's stream-origin rules).
            m("load-unsafe-playlists", s("no")),
            m("use-filedir-conf", s("no")),
            m("resume-playback", s("no")),
            m("save-position-on-quit", s("no")),
            // Never follow mkv ordered-chapter/segment links to other files.
            m("ordered-chapters", s("no")),
            // ffmpeg protocol allowlist for nested opens (HLS variant
            // playlists, segments, keys) done by libavformat itself.
            m("demuxer-lavf-o", Whitelist(protocols.clone())),
            // Same allowlist for the top-level stream opened via ffmpeg.
            m("stream-lavf-o", Whitelist(protocols)),
            // HTTPS certificate verification (ffmpeg's default is off).
            m("tls-verify", s("yes")),
            // --- Embedding.
            m("vo", s("libmpv")),
            m("idle", s("yes")),
            m("force-window", s("no")),
            // VOD end produces END_FILE(reason=eof) and the core goes idle.
            m("keep-open", s("no")),
            // --- Playback / network.
            m("hwdec", s(self.hwdec.as_mpv_str())),
            m(
                "network-timeout",
                s(&self.network_timeout.as_secs().to_string()),
            ),
            m("demuxer-max-bytes", s(&format!("{}MiB", self.cache_mib))),
            m(
                "demuxer-max-back-bytes",
                s(&format!("{}MiB", self.back_cache_mib)),
            ),
            m("volume", s(&self.initial_volume.to_string())),
            // --- Best effort: options that only exist in some mpv versions
            // or builds. Skipped (and reported) if mpv does not know them.
            b("input-builtin-bindings", s("no")),
            b("input-media-keys", s("no")),
            b("load-stats-overlay", s("no")),
            b("load-console", s("no")),
            b("load-osd-console", s("no")),
            b("load-auto-profiles", s("no")),
            b("load-select", s("no")),
            b("load-commands", s("no")),
            b("load-positioning", s("no")),
            b("load-context-menu", s("no")),
            // No auto-loading of sibling files next to (local) media.
            b("autoload-files", s("no")),
            b("sub-auto", s("no")),
            b("audio-file-auto", s("no")),
            b("cover-art-auto", s("no")),
            // Don't parse fonts embedded in media (FreeType attack surface).
            b("embeddedfonts", s("no")),
            // The app draws its own OSD.
            b("osd-level", s("0")),
            b("osd-on-seek", s("no")),
            // Rendering never blocks the UI thread waiting for frame timing.
            b("video-timing-offset", s("0")),
            b("audio-client-name", s("opensmarters")),
        ];
        if let Some(ua) = &self.user_agent {
            v.push(m("user-agent", s(ua.as_str())));
        }
        if !self.audio_output {
            v.push(m("ao", s("null")));
        }
        if !self.video_output {
            v.push(m("vid", s("no")));
        }
        if self.seekable_cache {
            v.push(m("demuxer-seekable-cache", s("yes")));
            v.push(m("force-seekable", s("yes")));
        }
        v
    }
}

/// One option applied with `mpv_set_option*` before `mpv_initialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MpvOption {
    pub name: &'static str,
    pub value: OptValue,
    /// If true, failure to set it aborts player creation.
    pub mandatory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptValue {
    /// Plain string value via `mpv_set_option_string`.
    Str(String),
    /// Key-value list `{protocol_whitelist: <value>}` passed as an
    /// `MPV_FORMAT_NODE_MAP`, so the commas in the value need no escaping.
    Whitelist(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(opts: &'a [MpvOption], name: &str) -> &'a MpvOption {
        match opts.iter().find(|o| o.name == name) {
            Some(o) => o,
            None => panic!("option {name} missing"),
        }
    }

    fn str_of(opts: &[MpvOption], name: &str) -> String {
        match &find(opts, name).value {
            OptValue::Str(s) => s.clone(),
            OptValue::Whitelist(w) => format!("whitelist:{w}"),
        }
    }

    #[test]
    fn hardened_options_are_mandatory_and_set() {
        let opts = PlayerConfig::default().mpv_options();
        for (name, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("ytdl", "no"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("terminal", "no"),
            ("load-unsafe-playlists", "no"),
            ("resume-playback", "no"),
            ("tls-verify", "yes"),
            ("vo", "libmpv"),
            ("hwdec", "auto-safe"),
            ("keep-open", "no"),
            ("input-ipc-server", ""),
        ] {
            let o = find(&opts, name);
            assert!(o.mandatory, "{name} must be mandatory");
            assert_eq!(str_of(&opts, name), value, "{name}");
        }
        assert_eq!(
            str_of(&opts, "demuxer-lavf-o"),
            "whitelist:http,https,tls,tcp,crypto,data,httpproxy"
        );
        assert_eq!(
            str_of(&opts, "stream-lavf-o"),
            "whitelist:http,https,tls,tcp,crypto,data,httpproxy"
        );
        assert!(opts.iter().all(|o| o.name != "user-agent"));
        assert!(opts.iter().all(|o| o.name != "ao"));
    }

    #[test]
    fn option_names_are_unique() {
        let opts = PlayerConfig::default()
            .user_agent(UserAgent::new("x").unwrap_or_else(|e| panic!("{e}")))
            .audio_output(false)
            .mpv_options();
        let mut names: Vec<_> = opts.iter().map(|o| o.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }

    #[test]
    fn config_values_flow_into_options() {
        let ua = UserAgent::new("IPTVSmartersPro").unwrap_or_else(|e| panic!("{e}"));
        let opts = PlayerConfig::default()
            .user_agent(ua)
            .network_timeout(Duration::from_millis(1))
            .hwdec(Hwdec::Disabled)
            .cache_mib(0, 5000)
            .initial_volume(250)
            .allow_local_files(true)
            .audio_output(false)
            .url_policy(UrlPolicy::ALL_STREAMING)
            .mpv_options();
        assert_eq!(str_of(&opts, "user-agent"), "IPTVSmartersPro");
        assert_eq!(str_of(&opts, "network-timeout"), "1");
        assert_eq!(str_of(&opts, "hwdec"), "no");
        assert_eq!(str_of(&opts, "demuxer-max-bytes"), "1MiB");
        assert_eq!(str_of(&opts, "demuxer-max-back-bytes"), "2048MiB");
        assert_eq!(str_of(&opts, "volume"), "100");
        assert_eq!(str_of(&opts, "ao"), "null");
        let wl = str_of(&opts, "demuxer-lavf-o");
        assert!(wl.contains(",file"), "{wl}");
        assert!(wl.contains("rtmp") && wl.contains("rtsp") && wl.contains("udp"));
    }

    #[test]
    fn option_values_have_no_control_chars() {
        let opts = PlayerConfig::default().mpv_options();
        for o in opts {
            let v = match o.value {
                OptValue::Str(s) | OptValue::Whitelist(s) => s,
            };
            assert!(!v.chars().any(char::is_control), "{}", o.name);
        }
    }

    #[test]
    fn log_level_names_round_trip() {
        for l in [
            LogLevel::None,
            LogLevel::Fatal,
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Verbose,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            assert_eq!(LogLevel::from_mpv_str(l.as_mpv_str()), Some(l));
        }
        assert_eq!(LogLevel::from_mpv_str("bogus"), None);
    }
}
