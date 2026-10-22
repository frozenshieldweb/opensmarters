// SPDX-License-Identifier: GPL-3.0-or-later
// Cached catalogue requests, keyed by account (switching or refreshing an account drops them).

import { api } from "../api/client";
import type { Category, Channel, ContinueItem, Movie, Series } from "../api/types";
import { cached } from "./cache";

const LIST_TTL = 10 * 60_000;

export const keys = {
  liveCategories: (a: string) => `${a}:live-categories`,
  channels: (a: string, c: string) => `${a}:channels:${c}`,
  movieCategories: (a: string) => `${a}:movie-categories`,
  seriesCategories: (a: string) => `${a}:series-categories`,
  movies: (a: string, c: string) => `${a}:movies:${c}`,
  series: (a: string, c: string) => `${a}:serieslist:${c}`,
  continue: (a: string) => `${a}:continue`,
};

export const liveCategories = (a: string): Promise<Category[]> =>
  cached(keys.liveCategories(a), () => api.liveCategories(a), LIST_TTL);
export const channels = (a: string, c: string): Promise<Channel[]> =>
  cached(keys.channels(a, c), () => api.channels(a, c), c === "favourites" ? 15_000 : LIST_TTL);
export const movieCategories = (a: string): Promise<Category[]> =>
  cached(keys.movieCategories(a), () => api.movieCategories(a), LIST_TTL);
export const seriesCategories = (a: string): Promise<Category[]> =>
  cached(keys.seriesCategories(a), () => api.seriesCategories(a), LIST_TTL);
export const movies = (a: string, c: string): Promise<Movie[]> => cached(keys.movies(a, c), () => api.movies(a, c), LIST_TTL);
export const series = (a: string, c: string): Promise<Series[]> => cached(keys.series(a, c), () => api.series(a, c), LIST_TTL);
export const continueWatching = (a: string): Promise<ContinueItem[]> =>
  cached(keys.continue(a), () => api.continueWatching(a), 5_000);

export type SortOrder = "provider" | "name" | "added" | "rating";

export const SORTS: { value: SortOrder; label: string }[] = [
  { value: "provider", label: "Provider order" },
  { value: "name", label: "Name" },
  { value: "added", label: "Recently added" },
  { value: "rating", label: "Rating" },
];
