// SPDX-License-Identifier: GPL-3.0-or-later
// Playback: one <video> element for the whole app (the Live TV preview and the fullscreen
// player share it, so going fullscreen never restarts the stream). The server remuxes every
// stream to HLS (WEB_PROMPT.md §3); hls.js plays it through Media Source Extensions, or the
// browser natively on Apple devices.
//
// Contract: POST /api/play (with `hevc` from the browser's own codec check), the HLS URL it
// returns, DELETE /api/streams/{sid} when the player closes or zaps, PUT …/progress every
// ~10 s and on close for films and episodes. Positions are `start + video.currentTime`: a
// VOD seek past what's available restarts the stream at the new position (`start`).

import { batch, computed, signal } from "@preact/signals";
import type Hls from "hls.js";
import type * as HlsModule from "hls.js";
import type { ErrorData } from "hls.js";
import workerUrl from "hls.js/dist/hls.worker.js?url";
import { api, ApiError } from "../api/client";
import type { Channel, PlayKind, PlayResponse } from "../api/types";
import { toast } from "../state/misc";
import type { EpisodeRef } from "../lib/series";

export type { EpisodeRef };

// ---- what is playing


export type Item =
  | { kind: "live"; account: string; channel: Channel; list: Channel[] }
  | {
      kind: "movie";
      account: string;
      id: string;
      title: string;
      subtitle: string;
      duration: number | null;
    }
  | {
      kind: "episode";
      account: string;
      id: string;
      seriesId: string;
      title: string;
      subtitle: string;
      duration: number | null;
      /** The following episodes, in order (next episode at the end). */
      next: EpisodeRef[];
    };

export type Phase = "idle" | "starting" | "buffering" | "playing" | "paused" | "ended" | "error";

export interface PlayError {
  title: string;
  message: string;
  /** Retrying makes sense (the stream was refused or the connection dropped). */
  retry: boolean;
}

export interface Track {
  id: number;
  label: string;
  detail: string;
}

export const item = signal<Item | null>(null);
export const phase = signal<Phase>("idle");
export const error = signal<PlayError | null>(null);
/** Absolute position in seconds (VOD). */
export const position = signal(0);
/** Total duration in seconds (VOD), from the server. */
export const duration = signal<number | null>(null);
/** How far the stream is available (absolute seconds): seeks inside it don't restart. */
export const availableEnd = signal(0);
export const muted = signal(false);
export const audioTracks = signal<Track[]>([]);
export const audioTrack = signal(-1);
export const subtitleTracks = signal<Track[]>([]);
export const subtitleTrack = signal(-1);
export const reconnecting = signal(0);
export const isLive = computed(() => item.value?.kind === "live");
/** The player's OSD is on screen (subtitles move above it). */
export const osdShown = signal(false);

let video: HTMLVideoElement | null = null;
let hls: Hls | null = null;
let sid: string | null = null;
let offset = 0;
let generation = 0;
let progressTimer: ReturnType<typeof setInterval> | null = null;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
let lastSaved = -1;
let mediaRecoveries = 0;
/** Silent restarts of a film after a network error (reset once it plays again). */
let vodRestarts = 0;
let hlsModule: Promise<typeof HlsModule> | null = null;

const loadHls = () => (hlsModule ??= import("hls.js"));

// ---- capabilities

const HEVC = 'video/mp4; codecs="hvc1.1.6.L120.90"';

/** True if this browser can play HEVC (MSE, or Safari's native player). */
export function canPlayHevc(): boolean {
  const w = window as unknown as { ManagedMediaSource?: typeof MediaSource };
  const mse = window.MediaSource ?? w.ManagedMediaSource;
  try {
    if (mse?.isTypeSupported?.(HEVC)) return true;
  } catch {
    // Some browsers throw on unknown codec strings.
  }
  const v = document.createElement("video");
  return v.canPlayType("application/vnd.apple.mpegurl") !== "" && v.canPlayType(HEVC) !== "";
}

