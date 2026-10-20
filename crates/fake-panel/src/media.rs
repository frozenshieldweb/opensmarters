//! Test media: generated once with ffmpeg into the media directory, then served from disk.
//!
//! | File | Content |
//! |---|---|
//! | `live-0.ts` … `live-3.ts` | 20 s, 640×360 H.264 + AAC MPEG-TS; each a different test pattern and tone |
//! | `live-N-hls/index.m3u8` + `seg-NNN.ts` | HLS (VOD playlist, 4 s segments) of the same clips |
//! | `movie-multitrack.mkv` | 30 s, 854×480 H.264, **2 AAC audio tracks** (eng, fra) + **1 SRT subtitle track** |
//! | `movie.mp4` | 30 s, 640×360 H.264 + AAC, `+faststart` |
//! | `movie.avi` | 10 s, 480×270 MPEG-4 Part 2 + MP3 |
//!
//! Files are written under a temporary name and renamed into place, so a crashed or
//! concurrent generation never leaves a half-written file behind.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError, RwLock};

use crate::catalogue::LIVE_CLIPS;

/// How [`spawn`](crate::spawn) handles missing media files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaMode {
    /// Generate missing files with ffmpeg before `spawn` returns (default).
    #[default]
    Generate,
    /// Start serving immediately and generate in the background; stream endpoints answer
    /// 503 with `Retry-After` until the files are ready.
    Background,
    /// Never run ffmpeg; serve whatever already exists (503 if anything is missing).
    Never,
}

/// Whether stream endpoints can serve media.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaStatus {
    /// All files present.
    Ready,
    /// ffmpeg is running.
    Generating,
    /// Files are missing and cannot be generated; the message explains why.
    Unavailable(String),
}

const LIVE_SOURCES: [(&str, u32); LIVE_CLIPS] = [
    ("testsrc2=size=640x360:rate=25", 440),
    ("smptehdbars=size=640x360:rate=25", 1000),
    (
        "life=size=640x360:rate=25:ratio=0.12:mold=10:life_color=0x30c0ff:\
         death_color=0x101030:mold_color=0xff4080:seed=7",
        523,
    ),
    ("gradients=size=640x360:rate=25:speed=0.02:seed=11", 659),
];

/// MKV with two audio tracks and one subtitle track.
pub(crate) const MOVIE_MKV: &str = "movie-multitrack.mkv";
/// Plain MP4.
pub(crate) const MOVIE_MP4: &str = "movie.mp4";
/// Small AVI.
pub(crate) const MOVIE_AVI: &str = "movie.avi";

pub(crate) fn live_ts_name(clip: usize) -> String {
    format!("live-{clip}.ts")
}

fn hls_dir_name(clip: usize) -> String {
    format!("live-{clip}-hls")
}

/// Every file name the library expects (relative to the media dir), for reporting.
pub fn expected_files() -> Vec<String> {
    let mut v: Vec<String> = (0..LIVE_CLIPS).map(live_ts_name).collect();
    v.extend((0..LIVE_CLIPS).map(|i| format!("{}/index.m3u8", hls_dir_name(i))));
    v.extend([MOVIE_MKV, MOVIE_MP4, MOVIE_AVI].map(str::to_owned));
    v
}

fn all_present(dir: &Path) -> bool {
    expected_files().iter().all(|f| dir.join(f).is_file())
}

static GENERATION_LOCK: Mutex<()> = Mutex::new(());

/// The media directory plus its readiness state and cached HLS playlists.
#[derive(Debug)]
pub(crate) struct MediaLibrary {
    dir: PathBuf,
    status: RwLock<MediaStatus>,
    playlists: RwLock<Vec<String>>,
}

