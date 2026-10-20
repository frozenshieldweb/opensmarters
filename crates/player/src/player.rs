// SPDX-License-Identifier: GPL-3.0-or-later
//! [`Player`]: a hardened mpv core, its event thread and observable state.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError};

use crate::config::{Hwdec, LogLevel, MpvOption, OptValue, PlayerConfig, StreamTuning};
use crate::error::{Error, MpvError};
use crate::ffi::{self, MpvApi, mpv_handle};
use crate::node::{NodeIn, NodeOut, OwnedNode};
use crate::playable::{MediaOutput, PlayableUrl, Referrer, UserAgent};

/// Capacity of the event channel. Events beyond it are dropped (and counted
/// in [`PlayerState::dropped_events`]); state is never lost because it is
/// also kept in [`PlayerState`].
const EVENT_CAPACITY: usize = 4096;
/// Cap on media-provided strings (track titles, media title).
const MAX_MEDIA_STRING_CHARS: usize = 256;

static NEXT_CORE_ID: AtomicU64 = AtomicU64::new(1);

/// An mpv core handle. Destroyed (`mpv_terminate_destroy`) when the last
/// reference goes away; a [`crate::RenderContext`] holds one, so the core
/// always outlives its render context as libmpv requires.
pub(crate) struct Core {
    pub(crate) api: &'static MpvApi,
    handle: NonNull<mpv_handle>,
    pub(crate) id: u64,
    video: Mutex<VideoAttach>,
}

/// Coordination between `Player::load` and the render context.
///
/// libmpv fails `vo=libmpv` (and disables video for that file) if playback
/// starts before a render context exists, and re-enabling video later needs
/// a seek, which live streams cannot do. So when video output is enabled,
/// loads issued before a render context is attached are held here and sent
/// the moment one attaches.
struct VideoAttach {
    /// Video output enabled in the config (otherwise loads never wait).
    enabled: bool,
    attached: bool,
    pending: Option<ResolvedLoad>,
}

/// A load with all values resolved to the exact strings sent to mpv.
#[derive(Clone)]
struct ResolvedLoad {
    url: PlayableUrl,
    user_agent: String,
    referrer: String,
    /// Per-file options: crate-defined names, values formatted by this crate (the resume
    /// position and [`StreamTuning`]).
    file_options: Vec<(&'static str, String)>,
}

// SAFETY: the libmpv client API is documented as fully thread-safe for a
// single mpv_handle. The one exception, mpv_wait_event (not reentrant), is
// only ever called from the Player's event thread.
unsafe impl Send for Core {}
// SAFETY: see `Send` above; all methods take `&self` and libmpv serialises
// internally.
unsafe impl Sync for Core {}

impl Core {
    pub(crate) fn raw(&self) -> *mut mpv_handle {
        self.handle.as_ptr()
    }

    fn mpv_err(&self, code: c_int) -> MpvError {
        MpvError {
            code,
            message: self.api.error_string(code),
        }
    }

    fn video(&self) -> MutexGuard<'_, VideoAttach> {
        self.video.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Called by `RenderContext` right after it was created. Sends a load
    /// that was waiting for video; otherwise re-selects the video track in
    /// case a file is already playing (its VO failed or was torn down when a
    /// previous render context went away). Only async mpv calls: safe on the
    /// render thread.
    pub(crate) fn render_attached(&self) {
        let pending = {
            let mut v = self.video();
            v.attached = true;
            v.pending.take()
        };
        if let Some(load) = pending {
            // Errors surface as CommandFailed events.
            let _ = issue_load(self, &load);
        } else {
            // Setting `auto` alone is a no-op when the option already says
            // `auto`, so toggle through `no` (requests run in order). For a
            // non-seekable live stream this may not bring video back; the
            // app should reload such a stream after re-attaching a view.
            let _ = set_string_async(self, Op::Other, c"vid", "no");
            let _ = set_string_async(self, Op::Other, c"vid", "auto");
        }
    }

    /// Called by `RenderContext::drop`.
    pub(crate) fn render_detached(&self) {
        self.video().attached = false;
    }

    /// Synchronous string property read. Only used during construction
    /// (before any render context exists), never from a render thread.
    fn get_string_sync(&self, name: &CStr) -> Option<String> {
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: valid handle; `name` is NUL-terminated; for
        // MPV_FORMAT_STRING mpv writes a `char*` into `out`.
        let r = unsafe {
            (self.api.get_property)(
                self.raw(),
                name.as_ptr(),
                ffi::MPV_FORMAT_STRING,
                (&raw mut out).cast::<c_void>(),
            )
        };
        if r < 0 || out.is_null() {
            return None;
        }
        // SAFETY: mpv returned a NUL-terminated string it allocated.
        let s = unsafe { ffi::opt_string(out) };
        // SAFETY: strings returned by mpv_get_property must be freed with mpv_free.
        unsafe { (self.api.free)(out.cast::<c_void>()) };
        s
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // SAFETY: we own the handle and this is the last reference. Any
        // render context was freed first (it holds an Arc<Core>), and the
        // event thread has exited (it holds an Arc<Core> too).
        unsafe { (self.api.terminate_destroy)(self.raw()) };
    }
}

/// Which request a failed asynchronous call belonged to
/// ([`PlayerEvent::CommandFailed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    /// [`Player::load`]
    Load,
    /// [`Player::stop`]
    Stop,
    /// pause / play / toggle
    Pause,
    /// seeks
    Seek,
    /// volume
    Volume,
    /// mute
    Mute,
    /// speed
    Speed,
    /// audio track selection
    AudioTrack,
    /// subtitle track selection
    SubtitleTrack,
    /// aspect / zoom / panscan
    VideoFit,
    /// user-agent / referrer
    HttpHeaders,
    /// runtime tuning (cache, buffering, decoding)
    Tuning,
    /// [`Player::set_stream_record`]
    Record,
    /// [`Player::dump_cache`] (also reported on success: [`PlayerEvent::CommandDone`])
    DumpCache,
    /// unknown reply id
    Other,
}

impl Op {
    const ALL: [Self; 14] = [
        Self::Load,
        Self::Stop,
        Self::Pause,
        Self::Seek,
        Self::Volume,
        Self::Mute,
        Self::Speed,
        Self::AudioTrack,
        Self::SubtitleTrack,
        Self::VideoFit,
        Self::HttpHeaders,
        Self::Tuning,
        Self::Record,
        Self::DumpCache,
    ];

    fn id(self) -> u64 {
        Self::ALL
            .iter()
            .position(|o| *o == self)
            .map_or(0, |i| i as u64 + 1)
    }

    fn from_id(id: u64) -> Self {
        usize::try_from(id)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or(Self::Other)
    }
}

/// Observed mpv properties. [`PlayerEvent::PropertyChanged`] is sent for the
/// low-frequency ones; all of them are reflected in [`PlayerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Property {
    /// `time-pos` (state only, no event: changes every frame)
    TimePos,
    /// `duration`
    Duration,
    /// `pause`
    Pause,
    /// `idle-active`
    Idle,
    /// `core-idle`
    CoreIdle,
    /// `paused-for-cache`
    PausedForCache,
    /// `cache-buffering-state` (state only)
    CacheBufferingState,
    /// `demuxer-cache-duration` (state only)
    DemuxerCacheDuration,
    /// `seekable`
    Seekable,
    /// `seeking`
    Seeking,
    /// `eof-reached`
    EofReached,
    /// `track-list`
    TrackList,
    /// `dwidth`
    VideoWidth,
    /// `dheight`
    VideoHeight,
    /// `media-title`
    MediaTitle,
    /// `volume`
    Volume,
    /// `mute`
    Mute,
    /// `speed`
    Speed,
    /// `video-aspect-override`
    VideoAspectOverride,
    /// `video-zoom`
    VideoZoom,
    /// `panscan`
    Panscan,
    /// `keepaspect`
    KeepAspect,
    /// `hwdec-current`
    HwdecCurrent,
    /// `width` (source size, before aspect correction)
    SourceWidth,
    /// `height`
    SourceHeight,
    /// `container-fps`
    ContainerFps,
    /// `video-format` (short codec name)
    VideoFormat,
    /// `audio-codec-name`
    AudioCodec,
    /// `video-bitrate` (state only: updated every second)
    VideoBitrate,
    /// `audio-bitrate` (state only)
    AudioBitrate,
    /// `cache-speed` (state only)
    CacheSpeed,
    /// `frame-drop-count` (state only)
    FrameDrops,
    /// `decoder-frame-drop-count` (state only)
    DecoderFrameDrops,
    /// `demuxer-cache-state` (state only: changes several times a second)
    CacheState,
}

struct PropSpec {
    prop: Property,
    name: &'static CStr,
    format: ffi::mpv_format,
    notify: bool,
}