/** Apple devices play HLS natively (and better); everything else uses hls.js. */
function preferNative(v: HTMLVideoElement): boolean {
  return /Apple/.test(navigator.vendor) && v.canPlayType("application/vnd.apple.mpegurl") !== "";
}

// ---- the video element

/** Called by the video layer once with its element. */
export function attachVideo(v: HTMLVideoElement): void {
  video = v;
  v.muted = muted.value;
  const set = (p: Phase) => {
    if (phase.value !== "error" && phase.value !== "ended") phase.value = p;
  };
  v.addEventListener("waiting", () => item.value && set("buffering"));
  v.addEventListener("stalled", () => item.value && set("buffering"));
  v.addEventListener("playing", () => {
    if (!item.value) return;
    reconnecting.value = 0;
    vodRestarts = 0;
    set("playing");
  });
  v.addEventListener("pause", () => item.value && !v.ended && set("paused"));
  v.addEventListener("timeupdate", () => {
    if (!item.value) return;
    position.value = offset + v.currentTime;
    const s = v.seekable;
    availableEnd.value = s.length ? offset + s.end(s.length - 1) : offset;
  });
  v.addEventListener("ended", () => {
    if (!item.value || item.value.kind === "live") return;
    phase.value = "ended";
    void saveProgress(true);
  });
  v.addEventListener("volumechange", () => {
    muted.value = v.muted;
  });
  v.addEventListener("error", () => {
    if (!item.value || hls) return; // hls.js reports its own errors
    fail(mediaError(v.error));
  });
}

function mediaError(e: MediaError | null): PlayError {
  if (e?.code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED || e?.code === MediaError.MEDIA_ERR_DECODE) {
    return {
      title: "This format isn't supported",
      message: "Your browser can't play this stream's video or audio format.",
      retry: false,
    };
  }
  return { title: "Connection lost", message: "The stream stopped. Check your connection and try again.", retry: true };
}

// ---- start and stop

function apiError(e: unknown): PlayError {
  if (e instanceof ApiError) {
    if (e.code === "limit_reached") return { title: "Stream limit reached", message: e.message, retry: true };
    if (e.code === "provider_error") return { title: "Your provider refused the stream", message: e.message, retry: true };
    if (e.status === 415 || /unsupported|format|codec/.test(e.code)) {
      return { title: "This format isn't supported", message: e.message, retry: false };
    }
    if (e.status === 0) return { title: "Can't reach the server", message: e.message, retry: true };
    return { title: "Can't play this", message: e.message, retry: e.status >= 500 };
  }
  return { title: "Can't play this", message: "Something went wrong. Try again.", retry: true };
}

function teardown(): void {
  if (retryTimer) clearTimeout(retryTimer);
  retryTimer = null;
  if (progressTimer) clearInterval(progressTimer);
  progressTimer = null;
  if (hls) {
    hls.destroy();
    hls = null;
  }
  if (video) {
    video.removeAttribute("src");
    video.load();
  }
}

function releaseStream(): void {
  if (sid) {
    const old = sid;
    sid = null;
    api.stopStream(old, { keepalive: true }).catch(() => {});
  }
}

function playKind(it: Item): PlayKind {
  return it.kind;
}

function itemId(it: Item): string {
  return it.kind === "live" ? it.channel.key : it.id;
}

