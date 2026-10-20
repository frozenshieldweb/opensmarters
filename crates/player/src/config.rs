// SPDX-License-Identifier: GPL-3.0-or-later
//! [`PlayerConfig`] and the hardened mpv option set derived from it, and [`StreamTuning`]
//! (per-load buffering).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::playable::{UrlPolicy, UserAgent};

/// libcurl connect timeout (`curl-connect-timeout`, seconds). mpv's default is 30 s; a
/// dead host should fail fast so the app can reconnect or say so.
pub const CURL_CONNECT_TIMEOUT_SECS: u32 = 10;
/// Redirects libcurl follows (`curl-max-redirects`; mpv's default is 16).
pub const CURL_MAX_REDIRECTS: u32 = 5;

/// Largest cache size [`StreamTuning`] accepts (8 GiB).
pub const MAX_CACHE_BYTES: u64 = 8 << 30;

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
    pub(crate) fn as_mpv_str(self) -> &'static str {
        match self {
            Self::AutoSafe => "auto-safe",
            Self::Disabled => "no",
        }
    }
}

/// How one stream is buffered. Pass it to [`crate::Player::load_with`] (sent as
/// `loadfile` per-file options, so it ends with the file) or apply it to the stream that
/// is playing with [`crate::Player::set_tuning`].
///
/// Every value is a number or a switch that this crate formats itself: nothing from
/// outside the crate reaches mpv as an option string.
///
/// ```
/// use std::time::Duration;
/// use opensmarters_player::StreamTuning;
/// // A live channel with a 30-minute rewind buffer on disk.
/// let live = StreamTuning {
///     start_buffer: Duration::from_secs(1),
///     forward_bytes: 1_800_000_000,
///     back_bytes: 1_800_000_000,
///     seekable_cache: true,
///     disk_cache: true,
///     ..StreamTuning::default()
/// };
/// # let _ = live;
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamTuning {
    /// Media to buffer before playback starts, and again after the buffer ran dry
    /// (`cache-pause-initial=yes` + `cache-pause-wait`). Zero starts at once and resumes
    /// after 1 s of buffer following a stall. At most 60 s.
    pub start_buffer: Duration,
    /// Forward cache cap in bytes (`demuxer-max-bytes`), 1 MiB to [`MAX_CACHE_BYTES`].
    /// On a live stream it also bounds how long a pause can last before mpv stops
    /// reading (data received while paused is ahead of the playback position).
    pub forward_bytes: u64,
    /// Backward cache cap in bytes (`demuxer-max-back-bytes`), up to [`MAX_CACHE_BYTES`]:
    /// how far back one can seek without reopening the stream (the live rewind window).
    pub back_bytes: u64,
    /// Keep the cache seekable even when the stream is not (a live TS has no `Range`
    /// support): `demuxer-seekable-cache=yes` + `force-seekable=yes`. Seeks then work
    /// inside what was received; seeks outside it fail. Off: mpv decides (`auto`, `no`).
    /// Needed for pause/rewind on live streams and for [`crate::Player::dump_cache`].
    pub seekable_cache: bool,
    /// Keep cached packets in a file in the player's cache folder
    /// ([`PlayerConfig::cache_dir`]) instead of in memory (`cache-on-disk`). Ignored when
    /// the player has no cache folder.
    pub disk_cache: bool,
}

impl Default for StreamTuning {
    /// mpv-like behaviour: start at once, 64 MiB ahead, 16 MiB behind, in memory.
    fn default() -> Self {
        Self {
            start_buffer: Duration::ZERO,
            forward_bytes: 64 << 20,
            back_bytes: 16 << 20,
            seekable_cache: false,
            disk_cache: false,
        }
    }
}