const PROPS: &[PropSpec] = &[
    PropSpec {
        prop: Property::TimePos,
        name: c"time-pos",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: false,
    },
    PropSpec {
        prop: Property::Duration,
        name: c"duration",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::Pause,
        name: c"pause",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::Idle,
        name: c"idle-active",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::CoreIdle,
        name: c"core-idle",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::PausedForCache,
        name: c"paused-for-cache",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::CacheBufferingState,
        name: c"cache-buffering-state",
        format: ffi::MPV_FORMAT_INT64,
        notify: false,
    },
    PropSpec {
        prop: Property::DemuxerCacheDuration,
        name: c"demuxer-cache-duration",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: false,
    },
    PropSpec {
        prop: Property::Seekable,
        name: c"seekable",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::Seeking,
        name: c"seeking",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::EofReached,
        name: c"eof-reached",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::TrackList,
        name: c"track-list",
        format: ffi::MPV_FORMAT_NODE,
        notify: true,
    },
    PropSpec {
        prop: Property::VideoWidth,
        name: c"dwidth",
        format: ffi::MPV_FORMAT_INT64,
        notify: true,
    },
    PropSpec {
        prop: Property::VideoHeight,
        name: c"dheight",
        format: ffi::MPV_FORMAT_INT64,
        notify: true,
    },
    PropSpec {
        prop: Property::MediaTitle,
        name: c"media-title",
        format: ffi::MPV_FORMAT_STRING,
        notify: true,
    },
    PropSpec {
        prop: Property::Volume,
        name: c"volume",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::Mute,
        name: c"mute",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::Speed,
        name: c"speed",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::VideoAspectOverride,
        name: c"video-aspect-override",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::VideoZoom,
        name: c"video-zoom",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::Panscan,
        name: c"panscan",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::KeepAspect,
        name: c"keepaspect",
        format: ffi::MPV_FORMAT_FLAG,
        notify: true,
    },
    PropSpec {
        prop: Property::HwdecCurrent,
        name: c"hwdec-current",
        format: ffi::MPV_FORMAT_STRING,
        notify: true,
    },
    // Stream details (health overlay) and the live rewind window.
    PropSpec {
        prop: Property::SourceWidth,
        name: c"width",
        format: ffi::MPV_FORMAT_INT64,
        notify: true,
    },
    PropSpec {
        prop: Property::SourceHeight,
        name: c"height",
        format: ffi::MPV_FORMAT_INT64,
        notify: true,
    },
    PropSpec {
        prop: Property::ContainerFps,
        name: c"container-fps",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: true,
    },
    PropSpec {
        prop: Property::VideoFormat,
        name: c"video-format",
        format: ffi::MPV_FORMAT_STRING,
        notify: true,
    },
    PropSpec {
        prop: Property::AudioCodec,
        name: c"audio-codec-name",
        format: ffi::MPV_FORMAT_STRING,
        notify: true,
    },
    PropSpec {
        prop: Property::VideoBitrate,
        name: c"video-bitrate",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: false,
    },
    PropSpec {
        prop: Property::AudioBitrate,
        name: c"audio-bitrate",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: false,
    },
    PropSpec {
        prop: Property::CacheSpeed,
        name: c"cache-speed",
        format: ffi::MPV_FORMAT_DOUBLE,
        notify: false,
    },
    PropSpec {
        prop: Property::FrameDrops,
        name: c"frame-drop-count",
        format: ffi::MPV_FORMAT_INT64,
        notify: false,
    },
    PropSpec {
        prop: Property::DecoderFrameDrops,
        name: c"decoder-frame-drop-count",
        format: ffi::MPV_FORMAT_INT64,
        notify: false,
    },
    PropSpec {
        prop: Property::CacheState,
        name: c"demuxer-cache-state",
        format: ffi::MPV_FORMAT_NODE,
        notify: false,
    },
];

/// Why playback of a file ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Reached the end. For VOD this means **finished**, not an error. For a
    /// live stream it means the server closed the connection.
    Eof,
    /// Stopped by [`Player::stop`] or replaced by another [`Player::load`].
    Stop,
    /// The core is shutting down.
    Quit,
    /// Failed to open or play (network error, unsupported format…).
    Error(MpvError),
    /// The URL resolved to a playlist and mpv moved on to its entries.
    Redirect,
    /// A reason this crate does not know.
    Unknown(i32),
}

/// One mpv log line, forwarded **raw**: it may contain stream URLs with
/// credentials. The app must redact it before logging or displaying it.
#[derive(Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Severity.
    pub level: LogLevel,
    /// mpv module prefix (e.g. `ffmpeg`, `cplayer`, `vo/libmpv`).
    pub prefix: String,
    /// Message text without the trailing newline. **Unredacted.**
    pub text: String,
}

impl std::fmt::Debug for LogLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the raw text through Debug.
        f.debug_struct("LogLine")
            .field("level", &self.level)
            .field("prefix", &self.prefix)
            .field(
                "text",
                &format_args!("<{} bytes, unredacted>", self.text.len()),
            )
            .finish()
    }
}

/// Events from the mpv core, in order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PlayerEvent {
    /// A [`Player::load`] started opening the file.
    StartFile,
    /// The file was opened and its tracks are known.
    FileLoaded,
    /// Playback of the current file ended.
    EndFile(EndReason),
    /// Playback (re)started after loading or seeking.
    PlaybackRestart,
    /// A seek started.
    Seek,
    /// Video output parameters changed (size, aspect…).
    VideoReconfig,
    /// Audio output parameters changed.
    AudioReconfig,
    /// A low-frequency observed property changed; read [`Player::state`].
    PropertyChanged(Property),
    /// An mpv log line (unredacted; see [`LogLine`]).
    Log(LogLine),
    /// A long-running request finished successfully (only sent for
    /// [`Op::DumpCache`]).
    CommandDone {
        /// Which request.
        op: Op,
    },
    /// An asynchronous request failed.
    CommandFailed {
        /// Which request.
        op: Op,
        /// mpv's error.
        error: MpvError,
    },
    /// mpv's internal event queue overflowed; some events were lost.
    QueueOverflow,
    /// The core shut down.
    Shutdown,
}

/// Kind of a [`Track`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// Video (or cover art).
    Video,
    /// Audio.
    Audio,
    /// Subtitles.
    Subtitle,
}

/// One entry of mpv's `track-list`. Strings come from the media file
/// (provider-controlled): control characters are stripped and lengths
/// capped, but treat them as untrusted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// mpv track id, unique per kind (pass to [`TrackSelection::Id`]).
    pub id: i64,
    /// Track type.
    pub kind: TrackKind,
    /// Title tag, if any.
    pub title: Option<String>,
    /// Language tag, if any.
    pub lang: Option<String>,
    /// Codec name, if known.
    pub codec: Option<String>,
    /// Marked as default in the container.
    pub default: bool,
    /// Marked as forced.
    pub forced: bool,
    /// Currently selected.
    pub selected: bool,
    /// Loaded from an external file.
    pub external: bool,
    /// Audio channel count, if known.
    pub channels: Option<i64>,
}

impl Track {
    /// A human-readable label such as `"#2 eng – Commentary (aac)"`.
    pub fn label(&self) -> String {
        let mut s = format!("#{}", self.id);
        if let Some(l) = &self.lang {
            s.push(' ');
            s.push_str(l);
        }
        if let Some(t) = &self.title {
            s.push_str(" – ");
            s.push_str(t);
        }
        if let Some(c) = &self.codec {
            s.push_str(" (");
            s.push_str(c);
            s.push(')');
        }
        s
    }
}

/// Which audio/subtitle track to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackSelection {
    /// mpv's automatic choice (`auto`).
    Auto,
    /// No track (`no`).
    Off,
    /// A specific [`Track::id`].
    Id(i64),
}

/// Aspect-ratio handling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Aspect {
    /// The stream's own aspect ratio.
    Original,
    /// Force `num:den` (e.g. 16:9, 4:3).
    Ratio(u32, u32),
    /// Stretch to fill the view, ignoring aspect (`keepaspect=no`).
    Stretch,
}

/// How the video is fitted into the view: aspect override, crop-to-fill
/// (`panscan`) and zoom. Set with [`Player::set_video_fit`]; it is stored in
/// global mpv options, so it survives rebuffering, seeking and loading the
/// next stream, and the player re-applies it on every `FileLoaded` in case
/// anything reset it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoFit {
    /// Aspect handling.
    pub aspect: Aspect,
    /// 0.0 = letterbox (whole picture visible), 1.0 = crop to fill.
    pub panscan: f64,
    /// Zoom as log2 factor (0.0 = none, 1.0 = 2×), mpv's `video-zoom`.
    pub zoom: f64,
}

impl Default for VideoFit {
    fn default() -> Self {
        Self::ORIGINAL
    }
}

impl VideoFit {
    /// Original aspect, letterboxed, no zoom.
    pub const ORIGINAL: Self = Self {
        aspect: Aspect::Original,
        panscan: 0.0,
        zoom: 0.0,
    };