impl MediaLibrary {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            status: RwLock::new(MediaStatus::Unavailable("media not prepared yet".into())),
            playlists: RwLock::new(Vec::new()),
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn status(&self) -> MediaStatus {
        self.status
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_status(&self, s: MediaStatus) {
        *self.status.write().unwrap_or_else(PoisonError::into_inner) = s;
    }

    /// Mark as generating (used before handing [`MediaLibrary::prepare`] to a background task).
    pub(crate) fn mark_generating(&self) {
        self.set_status(MediaStatus::Generating);
    }

    /// Whether all files are already on disk (cheap: only `stat` calls).
    pub(crate) fn files_present(&self) -> bool {
        all_present(&self.dir)
    }

    /// Check the files and, if allowed, generate the missing ones. Blocking.
    pub(crate) fn prepare(&self, allow_generate: bool) {
        if all_present(&self.dir) {
            self.load();
            return;
        }
        if !allow_generate {
            self.set_status(MediaStatus::Unavailable(format!(
                "generated test media is missing from {} and media generation is disabled; \
                 run fake-panel once with media generation enabled (requires ffmpeg on PATH)",
                self.dir.display()
            )));
            return;
        }
        self.set_status(MediaStatus::Generating);
        match generate_missing(&self.dir) {
            Ok(()) => self.load(),
            Err(e) => {
                tracing::warn!("test media unavailable: {e}");
                self.set_status(MediaStatus::Unavailable(e));
            }
        }
    }

    fn load(&self) {
        let mut lists = Vec::with_capacity(LIVE_CLIPS);
        for i in 0..LIVE_CLIPS {
            let path = self.dir.join(hls_dir_name(i)).join("index.m3u8");
            match fs::read_to_string(&path) {
                Ok(s) => lists.push(s),
                Err(e) => {
                    self.set_status(MediaStatus::Unavailable(format!(
                        "cannot read {}: {e}",
                        path.display()
                    )));
                    return;
                }
            }
        }
        *self
            .playlists
            .write()
            .unwrap_or_else(PoisonError::into_inner) = lists;
        self.set_status(MediaStatus::Ready);
    }

    pub(crate) fn live_ts(&self, clip: usize) -> PathBuf {
        self.dir.join(live_ts_name(clip % LIVE_CLIPS))
    }

    /// Path of an HLS segment; `segment` must already be validated by [`is_segment_name`].
    pub(crate) fn hls_segment(&self, clip: usize, segment: &str) -> PathBuf {
        self.dir.join(hls_dir_name(clip % LIVE_CLIPS)).join(segment)
    }

    /// The HLS playlist of `clip` with segment URIs rewritten to `{prefix}/{segment}`.
    pub(crate) fn playlist(&self, clip: usize, prefix: &str) -> Option<String> {
        let lists = self
            .playlists
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let raw = lists.get(clip % LIVE_CLIPS)?;
        let mut out = String::with_capacity(raw.len() + 256);
        for line in raw.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                out.push_str(t);
            } else {
                let name = t.rsplit('/').next().unwrap_or(t);
                out.push_str(prefix);
                out.push('/');
                out.push_str(name);
            }
            out.push('\n');
        }
        Some(out)
    }

    pub(crate) fn movie_file(&self, ext: &str) -> PathBuf {
        self.dir.join(match ext {
            "mp4" | "m4v" => MOVIE_MP4,
            "avi" => MOVIE_AVI,
            _ => MOVIE_MKV,
        })
    }
}

/// `seg-NNN.ts` only (no path separators, no traversal).
pub(crate) fn is_segment_name(s: &str) -> bool {
    s.strip_prefix("seg-")
        .and_then(|r| r.strip_suffix(".ts"))
        .is_some_and(|n| !n.is_empty() && n.len() <= 5 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn partial_name(final_name: &str) -> String {
    format!(".{final_name}.partial-{}", std::process::id())
}

fn run_ffmpeg(args: &[String]) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err
            .lines()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" | ");
        Err(format!("ffmpeg failed ({}): {tail}", out.status))
    }
}

/// Run ffmpeg writing to a temporary file next to `dest`, then rename into place.
fn produce(dest: &Path, mut args: Vec<String>) -> Result<(), String> {
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("bad media path {}", dest.display()))?;
    let tmp = dest.with_file_name(partial_name(name));
    args.push(tmp.to_string_lossy().into_owned());
    let res = run_ffmpeg(&args);
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if dest.exists() {
        let _ = fs::remove_file(&tmp);
        return Ok(());
    }
    fs::rename(&tmp, dest).map_err(|e| format!("rename to {} failed: {e}", dest.display()))
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn live_args(clip: usize) -> Vec<String> {
    let (video, freq) = LIVE_SOURCES[clip % LIVE_CLIPS];
    let mut a = s(&["-f", "lavfi", "-i", video, "-f", "lavfi", "-i"]);
    a.push(format!("sine=frequency={freq}:sample_rate=48000"));
    a.extend(s(&[
        "-t",
        "20",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-profile:v",
        "main",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "50",
        "-keyint_min",
        "50",
        "-sc_threshold",
        "0",
        "-b:v",
        "500k",
        "-maxrate",
        "650k",
        "-bufsize",
        "1000k",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-ac",
        "2",
        "-metadata",
    ]));
    a.push(format!("service_name=fake-panel live clip {clip}"));
    a.extend(s(&[
        "-metadata",
        "service_provider=fake-panel",
        "-f",
        "mpegts",
    ]));
    a
}

const SUBTITLES: &str = "1\n00:00:01,000 --> 00:00:04,500\nfake-panel subtitle track: cue one\n\n\
2\n00:00:06,000 --> 00:00:09,500\nUnicode check: Ελληνικά · 日本語 · café ✓\n\n\
3\n00:00:12,000 --> 00:00:15,500\nAudio track 1 beeps at 440 Hz, track 2 hums at 660 Hz\n\n\
4\n00:00:18,000 --> 00:00:21,500\n<i>Italic line</i> and a second\nline of text\n\n\
5\n00:00:24,000 --> 00:00:28,000\nEnd of the synthetic subtitle track\n";

