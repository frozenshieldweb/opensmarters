// SPDX-License-Identifier: GPL-3.0-or-later
// The mock provider's synthetic catalogue: deterministic (seeded), fictional, and big enough
// to exercise virtualisation (≈ 230 channels, 1 500 films, 200 series) and edge cases (long,
// quoted, emoji, Greek and Japanese names, channels without guide or logo).

import * as N from "./names.ts";
import type { Category, Channel, Episode, Movie, MovieDetail, Programme, Season, Series, SeriesDetail } from "../src/api/types.ts";

/** mulberry32: a small seeded PRNG. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function hashStr(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

const pick = <T>(r: () => number, xs: readonly T[]): T => xs[Math.floor(r() * xs.length)]!;

type Theme = keyof typeof N.EPG;

export interface MockChannel extends Channel {
  theme: Theme;
  /** No programme guide (radio stations). */
  noEpg: boolean;
  /** Scripted behaviour of the mock stream. */
  behaviour: "ok" | "limit" | "refused" | "drops" | "hevc";
}

export interface MockMovie extends Movie {
  category: string;
  detail: Omit<MovieDetail, "progress">;
}

export interface MockSeries extends Series {
  category: string;
  detail: Omit<SeriesDetail, "seasons">;
  seasons: (Omit<Season, "episodes"> & { episodes: Omit<Episode, "progress">[] })[];
}

export interface Catalogue {
  liveCategories: Category[];
  channels: MockChannel[];
  movieCategories: Category[];
  movies: MockMovie[];
  seriesCategories: Category[];
  series: MockSeries[];
  episodes: Map<string, { series: MockSeries; season: number; episode: Omit<Episode, "progress"> }>;
}

const img = (kind: string, id: string) => `/api/img/${kind}-${encodeURIComponent(id)}`;

function plot(r: () => number): string {
  return `${pick(r, N.PLOT_SUBJECTS)} ${pick(r, N.PLOT_VERBS)} ${pick(r, N.PLOT_OBJECTS)}. ${pick(r, N.PLOT_TWISTS)}`;
}

function person(r: () => number): string {
  return `${pick(r, N.FIRST)} ${pick(r, N.LAST)}`;
}