    /// Presets suitable for an "aspect" button that cycles through them.
    pub const PRESETS: [(&'static str, Self); 6] = [
        ("Original", Self::ORIGINAL),
        (
            "16:9",
            Self {
                aspect: Aspect::Ratio(16, 9),
                panscan: 0.0,
                zoom: 0.0,
            },
        ),
        (
            "4:3",
            Self {
                aspect: Aspect::Ratio(4, 3),
                panscan: 0.0,
                zoom: 0.0,
            },
        ),
        (
            "2.35:1",
            Self {
                aspect: Aspect::Ratio(235, 100),
                panscan: 0.0,
                zoom: 0.0,
            },
        ),
        (
            "Stretch",
            Self {
                aspect: Aspect::Stretch,
                panscan: 0.0,
                zoom: 0.0,
            },
        ),
        (
            "Crop to fill",
            Self {
                aspect: Aspect::Original,
                panscan: 1.0,
                zoom: 0.0,
            },
        ),
    ];

    /// Index of `self` in [`Self::PRESETS`], if it is one.
    pub fn preset_index(&self) -> Option<usize> {
        Self::PRESETS.iter().position(|(_, p)| p == self)
    }

    /// The next preset after `self` (wrapping), for cycling.
    pub fn next_preset(&self) -> Self {
        let i = self
            .preset_index()
            .map_or(0, |i| (i + 1) % Self::PRESETS.len());
        Self::PRESETS[i].1
    }

    fn aspect_override_str(&self) -> String {
        match self.aspect {
            Aspect::Ratio(n, d) if n > 0 && d > 0 => format!("{n}:{d}"),
            _ => "no".to_owned(),
        }
    }

    fn keepaspect(&self) -> bool {
        !matches!(self.aspect, Aspect::Stretch)
    }

    fn sanitized(self) -> Self {
        let clamp = |v: f64, lo: f64, hi: f64| if v.is_finite() { v.clamp(lo, hi) } else { 0.0 };
        Self {
            aspect: self.aspect,
            panscan: clamp(self.panscan, 0.0, 1.0),
            zoom: clamp(self.zoom, -2.0, 3.0),
        }
    }
}

/// Snapshot of the observed player state ([`Player::state`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerState {
    /// Playback position in seconds (`time-pos`).
    pub time_pos: Option<f64>,
    /// Duration in seconds, if known (live streams usually have none).
    pub duration: Option<f64>,
    /// Paused by the user/app (`pause`).
    pub paused: bool,
    /// Nothing loaded (`idle-active`).
    pub idle: bool,
    /// Playback is not advancing for any reason (`core-idle`).
    pub core_idle: bool,
    /// Waiting for the network: rebuffering (`paused-for-cache`).
    pub paused_for_cache: bool,
    /// Buffer fill 0..=100 while rebuffering (`cache-buffering-state`).
    pub cache_buffering_percent: Option<i64>,
    /// Seconds of media buffered ahead (`demuxer-cache-duration`).
    pub demuxer_cache_duration: Option<f64>,
    /// Seeking is possible (`seekable`).
    pub seekable: bool,
    /// A seek is in progress (`seeking`).
    pub seeking: bool,
    /// Reached end of file (`eof-reached`).
    pub eof_reached: bool,
    /// Between `FileLoaded` and `EndFile`.
    pub file_loaded: bool,
    /// Between `StartFile` and `FileLoaded`/`EndFile` (opening).
    pub loading: bool,
    /// Why the last file ended (cleared by the next `StartFile`).
    pub last_end: Option<EndReason>,
    /// All tracks of the current file.
    pub tracks: Vec<Track>,
    /// Display width after aspect correction (`dwidth`).
    pub video_width: Option<u32>,
    /// Display height after aspect correction (`dheight`).
    pub video_height: Option<u32>,
    /// `media-title` (untrusted: from the stream's metadata or URL).
    pub media_title: Option<String>,
    /// Volume 0..=130.
    pub volume: f64,
    /// Muted.
    pub muted: bool,
    /// Playback speed.
    pub speed: f64,
    /// Current `video-aspect-override` (<= 0 means no override; mpv reports
    /// `no` as -2).
    pub video_aspect_override: f64,
    /// Current `video-zoom`.
    pub video_zoom: f64,
    /// Current `panscan`.
    pub panscan: f64,
    /// Current `keepaspect`.
    pub keepaspect: bool,
    /// Active hardware decoder (`hwdec-current`), if any.
    pub hwdec_current: Option<String>,
    /// Source video width before aspect correction (`width`).
    pub source_width: Option<u32>,
    /// Source video height (`height`).
    pub source_height: Option<u32>,
    /// Frame rate the container declares (`container-fps`).
    pub fps: Option<f64>,
    /// Video codec short name such as `h264` (`video-format`; from the media, sanitised).
    pub video_codec: Option<String>,
    /// Audio codec short name such as `aac` (`audio-codec-name`; sanitised).
    pub audio_codec: Option<String>,
    /// Video bitrate in bits/s, measured on packets (`video-bitrate`).
    pub video_bitrate: Option<f64>,
    /// Audio bitrate in bits/s (`audio-bitrate`).
    pub audio_bitrate: Option<f64>,
    /// Download speed in bytes/s (`cache-speed`).
    pub cache_speed: Option<f64>,
    /// Frames dropped by the video output (`frame-drop-count`).
    pub frame_drops: Option<i64>,
    /// Frames dropped by the decoder (`decoder-frame-drop-count`).
    pub decoder_frame_drops: Option<i64>,
    /// The demuxer cache: seekable ranges, newest packet, bytes ahead
    /// (`demuxer-cache-state`).
    pub cache: CacheState,
    /// Events dropped because the event channel was full.
    pub dropped_events: u64,
}

/// A time range (seconds on the `time-pos` timeline) held in the demuxer cache.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeekRange {
    /// Oldest position that can be seeked to.
    pub start: f64,
    /// Newest position.
    pub end: f64,
}

impl SeekRange {
    /// Length in seconds.
    pub fn len(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// True if the range holds no time.
    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }
}

/// Most ranges kept from `demuxer-cache-state` (mpv normally reports one or two).
pub const MAX_SEEK_RANGES: usize = 16;

/// The demuxer cache (`demuxer-cache-state`), for the live rewind window and the stream
/// details overlay. Times share the `time-pos` timeline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheState {
    /// Ranges that can be seeked to without reopening the stream, oldest first (at most
    /// [`MAX_SEEK_RANGES`]). On a live stream played with a seekable cache, the range
    /// around `time-pos` is the rewind window: its `end` is the newest data received.
    pub seekable_ranges: Vec<SeekRange>,
    /// Timestamp of the newest packet received (`cache-end`).
    pub cache_end: Option<f64>,
    /// Bytes buffered ahead of the playback position (`fw-bytes`).
    pub forward_bytes: Option<u64>,
    /// Bytes held in the disk cache file (`file-cache-bytes`).
    pub file_cache_bytes: Option<u64>,
    /// The stream itself ended; what is cached can still be played (`eof`).
    pub eof: bool,
    /// Playback is waiting for data (`underrun`).
    pub underrun: bool,
    /// Average input rate in bytes/s (`raw-input-rate`).
    pub input_rate: Option<f64>,
}

impl CacheState {
    /// The range holding `t` (0.5 s tolerance), else the newest range.
    pub fn range_at(&self, t: f64) -> Option<SeekRange> {
        self.seekable_ranges
            .iter()
            .copied()
            .find(|r| t >= r.start - 0.5 && t <= r.end + 0.5)
            .or_else(|| self.seekable_ranges.last().copied())
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            time_pos: None,
            duration: None,
            paused: false,
            idle: true,
            core_idle: true,
            paused_for_cache: false,
            cache_buffering_percent: None,
            demuxer_cache_duration: None,
            seekable: false,
            seeking: false,
            eof_reached: false,
            file_loaded: false,
            loading: false,
            last_end: None,
            tracks: Vec::new(),
            video_width: None,
            video_height: None,
            media_title: None,
            volume: 100.0,
            muted: false,
            speed: 1.0,
            video_aspect_override: -1.0,
            video_zoom: 0.0,
            panscan: 0.0,
            keepaspect: true,
            hwdec_current: None,
            source_width: None,
            source_height: None,
            fps: None,
            video_codec: None,
            audio_codec: None,
            video_bitrate: None,
            audio_bitrate: None,
            cache_speed: None,
            frame_drops: None,
            decoder_frame_drops: None,
            cache: CacheState::default(),
            dropped_events: 0,
        }
    }
}

impl PlayerState {
    /// Audio tracks.
    pub fn audio_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio)
    }

    /// Subtitle tracks.
    pub fn subtitle_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle)
    }

    /// The selected track of `kind`, if any.
    pub fn selected(&self, kind: TrackKind) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == kind && t.selected)
    }

    /// Display size (`dwidth`×`dheight`) once both are known.
    pub fn video_size(&self) -> Option<(u32, u32)> {
        Some((self.video_width?, self.video_height?))
    }

    /// Whether there is a video picture to show.
    pub fn has_video(&self) -> bool {
        self.video_size().is_some() && !self.idle
    }

    /// Opening or rebuffering: show a spinner.
    pub fn is_buffering(&self) -> bool {
        self.loading || self.paused_for_cache
    }
}

/// Per-load settings for [`Player::load`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadOptions {
    /// HTTP User-Agent for this load (else the player default).
    pub user_agent: Option<UserAgent>,
    /// HTTP Referer for this load (else none).
    pub referrer: Option<Referrer>,
    /// Start position in seconds (resume). Ignored if not finite or < 0.
    pub start: Option<f64>,
}

