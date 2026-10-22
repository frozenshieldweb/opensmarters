// SPDX-License-Identifier: GPL-3.0-or-later
// Helpers for provider text. Provider strings are untrusted: they're always rendered as text
// nodes (never as HTML), and capped before layout (DESIGN.md §4.7).

/** Caps a provider string to `max` characters (by code point), with an ellipsis. */
export function cap(s: string | null | undefined, max = 200): string {
  if (!s) return "";
  const t = s.trim();
  const chars = Array.from(t);
  return chars.length > max ? chars.slice(0, max - 1).join("") + "…" : t;
}

/** The first one or two letters or digits of a name, for a monogram tile (DESIGN.md §6.4). */
export function monogram(name: string): string {
  const words = name
    .normalize("NFC")
    .split(/[\s\-_.:|/]+/u)
    .map((w) => Array.from(w).filter((c) => /[\p{L}\p{N}]/u.test(c)))
    .filter((w) => w.length > 0);
  if (words.length === 0) return "";
  if (words.length === 1) return words[0]!.slice(0, 2).join("").toUpperCase();
  return (words[0]![0]! + words[1]![0]!).toUpperCase();
}

/** A stable 32-bit hash (FNV-1a) of a string. */
export function hash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** One of the six muted monogram hues (0..5), picked by a stable hash of the name. */
export function hue(name: string): number {
  return hash(name) % 6;
}

/** Case- and accent-insensitive comparison key. */
export function fold(s: string): string {
  return s
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLocaleLowerCase();
}

const collator = new Intl.Collator(undefined, { sensitivity: "base", numeric: true });
/** Natural, locale-aware name order. */
export const byName = (a: string, b: string): number => collator.compare(a, b);