export function buildCatalogue(now: number): Catalogue {
  const r = rng(20261021);

  // ---- Live TV
  const liveDefs: { id: string; name: string; theme: Theme; names: () => string[] }[] = [
    { id: "news", name: "News", theme: "news", names: () => brandNames(r, N.SUFFIX.news, 30) },
    { id: "sports", name: "Sports", theme: "sports", names: () => brandNames(r, N.SUFFIX.sports, 30) },
    { id: "kids", name: "Kids", theme: "kids", names: () => brandNames(r, N.SUFFIX.kids, 18) },
    { id: "docs", name: "Documentary", theme: "doc", names: () => brandNames(r, N.SUFFIX.doc, 18) },
    { id: "music", name: "Music", theme: "music", names: () => brandNames(r, N.SUFFIX.music, 16) },
    { id: "ent", name: "Entertainment", theme: "ent", names: () => brandNames(r, N.SUFFIX.ent, 26) },
    {
      id: "variety",
      name: "International Variety, Culture, Lifestyle and General Interest Channels From Every Continent — Backup Feeds (HD / FHD / UHD) [Updated Weekly]",
      theme: "variety",
      names: () => [...N.VARIETY_SPECIALS, ...brandNames(r, N.SUFFIX.variety, 8)],
    },
    { id: "sussex", name: "Sussex Local", theme: "local", names: () => N.SUSSEX },
    { id: "essex", name: "Essex FM", theme: "local", names: () => N.ESSEX },
    { id: "greek", name: "Ελληνικά", theme: "greek", names: () => N.GREEK },
    { id: "japanese", name: "日本語", theme: "japanese", names: () => N.JAPANESE },
    { id: "cinema", name: "Cinema", theme: "emoji", names: () => N.EMOJI },
  ];
  const channels: MockChannel[] = [];
  const liveCategories: Category[] = [];
  let number = 1;
  for (const def of liveDefs) {
    const names = def.names();
    liveCategories.push({ id: def.id, name: def.name, count: names.length });
    for (const name of names) {
      const n = number++;
      const key = `ch${n}`;
      const radio = def.id === "essex" || /\b(FM|Radio)\b/.test(name);
      channels.push({
        key,
        name,
        number: n,
        logo: r() < 0.62 && !radio ? img("l", key) : null,
        categories: [def.id],
        archive_days: def.theme === "news" && n % 3 === 0 ? 7 : null,
        theme: def.theme,
        noEpg: radio,
        behaviour: n === 13 ? "limit" : n === 14 ? "refused" : n === 9 ? "drops" : name.includes("Θάλασσα HD") ? "hevc" : "ok",
      });
    }
  }

  // ---- Movies
  const movieCats: Category[] = [
    { id: "m-new", name: "New releases" },
    { id: "m-drama", name: "Drama" },
    { id: "m-comedy", name: "Comedy" },
    { id: "m-thriller", name: "Thriller & Mystery" },
    { id: "m-scifi", name: "Science fiction" },
    { id: "m-family", name: "Family" },
    { id: "m-doc", name: "Documentary" },
    { id: "m-greek", name: "Ελληνικές ταινίες" },
    { id: "m-specials", name: "Odd titles" },
  ];
  const movies: MockMovie[] = [];
  const seenTitles = new Set<string>();
  const movieTitle = (): string => {
    for (;;) {
      const a = pick(r, N.MOVIE_ADJ);
      const b = pick(r, N.MOVIE_NOUN);
      const c = pick(r, N.MOVIE_NOUN);
      const forms = [`The ${a} ${b}`, `${b} of the ${a} ${c}`, `Beyond the ${a} ${b}`, `A ${a} ${b}`, `Lantern of the ${a} ${c}`];
      const t = pick(r, forms);
      if (!seenTitles.has(t) || seenTitles.size > 1800) {
        seenTitles.add(t);
        return t;
      }
    }
  };
  const addMovie = (category: string, name: string) => {
    const id = String(10000 + movies.length);
    const year = 1968 + Math.floor(r() * 59);
    const rating = r() < 0.08 ? null : Math.round((4 + r() * 5) * 10) / 10;
    const added = now - Math.floor(r() * 720) * 86400 - Math.floor(r() * 86400);
    const poster = r() < 0.96 ? img("p", id) : null;
    const genre = [pick(r, N.GENRES), pick(r, N.GENRES)].filter((g, i, a) => a.indexOf(g) === i).join(", ");
    movies.push({
      id,
      name,
      poster,
      year,
      rating,
      added,
      category,
      detail: {
        id,
        name,
        poster,
        backdrop: r() < 0.9 ? img("b", id) : null,
        year,
        rating,
        duration_secs: (78 + Math.floor(r() * 70)) * 60,
        genre,
        plot: plot(r),
        cast: [person(r), person(r), person(r), person(r)].join(", "),
        director: person(r),
      },
    });
  };
  const counts: Record<string, number> = {
    "m-new": 120, "m-drama": 320, "m-comedy": 260, "m-thriller": 240, "m-scifi": 180, "m-family": 160, "m-doc": 140,
  };
  for (const [cat, n] of Object.entries(counts)) for (let i = 0; i < n; i++) addMovie(cat, movieTitle());
  for (const t of N.GREEK_MOVIES) addMovie("m-greek", t);
  for (const t of N.MOVIE_SPECIALS) addMovie("m-specials", t);
  // New releases were added recently.
  for (const m of movies) if (m.category === "m-new") m.added = now - Math.floor(r() * 21) * 86400 - Math.floor(r() * 86400);

  // ---- Series
  const seriesCats: Category[] = [
    { id: "s-drama", name: "Drama" },
    { id: "s-crime", name: "Crime" },
    { id: "s-comedy", name: "Comedy" },
    { id: "s-kids", name: "Kids" },
    { id: "s-anime", name: "アニメ" },
  ];
  const series: MockSeries[] = [];
  const episodes: Catalogue["episodes"] = new Map();
  const seriesNames = new Set<string>();
  const addSeries = (category: string, name: string) => {
    const id = String(50000 + series.length);
    const year = 1995 + Math.floor(r() * 31);
    const rating = Math.round((5 + r() * 4) * 10) / 10;
    const poster = img("q", id);
    const nSeasons = 1 + Math.floor(r() * 4);
    const seasons: MockSeries["seasons"] = [];
    for (let s = 1; s <= nSeasons; s++) {
      const eps: Omit<Episode, "progress">[] = [];
      const nEps = 6 + Math.floor(r() * 5);
      for (let e = 1; e <= nEps; e++) {
        const eid = `${id}-${s}-${e}`;
        const ep = {
          id: eid,
          number: e,
          title: e === 1 && s === 1 ? "Pilot" : pick(r, N.EPISODE_TITLES),
          plot: plot(r),
          duration_secs: (42 + Math.floor(r() * 18)) * 60,
          still: r() < 0.9 ? img("s", eid) : null,
        };
        eps.push(ep);
      }
      seasons.push({ number: s, name: `Season ${s}`, episodes: eps });
    }
    const updated = now - Math.floor(r() * 400) * 86400;
    const s: MockSeries = {
      id,
      name,
      poster,
      year,
      rating,
      updated,
      category,
      detail: {
        id,
        name,
        poster,
        backdrop: img("c", id),
        year,
        rating,
        genre: [pick(r, N.GENRES), pick(r, N.GENRES)].filter((g, i, a) => a.indexOf(g) === i).join(", "),
        plot: plot(r),
        cast: [person(r), person(r), person(r), person(r), person(r)].join(", "),
      },
      seasons,
    };
    series.push(s);
    for (const se of seasons) for (const ep of se.episodes) episodes.set(ep.id, { series: s, season: se.number, episode: ep });
  };
  const seriesTitle = () => {
    for (;;) {
      const t = `${pick(r, N.BRANDS)} ${pick(r, N.SERIES_NOUN)}`;
      if (!seriesNames.has(t)) {
        seriesNames.add(t);
        return t;
      }
    }
  };
  for (const [cat, n] of [["s-drama", 70], ["s-crime", 50], ["s-comedy", 45], ["s-kids", 30]] as const) {
    for (let i = 0; i < n; i++) addSeries(cat, seriesTitle());
  }
  for (const t of N.ANIME_SERIES) addSeries("s-anime", t);

  return { liveCategories, channels, movieCategories: movieCats, movies, seriesCategories: seriesCats, series, episodes };
}

