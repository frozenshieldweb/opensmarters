// SPDX-License-Identifier: GPL-3.0-or-later
// Now/next programmes for channel rows: requested in batches (≤ 200 keys per call, §4) as rows
// come into view, cached until the current programme ends.

import { signal } from "@preact/signals";
import { api } from "../api/client";
import type { Programme } from "../api/types";

interface Entry {
  progs: Programme[];
  /** Refetch after this time (the end of the current programme, or a minute for "no data"). */
  until: number;
}

const cache = new Map<string, Entry>();
const inflight = new Set<string>();
const queue = new Map<string, Set<string>>();
let timer: ReturnType<typeof setTimeout> | null = null;

/** Bumped when new programme data arrives (components read it to re-render). */
export const epgVersion = signal(0);

const k = (account: string, key: string) => `${account}\u0000${key}`;
const nowS = () => Date.now() / 1000;

/** The now/next programmes of a channel, if loaded (`[]` when the provider has none). */
export function nowNext(account: string, key: string): Programme[] | undefined {
  void epgVersion.value;
  const e = cache.get(k(account, key));
  if (!e) return undefined;
  const t = nowS();
  // Drop programmes that have ended (the next one becomes "now" until the refetch lands).
  return e.progs.filter((p) => p.end > t);
}

/** Asks for the now/next of these channels (missing or stale ones only). */
export function requestNowNext(account: string, keys: readonly string[]): void {
  const t = nowS();
  let q = queue.get(account);
  for (const key of keys) {
    const id = k(account, key);
    const e = cache.get(id);
    if (inflight.has(id) || (e && e.until > t)) continue;
    if (!q) {
      q = new Set();
      queue.set(account, q);
    }
    q.add(key);
  }
  if (q && q.size > 0 && !timer) timer = setTimeout(flush, 40);
}

function flush(): void {
  timer = null;
  for (const [account, keys] of queue) {
    queue.delete(account);
    const all = [...keys];
    for (let i = 0; i < all.length; i += 200) {
      const batch = all.slice(i, i + 200);
      for (const key of batch) inflight.add(k(account, key));
      api.epgNow(account, batch).then(
        (res) => {
          const t = nowS();
          for (const key of batch) {
            const progs = (res[key] ?? []).slice().sort((a, b) => a.start - b.start);
            const current = progs.find((p) => p.start <= t && p.end > t);
            cache.set(k(account, key), { progs, until: current ? current.end : t + 60 });
            inflight.delete(k(account, key));
          }
          epgVersion.value++;
        },
        () => {
          const t = nowS();
          for (const key of batch) {
            inflight.delete(k(account, key));
            cache.set(k(account, key), { progs: [], until: t + 60 });
          }
          epgVersion.value++;
        },
      );
    }
  }
}

/** Forgets everything (account removed or refreshed). */
export function clearEpg(): void {
  cache.clear();
  epgVersion.value++;
}
