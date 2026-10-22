// SPDX-License-Identifier: GPL-3.0-or-later
// The HTTP client for /api (docs/WEB_PROMPT.md §4). Same origin only, the session cookie is
// sent by the browser, and every non-GET request carries `X-Opensmarters: 1` (the CSRF guard).

import type {
  About,
  Account,
  AccountPatch,
  AdminUser,
  ApiErrorBody,
  Category,
  Channel,
  ContinueItem,
  FavouriteKind,
  Favourites,
  Invite,
  Movie,
  MovieDetail,
  NewAccount,
  PlayRequest,
  PlayResponse,
  Programme,
  ProgressUpdate,
  SearchResults,
  Series,
  SeriesDetail,
  Settings,
  Status,
  User,
} from "./types";

/** An error from the API, with the server's plain-English message. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

/** Fallback messages when the server didn't send one (or isn't reachable). */
function fallbackMessage(status: number): string {
  if (status === 0) return "Can't reach the server. Check your connection and try again.";
  if (status === 401) return "Your session has ended. Sign in again.";
  if (status === 403) return "You don't have access to this.";
  if (status === 404) return "This isn't available any more.";
  if (status === 429) return "Too many attempts. Wait a minute and try again.";
  if (status === 502) return "Your provider didn't answer. Try again in a moment.";
  if (status === 503) return "The server is busy. Try again in a moment.";
  return "Something went wrong. Try again.";
}

type Listener = (e: ApiError) => void;
const unauthorizedListeners = new Set<Listener>();

/** Called when any request comes back 401 (the session expired or was ended elsewhere). */
export function onUnauthorized(fn: Listener): () => void {
  unauthorizedListeners.add(fn);
  return () => unauthorizedListeners.delete(fn);
}

function isErrorBody(v: unknown): v is ApiErrorBody {
  if (typeof v !== "object" || v === null || !("error" in v)) return false;
  const e = (v as { error: unknown }).error;
  return (
    typeof e === "object" &&
    e !== null &&
    typeof (e as { code?: unknown }).code === "string" &&
    typeof (e as { message?: unknown }).message === "string"
  );
}

export interface RequestOptions {
  signal?: AbortSignal;
  /** Don't report a 401 to the session (sign-in forms expect it). */
  quiet401?: boolean;
  /** Keep the request alive while the page unloads (closing the player). */
  keepalive?: boolean;
}

export async function request<T>(
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  body?: unknown,
  opts: RequestOptions = {},
): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method !== "GET") headers["X-Opensmarters"] = "1";
  if (body !== undefined) headers["Content-Type"] = "application/json";
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers,
      body: body === undefined ? null : JSON.stringify(body),
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal: opts.signal ?? null,
      keepalive: opts.keepalive ?? false,
    });
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") throw e;
    throw new ApiError(0, "network", fallbackMessage(0));
  }
  let data: unknown = null;
  const text = await res.text();
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }
  if (!res.ok) {
    const err = isErrorBody(data)
      ? new ApiError(res.status, data.error.code, data.error.message || fallbackMessage(res.status))
      : new ApiError(res.status, "http_" + res.status, fallbackMessage(res.status));
    if (res.status === 401 && !opts.quiet401) {
      for (const fn of unauthorizedListeners) fn(err);
    }
    throw err;
  }
  return data as T;
}

const enc = encodeURIComponent;
const acc = (id: string) => `/api/accounts/${enc(id)}`;