/** Plays `it` (from `start` seconds for films and episodes). */
export async function play(it: Item, start = 0): Promise<void> {
  const gen = ++generation;
  if (item.value && item.value.kind !== "live" && itemId(item.value) !== itemId(it)) await saveProgress(true);
  teardown();
  releaseStream();
  mediaRecoveries = 0;
  batch(() => {
    item.value = it;
    phase.value = "starting";
    error.value = null;
    position.value = start;
    duration.value = it.kind === "live" ? null : it.duration;
    availableEnd.value = start;
    audioTracks.value = [];
    subtitleTracks.value = [];
    audioTrack.value = -1;
    subtitleTrack.value = -1;
  });
  lastSaved = -1;
  let res: PlayResponse;
  try {
    res = await api.play({
      account: it.account,
      kind: playKind(it),
      id: itemId(it),
      ...(it.kind !== "live" && start > 0 ? { start: Math.floor(start) } : {}),
      ...(it.kind === "episode" ? { series_id: it.seriesId } : {}),
      hevc: canPlayHevc(),
    });
  } catch (e) {
    if (gen !== generation) return;
    fail(apiError(e));
    return;
  }
  if (gen !== generation) {
    // Superseded while starting (fast zapping): give this one back at once.
    api.stopStream(res.stream, { keepalive: true }).catch(() => {});
    return;
  }
  sid = res.stream;
  offset = res.start || 0;
  if (res.duration) duration.value = res.duration;
  position.value = offset;
  await attachSource(res.url, res.live, gen);
  if (gen !== generation) return;
  if (it.kind !== "live") {
    progressTimer = setInterval(() => void saveProgress(false), 10_000);
  }
}

async function attachSource(url: string, live: boolean, gen: number): Promise<void> {
  const v = video;
  if (!v) return;
  if (preferNative(v)) {
    v.src = url;
    v.addEventListener("loadedmetadata", () => gen === generation && readNativeTracks(v), { once: true });
  } else {
    const { default: HlsCtor } = await loadHls();
    if (gen !== generation) return;
    if (!HlsCtor.isSupported()) {
      if (v.canPlayType("application/vnd.apple.mpegurl")) {
        v.src = url;
      } else {
        fail({ title: "This browser can't play video streams", message: "Try a current version of Chrome, Edge, Firefox or Safari.", retry: false });
        return;
      }
    } else {
      const h = new HlsCtor({
        workerPath: workerUrl,
        enableWorker: true,
        startPosition: live ? -1 : 0,
        liveSyncDurationCount: 3,
        maxBufferLength: 30,
        backBufferLength: 30,
        manifestLoadingMaxRetry: 2,
        levelLoadingMaxRetry: 3,
        fragLoadingMaxRetry: 3,
        capLevelToPlayerSize: true,
      });
      hls = h;
      h.on(HlsCtor.Events.AUDIO_TRACKS_UPDATED, () => {
        audioTracks.value = h.audioTracks.map((t, i) => ({ id: i, label: t.name || t.lang || `Track ${i + 1}`, detail: t.lang ?? "" }));
        audioTrack.value = h.audioTrack;
      });
      h.on(HlsCtor.Events.AUDIO_TRACK_SWITCHED, () => {
        audioTrack.value = h.audioTrack;
      });
      h.on(HlsCtor.Events.SUBTITLE_TRACKS_UPDATED, () => {
        subtitleTracks.value = h.subtitleTracks.map((t, i) => ({ id: i, label: t.name || t.lang || `Subtitles ${i + 1}`, detail: t.lang ?? "" }));
        subtitleTrack.value = h.subtitleTrack;
      });
      h.on(HlsCtor.Events.SUBTITLE_TRACK_SWITCH, () => {
        subtitleTrack.value = h.subtitleTrack;
      });
      h.on(HlsCtor.Events.ERROR, (_e, data) => onHlsError(h, data, gen));
      h.attachMedia(v);
      h.loadSource(url);
    }
  }
  try {
    await v.play();
  } catch (e) {
    if (gen !== generation) return;
    if (e instanceof DOMException && e.name === "NotAllowedError") {
      // Autoplay blocked: the user presses Play.
      phase.value = "paused";
    }
  }
}

