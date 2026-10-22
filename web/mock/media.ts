// SPDX-License-Identifier: GPL-3.0-or-later
// Playable HLS for the mock: 60-second clips (H.264, AAC, 2 s segments) generated once with
// ffmpeg into web/.media/ (gitignored), then looped by the playlists the mock writes on the fly:
// a sliding-window live playlist, and for films a master playlist (video, two audio tracks,
// WebVTT subtitles) with media playlists of any length, all with
// #EXT-X-DISCONTINUITY where the loop restarts.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync } from "node:fs";
import { join } from "node:path";

export const SEGMENT_SECS = 2;
export const CLIP_SEGMENTS = 30;

export interface MediaDirs {
  live: string;
  /** Films and episodes: video only, plus two audio renditions (alternate tracks). */
  vodVideo: string;
  vodAudioEn: string;
  vodAudioFr: string;
}

function segmentCount(dir: string): number {
  if (!existsSync(dir)) return 0;
  return readdirSync(dir).filter((f) => /^seg\d{3}\.ts$/.test(f)).length;
}

type Kind = "muxed" | "video" | "audio";

function generate(dir: string, kind: Kind, source: string, tone: number): void {
  mkdirSync(dir, { recursive: true });
  const inputs =
    kind === "audio"
      ? ["-f", "lavfi", "-i", `sine=frequency=${tone}:sample_rate=48000`]
      : ["-f", "lavfi", "-i", `${source}=size=960x540:rate=25`, "-f", "lavfi", "-i", `sine=frequency=${tone}:sample_rate=48000`];
  const video =
    kind === "audio"
      ? ["-vn"]
      : ["-c:v", "libx264", "-preset", "veryfast", "-profile:v", "main", "-pix_fmt", "yuv420p",
         "-g", "50", "-keyint_min", "50", "-sc_threshold", "0", "-b:v", "900k", "-maxrate", "1200k", "-bufsize", "2400k"];
  const audio = kind === "video" ? ["-an"] : ["-c:a", "aac", "-b:a", "96k", "-ac", "2"];
  const args = [
    "-hide_banner", "-loglevel", "error", "-y",
    ...inputs,
    "-t", String(CLIP_SEGMENTS * SEGMENT_SECS),
    ...video,
    ...audio,
    "-f", "hls", "-hls_time", String(SEGMENT_SECS), "-hls_playlist_type", "vod",
    "-hls_segment_filename", join(dir, "seg%03d.ts"),
    join(dir, "clip.m3u8"),
  ];
  const res = spawnSync("ffmpeg", args, { stdio: ["ignore", "inherit", "inherit"] });
  if (res.status !== 0) throw new Error(`ffmpeg failed (${res.error?.message ?? "exit " + res.status}); is ffmpeg on PATH?`);
}

/** Makes sure the clips exist (generates them the first time: a few seconds). */
export function ensureMedia(root: string): MediaDirs | null {
  const dirs: MediaDirs = {
    live: join(root, "live"),
    vodVideo: join(root, "vod-video"),
    vodAudioEn: join(root, "vod-audio-en"),
    vodAudioFr: join(root, "vod-audio-fr"),
  };
  const jobs: [string, Kind, string, number][] = [
    [dirs.live, "muxed", "testsrc2", 440],
    [dirs.vodVideo, "video", "testsrc", 0],
    [dirs.vodAudioEn, "audio", "", 660],
    [dirs.vodAudioFr, "audio", "", 330],
  ];
  try {
    for (const [dir, kind, source, tone] of jobs) {
      if (segmentCount(dir) < CLIP_SEGMENTS) {
        console.log(`[mock] generating ${dir} with ffmpeg…`);
        generate(dir, kind, source, tone);
      }
    }
    return dirs;
  } catch (e) {
    console.warn(`[mock] no playable media: ${(e as Error).message}`);
    return null;
  }
}

const seg = (i: number) => `seg${String(i % CLIP_SEGMENTS).padStart(3, "0")}.ts`;

