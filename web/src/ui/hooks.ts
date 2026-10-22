// SPDX-License-Identifier: GPL-3.0-or-later
// Screen hooks: initial focus and its restoration, back handlers, screen keys, the breakpoint.

import { signal } from "@preact/signals";
import { useEffect, useLayoutEffect, useRef } from "preact/hooks";
import { focusFallback, focusInRail, focusKey, hasFocusIn, interactionStamp, pushBack, pushKeys } from "../focus/dom";
import type { Memo } from "../state/router";

/**
 * Focuses the screen once its content is `ready`: the remembered item when returning to the
 * screen, else its autofocus item, else its top-left item. It doesn't take the focus from the
 * user if they've pressed something since the screen opened (except out of the side rail when
 * they haven't touched anything).
 */
export function useInitialFocus(ready: boolean, memo?: Memo): void {
  const stamp = useRef(interactionStamp());
  const done = useRef(false);
  useLayoutEffect(() => {
    if (!ready || done.current) return;
    done.current = true;
    const root = document.querySelector('[data-group="content"]') ?? undefined;
    if (!root) return;
    const userActed = interactionStamp() !== stamp.current;
    if (userActed && hasFocusIn(root) && !focusInRail()) return;
    if (userActed && focusInRail()) return;
    if (memo?.focus && focusKey(memo.focus, root)) return;
    focusFallback(root);
  }, [ready]);
}

/** Registers a back handler while mounted (`active`). Return true to consume Esc/Backspace. */
export function useBack(handler: () => boolean, active = true): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => (active ? pushBack(() => ref.current()) : undefined), [active]);
}

/** Registers screen keys (run before the focus engine). Return true to claim the key. */
export function useKeys(handler: (e: KeyboardEvent) => boolean, active = true): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => (active ? pushKeys((e) => ref.current(e)) : undefined), [active]);
}

export type Breakpoint = "compact" | "medium" | "wide";

function bp(): Breakpoint {
  const w = window.innerWidth;
  return w < 960 ? "compact" : w < 1440 ? "medium" : "wide";
}

/** The layout breakpoint (DESIGN.md §3): compact < 960 ≤ medium < 1440 ≤ wide. */
export const breakpoint = signal<Breakpoint>(bp());
window.addEventListener("resize", () => {
  const b = bp();
  if (b !== breakpoint.value) breakpoint.value = b;
});

/** Calls `fn` every `ms` while mounted. */
export function useInterval(fn: () => void, ms: number): void {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    const t = setInterval(() => ref.current(), ms);
    return () => clearInterval(t);
  }, [ms]);
}
