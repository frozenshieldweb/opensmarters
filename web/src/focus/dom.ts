// SPDX-License-Identifier: GPL-3.0-or-later
// The DOM glue of the focus engine (DESIGN.md §5): arrow keys move the real DOM focus to the
// spatial nearest neighbour, Enter/Space activate (native <button> behaviour), Esc and
// Backspace go back (never through the browser's history away from the app), a moving mouse
// pointer moves the focus (one highlight for mouse and keyboard), and every focus change is
// remembered per scope and group.
//
// Markup contract:
// - `data-group="rail" | "content"` on regions; `data-scope="…"` on lists, rails and forms.
// - Focusable: buttons, links, inputs, and anything with `data-f`. Rows of virtualised lists
//   are buttons with tabindex -1 (still focusable by script, so they count here).
// - `data-autofocus` marks a screen's preferred initial focus; `data-fk="…"` gives an item a
//   stable key so the focus can be restored when returning to a screen.
// - A container that handles some arrows itself (a virtualised list) calls preventDefault()
//   on the keydown; the engine skips events already handled.
// - An open dialog or the player is a `[data-modal]` layer; the rest of the app is `inert`.

import { FocusMemory, fallback, navigate, type Candidate, type Dir } from "./engine";

const SELECTOR =
  'button, a[href], input:not([type="hidden"]), select, textarea, [data-f]';

export const RAIL_GROUP = "rail";
export const CONTENT_GROUP = "content";

const memory = new FocusMemory<Element>();

type BackHandler = () => boolean;
const backHandlers: BackHandler[] = [];
type KeyHandler = (e: KeyboardEvent) => boolean;
const keyHandlers: KeyHandler[] = [];
let defaultBack: () => void = () => {};

/** Input modality: the focus ring hides after touch input. */
function setModality(m: "keys" | "pointer" | "touch"): void {
  const root = document.documentElement;
  if (root.dataset["input"] !== m) root.dataset["input"] = m;
}

export function isTextInput(el: Element | null): el is HTMLInputElement | HTMLTextAreaElement {
  if (!el) return false;
  if (el instanceof HTMLTextAreaElement) return true;
  if (!(el instanceof HTMLInputElement)) return false;
  return !["button", "checkbox", "radio", "range", "submit", "reset", "color", "file", "image"].includes(el.type);
}

function isUsable(el: Element): el is HTMLElement {
  if (!(el instanceof HTMLElement)) return false;
  if ((el as HTMLButtonElement).disabled) return false;
  if (el.closest("[inert]")) return false;
  if (el.dataset["f"] === "no") return false;
  const r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return false;
  const style = getComputedStyle(el);
  return style.visibility !== "hidden";
}

/** The layer navigation is confined to: the top-most modal, else the app. */
export function activeRoot(): HTMLElement {
  const modals = document.querySelectorAll<HTMLElement>("[data-modal]");
  const top = modals[modals.length - 1];
  return top ?? document.getElementById("app") ?? document.body;
}

function candidateOf(el: HTMLElement): Candidate<Element> {
  const r = el.getBoundingClientRect();
  const scopeEl = el.closest<HTMLElement>("[data-scope]");
  const groupEl = el.closest<HTMLElement>("[data-group]");
  const g = groupEl?.getBoundingClientRect();
  return {
    id: el,
    rect: { x: r.left, y: r.top, w: r.width, h: r.height },
    scope: scopeEl?.dataset["scope"] ?? groupEl?.dataset["group"] ?? "",
    group: groupEl?.dataset["group"] ?? "",
    autofocus: el.hasAttribute("data-autofocus"),
    ...(g ? { groupRect: { x: g.left, y: g.top, w: g.width, h: g.height } } : {}),
  };
}

/** Every usable focusable item of the active layer (or of `root`). */
export function candidates(root: Element = activeRoot()): Candidate<Element>[] {
  const out: Candidate<Element>[] = [];
  for (const el of root.querySelectorAll(SELECTOR)) {
    if (isUsable(el)) out.push(candidateOf(el));
  }
  return out;
}

const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Focuses `el` and scrolls it (and its ring) into view. */
export function focusEl(el: Element | null | undefined, opts: { scroll?: boolean } = {}): boolean {
  if (!(el instanceof HTMLElement)) return false;
  el.focus({ preventScroll: true });
  if (opts.scroll !== false) {
    el.scrollIntoView({ block: "nearest", inline: "nearest", behavior: reducedMotion() ? "auto" : "smooth" });
  }
  return document.activeElement === el;
}

/** Moves the focus one step in `dir`. Returns true if something was focused. */
export function move(dir: Dir): boolean {
  const root = activeRoot();
  const pool = candidates(root);
  const active = document.activeElement;
  const src = pool.find((c) => c.id === active);
  if (!src) return focusFallback(root, pool);
  for (const [k, v] of memory.scopes) if (!v.isConnected) memory.scopes.delete(k);
  for (const [k, v] of memory.groups) if (!v.isConnected) memory.groups.delete(k);
  const target = navigate(src, dir, pool, memory);
  return target ? focusEl(target) : false;
}