/** A live playlist: a 6-segment window sliding with the wall clock since `t0` (ms). */
export function livePlaylist(t0: number, nowMs: number): string {
  const first = Math.max(0, Math.floor((nowMs - t0) / 1000 / SEGMENT_SECS));
  const lines = [
    "#EXTM3U",
    "#EXT-X-VERSION:3",
    `#EXT-X-TARGETDURATION:${SEGMENT_SECS}`,
    `#EXT-X-MEDIA-SEQUENCE:${first}`,
    `#EXT-X-DISCONTINUITY-SEQUENCE:${Math.floor(first / CLIP_SEGMENTS)}`,
  ];
  for (let i = first; i < first + 6; i++) {
    if (i > first && i % CLIP_SEGMENTS === 0) lines.push("#EXT-X-DISCONTINUITY");
    lines.push(`#EXTINF:${SEGMENT_SECS.toFixed(3)},`, `l${i}-${seg(i)}`);
  }
  return lines.join("\n") + "\n";
}

/** The master playlist of a film: video, two audio renditions and English subtitles. */
export function vodMaster(): string {
  return [
    "#EXTM3U",
    "#EXT-X-VERSION:3",
    '#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="English",LANGUAGE="en",DEFAULT=YES,AUTOSELECT=YES,URI="audio-en.m3u8"',
    '#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="aud",NAME="Français",LANGUAGE="fr",DEFAULT=NO,AUTOSELECT=YES,URI="audio-fr.m3u8"',
    '#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",NAME="English",LANGUAGE="en",DEFAULT=NO,AUTOSELECT=NO,FORCED=NO,URI="subs-en.m3u8"',
    '#EXT-X-STREAM-INF:BANDWIDTH=1100000,RESOLUTION=960x540,CODECS="avc1.4d401e,mp4a.40.2",AUDIO="aud",SUBTITLES="subs"',
    "video.m3u8",
    "",
  ].join("\n");
}

/**
 * A film's media playlist from segment `firstSeg` (`prefix`: v, a, f or s). Like the real server
 * it's an EVENT playlist that grows as the stream is "converted" (`available` segments so far),
 * and gets #EXT-X-ENDLIST once the whole `duration` is there.
 */
export function vodPlaylist(duration: number, firstSeg: number, prefix: "v" | "a" | "f" | "s" = "v", available = Infinity): string {
  const total = Math.ceil(duration / SEGMENT_SECS);
  const last = Math.min(total, firstSeg + available);
  const lines = [
    "#EXTM3U",
    "#EXT-X-VERSION:3",
    `#EXT-X-TARGETDURATION:${SEGMENT_SECS}`,
    "#EXT-X-MEDIA-SEQUENCE:0",
    "#EXT-X-PLAYLIST-TYPE:EVENT",
  ];
  for (let i = firstSeg; i < last; i++) {
    if (i > firstSeg && i % CLIP_SEGMENTS === 0) lines.push("#EXT-X-DISCONTINUITY");
    const len = Math.min(SEGMENT_SECS, duration - i * SEGMENT_SECS);
    lines.push(`#EXTINF:${len.toFixed(3)},`, prefix === "s" ? `s${i}.vtt` : `${prefix}${i}-${seg(i)}`);
  }
  if (last >= total) lines.push("#EXT-X-ENDLIST");
  return lines.join("\n") + "\n";
}

/** The WebVTT segment `i` of a film: one cue that says where in the film it is. */
export function subtitleSegment(i: number): string {
  const local = (i % CLIP_SEGMENTS) * SEGMENT_SECS;
  const t = (n: number) => `00:${String(Math.floor(n / 60)).padStart(2, "0")}:${(n % 60).toFixed(3).padStart(6, "0")}`;
  const at = i * SEGMENT_SECS;
  const label = `${Math.floor(at / 60)}:${String(at % 60).padStart(2, "0")}`;
  return `WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:126000,LOCAL:00:00:00.000\n\n${t(local + 0.1)} --> ${t(local + 1.9)}\nSubtitles at ${label} in the film\n`;
}

/** The clip file behind a segment name the playlists use (`l123-seg003.ts` / `v9-seg009.ts`). */
export function segmentFile(dirs: MediaDirs, name: string): string | null {
  const m = /^([lvaf])\d+-(seg\d{3}\.ts)$/.exec(name);
  if (!m) return null;
  const dir = { l: dirs.live, v: dirs.vodVideo, a: dirs.vodAudioEn, f: dirs.vodAudioFr }[m[1] as "l" | "v" | "a" | "f"];
  return join(dir, m[2]!);
}
