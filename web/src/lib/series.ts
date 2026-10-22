// SPDX-License-Identifier: GPL-3.0-or-later
// Series logic without UI: where "Continue" starts, and the episodes that follow one (the
// next episode at the end).

import type { Episode, Season, SeriesDetail } from "../api/types";
import { cap } from "./text";

/** Watched this much of a film or episode: it counts as finished (the server's rule too). */
export const FINISHED = 0.95;

/** A film or episode watched for at least this long (seconds) offers Resume. */
export const RESUME_AFTER = 10;

/** One of the episodes that follow (enough to play it and show what it is). */
export interface EpisodeRef {
  id: string;
  seriesId: string;
  title: string;
  subtitle: string;
  duration: number | null;
}

/** Where "Continue" starts: the episode in progress, else the one after the last watched. */
export function continuePoint(s: SeriesDetail): { season: Season; episode: Episode; resume: number; label: string } | null {
  const all = s.seasons.flatMap((season) => season.episodes.map((episode) => ({ season, episode })));
  if (all.length === 0) return null;
  let lastIdx = -1;
  all.forEach((x, i) => {
    if (x.episode.progress && x.episode.progress.position > 0) lastIdx = i;
  });
  if (lastIdx < 0) {
    const f = all[0]!;
    return { ...f, resume: 0, label: `Play S${f.season.number} · E${f.episode.number}` };
  }
  const last = all[lastIdx]!;
  const pr = last.episode.progress!;
  if (pr.position < pr.duration * FINISHED) {
    return { ...last, resume: pr.position, label: `Continue S${last.season.number} · E${last.episode.number}` };
  }
  const n = all[lastIdx + 1];
  if (!n) return { ...last, resume: 0, label: `Watch S${last.season.number} · E${last.episode.number} again` };
  return { ...n, resume: 0, label: `Continue S${n.season.number} · E${n.episode.number}` };
}

/** The episodes after `episodeId`, in order. */
export function episodesAfter(s: SeriesDetail, episodeId: string): EpisodeRef[] {
  const all = s.seasons.flatMap((se) => se.episodes.map((e) => ({ se, e })));
  const i = all.findIndex((x) => x.e.id === episodeId);
  return all.slice(i + 1).map(({ se, e }) => ({
    id: e.id,
    seriesId: s.id,
    title: cap(s.name, 120),
    subtitle: `S${se.number} · E${e.number} · ${cap(e.title, 80)}`,
    duration: e.duration_secs ?? null,
  }));
}
