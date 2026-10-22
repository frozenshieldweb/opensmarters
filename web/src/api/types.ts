// SPDX-License-Identifier: GPL-3.0-or-later
// The JSON contract of the opensmarters web server (docs/WEB_PROMPT.md §4). Build strictly
// against it: every field here is in the contract. Timestamps are Unix seconds (UTC), ids are
// strings, and all text from the provider is plain text (rendered as text, never as HTML).

export interface User {
  id: string;
  name: string;
  is_admin: boolean;
}

export interface Status {
  version: string;
  setup_required: boolean;
  user: User | null;
}

export interface AdminUser {
  id: string;
  name: string;
  is_admin: boolean;
  disabled: boolean;
  created_at: number;
  last_seen_at: number | null;
}

export interface Invite {
  url: string;
  expires_at: number;
}

export type AccountKind = "xtream" | "m3u";
export type AccountStatus = "active" | "expired" | "banned" | "disabled" | "unknown";

export interface Account {
  id: string;
  name: string;
  kind: AccountKind;
  /** The server's host name only, never a URL with credentials. */
  host: string;
  status: AccountStatus;
  expires_at: number | null;
  max_connections: number | null;
  active_connections: number | null;
}

export type NewAccount =
  | { name: string; kind: "xtream"; server: string; username: string; password: string }
  | { name: string; kind: "m3u"; m3u_url: string; epg_url?: string };

export type AccountPatch = Partial<{
  name: string;
  server: string;
  username: string;
  password: string;
  m3u_url: string;
  epg_url: string;
}>;

export interface Category {
  id: string;
  name: string;
  /** Live categories only. */
  count?: number;
}

export interface Channel {
  key: string;
  name: string;
  number: number | null;
  logo: string | null;
  categories: string[];
  archive_days: number | null;
}

export interface Programme {
  start: number;
  end: number;
  title: string;
  description: string | null;
}

export interface Movie {
  id: string;
  name: string;
  poster: string | null;
  year: number | null;
  rating: number | null;
  added: number | null;
}

export interface Progress {
  position: number;
  duration: number;
}

export interface MovieDetail {
  id: string;
  name: string;
  poster: string | null;
  backdrop: string | null;
  year: number | null;
  rating: number | null;
  duration_secs: number | null;
  genre: string | null;
  plot: string | null;
  cast: string | null;
  director: string | null;
  progress: Progress | null;
}

export interface Series {
  id: string;
  name: string;
  poster: string | null;
  year: number | null;
  rating: number | null;
  updated: number | null;
}

export interface Episode {
  id: string;
  number: number;
  title: string;
  plot: string | null;
  duration_secs: number | null;
  still: string | null;
  progress: Progress | null;
}

export interface Season {
  number: number;
  name: string;
  episodes: Episode[];
}

export interface SeriesDetail {
  id: string;
  name: string;
  poster: string | null;
  backdrop: string | null;
  year: number | null;
  rating: number | null;
  genre: string | null;
  plot: string | null;
  cast: string | null;
  seasons: Season[];
}

export interface SearchResults {
  channels: Channel[];
  movies: Movie[];
  series: Series[];
}

export type Favourites = SearchResults;
export type FavouriteKind = "channel" | "movie" | "series";

export interface ContinueItem {
  kind: "movie" | "episode";
  id: string;
  series_id: string | null;
  name: string;
  poster: string | null;
  position: number;
  duration: number;
  updated: number;
}

export interface ProgressUpdate {
  kind: "movie" | "episode";
  id: string;
  series_id?: string;
  position: number;
  duration: number;
}

export type PlayKind = "live" | "movie" | "episode" | "catchup";

export interface PlayRequest {
  account: string;
  kind: PlayKind;
  id: string;
  start?: number;
  catchup_start?: number;
  catchup_minutes?: number;
  /** Episodes: the series they belong to (the server needs it to find the stream). */
  series_id?: string;
  hevc: boolean;
}

export interface PlayResponse {
  stream: string;
  url: string;
  live: boolean;
  duration: number | null;
  start: number;
}

export interface Settings {
  time_format: "24h" | "12h";
  live_format: "ts" | "m3u8";
  autoplay_next: boolean;
}

export interface About {
  version: string;
  source_url: string;
  transcode_video: "never" | "hevc" | "always";
  streams_per_user: number;
  max_streams: number;
}

export interface ApiErrorBody {
  error: { code: string; message: string };
}