fn mkv_args(srt: &Path) -> Vec<String> {
    let mut a = s(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=854x480:rate=25",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:beep_factor=4:sample_rate=48000",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=660:sample_rate=48000",
        "-i",
    ]);
    a.push(srt.to_string_lossy().into_owned());
    a.extend(s(&[
        "-t",
        "30",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-map",
        "2:a",
        "-map",
        "3:s",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "50",
        "-b:v",
        "700k",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-ac",
        "2",
        "-c:s",
        "srt",
        "-metadata",
        "title=fake-panel multitrack test movie",
        "-metadata:s:a:0",
        "language=eng",
        "-metadata:s:a:0",
        "title=English (440 Hz beeps)",
        "-metadata:s:a:1",
        "language=fra",
        "-metadata:s:a:1",
        "title=Français (660 Hz tone)",
        "-metadata:s:s:0",
        "language=eng",
        "-metadata:s:s:0",
        "title=English",
        "-disposition:a:0",
        "default",
        "-disposition:a:1",
        "0",
        "-disposition:s:0",
        "0",
        "-f",
        "matroska",
    ]));
    a
}

fn mp4_args() -> Vec<String> {
    s(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=25",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=330:sample_rate=48000",
        "-t",
        "30",
        "-vf",
        "hue=H=2*PI*t/10",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "50",
        "-b:v",
        "500k",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        "-ac",
        "2",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
    ])
}

fn avi_args() -> Vec<String> {
    s(&[
        "-f",
        "lavfi",
        "-i",
        "smptebars=size=480x270:rate=25",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=880:sample_rate=44100",
        "-t",
        "10",
        "-c:v",
        "mpeg4",
        "-q:v",
        "6",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "96k",
        "-f",
        "avi",
    ])
}

fn generate_hls(dir: &Path, clip: usize) -> Result<(), String> {
    let final_dir = dir.join(hls_dir_name(clip));
    let tmp_dir = dir.join(partial_name(&hls_dir_name(clip)));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir {}: {e}", tmp_dir.display()))?;
    let mut a = vec!["-i".to_owned()];
    a.push(dir.join(live_ts_name(clip)).to_string_lossy().into_owned());
    a.extend(s(&[
        "-c",
        "copy",
        "-f",
        "hls",
        "-hls_time",
        "4",
        "-hls_list_size",
        "0",
        "-hls_playlist_type",
        "vod",
        "-hls_segment_filename",
    ]));
    a.push(tmp_dir.join("seg-%03d.ts").to_string_lossy().into_owned());
    a.push(tmp_dir.join("index.m3u8").to_string_lossy().into_owned());
    if let Err(e) = run_ffmpeg(&a) {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }
    if final_dir.join("index.m3u8").is_file() {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Ok(());
    }
    let _ = fs::remove_dir_all(&final_dir);
    fs::rename(&tmp_dir, &final_dir)
        .map_err(|e| format!("rename to {} failed: {e}", final_dir.display()))
}

fn generate_missing(dir: &Path) -> Result<(), String> {
    let _guard = GENERATION_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if all_present(dir) {
        return Ok(());
    }
    if !ffmpeg_available() {
        return Err(format!(
            "ffmpeg was not found on PATH, so the test media in {} cannot be generated; \
             install ffmpeg (or copy previously generated media there) and restart fake-panel",
            dir.display()
        ));
    }
    fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    tracing::info!(
        "generating test media with ffmpeg in {} (first run only)",
        dir.display()
    );
    let started = std::time::Instant::now();
    for clip in 0..LIVE_CLIPS {
        let dest = dir.join(live_ts_name(clip));
        if !dest.is_file() {
            produce(&dest, live_args(clip))?;
        }
    }
    for clip in 0..LIVE_CLIPS {
        if !dir.join(hls_dir_name(clip)).join("index.m3u8").is_file() {
            generate_hls(dir, clip)?;
        }
    }
    let mkv = dir.join(MOVIE_MKV);
    if !mkv.is_file() {
        let srt = dir.join(partial_name("subtitles.srt"));
        fs::write(&srt, SUBTITLES).map_err(|e| format!("write {}: {e}", srt.display()))?;
        let res = produce(&mkv, mkv_args(&srt));
        let _ = fs::remove_file(&srt);
        res?;
    }
    let mp4 = dir.join(MOVIE_MP4);
    if !mp4.is_file() {
        produce(&mp4, mp4_args())?;
    }
    let avi = dir.join(MOVIE_AVI);
    if !avi.is_file() {
        produce(&avi, avi_args())?;
    }
    tracing::info!(
        "test media ready in {:.1} s",
        started.elapsed().as_secs_f32()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_names() {
        assert!(is_segment_name("seg-000.ts"));
        assert!(is_segment_name("seg-12.ts"));
        assert!(!is_segment_name("seg-.ts"));
        assert!(!is_segment_name("../seg-000.ts"));
        assert!(!is_segment_name("seg-000.ts/.."));
        assert!(!is_segment_name("index.m3u8"));
    }

    #[test]
    fn playlist_rewrite() {
        let lib = MediaLibrary::new(PathBuf::from("."));
        *lib.playlists
            .write()
            .unwrap_or_else(PoisonError::into_inner) =
            vec!["#EXTM3U\n#EXTINF:4.0,\nseg-000.ts\n#EXT-X-ENDLIST\n".to_owned(); LIVE_CLIPS];
        let out = lib.playlist(1, "/hls/tok/1").unwrap_or_default();
        assert!(out.contains("\n/hls/tok/1/seg-000.ts\n"), "{out}");
    }
}