function brandNames(r: () => number, suffixes: readonly string[], n: number): string[] {
  const out = new Set<string>();
  while (out.size < n) out.add(`${pick(r, N.BRANDS)} ${pick(r, suffixes)}${r() < 0.12 ? " HD" : ""}`);
  return [...out];
}

// ---- EPG: a deterministic schedule per channel and day.

const SLOT_MINUTES = [15, 30, 30, 45, 60, 60, 90, 120];

/** The programmes of `ch` overlapping [from, to). */
export function schedule(ch: MockChannel, from: number, to: number): Programme[] {
  if (ch.noEpg) return [];
  const out: Programme[] = [];
  const day = 86400;
  let dayStart = Math.floor(from / day) * day - day;
  while (dayStart < to) {
    const r = rng(hashStr(`${ch.key}:${dayStart}`));
    let t = dayStart;
    let seg = 1;
    while (t < dayStart + day) {
      const mins = SLOT_MINUTES[Math.floor(r() * SLOT_MINUTES.length)]!;
      const end = Math.min(t + mins * 60, dayStart + day);
      const base = pick(r, N.EPG[ch.theme]);
      const title = r() < 0.18 ? `${base} — Repeat` : r() < 0.1 ? `${base} (Live)` : base;
      if (end > from && t < to) {
        out.push({
          start: t,
          end,
          title,
          description:
            r() < 0.9
              ? `${base} — a synthetic programme on ${ch.name}. Segment ${seg} of the day's schedule; café-style chat and naïve plots, all invented for testing.`
              : null,
        });
      }
      t = end;
      seg++;
    }
    dayStart += day;
  }
  return out;
}
