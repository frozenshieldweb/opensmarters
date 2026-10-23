// SPDX-License-Identifier: GPL-3.0-or-later
// In-app navigation: a stack of screens, mirrored into the browser history so the browser's
// own Back button (and a phone's back gesture) works too. Esc and Backspace go back through
// this stack only; at Home they do nothing, so they never take the user out of the app.

import { computed, signal } from "@preact/signals";

export type SettingsSection = "providers" | "playback" | "account" | "users" | "about";

export type Route =
  | { name: "home" }
  | { name: "live"; category?: string }
  | { name: "catchup"; account: string; key: string }
  | { name: "guide"; category?: string }
  | { name: "movies"; category?: string }
  | { name: "series"; category?: string }
  | { name: "movie"; id: string }
  | { name: "show"; id: string }
  | { name: "search"; q?: string }
  | { name: "favourites" }
  | { name: "settings"; section?: SettingsSection }
  | { name: "provider-new" }
  | { name: "provider-edit"; id: string }
  | { name: "watch" };

export type RouteName = Route["name"];

/** Per-visit memory of a screen (focus, scroll, selections), kept while it is in the stack. */
export interface Memo {
  focus?: string;
  [key: string]: unknown;
}

export interface Entry {
  route: Route;
  memo: Memo;
  key: number;
}

let nextKey = 1;
const entry = (route: Route, memo: Memo = {}): Entry => ({ route, memo, key: nextKey++ });

const BASE = "/app";
const SECTIONS: readonly SettingsSection[] = ["providers", "playback", "account", "users", "about"];

/** The URL of a route. */
export function urlOf(r: Route): string {
  const q = (k: string, v: string | undefined) => (v ? `?${k}=${encodeURIComponent(v)}` : "");
  switch (r.name) {
    case "home":
      return `${BASE}/`;
    case "live":
    case "guide":
    case "movies":
    case "series":
      return `${BASE}/${r.name}${q("c", r.category)}`;
    case "catchup":
      return `${BASE}/catchup/${encodeURIComponent(r.account)}/${encodeURIComponent(r.key)}`;
    case "movie":
      return `${BASE}/movies/${encodeURIComponent(r.id)}`;
    case "show":
      return `${BASE}/series/${encodeURIComponent(r.id)}`;
    case "search":
      return `${BASE}/search${q("q", r.q)}`;
    case "favourites":
      return `${BASE}/favourites`;
    case "settings":
      return `${BASE}/settings${r.section ? "/" + r.section : ""}`;
    case "provider-new":
      return `${BASE}/providers/new`;
    case "provider-edit":
      return `${BASE}/providers/${encodeURIComponent(r.id)}/edit`;
    case "watch":
      return `${BASE}/watch`;
  }
}

/** The route of a URL (unknown paths give Home). */
export function parseUrl(pathname: string, search = ""): Route {
  const params = new URLSearchParams(search);
  const c = params.get("c") ?? undefined;
  const parts = pathname
    .replace(/^\/app\/?/, "")
    .split("/")
    .filter(Boolean)
    .map((p) => {
      try {
        return decodeURIComponent(p);
      } catch {
        return p;
      }
    });
  const [a, b, d] = parts;
  switch (a) {
    case "live":
      return c ? { name: "live", category: c } : { name: "live" };
    case "guide":
      return c ? { name: "guide", category: c } : { name: "guide" };
    case "catchup":
      return b && d ? { name: "catchup", account: b, key: d } : { name: "live" };
    case "movies":
      return b ? { name: "movie", id: b } : c ? { name: "movies", category: c } : { name: "movies" };
    case "series":
      return b ? { name: "show", id: b } : c ? { name: "series", category: c } : { name: "series" };
    case "search": {
      const q = params.get("q");
      return q ? { name: "search", q } : { name: "search" };
    }
    case "favourites":
      return { name: "favourites" };
    case "settings":
      return b && (SECTIONS as readonly string[]).includes(b)
        ? { name: "settings", section: b as SettingsSection }
        : { name: "settings" };
    case "providers":
      if (b === "new") return { name: "provider-new" };
      if (b && d === "edit") return { name: "provider-edit", id: b };
      return { name: "settings", section: "providers" };
    default:
      // "watch" can't be restored after a reload (nothing is playing): go Home.
      return { name: "home" };
  }
}

/** Where Back goes from a screen opened directly (a reload or a link): its parent. */
export function parentOf(r: Route): Route | null {
  switch (r.name) {
    case "home":
      return null;
    case "movie":
      return { name: "movies" };
    case "show":
      return { name: "series" };
    case "catchup":
      return { name: "live" };
    case "provider-new":
    case "provider-edit":
      return { name: "settings", section: "providers" };
    default:
      return { name: "home" };
  }
}

/** The side-rail section a route belongs to. */
export function sectionOf(r: Route): RouteName | null {
  switch (r.name) {
    case "movie":
      return "movies";
    case "show":
      return "series";
    case "catchup":
      return "live";
    case "provider-new":
    case "provider-edit":
      return "settings";
    case "watch":
      return null;
    default:
      return r.name;
  }
}

