// SPDX-License-Identifier: GPL-3.0-or-later
// The current account's favourites: loaded once per account, toggled optimistically.

import { signal } from "@preact/signals";
import { api } from "../api/client";
import type { Channel, FavouriteKind, Favourites, Movie, Series } from "../api/types";
import { errorText, invalidate } from "./cache";
import { toast } from "./misc";

export const favourites = signal<Favourites | null>(null);
let loadedFor: string | null = null;
let loading: Promise<void> | null = null;

/** Loads the favourites of `account` (once; `force` reloads). */
export function loadFavourites(account: string, force = false): Promise<void> {
  if (!force && loadedFor === account && (favourites.value || loading)) return loading ?? Promise.resolve();
  if (loadedFor !== account) favourites.value = null;
  loadedFor = account;
  const p = api.favourites(account).then(
    (f) => {
      if (loadedFor === account) favourites.value = f;
    },
    () => {
      if (loadedFor === account) favourites.value = favourites.value ?? { channels: [], movies: [], series: [] };
    },
  );
  loading = p.finally(() => {
    if (loading === p) loading = null;
  });
  return p;
}

function listOf(f: Favourites, kind: FavouriteKind): { id: string }[] {
  if (kind === "channel") return f.channels.map((c) => ({ id: c.key }));
  if (kind === "movie") return f.movies;
  return f.series;
}

/** True if the item is a favourite (reactive). */
export function isFavourite(kind: FavouriteKind, id: string): boolean {
  const f = favourites.value;
  return !!f && listOf(f, kind).some((x) => x.id === id);
}

type Item = { kind: "channel"; item: Channel } | { kind: "movie"; item: Movie } | { kind: "series"; item: Series };

/** Adds or removes a favourite. */
export async function toggleFavourite(account: string, it: Item): Promise<void> {
  const f = favourites.value ?? { channels: [], movies: [], series: [] };
  const id = it.kind === "channel" ? it.item.key : it.item.id;
  const on = !isFavourite(it.kind, id);
  const before = f;
  let next: Favourites;
  if (it.kind === "channel") {
    next = { ...f, channels: on ? [...f.channels, it.item] : f.channels.filter((c) => c.key !== id) };
  } else if (it.kind === "movie") {
    next = { ...f, movies: on ? [...f.movies, it.item] : f.movies.filter((m) => m.id !== id) };
  } else {
    next = { ...f, series: on ? [...f.series, it.item] : f.series.filter((s) => s.id !== id) };
  }
  favourites.value = next;
  try {
    if (on) await api.addFavourite(account, it.kind, id);
    else await api.removeFavourite(account, it.kind, id);
    // The "favourites" live category changes too.
    invalidate(`${account}:channels:favourites`);
    toast(on ? "Added to Favourites" : "Removed from Favourites", "success");
  } catch (e) {
    favourites.value = before;
    toast(errorText(e), "error");
  }
}