impl StreamTuning {
    /// The mpv options for this tuning, values formatted here. `disk_available`: the
    /// player has a cache folder.
    pub(crate) fn mpv_options(&self, disk_available: bool) -> Vec<(&'static str, String)> {
        let yes_no = |b: bool| if b { "yes" } else { "no" }.to_owned();
        let start = self.start_buffer.as_secs_f64().min(60.0);
        let wait = if start > 0.0 { start.max(0.1) } else { 1.0 };
        vec![
            ("cache-pause-initial", yes_no(start > 0.0)),
            ("cache-pause-wait", format!("{wait:.3}")),
            (
                "demuxer-max-bytes",
                self.forward_bytes
                    .clamp(1 << 20, MAX_CACHE_BYTES)
                    .to_string(),
            ),
            (
                "demuxer-max-back-bytes",
                self.back_bytes.min(MAX_CACHE_BYTES).to_string(),
            ),
            (
                "demuxer-seekable-cache",
                if self.seekable_cache { "yes" } else { "auto" }.to_owned(),
            ),
            ("force-seekable", yes_no(self.seekable_cache)),
            ("cache-on-disk", yes_no(self.disk_cache && disk_available)),
        ]
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
    pub(crate) cache_dir: Option<PathBuf>,
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
            cache_dir: None,
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

    /// Network connect/read timeout (`network-timeout`). Clamped to 1..=600 s. It governs
    /// what FFmpeg's own protocols fetch (`rtmp`/`rtsp`/`udp` when enabled). `http(s)` is
    /// read by libcurl in this mpv build, HLS segments included, and libcurl has only a
    /// connect timeout ([`CURL_CONNECT_TIMEOUT_SECS`]) and no read timeout: the app must
    /// detect stalls itself (see `docs/LIBMPV.md`).
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

    /// Folder for disk-backed caches ([`StreamTuning::disk_cache`]): `demuxer-cache-dir`.
    /// It must be an absolute path the app owns (e.g. `<data>/cache/player`); mpv creates
    /// one temporary file per stream there and deletes it when the stream closes
    /// (`demuxer-cache-unlink-files=whendone`: Windows cannot delete a file that is still
    /// open). The app should empty the folder at startup in case of a crash.
    /// [`crate::Player::new`] fails with [`crate::Error::InvalidArgument`] if the path is
    /// relative, not UTF-8, or contains control characters.
    #[must_use]
    pub fn cache_dir(mut self, dir: PathBuf) -> Self {
        self.cache_dir = Some(dir);
        self
    }

    /// Whether a cache folder is configured (disk-backed caches are possible).
    pub fn has_cache_dir(&self) -> bool {
        self.cache_dir.is_some()
    }

    /// The cache folder as the string given to mpv, if one is set and acceptable.
    pub(crate) fn cache_dir_str(&self) -> Result<Option<&str>, &'static str> {
        self.cache_dir
            .as_deref()
            .map(validate_cache_dir)
            .transpose()
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
            // libcurl reads top-level http(s) streams in this build (docs/LIBMPV.md):
            // fail fast on a dead host, and follow fewer redirects than mpv's 16.
            b(
                "curl-connect-timeout",
                s(&CURL_CONNECT_TIMEOUT_SECS.to_string()),
            ),
            b("curl-max-redirects", s(&CURL_MAX_REDIRECTS.to_string())),
        ];
        if let Ok(Some(dir)) = self.cache_dir_str() {
            v.push(m("demuxer-cache-dir", s(dir)));
            v.push(m("demuxer-cache-unlink-files", s("whendone")));
        }
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

/// Accepts an absolute, UTF-8 path without control characters (mpv expands a leading `~`,
/// so that is refused too).
fn validate_cache_dir(dir: &Path) -> Result<&str, &'static str> {
    let s = dir.to_str().ok_or("cache folder is not UTF-8")?;
    if !dir.is_absolute() || s.starts_with('~') {
        return Err("cache folder must be an absolute path");
    }
    if s.chars().any(char::is_control) {
        return Err("cache folder contains control characters");
    }
    Ok(s)
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

