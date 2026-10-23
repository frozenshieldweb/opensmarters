// SPDX-License-Identifier: GPL-3.0-or-later
// Catch-up (pure, unit-tested): archived programmes grouped into days on the viewer's clock
// ("Today", "Yesterday", then weekday + date), newest day and newest programme first, like
// the Windows app (crates/app/src/screens/catchup/days.rs). The provider's own clock only
// matters to the server, which builds the archive link.

import type { Channel, Programme } from "../api/types";

export interface CatchupDay {
  /** "2026-10-21" on the viewer's clock. */
  key: string;
  /** "Today", "Yesterday", "Mon 19 Oct". */
  label: string;
  /** Newest first. */
  programmes: Programme[];
}

interface Civil {
  y: number;
  m: number;
  d: number;
  /** 0 = Sunday. */
  wd: number;
}

const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"] as const;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"] as const;

/**
 * The calendar date of a Unix time on the viewer's clock, or at a fixed offset from UTC in
 * minutes (`offsetMin`, for tests and for clocks other than the browser's).
 */
export function civil(ts: number, offsetMin?: number): Civil {
  if (offsetMin === undefined) {
    const d = new Date(ts * 1000);
    return { y: d.getFullYear(), m: d.getMonth(), d: d.getDate(), wd: d.getDay() };
  }
  const d = new Date((ts + offsetMin * 60) * 1000);
  return { y: d.getUTCFullYear(), m: d.getUTCMonth(), d: d.getUTCDate(), wd: d.getUTCDay() };
}

const dayNumber = (c: Civil) => Math.round(Date.UTC(c.y, c.m, c.d) / 86_400_000);
const pad2 = (n: number) => String(n).padStart(2, "0");

/** "Today", "Yesterday", else "Sun 18 Oct" (relative to `today`). */
export function dayLabel(date: Civil, today: Civil): string {
  const diff = dayNumber(today) - dayNumber(date);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  if (diff === -1) return "Tomorrow";
  return `${WEEKDAYS[date.wd]} ${date.d} ${MONTHS[date.m]}`;
}

/** Groups programmes into days on the viewer's clock, newest day and programme first. */
export function groupByDay(programmes: readonly Programme[], now: number, offsetMin?: number): CatchupDay[] {
  const today = civil(now, offsetMin);
  const sorted = [...programmes].sort((a, b) => b.start - a.start);
  const days: CatchupDay[] = [];
  for (const p of sorted) {
    const c = civil(p.start, offsetMin);
    const key = `${c.y}-${pad2(c.m + 1)}-${pad2(c.d)}`;
    const last = days[days.length - 1];
    if (last && last.key === key) last.programmes.push(p);
    else days.push({ key, label: dayLabel(c, today), programmes: [p] });
  }
  return days;
}

/** Whole minutes of a programme, rounded up, at least 1 (what the archive link asks for). */
export function minutesOf(p: Pick<Programme, "start" | "end">): number {
  return Math.max(1, Math.ceil((p.end - p.start) / 60));
}

/**
 * Whether a programme of the guide can be watched as catch-up at `now`: it has ended, and the
 * channel keeps an archive reaching back to its start.
 */
export function canCatchUp(channel: Pick<Channel, "archive_days">, p: Pick<Programme, "start" | "end">, now: number): boolean {
  const days = channel.archive_days ?? 0;
  return days > 0 && p.end <= now && p.start >= now - days * 86_400;
}
