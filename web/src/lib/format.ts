// SPDX-License-Identifier: GPL-3.0-or-later
// Plain-language formatting of times, durations and numbers.

export type TimeFormat = "24h" | "12h";

const pad2 = (n: number) => String(n).padStart(2, "0");

/** Wall-clock time of a Unix timestamp (seconds): "15:05" or "3:05 pm". */
export function clock(ts: number, fmt: TimeFormat = "24h"): string {
  const d = new Date(ts * 1000);
  const h = d.getHours();
  const m = pad2(d.getMinutes());
  if (fmt === "12h") {
    const h12 = h % 12 === 0 ? 12 : h % 12;
    return `${h12}:${m} ${h < 12 ? "am" : "pm"}`;
  }
  return `${pad2(h)}:${m}`;
}

/** "15:00–15:45". */
export function timeRange(start: number, end: number, fmt: TimeFormat = "24h"): string {
  return `${clock(start, fmt)}–${clock(end, fmt)}`;
}

/** A playback position: "4:05", "1:02:13". */
export function timecode(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return h > 0 ? `${h}:${pad2(m)}:${pad2(r)}` : `${m}:${pad2(r)}`;
}

/** A running time: "1 h 52", "45 min". */
export function runtime(secs: number | null | undefined): string {
  if (!secs || secs <= 0) return "";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} min`;
  return `${Math.floor(mins / 60)} h ${pad2(mins % 60)}`;
}

/** "39 min left", "1 h 05 left". */
export function remaining(secs: number): string {
  const mins = Math.max(1, Math.round(secs / 60));
  if (mins < 60) return `${mins} min left`;
  return `${Math.floor(mins / 60)} h ${pad2(mins % 60)} left`;
}

/** Fraction 0..1 of `now` between `start` and `end`. */
export function fraction(start: number, end: number, now: number): number {
  if (end <= start) return 0;
  return Math.min(1, Math.max(0, (now - start) / (end - start)));
}

/** A date: "21 Oct 2026". */
export function date(ts: number): string {
  return new Date(ts * 1000).toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric" });
}

/** "today", "yesterday", "3 days ago", or a date. */
export function ago(ts: number, now = Date.now() / 1000): string {
  const days = Math.floor((now - ts) / 86400);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days} days ago`;
  return date(ts);
}

/** "★ 7.4" style rating with one decimal, or "". */
export function rating(r: number | null | undefined): string {
  return r && r > 0 ? r.toFixed(1) : "";
}

/** Joins the non-empty parts with " · ". */
export function dot(...parts: (string | number | null | undefined | false)[]): string {
  return parts.filter((p) => p !== null && p !== undefined && p !== false && p !== "").join(" · ");
}

/** "1 channel", "242 channels". */
export function plural(n: number, one: string, many = one + "s"): string {
  return `${n.toLocaleString("en-GB")} ${n === 1 ? one : many}`;
}

/** Unix seconds now. */
export const nowSecs = (): number => Math.floor(Date.now() / 1000);