    fn tuning_value(opts: &[(&'static str, String)], name: &str) -> String {
        match opts.iter().find(|(n, _)| *n == name) {
            Some((_, v)) => v.clone(),
            None => panic!("tuning option {name} missing"),
        }
    }

    #[test]
    fn stream_tuning_formats_numbers_and_switches() {
        let live = StreamTuning {
            start_buffer: Duration::from_millis(2500),
            forward_bytes: 1_800_000_000,
            back_bytes: 1_800_000_000,
            seekable_cache: true,
            disk_cache: true,
        };
        let o = live.mpv_options(true);
        assert_eq!(tuning_value(&o, "cache-pause-initial"), "yes");
        assert_eq!(tuning_value(&o, "cache-pause-wait"), "2.500");
        assert_eq!(tuning_value(&o, "demuxer-max-bytes"), "1800000000");
        assert_eq!(tuning_value(&o, "demuxer-max-back-bytes"), "1800000000");
        assert_eq!(tuning_value(&o, "demuxer-seekable-cache"), "yes");
        assert_eq!(tuning_value(&o, "force-seekable"), "yes");
        assert_eq!(tuning_value(&o, "cache-on-disk"), "yes");
        // Without a cache folder the cache stays in memory.
        assert_eq!(
            tuning_value(&live.mpv_options(false), "cache-on-disk"),
            "no"
        );

        let d = StreamTuning::default().mpv_options(true);
        assert_eq!(tuning_value(&d, "cache-pause-initial"), "no");
        assert_eq!(tuning_value(&d, "cache-pause-wait"), "1.000");
        assert_eq!(tuning_value(&d, "demuxer-seekable-cache"), "auto");
        assert_eq!(tuning_value(&d, "force-seekable"), "no");
        assert_eq!(tuning_value(&d, "cache-on-disk"), "no");
        // Every option name is unique and every value is plain text.
        let mut names: Vec<_> = d.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), d.len());
    }

    #[test]
    fn stream_tuning_clamps() {
        let extreme = StreamTuning {
            start_buffer: Duration::from_secs(3600),
            forward_bytes: 0,
            back_bytes: u64::MAX,
            seekable_cache: false,
            disk_cache: false,
        };
        let o = extreme.mpv_options(true);
        assert_eq!(tuning_value(&o, "cache-pause-wait"), "60.000");
        assert_eq!(
            tuning_value(&o, "demuxer-max-bytes"),
            (1u64 << 20).to_string()
        );
        assert_eq!(
            tuning_value(&o, "demuxer-max-back-bytes"),
            MAX_CACHE_BYTES.to_string()
        );
        let tiny = StreamTuning {
            start_buffer: Duration::from_millis(1),
            ..StreamTuning::default()
        };
        assert_eq!(
            tuning_value(&tiny.mpv_options(false), "cache-pause-wait"),
            "0.100"
        );
        for (name, v) in o {
            assert!(
                v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'),
                "{name}={v}"
            );
        }
    }

    #[test]
    fn network_options_include_curl_limits() {
        let opts = PlayerConfig::default().mpv_options();
        assert_eq!(str_of(&opts, "curl-connect-timeout"), "10");
        assert_eq!(str_of(&opts, "curl-max-redirects"), "5");
        // Best effort: an mpv build without the libcurl stream skips them.
        assert!(!find(&opts, "curl-connect-timeout").mandatory);
    }

    #[test]
    fn cache_dir_must_be_absolute_plain_text() {
        let opts = PlayerConfig::default().mpv_options();
        assert!(opts.iter().all(|o| o.name != "demuxer-cache-dir"));
        assert!(!PlayerConfig::default().has_cache_dir());

        let dir = std::env::temp_dir().join("opensmarters-cache-test");
        let cfg = PlayerConfig::default().cache_dir(dir.clone());
        assert!(cfg.has_cache_dir());
        let opts = cfg.mpv_options();
        assert_eq!(
            str_of(&opts, "demuxer-cache-dir"),
            dir.to_str().unwrap_or_default()
        );
        assert_eq!(str_of(&opts, "demuxer-cache-unlink-files"), "whendone");
        assert!(find(&opts, "demuxer-cache-dir").mandatory);

        for bad in ["relative/dir", "~~/cache", "~/cache"] {
            let cfg = PlayerConfig::default().cache_dir(PathBuf::from(bad));
            assert!(cfg.cache_dir_str().is_err(), "{bad}");
            assert!(
                cfg.mpv_options()
                    .iter()
                    .all(|o| o.name != "demuxer-cache-dir")
            );
        }
        let ctl = PlayerConfig::default().cache_dir(std::env::temp_dir().join("a\nb"));
        assert!(ctl.cache_dir_str().is_err());
    }
}
