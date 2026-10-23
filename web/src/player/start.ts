// SPDX-License-Identifier: GPL-3.0-or-later
// Starting playback from anywhere (Home, detail pages, search, favourites): builds the
// playback item (title, subtitle, the episodes that follow) and opens the player.

import { api } from "../api/client";
import type { Channel, ContinueItem, MovieDetail, Programme, SeriesDetail } from "../api/types";
import { cached } from "../state/cache";
import { current, go } from "../state/router";
import { clock, dot, runtime, type TimeFormat } from "../lib/format";
import { dayLabel, civil, minutesOf } from "../lib/catchup";
import { cap } from "../lib/text";
import { play } from "./playback";
import { episodesAfter } from "../lib/series";

function openPlayer(): void {
  if (current.value.route.name !== "watch") go({ name: "watch" });
}

export function seriesDetail(account: string, id: string): Promise<SeriesDetail> {
  return cached(`${account}:series:${id}`, () => api.seriesDetail(account, id), 60_000);
}

export function movieDetail(account: string, id: string): Promise<MovieDetail> {
  return cached(`${account}:movie:${id}`, () => api.movie(account, id), 60_000);
}

/** Plays a live channel fullscreen (zapping walks `list`). */
export function watchChannel(account: string, channel: Channel, list: Channel[]): void {
  void play({ kind: "live", account, channel, list: list.length ? list : [channel] });
  openPlayer();
}

/** Plays a past programme of a channel from its archive (catch-up), from `start` seconds. */
export function watchCatchup(account: string, channel: Channel, p: Programme, fmt: TimeFormat, start = 0): void {
  const day = dayLabel(civil(p.start), civil(Date.now() / 1000));
  void play(
    {
      kind: "catchup",
      account,
      channel,
      programme: p,
      title: cap(p.title, 160) || cap(channel.name, 120),
      subtitle: dot(cap(channel.name, 60), `${day} ${clock(p.start, fmt)}`, runtime(p.end - p.start)),
      duration: minutesOf(p) * 60,
    },
    start,
  );
  openPlayer();
}

/** Plays a film from `start` seconds. */
export function watchMovie(account: string, m: Pick<MovieDetail, "id" | "name" | "year" | "genre" | "duration_secs">, start = 0): void {
  void play(
    {
      kind: "movie",
      account,
      id: m.id,
      title: cap(m.name, 160),
      subtitle: dot(m.year ?? undefined, cap(m.genre?.split(",")[0] ?? "", 40), runtime(m.duration_secs)),
      duration: m.duration_secs ?? null,
    },
    start,
  );
  openPlayer();
}

/** Plays an episode of a series from `start` seconds. */
export function watchEpisode(account: string, s: SeriesDetail, episodeId: string, start = 0): void {
  for (const se of s.seasons) {
    const e = se.episodes.find((x) => x.id === episodeId);
    if (!e) continue;
    void play(
      {
        kind: "episode",
        account,
        id: e.id,
        seriesId: s.id,
        title: cap(s.name, 120),
        subtitle: `S${se.number} · E${e.number} · ${cap(e.title, 80)}`,
        duration: e.duration_secs ?? null,
        next: episodesAfter(s, e.id),
      },
      start,
    );
    openPlayer();
    return;
  }
}

/** Resumes a Continue watching item. */
export async function resumeItem(account: string, c: ContinueItem): Promise<void> {
  if (c.kind === "movie") {
    const d = await movieDetail(account, c.id).catch(() => null);
    watchMovie(account, d ?? { id: c.id, name: c.name, year: null, genre: null, duration_secs: c.duration }, c.position);
    return;
  }
  if (!c.series_id) return;
  const s = await seriesDetail(account, c.series_id);
  watchEpisode(account, s, c.id, c.position);
}