struct Shared {
    state: Mutex<PlayerState>,
    fit: Mutex<VideoFit>,
    notify: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    stop: AtomicBool,
    dropped: AtomicU64,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, PlayerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A hardened libmpv player instance.
///
/// * All control methods are **non-blocking** (they use mpv's async API) and
///   are safe to call from the UI/GL thread, including while a
///   [`crate::RenderContext`] exists. Failures that mpv reports later arrive
///   as [`PlayerEvent::CommandFailed`].
/// * A background thread drains mpv events, updates [`PlayerState`] and
///   queues [`PlayerEvent`]s; call [`Player::set_event_notify`] to be woken
///   (e.g. `egui::Context::request_repaint`).
/// * `Player` is `Send + Sync`.
///
/// Dropping the player stops playback and joins the event thread. The mpv
/// core itself is destroyed once any `RenderContext`/`VideoView` bound to it
/// has also released it.
pub struct Player {
    core: Arc<Core>,
    shared: Arc<Shared>,
    events: Receiver<PlayerEvent>,
    thread: Option<JoinHandle<()>>,
    config: PlayerConfig,
    default_user_agent: String,
    mpv_version: String,
    ffmpeg_version: String,
    skipped_options: Vec<String>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("core_id", &self.core.id)
            .field("mpv_version", &self.mpv_version)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Loads libmpv (if needed), creates a core with the hardened options
    /// from `config`, and starts the event thread.
    ///
    /// # Errors
    /// [`Error::Load`] if libmpv is missing (the message points to
    /// `docs/LIBMPV.md`), [`Error::MandatoryOption`] if a hardening option
    /// cannot be applied, [`Error::InvalidArgument`] for an unusable
    /// [`PlayerConfig::cache_dir`], or another [`Error`] from libmpv.
    pub fn new(config: PlayerConfig) -> Result<Self, Error> {
        config.cache_dir_str().map_err(Error::InvalidArgument)?;
        let api = ffi::api(config.libmpv_path.as_deref())?;
        // SAFETY: no preconditions.
        let raw = unsafe { (api.create)() };
        let handle = NonNull::new(raw).ok_or(Error::Create)?;
        let core = Arc::new(Core {
            api,
            handle,
            id: NEXT_CORE_ID.fetch_add(1, Ordering::Relaxed),
            video: Mutex::new(VideoAttach {
                enabled: config.video_output,
                attached: false,
                pending: None,
            }),
        });

        let skipped_options = apply_options(&core, &config.mpv_options())?;
        request_log_level(&core, config.log_level)?;

        // SAFETY: valid, not yet initialised handle.
        let r = unsafe { (api.initialize)(core.raw()) };
        if r < 0 {
            return Err(Error::Initialize(core.mpv_err(r)));
        }

        for (i, p) in PROPS.iter().enumerate() {
            // SAFETY: valid handle, NUL-terminated name, supported format.
            let r = unsafe {
                (api.observe_property)(core.raw(), i as u64 + 1, p.name.as_ptr(), p.format)
            };
            if r < 0 {
                return Err(Error::Mpv {
                    op: "observe_property",
                    source: core.mpv_err(r),
                });
            }
        }

        let default_user_agent = core.get_string_sync(c"user-agent").unwrap_or_default();
        let mpv_version = core.get_string_sync(c"mpv-version").unwrap_or_default();
        let ffmpeg_version = core.get_string_sync(c"ffmpeg-version").unwrap_or_default();

        let shared = Arc::new(Shared {
            state: Mutex::new(PlayerState::default()),
            fit: Mutex::new(VideoFit::ORIGINAL),
            notify: Mutex::new(None),
            stop: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
        });
        let (tx, rx) = crossbeam_channel::bounded(EVENT_CAPACITY);
        let thread = {
            let core = Arc::clone(&core);
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("mpv-events".into())
                .spawn(move || event_loop(&core, &shared, &tx))
                .map_err(|_| Error::Mpv {
                    op: "spawn event thread",
                    source: MpvError {
                        code: 0,
                        message: "thread spawn failed".into(),
                    },
                })?
        };

        Ok(Self {
            core,
            shared,
            events: rx,
            thread: Some(thread),
            config,
            default_user_agent,
            mpv_version,
            ffmpeg_version,
            skipped_options,
        })
    }

    pub(crate) fn core(&self) -> &Arc<Core> {
        &self.core
    }

    /// A process-unique id for this player's mpv core.
    pub fn id(&self) -> u64 {
        self.core.id
    }

    /// `mpv-version`, e.g. `mpv v0.41.0-…`.
    pub fn mpv_version(&self) -> &str {
        &self.mpv_version
    }

    /// `ffmpeg-version` of the loaded libmpv build.
    pub fn ffmpeg_version(&self) -> &str {
        &self.ffmpeg_version
    }

    /// Path libmpv was loaded from.
    pub fn libmpv_path(&self) -> &Path {
        &self.core.api.path
    }

    /// libmpv client API version (major, minor).
    pub fn client_api_version(&self) -> (u32, u32) {
        self.core.api.version()
    }

    /// Best-effort hardening options this libmpv build does not know (and
    /// therefore were not applied). Mandatory ones never appear here: a
    /// failure there aborts [`Player::new`].
    pub fn skipped_options(&self) -> &[String] {
        &self.skipped_options
    }

    /// The configuration this player was built with.
    pub fn config(&self) -> &PlayerConfig {
        &self.config
    }

    /// Calls `f` from the event thread after each batch of events/state
    /// changes. Keep it cheap and non-blocking, e.g.
    /// `move || egui_ctx.request_repaint()`.
    pub fn set_event_notify(&self, f: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .notify
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(f));
    }

    /// A snapshot of the observed state.
    pub fn state(&self) -> PlayerState {
        let mut s = self.shared.state().clone();
        s.dropped_events = self.shared.dropped.load(Ordering::Relaxed);
        s
    }

    /// Runs `f` with the current state without cloning it.
    pub fn with_state<R>(&self, f: impl FnOnce(&PlayerState) -> R) -> R {
        f(&self.shared.state())
    }

    /// Next queued event, if any (non-blocking).
    pub fn poll_event(&self) -> Option<PlayerEvent> {
        self.events.try_recv().ok()
    }