function onHlsError(h: Hls, data: ErrorData, gen: number): void {
  if (gen !== generation || !data.fatal) return;
  const it = item.value;
  const details = String(data.details);
  if (/IncompatibleCodecs|codec/i.test(details)) {
    fail({ title: "This format isn't supported", message: "Your browser can't play this stream's video or audio format.", retry: false });
    return;
  }
  if (data.type === "mediaError" && mediaRecoveries < 2) {
    mediaRecoveries++;
    h.recoverMediaError();
    return;
  }
  const status = (data.response as { code?: number } | undefined)?.code;
  if (status === 409) {
    // stream_replaced: this account started another stream (another tab or device).
    fail({
      title: "Playing somewhere else",
      message: "You started watching something else with this account, so this stream stopped.",
      retry: true,
    });
    return;
  }
  if (it?.kind === "live") {
    reconnect();
    return;
  }
  // Films and episodes: the server stops a stream nobody asked for in 30 s (a long pause).
  // Start it again where it was, once, before saying anything.
  if (it && vodRestarts < 1) {
    vodRestarts++;
    void play(it, position.value).then(() => {
      vodRestarts = 1;
    });
    return;
  }
  fail({ title: "Connection lost", message: "The stream stopped. Check your connection and try again.", retry: true });
}

/** Live: start the channel again with a growing delay (2, 4, 8… s), up to 5 times. */
function reconnect(): void {
  const it = item.value;
  if (!it || it.kind !== "live") return;
  const n = reconnecting.value + 1;
  if (n > 5) {
    reconnecting.value = 0;
    fail({ title: "Connection lost", message: "We couldn't get the channel back. Try again in a moment.", retry: true });
    return;
  }
  reconnecting.value = n;
  phase.value = "buffering";
  toast(`Reconnecting… (${n})`, "warning");
  teardown();
  releaseStream();
  const gen = generation;
  retryTimer = setTimeout(() => {
    if (gen !== generation || item.value !== it) return;
    void play(it);
  }, 1000 * 2 ** (n - 1));
}

function fail(e: PlayError): void {
  teardown();
  releaseStream();
  batch(() => {
    error.value = e;
    phase.value = "error";
  });
}

function readNativeTracks(v: HTMLVideoElement): void {
  const at = (v as unknown as { audioTracks?: { length: number; [i: number]: { label: string; language: string; enabled: boolean } } }).audioTracks;
  if (at && at.length > 0) {
    const list: Track[] = [];
    let sel = -1;
    for (let i = 0; i < at.length; i++) {
      const t = at[i]!;
      list.push({ id: i, label: t.label || t.language || `Track ${i + 1}`, detail: t.language });
      if (t.enabled) sel = i;
    }
    audioTracks.value = list;
    audioTrack.value = sel;
  }
  const subs: Track[] = [];
  for (let i = 0; i < v.textTracks.length; i++) {
    const t = v.textTracks[i]!;
    if (t.kind === "subtitles" || t.kind === "captions") subs.push({ id: i, label: t.label || t.language || `Subtitles ${i + 1}`, detail: t.language });
  }
  subtitleTracks.value = subs;
}

/** Stops playback and gives the stream back to the server. */
export function stop(): void {
  generation++;
  void saveProgress(true);
  teardown();
  releaseStream();
  mediaRecoveries = 0;
  batch(() => {
    item.value = null;
    phase.value = "idle";
    error.value = null;
    reconnecting.value = 0;
    audioTracks.value = [];
    subtitleTracks.value = [];
  });
}

/** Tries again after an error (VOD resumes where it stopped). */
export function retry(): void {
  const it = item.value;
  if (!it) return;
  reconnecting.value = 0;
  mediaRecoveries = 0;
  void play(it, it.kind === "live" ? 0 : position.value);
}

// ---- controls

export function togglePause(): void {
  const v = video;
  if (!v || !item.value) return;
  if (phase.value === "ended") {
    seekTo(0);
    return;
  }
  if (v.paused) void v.play().catch(() => {});
  else v.pause();
}

export function toggleMute(): boolean {
  const v = video;
  muted.value = !muted.value;
  if (v) v.muted = muted.value;
  return muted.value;
}