const sameRoute = (a: Route, b: Route) => urlOf(a) === urlOf(b);

export const stack = signal<Entry[]>([entry({ name: "home" })]);
export const current = computed<Entry>(() => stack.value[stack.value.length - 1]!);

type LeaveHook = (from: Entry) => void;
const leaveHooks = new Set<LeaveHook>();
/** Runs before the current screen is left (it saves its focus and scroll into its memo). */
export function onLeave(fn: LeaveHook): () => void {
  leaveHooks.add(fn);
  return () => leaveHooks.delete(fn);
}

function leaving(): void {
  const top = current.value;
  for (const fn of leaveHooks) fn(top);
}

function writeHistory(replace: boolean): void {
  const depth = stack.value.length - 1;
  const url = urlOf(current.value.route);
  const state = { os: depth };
  if (replace) history.replaceState(state, "", url);
  else history.pushState(state, "", url);
}

/** Work to finish once an asynchronous `history.go(-n)` has landed (see goSection). */
let pending: (() => void) | null = null;

/** Opens a screen on top of the current one. */
export function go(route: Route, opts: { replace?: boolean } = {}): void {
  if (sameRoute(route, current.value.route) && !opts.replace) return;
  leaving();
  const s = stack.value;
  stack.value = opts.replace ? [...s.slice(0, -1), entry(route)] : [...s, entry(route)];
  writeHistory(!!opts.replace);
}

/**
 * Opens a side-rail section: the stack becomes Home + the section, so Back from any section
 * goes Home. The browser history is rewound to match, so its Back button agrees with Esc.
 */
export function goSection(route: Route): void {
  if (sameRoute(route, current.value.route)) return;
  const depth = stack.value.length - 1;
  if (route.name === "home") {
    if (depth > 0) {
      leaving();
      pending = null;
      history.go(-depth);
    }
    return;
  }
  if (depth === 0) {
    go(route);
  } else if (depth === 1) {
    go(route, { replace: true });
  } else {
    leaving();
    pending = () => {
      stack.value = [stack.value[0]!, entry(route)];
      writeHistory(true);
    };
    history.go(-(depth - 1));
  }
}

/** Replaces the current route's parameters in place (e.g. the selected category). */
export function setParams(route: Route): void {
  const top = current.value;
  if (sameRoute(route, top.route)) return;
  const s = stack.value;
  stack.value = [...s.slice(0, -1), { ...top, route }];
  history.replaceState({ os: s.length - 1 }, "", urlOf(route));
}

/** Goes back one screen. Returns false at Home (nothing to go back to: the app stays open). */
export function back(): boolean {
  if (stack.value.length <= 1) return false;
  leaving();
  history.back();
  return true;
}

/** The chain of screens leading to `r` (Home first). */
function chainTo(r: Route): Route[] {
  const chain: Route[] = [r];
  let p = parentOf(r);
  while (p) {
    chain.unshift(p);
    p = parentOf(p);
  }
  return chain;
}

function onPop(e: PopStateEvent): void {
  const raw = (e.state as { os?: unknown } | null)?.os;
  const depth = typeof raw === "number" && raw >= 0 ? raw : 0;
  const r = parseUrl(location.pathname, location.search);
  const s = stack.value;
  const keep = s.slice(0, Math.min(depth, s.length));
  const existing = s[keep.length];
  const top = existing && sameRoute(existing.route, r) ? existing : entry(r);
  let next = [...keep, top];
  if (next[0]!.route.name !== "home") next = [entry({ name: "home" }), ...next.slice(-1)];
  stack.value = next;
  if (next.length - 1 !== depth) history.replaceState({ os: next.length - 1 }, "", urlOf(top.route));
  const p = pending;
  pending = null;
  p?.();
}

/** Starts the router from the current URL and follows the browser's Back and Forward. */
export function startRouter(): () => void {
  const chain = chainTo(parseUrl(location.pathname, location.search));
  const here = location.pathname + location.search;
  stack.value = chain.map((r) => entry(r));
  // Rebuild the history to match the stack: Home, then each screen above it.
  history.replaceState({ os: 0 }, "", urlOf(chain[0]!));
  chain.slice(1).forEach((r, i) => history.pushState({ os: i + 1 }, "", urlOf(r)));
  if (chain.length === 1 && here !== urlOf(chain[0]!)) history.replaceState({ os: 0 }, "", urlOf(chain[0]!));
  window.addEventListener("popstate", onPop);
  return () => window.removeEventListener("popstate", onPop);
}

/** Resets to a single screen (after signing in or switching account). */
export function resetTo(route: Route = { name: "home" }): void {
  const depth = stack.value.length - 1;
  const apply = () => {
    const chain = chainTo(route);
    stack.value = chain.map((r) => entry(r));
    history.replaceState({ os: 0 }, "", urlOf(chain[0]!));
    chain.slice(1).forEach((r, i) => history.pushState({ os: i + 1 }, "", urlOf(r)));
  };
  if (depth > 0) {
    pending = apply;
    history.go(-depth);
  } else {
    apply();
  }
}