/** Every endpoint of §4, typed. */
export const api = {
  // Session and users
  status: (o?: RequestOptions) => request<Status>("GET", "/api/status", undefined, o),
  setup: (setup_code: string, name: string, password: string) =>
    request<{ user: User }>("POST", "/api/setup", { setup_code, name, password }, { quiet401: true }),
  login: (name: string, password: string) =>
    request<{ user: User }>("POST", "/api/login", { name, password }, { quiet401: true }),
  logout: () => request<Record<string, never>>("POST", "/api/logout", undefined, { quiet401: true }),
  changePassword: (current: string, next: string) =>
    request<Record<string, never>>("POST", "/api/password", { current, new: next }, { quiet401: true }),
  users: () => request<AdminUser[]>("GET", "/api/admin/users"),
  invite: () => request<Invite>("POST", "/api/admin/invites", {}),
  disableUser: (id: string) => request<Record<string, never>>("POST", `/api/admin/users/${enc(id)}/disable`),
  enableUser: (id: string) => request<Record<string, never>>("POST", `/api/admin/users/${enc(id)}/enable`),
  deleteUser: (id: string) => request<Record<string, never>>("DELETE", `/api/admin/users/${enc(id)}`),
  acceptInvite: (token: string, name: string, password: string) =>
    request<{ user: User }>("POST", "/api/invites/accept", { token, name, password }, { quiet401: true }),

  // Provider accounts
  accounts: () => request<Account[]>("GET", "/api/accounts"),
  addAccount: (a: NewAccount) => request<Account>("POST", "/api/accounts", a),
  editAccount: (id: string, p: AccountPatch) => request<Account>("PATCH", acc(id), p),
  removeAccount: (id: string) => request<Record<string, never>>("DELETE", acc(id)),
  refreshAccount: (id: string) => request<Account>("POST", `${acc(id)}/refresh`),

  // Catalogue
  liveCategories: (a: string, o?: RequestOptions) =>
    request<Category[]>("GET", `${acc(a)}/live/categories`, undefined, o),
  channels: (a: string, category: string, o?: RequestOptions) =>
    request<Channel[]>("GET", `${acc(a)}/live/channels?category=${enc(category)}`, undefined, o),
  epgNow: (a: string, keys: string[], o?: RequestOptions) =>
    request<Record<string, Programme[]>>(
      "GET",
      `${acc(a)}/epg/now?keys=${keys.map(enc).join(",")}`,
      undefined,
      o,
    ),
  epgChannel: (a: string, key: string, from: number, to: number, o?: RequestOptions) =>
    request<Programme[]>("GET", `${acc(a)}/epg/channel/${enc(key)}?from=${from}&to=${to}`, undefined, o),
  movieCategories: (a: string, o?: RequestOptions) =>
    request<Category[]>("GET", `${acc(a)}/movies/categories`, undefined, o),
  seriesCategories: (a: string, o?: RequestOptions) =>
    request<Category[]>("GET", `${acc(a)}/series/categories`, undefined, o),
  movies: (a: string, category: string, o?: RequestOptions) =>
    request<Movie[]>("GET", `${acc(a)}/movies?category=${enc(category)}`, undefined, o),
  movie: (a: string, id: string, o?: RequestOptions) =>
    request<MovieDetail>("GET", `${acc(a)}/movies/${enc(id)}`, undefined, o),
  series: (a: string, category: string, o?: RequestOptions) =>
    request<Series[]>("GET", `${acc(a)}/series?category=${enc(category)}`, undefined, o),
  seriesDetail: (a: string, id: string, o?: RequestOptions) =>
    request<SeriesDetail>("GET", `${acc(a)}/series/${enc(id)}`, undefined, o),
  search: (a: string, q: string, o?: RequestOptions) =>
    request<SearchResults>("GET", `${acc(a)}/search?q=${enc(q)}`, undefined, o),
  favourites: (a: string, o?: RequestOptions) =>
    request<Favourites>("GET", `${acc(a)}/favourites`, undefined, o),
  addFavourite: (a: string, kind: FavouriteKind, item: string) =>
    request<Record<string, never>>("PUT", `${acc(a)}/favourites/${kind}/${enc(item)}`),
  removeFavourite: (a: string, kind: FavouriteKind, item: string) =>
    request<Record<string, never>>("DELETE", `${acc(a)}/favourites/${kind}/${enc(item)}`),
  continueWatching: (a: string, o?: RequestOptions) =>
    request<ContinueItem[]>("GET", `${acc(a)}/continue`, undefined, o),
  progress: (a: string, p: ProgressUpdate, o?: RequestOptions) =>
    request<Record<string, never>>("PUT", `${acc(a)}/progress`, p, o),

  // Playback
  play: (r: PlayRequest, o?: RequestOptions) => request<PlayResponse>("POST", "/api/play", r, o),
  stopStream: (sid: string, o?: RequestOptions) =>
    request<Record<string, never>>("DELETE", `/api/streams/${enc(sid)}`, undefined, o),

  // Settings
  settings: () => request<Settings>("GET", "/api/settings"),
  saveSettings: (s: Settings) => request<Settings>("PUT", "/api/settings", s),
  about: () => request<About>("GET", "/api/about"),
};
