// SPDX-License-Identifier: GPL-3.0-or-later
// A small request cache with in-flight de-duplication, and the `useLoad` hook screens use to
// fetch data with loading and error states.

import { useCallback, useEffect, useRef, useState } from "preact/hooks";
import { ApiError } from "../api/client";

interface Slot {
  promise: Promise<unknown>;
  data?: unknown;
  at: number;
  done: boolean;
}

const slots = new Map<string, Slot>();

/** Fetches through the cache: a fresh result (younger than `ttlMs`) is reused. */
export function cached<T>(key: string, fetcher: () => Promise<T>, ttlMs = 5 * 60_000): Promise<T> {
  const s = slots.get(key);
  if (s && (!s.done || Date.now() - s.at < ttlMs)) return s.promise as Promise<T>;
  const slot: Slot = { promise: Promise.resolve(), at: Date.now(), done: false };
  slot.promise = fetcher().then(
    (v) => {
      slot.data = v;
      slot.done = true;
      slot.at = Date.now();
      return v;
    },
    (e: unknown) => {
      if (slots.get(key) === slot) slots.delete(key);
      throw e;
    },
  );
  slots.set(key, slot);
  return slot.promise as Promise<T>;
}

/** The cached value of `key`, if it has arrived. */
export function peek<T>(key: string): T | undefined {
  const s = slots.get(key);
  return s?.done ? (s.data as T) : undefined;
}

/** Forgets every entry whose key starts with `prefix` (or everything). */
export function invalidate(prefix = ""): void {
  for (const k of [...slots.keys()]) if (k.startsWith(prefix)) slots.delete(k);
}

export function errorText(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  return "Something went wrong. Try again.";
}

export interface Loaded<T> {
  data: T | undefined;
  error: string | null;
  loading: boolean;
  reload: () => void;
}

/**
 * Loads `key` with `fetcher` (through the cache) while the component is mounted. A `null` key
 * loads nothing. Changing the key reloads; the previous data stays until the new data arrives
 * only when `keepPrevious` is set.
 */
export function useLoad<T>(
  key: string | null,
  fetcher: () => Promise<T>,
  opts: { ttlMs?: number; keepPrevious?: boolean } = {},
): Loaded<T> {
  const [state, setState] = useState<{ key: string | null; data: T | undefined; error: string | null }>(() => ({
    key,
    data: key ? peek<T>(key) : undefined,
    error: null,
  }));
  const [nonce, setNonce] = useState(0);
  const fetchRef = useRef(fetcher);
  fetchRef.current = fetcher;

  useEffect(() => {
    if (!key) {
      setState({ key, data: undefined, error: null });
      return;
    }
    let live = true;
    const hit = peek<T>(key);
    setState((s) => ({ key, data: hit ?? (opts.keepPrevious ? s.data : undefined), error: null }));
    cached(key, () => fetchRef.current(), opts.ttlMs).then(
      (data) => live && setState({ key, data, error: null }),
      (e: unknown) => live && setState({ key, data: undefined, error: errorText(e) }),
    );
    return () => {
      live = false;
    };
  }, [key, nonce]);

  const reload = useCallback(() => {
    if (key) invalidate(key);
    setNonce((n) => n + 1);
  }, [key]);

  const fresh = state.key === key;
  return {
    data: fresh ? state.data : opts.keepPrevious ? state.data : undefined,
    error: fresh ? state.error : null,
    loading: !!key && (!fresh || (state.data === undefined && state.error === null)),
    reload,
  };
}

/** True once `on` has stayed true for `ms` (skeletons appear only after 150 ms, §6.16). */
export function useDelayed(on: boolean, ms = 150): boolean {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!on) {
      setShown(false);
      return;
    }
    const t = setTimeout(() => setShown(true), ms);
    return () => clearTimeout(t);
  }, [on, ms]);
  return on && shown;
}