    /// Drains all queued events (non-blocking).
    pub fn poll_events(&self) -> impl Iterator<Item = PlayerEvent> + '_ {
        self.events.try_iter()
    }

    /// Waits up to `timeout` for the next event. Do not call this on the UI
    /// thread.
    pub fn wait_event(&self, timeout: Duration) -> Option<PlayerEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Changes the forwarded log level.
    ///
    /// # Errors
    /// If libmpv rejects the request.
    pub fn set_log_level(&self, level: LogLevel) -> Result<(), Error> {
        request_log_level(&self.core, level)
    }

    /// Loads and plays `url`, replacing the current file.
    ///
    /// The User-Agent and Referer are set through mpv's `user-agent` and
    /// `referrer` properties first, then `loadfile` runs with the URL as a
    /// single argument (never a string-built command line). The only
    /// per-file option is a numeric `start`, formatted here. The cache uses the
    /// player's global settings; [`Player::load_with`] tunes it per stream.
    ///
    /// With video output enabled (the default), a load issued before any
    /// [`crate::VideoView`]/[`crate::RenderContext`] is attached is held and
    /// sent as soon as one attaches (libmpv cannot start video without a
    /// render context, and live streams cannot recover it later). See
    /// [`Player::has_pending_load`].
    ///
    /// # Errors
    /// [`Error::SchemeNotEnabled`]/[`Error::LocalFilesDisabled`] if this
    /// player's config does not allow the URL (files from
    /// [`PlayableUrl::from_owned_media`] are always allowed); [`Error::Mpv`] if
    /// mpv refuses to queue the request. Loading failures arrive later as
    /// [`PlayerEvent::EndFile`] with [`EndReason::Error`].
    pub fn load(&self, url: &PlayableUrl, opts: &LoadOptions) -> Result<(), Error> {
        self.load_inner(url, opts, None)
    }

    /// Like [`Player::load`], with the stream's buffering ([`StreamTuning`]) sent as
    /// `loadfile` per-file options next to `start`. They apply from the first byte (the
    /// initial buffer, cache sizes, a seekable or disk-backed cache) and end with the
    /// file, so the next load starts from the player's defaults again.
    ///
    /// # Errors
    /// As [`Player::load`].
    pub fn load_with(
        &self,
        url: &PlayableUrl,
        opts: &LoadOptions,
        tuning: &StreamTuning,
    ) -> Result<(), Error> {
        self.load_inner(url, opts, Some(tuning))
    }

    /// Applies `tuning` to the stream that is playing, at once (cache sizes, the buffer
    /// to wait for after a stall, disk or memory). [`PlayerState::seekable`] is decided
    /// when a stream opens, so turning on [`StreamTuning::seekable_cache`] here does not
    /// change it (seeks inside the cache still work). When nothing is loaded this changes the
    /// player's defaults instead. Failures arrive as [`PlayerEvent::CommandFailed`]
    /// with [`Op::Tuning`].
    ///
    /// # Errors
    /// [`Error::Mpv`] if mpv refuses to queue a request.
    pub fn set_tuning(&self, tuning: &StreamTuning) -> Result<(), Error> {
        let disk = self.config.has_cache_dir();
        for (name, value) in tuning.mpv_options(disk) {
            let name = CString::new(name).map_err(|_| Error::InvalidArgument("option name"))?;
            self.set_string(Op::Tuning, &name, &value)?;
        }
        Ok(())
    }

    /// Switches hardware decoding on or off, at once (mpv re-creates the decoder; the
    /// stream keeps playing). [`PlayerState::hwdec_current`] reports the decoder in use.
    ///
    /// # Errors
    /// [`Error::Mpv`] if mpv refuses to queue the request.
    pub fn set_hwdec(&self, hwdec: Hwdec) -> Result<(), Error> {
        self.set_string(Op::Tuning, c"hwdec", hwdec.as_mpv_str())
    }

    fn load_inner(
        &self,
        url: &PlayableUrl,
        opts: &LoadOptions,
        tuning: Option<&StreamTuning>,
    ) -> Result<(), Error> {
        if url.is_local_file() {
            if !url.is_owned_media() && !self.config.allow_local_files {
                return Err(Error::LocalFilesDisabled);
            }
        } else if !self.config.url_policy.allows_scheme(url.scheme()) {
            return Err(Error::SchemeNotEnabled(url.scheme().to_owned()));
        }

        let user_agent = opts
            .user_agent
            .as_ref()
            .or(self.config.user_agent.as_ref())
            .map_or(self.default_user_agent.as_str(), UserAgent::as_str)
            .to_owned();
        let referrer = opts
            .referrer
            .as_ref()
            .map_or("", Referrer::as_str)
            .to_owned();
        let mut file_options = Vec::new();
        if let Some(s) = opts.start.filter(|s| s.is_finite() && *s > 0.0) {
            file_options.push(("start", format!("{:.3}", s.min(1.0e7))));
        }
        if let Some(t) = tuning {
            file_options.extend(t.mpv_options(self.config.has_cache_dir()));
        }
        let load = ResolvedLoad {
            url: url.clone(),
            user_agent,
            referrer,
            file_options,
        };
        {
            let mut v = self.core.video();
            if v.enabled && !v.attached {
                // Sent by `render_attached` once a VideoView/RenderContext
                // exists; a later load replaces an earlier pending one.
                v.pending = Some(load);
                return Ok(());
            }
        }
        issue_load(&self.core, &load)
    }

    /// Whether a load is waiting for a render context (see
    /// [`PlayerConfig::video_output`]).
    pub fn has_pending_load(&self) -> bool {
        self.core.video().pending.is_some()
    }

    /// Stops playback and unloads the file (the core goes idle).
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn stop(&self) -> Result<(), Error> {
        self.core.video().pending = None;
        self.command(Op::Stop, &["stop"])
    }

    /// Unpauses.
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn play(&self) -> Result<(), Error> {
        self.set_flag(Op::Pause, c"pause", false)
    }

    /// Pauses.
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn pause(&self) -> Result<(), Error> {
        self.set_flag(Op::Pause, c"pause", true)
    }

    /// Toggles pause.
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn toggle_pause(&self) -> Result<(), Error> {
        self.command(Op::Pause, &["cycle", "pause"])
    }

    /// Seeks by `seconds` (negative = backwards).
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] if not finite.
    pub fn seek_relative(&self, seconds: f64) -> Result<(), Error> {
        let s = finite(seconds, "seek offset")?.clamp(-1.0e7, 1.0e7);
        self.command(Op::Seek, &["seek", &format!("{s:.3}"), "relative"])
    }

    /// Seeks to `seconds` from the start.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] if not finite.
    pub fn seek_absolute(&self, seconds: f64) -> Result<(), Error> {
        let s = finite(seconds, "seek position")?.clamp(0.0, 1.0e7);
        self.command(Op::Seek, &["seek", &format!("{s:.3}"), "absolute"])
    }

    /// Sets volume, 0..=130 (clamped).
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] if not finite.
    pub fn set_volume(&self, volume: f64) -> Result<(), Error> {
        let v = finite(volume, "volume")?.clamp(0.0, 130.0);
        self.set_double(Op::Volume, c"volume", v)
    }

    /// Mutes or unmutes.
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn set_mute(&self, mute: bool) -> Result<(), Error> {
        self.set_flag(Op::Mute, c"mute", mute)
    }

    /// Toggles mute.
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn toggle_mute(&self) -> Result<(), Error> {
        self.command(Op::Mute, &["cycle", "mute"])
    }

    /// Playback speed, 0.25..=4.0 (clamped).
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] if not finite.
    pub fn set_speed(&self, speed: f64) -> Result<(), Error> {
        let v = finite(speed, "speed")?.clamp(0.25, 4.0);
        self.set_double(Op::Speed, c"speed", v)
    }

    /// Selects the audio track.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a negative id.
    pub fn select_audio(&self, sel: TrackSelection) -> Result<(), Error> {
        self.set_string(Op::AudioTrack, c"aid", &selection_str(sel)?)
    }

    /// Selects the subtitle track.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a negative id.
    pub fn select_subtitle(&self, sel: TrackSelection) -> Result<(), Error> {
        self.set_string(Op::SubtitleTrack, c"sid", &selection_str(sel)?)
    }

    /// Sets aspect/crop/zoom. Persists across rebuffering and later loads.
    ///
    /// # Errors
    /// If mpv refuses to queue the request.
    pub fn set_video_fit(&self, fit: VideoFit) -> Result<(), Error> {
        let fit = fit.sanitized();
        *self
            .shared
            .fit
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = fit;
        apply_fit(&self.core, &fit)
    }

    /// The fit last requested with [`Player::set_video_fit`].
    pub fn video_fit(&self) -> VideoFit {
        *self
            .shared
            .fit
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes part of the demuxer cache (what has been received of the current stream) to
    /// `out`: from `start` to `end`, in seconds of `time-pos`. Used to save "the last N
    /// minutes" of a live channel without opening another connection: pass
    /// `time_pos - N*60 .. time_pos` (clamped to what is cached). mpv remuxes the packets
    /// of the selected tracks; the container follows the file extension. Needs a player
    /// built with [`crate::PlayerConfig::seekable_cache`] for streams the server cannot
    /// seek (live TS).
    ///
    /// The range must be explicit: with this libmpv build, `dump-cache no no` ("the whole
    /// cache") reports success but writes nothing.
    ///
    /// Asynchronous: [`PlayerEvent::CommandDone`] (`Op::DumpCache`) when the file is
    /// complete, [`PlayerEvent::CommandFailed`] otherwise. Only a [`MediaOutput`] (a new,
    /// validated file in an app folder) can be written.
    ///
    /// # Errors
    /// [`Error::InvalidArgument`] for a non-finite, negative or empty range; [`Error::Mpv`]
    /// if mpv refuses to queue the request.
    pub fn dump_cache(&self, start: f64, end: f64, out: &MediaOutput) -> Result<(), Error> {
        let start = finite(start, "dump start")?.clamp(0.0, 1.0e7);
        let end = finite(end, "dump end")?.clamp(0.0, 1.0e7);
        if end <= start {
            return Err(Error::InvalidArgument("dump range"));
        }
        let path = out
            .path()
            .to_str()
            .ok_or(Error::InvalidArgument("output path"))?;
        self.command(
            Op::DumpCache,
            &[
                "dump-cache",
                &format!("{start:.3}"),
                &format!("{end:.3}"),
                path,
            ],
        )
    }

    /// Starts (`Some`) or stops (`None`) recording the current stream to a file as it is
    /// received (`stream-record`). Uses no extra connection; the recording ends when the
    /// file is replaced or playback stops. A new [`MediaOutput`] is required each time.
    ///
    /// # Errors
    /// [`Error::Mpv`] if mpv refuses to queue the request.
    pub fn set_stream_record(&self, out: Option<&MediaOutput>) -> Result<(), Error> {
        let path = match out {
            Some(o) => o
                .path()
                .to_str()
                .ok_or(Error::InvalidArgument("output path"))?,
            None => "",
        };
        self.set_string(Op::Record, c"stream-record", path)
    }

    // ---- async primitives ------------------------------------------------

    fn command(&self, op: Op, args: &[&str]) -> Result<(), Error> {
        command_async(&self.core, op, args)
    }

    fn set_flag(&self, op: Op, name: &CStr, v: bool) -> Result<(), Error> {
        set_flag_async(&self.core, op, name, v)
    }

    fn set_double(&self, op: Op, name: &CStr, v: f64) -> Result<(), Error> {
        set_double_async(&self.core, op, name, v)
    }

    fn set_string(&self, op: Op, name: &CStr, v: &str) -> Result<(), Error> {
        set_string_async(&self.core, op, name, v)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Stop playback now: a VideoView may keep the core alive a little
        // longer, and it must neither keep playing audio nor start a load
        // that was waiting for it.
        let _ = self.stop();
        self.shared.stop.store(true, Ordering::SeqCst);
        // SAFETY: valid handle; mpv_wakeup is safe from any thread.
        unsafe { (self.core.api.wakeup)(self.core.raw()) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn finite(v: f64, what: &'static str) -> Result<f64, Error> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(Error::InvalidArgument(what))
    }
}

fn selection_str(sel: TrackSelection) -> Result<String, Error> {
    Ok(match sel {
        TrackSelection::Auto => "auto".to_owned(),
        TrackSelection::Off => "no".to_owned(),
        TrackSelection::Id(id) if id >= 0 => id.to_string(),
        TrackSelection::Id(_) => return Err(Error::InvalidArgument("track id")),
    })
}

fn check(core: &Core, r: c_int, op: &'static str) -> Result<(), Error> {
    if r < 0 {
        Err(Error::Mpv {
            op,
            source: core.mpv_err(r),
        })
    } else {
        Ok(())
    }
}

fn command_async(core: &Core, op: Op, args: &[&str]) -> Result<(), Error> {
    let owned: Vec<CString> = args
        .iter()
        .map(|a| CString::new(*a))
        .collect::<Result<_, _>>()
        .map_err(|_| Error::InvalidArgument("NUL in argument"))?;
    let mut argv: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: valid handle; argv is a NULL-terminated array of NUL-terminated
    // strings that outlive the call (mpv copies what it keeps).
    let r = unsafe { (core.api.command_async)(core.raw(), op.id(), argv.as_mut_ptr()) };
    check(core, r, "command_async")
}

fn set_flag_async(core: &Core, op: Op, name: &CStr, v: bool) -> Result<(), Error> {
    let mut flag: c_int = c_int::from(v);
    // SAFETY: valid handle; for MPV_FORMAT_FLAG data points to an int, which
    // mpv copies before returning.
    let r = unsafe {
        (core.api.set_property_async)(
            core.raw(),
            op.id(),
            name.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            (&raw mut flag).cast::<c_void>(),
        )
    };
    check(core, r, "set_property_async")
}

fn set_double_async(core: &Core, op: Op, name: &CStr, v: f64) -> Result<(), Error> {
    let mut d = v;
    // SAFETY: valid handle; for MPV_FORMAT_DOUBLE data points to a double,
    // which mpv copies before returning.
    let r = unsafe {
        (core.api.set_property_async)(
            core.raw(),
            op.id(),
            name.as_ptr(),
            ffi::MPV_FORMAT_DOUBLE,
            (&raw mut d).cast::<c_void>(),
        )
    };
    check(core, r, "set_property_async")
}

fn set_string_async(core: &Core, op: Op, name: &CStr, v: &str) -> Result<(), Error> {
    let value = CString::new(v).map_err(|_| Error::InvalidArgument("NUL in value"))?;
    let mut p: *const c_char = value.as_ptr();
    // SAFETY: valid handle; for MPV_FORMAT_STRING data points to a `char*`;
    // mpv copies the string before returning.
    let r = unsafe {
        (core.api.set_property_async)(
            core.raw(),
            op.id(),
            name.as_ptr(),
            ffi::MPV_FORMAT_STRING,
            (&raw mut p).cast::<c_void>(),
        )
    };
    check(core, r, "set_property_async")
}

/// Sets the HTTP headers, then runs `loadfile` with named arguments.
///
/// Invariant: the URL is one argument, never part of a string-built command. The
/// per-file `options` map holds only crate-defined option names (`start` and the
/// [`StreamTuning`] names) whose values this crate formats itself: numbers and
/// `yes`/`no`/`auto`. No caller-supplied text becomes an option.
fn issue_load(core: &Core, load: &ResolvedLoad) -> Result<(), Error> {
    set_string_async(core, Op::HttpHeaders, c"user-agent", &load.user_agent)?;
    set_string_async(core, Op::HttpHeaders, c"referrer", &load.referrer)?;
    let mut args = vec![
        ("name", NodeIn::Str("loadfile")),
        ("url", NodeIn::Str(load.url.as_str())),
        ("flags", NodeIn::Str("replace")),
    ];
    if !load.file_options.is_empty() {
        let options = load
            .file_options
            .iter()
            .map(|(k, v)| (*k, NodeIn::Str(v.as_str())))
            .collect();
        args.push(("options", NodeIn::Map(options)));
    }
    let mut owned = OwnedNode::new(&NodeIn::Map(args))
        .map_err(|_| Error::InvalidArgument("NUL in argument"))?;
    // SAFETY: valid handle; `owned` is a well-formed node tree that outlives
    // the call (mpv parses it synchronously).
    let r = unsafe { (core.api.command_node_async)(core.raw(), Op::Load.id(), owned.as_mut_ptr()) };
    check(core, r, "command_node_async")
}

fn apply_fit(core: &Core, fit: &VideoFit) -> Result<(), Error> {
    set_string_async(
        core,
        Op::VideoFit,
        c"video-aspect-override",
        &fit.aspect_override_str(),
    )?;
    set_flag_async(core, Op::VideoFit, c"keepaspect", fit.keepaspect())?;
    set_double_async(core, Op::VideoFit, c"panscan", fit.panscan)?;
    set_double_async(core, Op::VideoFit, c"video-zoom", fit.zoom)
}

fn request_log_level(core: &Core, level: LogLevel) -> Result<(), Error> {
    let lvl = CString::new(level.as_mpv_str()).map_err(|_| Error::InvalidArgument("log level"))?;
    // SAFETY: valid handle, NUL-terminated level name.
    let r = unsafe { (core.api.request_log_messages)(core.raw(), lvl.as_ptr()) };
    check(core, r, "request_log_messages")
}

/// Applies the pre-init options. Returns the names of skipped best-effort
/// options.
fn apply_options(core: &Core, options: &[MpvOption]) -> Result<Vec<String>, Error> {
    let mut skipped = Vec::new();
    for opt in options {
        let name = CString::new(opt.name).map_err(|_| Error::InvalidArgument("option name"))?;
        let r = match &opt.value {
            OptValue::Str(v) => {
                let v =
                    CString::new(v.as_str()).map_err(|_| Error::InvalidArgument("option value"))?;
                // SAFETY: valid, uninitialised handle; both strings are
                // NUL-terminated and copied by mpv.
                unsafe { (core.api.set_option_string)(core.raw(), name.as_ptr(), v.as_ptr()) }
            }
            OptValue::Whitelist(protocols) => {
                let mut node = OwnedNode::new(&NodeIn::Map(vec![(
                    "protocol_whitelist",
                    NodeIn::Str(protocols),
                )]))
                .map_err(|_| Error::InvalidArgument("option value"))?;
                // SAFETY: valid handle; MPV_FORMAT_NODE with a pointer to a
                // well-formed node tree that mpv copies before returning.
                unsafe {
                    (core.api.set_option)(
                        core.raw(),
                        name.as_ptr(),
                        ffi::MPV_FORMAT_NODE,
                        node.as_mut_ptr().cast::<c_void>(),
                    )
                }
            }
        };
        if r < 0 {
            if opt.mandatory {
                return Err(Error::MandatoryOption {
                    name: opt.name.to_owned(),
                    source: core.mpv_err(r),
                });
            }
            skipped.push(if r == ffi::MPV_ERROR_OPTION_NOT_FOUND {
                opt.name.to_owned()
            } else {
                format!("{} ({})", opt.name, core.api.error_string(r))
            });
        }
    }
    Ok(skipped)
}

// ---- event thread ---------------------------------------------------------

fn event_loop(core: &Core, shared: &Shared, tx: &Sender<PlayerEvent>) {
    let send = |ev: PlayerEvent| match tx.try_send(ev) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {
            shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
    };
    loop {
        // Block for the first event, then drain without waiting so that the
        // notify callback runs once per batch.
        let mut timeout = -1.0;
        let mut handled = 0usize;
        loop {
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            // SAFETY: valid handle; this thread is the only caller of
            // mpv_wait_event for this handle. The returned event stays valid
            // until the next call, and we copy everything we need below.
            let ev = unsafe { &*(core.api.wait_event)(core.raw(), timeout) };
            timeout = 0.0;
            if ev.event_id == ffi::MPV_EVENT_NONE {
                break;
            }
            handled += 1;
            if ev.event_id == ffi::MPV_EVENT_SHUTDOWN {
                send(PlayerEvent::Shutdown);
                notify(shared);
                return;
            }
            // SAFETY: `ev` came from mpv_wait_event and is valid until the
            // next call; handle_event only reads it.
            if let Some(out) = unsafe { handle_event(core, shared, ev) } {
                send(out);
            }
        }
        if handled > 0 {
            notify(shared);
        }
    }
}

fn notify(shared: &Shared) {
    let f = shared
        .notify
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if let Some(f) = f {
        f();
    }
}

/// Converts one raw event, updating shared state.
///
/// # Safety
/// `ev` must be the event most recently returned by `mpv_wait_event`.
unsafe fn handle_event(core: &Core, shared: &Shared, ev: &ffi::mpv_event) -> Option<PlayerEvent> {
    match ev.event_id {
        ffi::MPV_EVENT_LOG_MESSAGE => {
            if ev.data.is_null() {
                return None;
            }
            // SAFETY: LOG_MESSAGE events carry an mpv_event_log_message.
            let m = unsafe { &*ev.data.cast::<ffi::mpv_event_log_message>() };
            // SAFETY: mpv guarantees these are NUL-terminated strings.
            let (prefix, level, text) = unsafe {
                (
                    ffi::opt_string(m.prefix),
                    ffi::opt_string(m.level),
                    ffi::opt_string(m.text),
                )
            };
            let mut text = text.unwrap_or_default();
            while text.ends_with('\n') || text.ends_with('\r') {
                text.pop();
            }
            Some(PlayerEvent::Log(LogLine {
                level: level
                    .as_deref()
                    .and_then(LogLevel::from_mpv_str)
                    .unwrap_or(LogLevel::Info),
                prefix: prefix.unwrap_or_default(),
                text,
            }))
        }
        ffi::MPV_EVENT_START_FILE => {
            let mut s = shared.state();
            s.loading = true;
            s.file_loaded = false;
            s.last_end = None;
            s.eof_reached = false;
            Some(PlayerEvent::StartFile)
        }
        ffi::MPV_EVENT_FILE_LOADED => {
            {
                let mut s = shared.state();
                s.loading = false;
                s.file_loaded = true;
            }
            reapply_fit_if_needed(core, shared);
            Some(PlayerEvent::FileLoaded)
        }
        ffi::MPV_EVENT_END_FILE => {
            let reason = if ev.data.is_null() {
                EndReason::Unknown(-1)
            } else {
                // SAFETY: END_FILE events carry an mpv_event_end_file.
                let e = unsafe { &*ev.data.cast::<ffi::mpv_event_end_file>() };
                match e.reason {
                    ffi::MPV_END_FILE_REASON_EOF => EndReason::Eof,
                    ffi::MPV_END_FILE_REASON_STOP => EndReason::Stop,
                    ffi::MPV_END_FILE_REASON_QUIT => EndReason::Quit,
                    ffi::MPV_END_FILE_REASON_ERROR => EndReason::Error(core.mpv_err(e.error)),
                    ffi::MPV_END_FILE_REASON_REDIRECT => EndReason::Redirect,
                    other => EndReason::Unknown(other),
                }
            };
            {
                let mut s = shared.state();
                s.loading = false;
                s.file_loaded = false;
                s.last_end = Some(reason.clone());
            }
            Some(PlayerEvent::EndFile(reason))
        }
        ffi::MPV_EVENT_PLAYBACK_RESTART => Some(PlayerEvent::PlaybackRestart),
        ffi::MPV_EVENT_SEEK => Some(PlayerEvent::Seek),
        ffi::MPV_EVENT_VIDEO_RECONFIG => Some(PlayerEvent::VideoReconfig),
        ffi::MPV_EVENT_AUDIO_RECONFIG => Some(PlayerEvent::AudioReconfig),
        ffi::MPV_EVENT_QUEUE_OVERFLOW => Some(PlayerEvent::QueueOverflow),
        ffi::MPV_EVENT_COMMAND_REPLY | ffi::MPV_EVENT_SET_PROPERTY_REPLY => {
            let op = Op::from_id(ev.reply_userdata);
            if ev.error < 0 {
                Some(PlayerEvent::CommandFailed {
                    op,
                    error: core.mpv_err(ev.error),
                })
            } else {
                (op == Op::DumpCache).then_some(PlayerEvent::CommandDone { op })
            }
        }
        ffi::MPV_EVENT_PROPERTY_CHANGE => {
            if ev.data.is_null() {
                return None;
            }
            let idx = usize::try_from(ev.reply_userdata).ok()?.checked_sub(1)?;
            let spec = PROPS.get(idx)?;
            // SAFETY: PROPERTY_CHANGE events carry an mpv_event_property.
            let p = unsafe { &*ev.data.cast::<ffi::mpv_event_property>() };
            // SAFETY: `p` is valid for this event; read_value checks format.
            let value = unsafe { read_value(p) };
            apply_property(&mut shared.state(), spec.prop, value);
            spec.notify
                .then_some(PlayerEvent::PropertyChanged(spec.prop))
        }
        _ => None,
    }
}

fn reapply_fit_if_needed(core: &Core, shared: &Shared) {
    let fit = *shared.fit.lock().unwrap_or_else(PoisonError::into_inner);
    let s = shared.state().clone();
    let aspect_ok = match fit.aspect {
        Aspect::Ratio(n, d) if n > 0 && d > 0 => {
            (s.video_aspect_override - f64::from(n) / f64::from(d)).abs() < 1e-3
        }
        _ => s.video_aspect_override <= 0.0,
    };
    let same = aspect_ok
        && s.keepaspect == fit.keepaspect()
        && (s.panscan - fit.panscan).abs() < 1e-6
        && (s.video_zoom - fit.zoom).abs() < 1e-6;
    if !same {
        // Errors surface as CommandFailed events.
        let _ = apply_fit(core, &fit);
    }
}

enum Value {
    None,
    Flag(bool),
    Int(i64),
    Double(f64),
    Str(String),
    Node(NodeOut),
}

/// # Safety
/// `p` must come from a current `MPV_EVENT_PROPERTY_CHANGE`.
unsafe fn read_value(p: &ffi::mpv_event_property) -> Value {
    if p.data.is_null() {
        return Value::None;
    }
    // SAFETY: (all arms) mpv sets `data` to point to a value of the type
    // named by `format`; MPV_FORMAT_NONE means unavailable.
    unsafe {
        match p.format {
            ffi::MPV_FORMAT_FLAG => Value::Flag(*p.data.cast::<c_int>() != 0),
            ffi::MPV_FORMAT_INT64 => Value::Int(*p.data.cast::<i64>()),
            ffi::MPV_FORMAT_DOUBLE => Value::Double(*p.data.cast::<f64>()),
            ffi::MPV_FORMAT_STRING => {
                ffi::opt_string(*p.data.cast::<*const c_char>()).map_or(Value::None, Value::Str)
            }
            ffi::MPV_FORMAT_NODE => Value::Node(NodeOut::from_raw(p.data.cast::<ffi::mpv_node>())),
            _ => Value::None,
        }
    }
}

fn apply_property(s: &mut PlayerState, prop: Property, v: Value) {
    let dbl = |v: &Value| match v {
        Value::Double(d) if d.is_finite() => Some(*d),
        #[allow(clippy::cast_precision_loss)]
        Value::Int(i) => Some(*i as f64),
        _ => None,
    };
    let flag = |v: &Value| matches!(v, Value::Flag(true));
    let int = |v: &Value| match v {
        Value::Int(i) => Some(*i),
        _ => None,
    };
    match prop {
        Property::TimePos => s.time_pos = dbl(&v),
        Property::Duration => s.duration = dbl(&v),
        Property::Pause => s.paused = flag(&v),
        Property::Idle => s.idle = flag(&v),
        Property::CoreIdle => s.core_idle = flag(&v),
        Property::PausedForCache => s.paused_for_cache = flag(&v),
        Property::CacheBufferingState => s.cache_buffering_percent = int(&v),
        Property::DemuxerCacheDuration => s.demuxer_cache_duration = dbl(&v),
        Property::Seekable => s.seekable = flag(&v),
        Property::Seeking => s.seeking = flag(&v),
        Property::EofReached => s.eof_reached = flag(&v),
        Property::TrackList => {
            s.tracks = match &v {
                Value::Node(n) => parse_tracks(n),
                _ => Vec::new(),
            };
        }
        Property::VideoWidth => {
            s.video_width = int(&v)
                .and_then(|i| u32::try_from(i).ok())
                .filter(|i| *i > 0);
        }
        Property::VideoHeight => {
            s.video_height = int(&v)
                .and_then(|i| u32::try_from(i).ok())
                .filter(|i| *i > 0);
        }
        Property::MediaTitle => {
            s.media_title = match v {
                Value::Str(t) => Some(sanitize(&t)).filter(|t| !t.is_empty()),
                _ => None,
            };
        }
        Property::Volume => s.volume = dbl(&v).unwrap_or(s.volume),
        Property::Mute => s.muted = flag(&v),
        Property::Speed => s.speed = dbl(&v).unwrap_or(1.0),
        Property::VideoAspectOverride => s.video_aspect_override = dbl(&v).unwrap_or(-1.0),
        Property::VideoZoom => s.video_zoom = dbl(&v).unwrap_or(0.0),
        Property::Panscan => s.panscan = dbl(&v).unwrap_or(0.0),
        Property::KeepAspect => s.keepaspect = flag(&v),
        Property::HwdecCurrent => {
            s.hwdec_current = match v {
                Value::Str(t) if !t.is_empty() && t != "no" => Some(sanitize(&t)),
                _ => None,
            };
        }
        Property::SourceWidth => s.source_width = positive_u32(int(&v)),
        Property::SourceHeight => s.source_height = positive_u32(int(&v)),
        Property::ContainerFps => s.fps = dbl(&v).filter(|f| *f > 0.0 && *f < 1000.0),
        Property::VideoFormat => s.video_codec = short_text(v),
        Property::AudioCodec => s.audio_codec = short_text(v),
        Property::VideoBitrate => s.video_bitrate = dbl(&v).filter(|b| *b >= 0.0),
        Property::AudioBitrate => s.audio_bitrate = dbl(&v).filter(|b| *b >= 0.0),
        Property::CacheSpeed => s.cache_speed = dbl(&v).filter(|b| *b >= 0.0),
        Property::FrameDrops => s.frame_drops = int(&v).filter(|n| *n >= 0),
        Property::DecoderFrameDrops => s.decoder_frame_drops = int(&v).filter(|n| *n >= 0),
        Property::CacheState => {
            s.cache = match &v {
                Value::Node(n) => parse_cache_state(n),
                _ => CacheState::default(),
            };
        }
    }
}

fn positive_u32(v: Option<i64>) -> Option<u32> {
    v.and_then(|i| u32::try_from(i).ok()).filter(|i| *i > 0)
}

/// A short media-provided name (codec), sanitised and capped at 32 characters.
fn short_text(v: Value) -> Option<String> {
    match v {
        Value::Str(t) => Some(sanitize(&t).chars().take(32).collect::<String>())
            .filter(|t: &String| !t.is_empty()),
        _ => None,
    }
}

fn node_f64(n: &NodeOut) -> Option<f64> {
    match n {
        NodeOut::Double(d) if d.is_finite() => Some(*d),
        #[allow(clippy::cast_precision_loss)]
        NodeOut::Int(i) => Some(*i as f64),
        _ => None,
    }
}

/// Reads mpv's `demuxer-cache-state` map. Unknown or malformed entries are skipped.
fn parse_cache_state(n: &NodeOut) -> CacheState {
    let num = |k: &str| n.get(k).and_then(node_f64);
    let bytes = |k: &str| {
        n.get(k)
            .and_then(NodeOut::as_i64)
            .and_then(|b| u64::try_from(b).ok())
    };
    let flag = |k: &str| n.get(k).and_then(NodeOut::as_bool).unwrap_or(false);
    let mut seekable_ranges: Vec<SeekRange> = match n.get("seekable-ranges") {
        Some(NodeOut::Array(items)) => items
            .iter()
            .filter_map(|r| {
                let start = r.get("start").and_then(node_f64)?;
                let end = r.get("end").and_then(node_f64)?;
                (end >= start).then_some(SeekRange { start, end })
            })
            .take(MAX_SEEK_RANGES)
            .collect(),
        _ => Vec::new(),
    };
    seekable_ranges.sort_by(|a, b| a.start.total_cmp(&b.start));
    CacheState {
        seekable_ranges,
        cache_end: num("cache-end"),
        forward_bytes: bytes("fw-bytes"),
        file_cache_bytes: bytes("file-cache-bytes"),
        eof: flag("eof"),
        underrun: flag("underrun"),
        input_rate: num("raw-input-rate").filter(|r| *r >= 0.0),
    }
}

/// Strips control characters and caps length of a media-provided string.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_MEDIA_STRING_CHARS)
        .collect()
}

fn parse_tracks(n: &NodeOut) -> Vec<Track> {
    let NodeOut::Array(items) = n else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|t| {
            let kind = match t.get("type").and_then(NodeOut::as_str)? {
                "video" => TrackKind::Video,
                "audio" => TrackKind::Audio,
                "sub" => TrackKind::Subtitle,
                _ => return None,
            };
            let text = |k: &str| {
                t.get(k)
                    .and_then(NodeOut::as_str)
                    .map(sanitize)
                    .filter(|s| !s.is_empty())
            };
            let flag = |k: &str| t.get(k).and_then(NodeOut::as_bool).unwrap_or(false);
            Some(Track {
                id: t.get("id").and_then(NodeOut::as_i64)?,
                kind,
                title: text("title"),
                lang: text("lang"),
                codec: text("codec"),
                default: flag("default"),
                forced: flag("forced"),
                selected: flag("selected"),
                external: flag("external"),
                channels: t.get("demux-channel-count").and_then(NodeOut::as_i64),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_ids_round_trip() {
        for op in Op::ALL {
            assert_ne!(op.id(), 0);
            assert_eq!(Op::from_id(op.id()), op);
        }
        assert_eq!(Op::from_id(0), Op::Other);
        assert_eq!(Op::from_id(9999), Op::Other);
    }

    #[test]
    fn video_fit_presets_cycle() {
        let mut f = VideoFit::ORIGINAL;
        let mut seen = Vec::new();
        for _ in 0..VideoFit::PRESETS.len() {
            seen.push(f.preset_index());
            f = f.next_preset();
        }
        assert_eq!(f, VideoFit::ORIGINAL);
        assert_eq!(
            seen,
            (0..VideoFit::PRESETS.len()).map(Some).collect::<Vec<_>>()
        );
        let custom = VideoFit {
            zoom: 0.5,
            ..VideoFit::ORIGINAL
        };
        assert_eq!(custom.preset_index(), None);
        assert_eq!(custom.next_preset(), VideoFit::ORIGINAL);
    }

    #[test]
    fn video_fit_option_strings() {
        assert_eq!(VideoFit::ORIGINAL.aspect_override_str(), "no");
        let f = VideoFit {
            aspect: Aspect::Ratio(16, 9),
            ..VideoFit::ORIGINAL
        };
        assert_eq!(f.aspect_override_str(), "16:9");
        let z = VideoFit {
            aspect: Aspect::Ratio(0, 9),
            ..VideoFit::ORIGINAL
        };
        assert_eq!(z.aspect_override_str(), "no");
        assert!(
            !VideoFit {
                aspect: Aspect::Stretch,
                ..VideoFit::ORIGINAL
            }
            .keepaspect()
        );
        let s = VideoFit {
            aspect: Aspect::Original,
            panscan: f64::NAN,
            zoom: 99.0,
        }
        .sanitized();
        assert_eq!((s.panscan, s.zoom), (0.0, 3.0));
    }

    #[test]
    fn selection_strings() {
        assert_eq!(
            selection_str(TrackSelection::Auto).ok().as_deref(),
            Some("auto")
        );
        assert_eq!(
            selection_str(TrackSelection::Off).ok().as_deref(),
            Some("no")
        );
        assert_eq!(
            selection_str(TrackSelection::Id(3)).ok().as_deref(),
            Some("3")
        );
        assert!(selection_str(TrackSelection::Id(-1)).is_err());
    }

    #[test]
    fn sanitize_strips_controls_and_caps() {
        assert_eq!(sanitize("a\u{1b}[31mb\nc"), "a[31mbc");
        assert_eq!(
            sanitize(&"x".repeat(1000)).chars().count(),
            MAX_MEDIA_STRING_CHARS
        );
    }

    #[test]
    fn tracks_parse_from_node() {
        let node = NodeOut::Array(vec![
            NodeOut::Map(vec![
                ("id".into(), NodeOut::Int(1)),
                ("type".into(), NodeOut::Str("audio".into())),
                ("lang".into(), NodeOut::Str("eng".into())),
                ("title".into(), NodeOut::Str("Main\u{7}".into())),
                ("selected".into(), NodeOut::Flag(true)),
                ("demux-channel-count".into(), NodeOut::Int(2)),
            ]),
            NodeOut::Map(vec![
                ("id".into(), NodeOut::Int(1)),
                ("type".into(), NodeOut::Str("sub".into())),
            ]),
            NodeOut::Map(vec![("type".into(), NodeOut::Str("audio".into()))]),
            NodeOut::Map(vec![
                ("id".into(), NodeOut::Int(5)),
                ("type".into(), NodeOut::Str("weird".into())),
            ]),
            NodeOut::Str("garbage".into()),
        ]);
        let t = parse_tracks(&node);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].kind, TrackKind::Audio);
        assert_eq!(t[0].title.as_deref(), Some("Main"));
        assert_eq!(t[0].channels, Some(2));
        assert!(t[0].selected);
        assert_eq!(t[1].kind, TrackKind::Subtitle);
        assert_eq!(t[1].label(), "#1");
        assert_eq!(t[0].label(), "#1 eng – Main");
        assert!(parse_tracks(&NodeOut::None).is_empty());
    }

    #[test]
    fn video_size_needs_both_dimensions() {
        let mut s = PlayerState::default();
        apply_property(&mut s, Property::VideoWidth, Value::Int(1280));
        assert_eq!(s.video_size(), None);
        apply_property(&mut s, Property::VideoHeight, Value::Int(720));
        assert_eq!(s.video_size(), Some((1280, 720)));
        apply_property(&mut s, Property::VideoWidth, Value::None);
        assert_eq!(s.video_size(), None);
    }

    #[test]
    fn cache_state_parses_ranges_and_skips_garbage() {
        let range = |s: f64, e: f64| {
            NodeOut::Map(vec![
                ("start".into(), NodeOut::Double(s)),
                ("end".into(), NodeOut::Double(e)),
            ])
        };
        let node = NodeOut::Map(vec![
            (
                "seekable-ranges".into(),
                NodeOut::Array(vec![
                    range(40.0, 70.0),
                    range(0.0, 12.5),
                    range(9.0, 3.0),
                    NodeOut::Str("x".into()),
                    range(f64::NAN, 1.0),
                ]),
            ),
            ("cache-end".into(), NodeOut::Double(70.0)),
            ("fw-bytes".into(), NodeOut::Int(1_000_000)),
            ("file-cache-bytes".into(), NodeOut::Int(-5)),
            ("eof".into(), NodeOut::Flag(true)),
            ("raw-input-rate".into(), NodeOut::Int(125_000)),
        ]);
        let c = parse_cache_state(&node);
        assert_eq!(
            c.seekable_ranges,
            vec![
                SeekRange {
                    start: 0.0,
                    end: 12.5
                },
                SeekRange {
                    start: 40.0,
                    end: 70.0
                }
            ]
        );
        assert_eq!(c.cache_end, Some(70.0));
        assert_eq!(c.forward_bytes, Some(1_000_000));
        assert_eq!(c.file_cache_bytes, None);
        assert!(c.eof && !c.underrun);
        assert_eq!(c.input_rate, Some(125_000.0));
        assert_eq!(c.range_at(50.0).map(|r| r.start), Some(40.0));
        assert_eq!(c.range_at(5.0).map(|r| r.end), Some(12.5));
        assert_eq!(c.range_at(30.0).map(|r| r.start), Some(40.0), "newest");
        assert_eq!(parse_cache_state(&NodeOut::None), CacheState::default());

        let many = NodeOut::Map(vec![(
            "seekable-ranges".into(),
            NodeOut::Array(
                (0..100)
                    .map(|i| range(f64::from(i), f64::from(i) + 0.5))
                    .collect(),
            ),
        )]);
        assert_eq!(
            parse_cache_state(&many).seekable_ranges.len(),
            MAX_SEEK_RANGES
        );
    }

    #[test]
    fn stream_details_are_sanitised() {
        let mut s = PlayerState::default();
        apply_property(
            &mut s,
            Property::VideoFormat,
            Value::Str("h264\u{1b}".into()),
        );
        assert_eq!(s.video_codec.as_deref(), Some("h264"));
        apply_property(&mut s, Property::AudioCodec, Value::Str("a".repeat(500)));
        assert_eq!(s.audio_codec.as_ref().map(String::len), Some(32));
        apply_property(&mut s, Property::ContainerFps, Value::Double(f64::INFINITY));
        assert_eq!(s.fps, None);
        apply_property(&mut s, Property::ContainerFps, Value::Double(25.0));
        assert_eq!(s.fps, Some(25.0));
        apply_property(&mut s, Property::SourceWidth, Value::Int(-1));
        assert_eq!(s.source_width, None);
        apply_property(&mut s, Property::FrameDrops, Value::Int(3));
        assert_eq!(s.frame_drops, Some(3));
        apply_property(&mut s, Property::CacheSpeed, Value::Double(-2.0));
        assert_eq!(s.cache_speed, None);
    }

    #[test]
    fn log_line_debug_hides_text() {
        let l = LogLine {
            level: LogLevel::Info,
            prefix: "cplayer".into(),
            text: "Playing: http://u:p@example.com/x".into(),
        };
        let d = format!("{l:?}");
        assert!(!d.contains("example.com"), "{d}");
    }
}