/** Focuses the layer's preferred item: autofocus, else top-left content, else the rail. */
export function focusFallback(root: Element = activeRoot(), pool = candidates(root)): boolean {
  const c = fallback(pool, RAIL_GROUP);
  return c ? focusEl(c.id) : false;
}

/** Focuses the item with focus key `key` inside `root`, if it is there. */
export function focusKey(key: string, root: Element = activeRoot()): boolean {
  const el = root.querySelector(`[data-fk="${CSS.escape(key)}"]`);
  return el ? focusEl(el) : false;
}

/** The focus key of the focused element (or its closest keyed ancestor). */
export function currentKey(): string | null {
  const a = document.activeElement;
  if (!a || a === document.body) return null;
  return a.closest<HTMLElement>("[data-fk]")?.dataset["fk"] ?? null;
}

/** True if the focus is somewhere usable in the active layer (not on <body>). */
export function hasFocusIn(root: Element = activeRoot()): boolean {
  const a = document.activeElement;
  return !!a && a !== document.body && root.contains(a) && isUsable(a);
}

/** True if the focus is in the side rail. */
export function focusInRail(): boolean {
  return !!document.activeElement?.closest(`[data-group="${RAIL_GROUP}"]`);
}

/** Sets the item a group lands on when the focus moves into it (the rail's current section). */
export function rememberInGroup(group: string, el: Element | null): void {
  if (el) memory.groups.set(group, el);
}

/** Registers a back handler (dialogs, OSD, number entry). The newest runs first. */
export function pushBack(handler: BackHandler): () => void {
  backHandlers.push(handler);
  return () => {
    const i = backHandlers.lastIndexOf(handler);
    if (i >= 0) backHandlers.splice(i, 1);
  };
}

/**
 * Registers a screen-level key handler that runs before the engine (digits, zapping, player
 * keys). Return true to claim the key. The newest handler runs first.
 */
export function pushKeys(handler: KeyHandler): () => void {
  keyHandlers.push(handler);
  return () => {
    const i = keyHandlers.lastIndexOf(handler);
    if (i >= 0) keyHandlers.splice(i, 1);
  };
}

/** Runs "back": the newest handler that accepts it, else the router. */
export function back(): void {
  for (let i = backHandlers.length - 1; i >= 0; i--) {
    if (backHandlers[i]?.()) return;
  }
  defaultBack();
}

let interacted = 0;
/** A counter that changes whenever the user presses a key or a pointer (initial focus yields). */
export function interactionStamp(): number {
  return interacted;
}

const ARROWS: Record<string, Dir> = {
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
};

function onKeyDown(e: KeyboardEvent): void {
  interacted++;
  setModality("keys");
  if (e.defaultPrevented || e.altKey || e.metaKey || e.isComposing) return;
  for (let i = keyHandlers.length - 1; i >= 0; i--) {
    if (keyHandlers[i]?.(e)) {
      e.preventDefault();
      return;
    }
  }
  const active = document.activeElement;
  const text = isTextInput(active);
  const dir = ARROWS[e.key];
  if (dir && !e.ctrlKey && !e.shiftKey) {
    if (text && (dir === "left" || dir === "right")) return;
    e.preventDefault();
    move(dir);
    return;
  }
  if (e.key === "Escape" || (e.key === "Backspace" && !text && !e.ctrlKey)) {
    e.preventDefault();
    back();
    return;
  }
  if ((e.key === "Enter" || e.key === " ") && (!active || active === document.body)) {
    // Nothing focused yet: the first Enter focuses the screen instead of doing nothing.
    e.preventDefault();
    focusFallback();
  }
}

function onPointerMove(e: PointerEvent): void {
  if (e.pointerType !== "mouse" || (e.movementX === 0 && e.movementY === 0)) return;
  setModality("pointer");
  const t = e.target instanceof Element ? e.target.closest(SELECTOR) : null;
  if (!t || t === document.activeElement || isTextInput(t) || t.hasAttribute("data-nohover")) return;
  if (!activeRoot().contains(t) || !isUsable(t)) return;
  // A text field keeps its focus while the pointer passes over other items.
  if (isTextInput(document.activeElement)) return;
  (t as HTMLElement).focus({ preventScroll: true });
}

function onPointerDown(e: PointerEvent): void {
  interacted++;
  setModality(e.pointerType === "touch" || e.pointerType === "pen" ? "touch" : "pointer");
}

function onFocusIn(e: FocusEvent): void {
  const t = e.target;
  if (!(t instanceof HTMLElement) || !t.matches(SELECTOR)) return;
  memory.record(candidateOf(t));
}

/** Installs the global listeners. `onBack` is the router's back (used when no handler takes it). */
export function installFocus(onBack: () => void): () => void {
  defaultBack = onBack;
  window.addEventListener("keydown", onKeyDown);
  document.addEventListener("pointermove", onPointerMove, { passive: true });
  document.addEventListener("pointerdown", onPointerDown, { passive: true, capture: true });
  document.addEventListener("focusin", onFocusIn);
  setModality("pointer");
  return () => {
    window.removeEventListener("keydown", onKeyDown);
    document.removeEventListener("pointermove", onPointerMove);
    document.removeEventListener("pointerdown", onPointerDown, { capture: true });
    document.removeEventListener("focusin", onFocusIn);
  };
}
