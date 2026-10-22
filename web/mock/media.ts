// SPDX-License-Identifier: GPL-3.0-or-later
// Playable HLS for the mock: two 60-second clips (H.264 + AAC, 2 s segments) generated once with
// ffmpeg into web/.media/ (gitignored), then looped by the playlists the mock writes on the fly:
// a sliding-window live playlist and a full VOD playlist of any length, both with
// #EXT-X-DISCONTINUITY where the loop restarts.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync } from "node:fs";
import { join } from "node:path";

export const SEGMENT_SECS = 2;
export const CLIP_SEGMENTS = 30;

export interface MediaDirs {
  live: string;
  vod: string;
}

function segmentCount(dir: string): number {
  if (!existsSync(dir)) return 0;
  return readdirSync(dir).filter((f) => /^seg\d{3}\.ts$/.test(f)).length;
}

function generate(dir: string, source: string, tone: number): void {
  mkdirSync(dir, { recursive: true });
  const args = [
    "-hide_banner", "-loglevel", "error", "-y",
    "-f", "lavfi", "-i", `${source}=size=960x540:rate=25`,
    "-f", "lavfi", "-i", `sine=frequency=${tone}:sample_rate=48000`,
    "-t", String(CLIP_SEGMENTS * SEGMENT_SECS),
    "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "main", "-pix_fmt", "yuv420p",
    "-g", "50", "-keyint_min", "50", "-sc_threshold", "0", "-b:v", "900k", "-maxrate", "1200k", "-bufsize", "2400k",
    "-c:a", "aac", "-b:a", "96k", "-ac", "2",
    "-f", "hls", "-hls_time", String(SEGMENT_SECS), "-hls_playlist_type", "vod",
    "-hls_segment_filename", join(dir, "seg%03d.ts"),
    join(dir, "clip.m3u8"),
  ];
  const res = spawnSync("ffmpeg", args, { stdio: ["ignore", "inherit", "inherit"] });
  if (res.status !== 0) throw new Error(`ffmpeg failed (${res.error?.message ?? "exit " + res.status}); is ffmpeg on PATH?`);
}

/** Makes sure the two clips exist (generates them the first time: a few seconds). */
export function ensureMedia(root: string): MediaDirs | null {
  const dirs = { live: join(root, "live"), vod: join(root, "vod") };
  try {
    if (segmentCount(dirs.live) < CLIP_SEGMENTS) {
      console.log("[mock] generating the live test clip with ffmpeg…");
      generate(dirs.live, "testsrc2", 440);
    }
    if (segmentCount(dirs.vod) < CLIP_SEGMENTS) {
      console.log("[mock] generating the film test clip with ffmpeg…");
      generate(dirs.vod, "testsrc", 660);
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

/** A VOD playlist of `duration` seconds starting at segment `firstSeg`. */
export function vodPlaylist(duration: number, firstSeg: number): string {
  const total = Math.ceil(duration / SEGMENT_SECS);
  const lines = [
    "#EXTM3U",
    "#EXT-X-VERSION:3",
    `#EXT-X-TARGETDURATION:${SEGMENT_SECS}`,
    "#EXT-X-MEDIA-SEQUENCE:0",
    "#EXT-X-PLAYLIST-TYPE:VOD",
  ];
  for (let i = firstSeg; i < total; i++) {
    if (i > firstSeg && i % CLIP_SEGMENTS === 0) lines.push("#EXT-X-DISCONTINUITY");
    const len = Math.min(SEGMENT_SECS, duration - i * SEGMENT_SECS);
    lines.push(`#EXTINF:${len.toFixed(3)},`, `v${i}-${seg(i)}`);
  }
  lines.push("#EXT-X-ENDLIST");
  return lines.join("\n") + "\n";
}

/** The clip file behind a segment name the playlists use (`l123-seg003.ts` / `v9-seg009.ts`). */
export function segmentFile(dirs: MediaDirs, name: string): string | null {
  const m = /^([lv])\d+-(seg\d{3}\.ts)$/.exec(name);
  if (!m) return null;
  return join(m[1] === "l" ? dirs.live : dirs.vod, m[2]!);
}