/** Seeks to an absolute position (VOD). */
export function seekTo(abs: number): void {
  const it = item.value;
  const v = video;
  if (!it || it.kind === "live" || !v) return;
  const dur = duration.value ?? Infinity;
  const target = Math.max(0, Math.min(abs, dur - 1));
  position.value = target;
  if (phase.value === "ended") phase.value = "buffering";
  const s = v.seekable;
  const end = s.length ? offset + s.end(s.length - 1) : offset;
  if (target >= offset && target <= end) {
    v.currentTime = target - offset;
    if (v.paused) void v.play().catch(() => {});
  } else {
    // Outside what the server has transcoded: restart the stream there.
    void play(it, target);
  }
}

export function seekBy(delta: number): void {
  seekTo(position.value + delta);
}

export function setAudioTrack(id: number): void {
  if (hls) hls.audioTrack = id;
  else if (video) {
    const at = (video as unknown as { audioTracks?: { length: number; [i: number]: { enabled: boolean } } }).audioTracks;
    if (at) for (let i = 0; i < at.length; i++) at[i]!.enabled = i === id;
  }
  audioTrack.value = id;
}

export function setSubtitleTrack(id: number): void {
  if (hls) {
    hls.subtitleTrack = id;
    hls.subtitleDisplay = id >= 0;
  } else if (video) {
    for (let i = 0; i < video.textTracks.length; i++) video.textTracks[i]!.mode = i === id ? "showing" : "disabled";
  }
  subtitleTrack.value = id;
}

/** Zaps to the neighbouring channel in the playing list (no wrap at the ends). */
export function zap(step: number): Channel | null {
  const it = item.value;
  if (!it || it.kind !== "live" || step === 0) return null;
  const i = it.list.findIndex((c) => c.key === it.channel.key);
  const j = i + Math.sign(step);
  const next = it.list[j];
  if (i < 0 || !next) return null;
  void play({ ...it, channel: next });
  return next;
}

/** Zaps to a channel number; returns the channel or null. */
export function zapNumber(n: number, list?: Channel[]): Channel | null {
  const it = item.value;
  const pool = list ?? (it?.kind === "live" ? it.list : []);
  const ch = pool.find((c) => c.number === n);
  if (!ch || !it || it.kind !== "live") return ch ?? null;
  void play({ ...it, channel: ch, list: pool });
  return ch;
}

/** The next episode after the playing one, if any. */
export const nextEpisode = computed<EpisodeRef | null>(() => {
  const it = item.value;
  return it?.kind === "episode" ? (it.next[0] ?? null) : null;
});

/** Plays the next episode (from its start). */
export function playNext(): void {
  const it = item.value;
  if (!it || it.kind !== "episode") return;
  const [n, ...rest] = it.next;
  if (!n) return;
  void play({ kind: "episode", account: it.account, id: n.id, seriesId: n.seriesId, title: n.title, subtitle: n.subtitle, duration: n.duration, next: rest });
}

/** Saves the position (VOD) every ~10 s, and when closing or switching. */
export async function saveProgress(final: boolean): Promise<void> {
  const it = item.value;
  const dur = duration.value;
  if (!it || it.kind === "live" || !dur) return;
  const pos = Math.floor(phase.value === "ended" ? dur : position.value);
  if (pos <= 0 || (!final && Math.abs(pos - lastSaved) < 5)) return;
  lastSaved = pos;
  try {
    await api.progress(
      it.account,
      {
        kind: it.kind,
        id: it.id,
        ...(it.kind === "episode" ? { series_id: it.seriesId } : {}),
        position: pos,
        duration: Math.floor(dur),
      },
      { keepalive: final },
    );
  } catch {
    // Progress is best effort; the next save tries again.
  }
}

// Closing the tab or the app: give the stream back and save the position.
window.addEventListener("pagehide", () => {
  if (!item.value) return;
  void saveProgress(true);
  releaseStream();
});
