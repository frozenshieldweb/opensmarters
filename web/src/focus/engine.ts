// SPDX-License-Identifier: GPL-3.0-or-later
// The spatial focus engine: pure geometry and state, no DOM (unit tested). A port of
// crates/app/src/focus/engine.rs. The DOM glue is in ./dom.ts.
//
// There is one focus. Arrow keys move it to the geometric nearest neighbour, first inside the
// current *scope* (a list, rail or form), then across scopes, restoring each scope's and
// group's last focused item. Up/Down never leave the current *group* (the side rail is beside
// the page: Down at the bottom of a page must not jump into the rail's bottom items).

export type Dir = "up" | "down" | "left" | "right";
export type Axis = "horizontal" | "vertical";

export function axisOf(dir: Dir): Axis {
  return dir === "left" || dir === "right" ? "horizontal" : "vertical";
}

/** `-1` for up/left, `+1` for down/right. */
export function signOf(dir: Dir): -1 | 1 {
  return dir === "up" || dir === "left" ? -1 : 1;
}

/** A screen rectangle (CSS pixels): left, top, width, height. */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const rect = (x: number, y: number, w: number, h: number): Rect => ({ x, y, w, h });

const right = (r: Rect) => r.x + r.w;
const bottom = (r: Rect) => r.y + r.h;
const cx = (r: Rect) => r.x + r.w / 2;
const cy = (r: Rect) => r.y + r.h / 2;

/** One focusable item. `K` is the item's identity (a DOM element in the app, a string in tests). */
export interface Candidate<K> {
  id: K;
  rect: Rect;
  /** Innermost focus scope (list, rail, form…). */
  scope: string;
  /** Outer region (the side rail vs the screen content). */
  group: string;
  /** Preferred initial focus when nothing else is focused. */
  autofocus?: boolean;
  /** The rectangle of the whole group (region), when known. */
  groupRect?: Rect;
}

/**
 * True if `to` lies in direction `dir` from `from`: its centre is beyond `from`'s centre and
 * its near edge overlaps `from` by at most half of the smaller extent (so a big neighbour that
 * merely overlaps sideways does not count).
 */
export function isInDirection(from: Rect, to: Rect, dir: Dir): boolean {
  const EPS = 0.5;
  const tolX = 0.5 * Math.min(from.w, to.w);
  const tolY = 0.5 * Math.min(from.h, to.h);
  switch (dir) {
    case "right":
      return cx(to) > cx(from) + EPS && to.x >= right(from) - tolX;
    case "left":
      return cx(to) < cx(from) - EPS && right(to) <= from.x + tolX;
    case "down":
      return cy(to) > cy(from) + EPS && to.y >= bottom(from) - tolY;
    case "up":
      return cy(to) < cy(from) - EPS && bottom(to) <= from.y + tolY;
  }
}

/**
 * Distance score of moving from `from` to `to` in `dir` (lower is better), or `null` if `to` is
 * not in that direction: the edge gap along the axis, plus twice the orthogonal gap (zero when
 * the two overlap on the orthogonal axis, i.e. `to` is "in the beam"), plus a small
 * centre-alignment tiebreak.
 */
export function score(from: Rect, to: Rect, dir: Dir): number | null {
  if (!isInDirection(from, to, dir)) return null;
  let major: number;
  let a0: number, a1: number, b0: number, b1: number, ca: number, cb: number;
  if (dir === "right" || dir === "left") {
    major = dir === "right" ? to.x - right(from) : from.x - right(to);
    [a0, a1, b0, b1] = [from.y, bottom(from), to.y, bottom(to)];
    [ca, cb] = [cy(from), cy(to)];
  } else {
    major = dir === "down" ? to.y - bottom(from) : from.y - bottom(to);
    [a0, a1, b0, b1] = [from.x, right(from), to.x, right(to)];
    [ca, cb] = [cx(from), cx(to)];
  }
  major = Math.max(0, major);
  const minorGap = b0 > a1 ? b0 - a1 : a0 > b1 ? a0 - b1 : 0;
  return major + 2 * minorGap + 0.05 * Math.abs(ca - cb);
}

/** True if the two rectangles share some horizontal extent. */
function overlapsX(a: Rect, b: Rect): boolean {
  return a.x < right(b) && b.x < right(a);
}

/** The best candidate in `dir` from `from`. Ties keep the earlier one (reading order). */
export function bestInDirection<K>(
  from: Rect,
  dir: Dir,
  candidates: Iterable<Candidate<K>>,
): Candidate<K> | null {
  let best: Candidate<K> | null = null;
  let bestScore = Infinity;
  for (const c of candidates) {
    const s = score(from, c.rect, dir);
    if (s !== null && s < bestScore) {
      best = c;
      bestScore = s;
    }
  }
  return best;
}

/** Remembers the last focused item per scope and per group. */
export class FocusMemory<K> {
  readonly scopes = new Map<string, K>();
  readonly groups = new Map<string, K>();

  /** Records that `c` has the focus. */
  record(c: Pick<Candidate<K>, "id" | "scope" | "group">): void {
    this.scopes.set(c.scope, c.id);
    this.groups.set(c.group, c.id);
  }

  /** Forgets an item (it left the page). */
  forget(id: K): void {
    for (const [k, v] of this.scopes) if (v === id) this.scopes.delete(k);
    for (const [k, v] of this.groups) if (v === id) this.groups.delete(k);
  }

  clear(): void {
    this.scopes.clear();
    this.groups.clear();
  }
}

/**
 * Where an arrow press moves the focus from `src`: the best candidate inside its scope, else
 * across scopes (preferring the target group's, then the target scope's remembered item when
 * it also lies in that direction). `null` = stay.
 */
export function navigate<K>(
  src: Candidate<K>,
  dir: Dir,
  pool: readonly Candidate<K>[],
  memory?: FocusMemory<K>,
): K | null {
  const others = pool.filter((c) => c.id !== src.id);
  // 1. Within the same scope.
  const same = bestInDirection(
    src.rect,
    dir,
    others.filter((c) => c.scope === src.scope),
  );
  if (same) return same.id;
  // 2. Across scopes. Up/down stay inside the group, unless the other region spans across the
  //    item (a top bar above the page, in the compact layout); a side rail beside the page never
  //    does, so Down at the bottom of a page doesn't jump into the rail's bottom items.
  const reachable = others.filter(
    (c) => axisOf(dir) === "horizontal" || c.group === src.group || overlapsX(c.groupRect ?? c.rect, src.rect),
  );
  const best = bestInDirection(src.rect, dir, reachable);
  if (!best) return null;
  const remembered = (id: K | undefined): K | null => {
    if (id === undefined) return null;
    const c = others.find((o) => o.id === id);
    return c && isInDirection(src.rect, c.rect, dir) ? c.id : null;
  };
  if (memory && best.group !== src.group) {
    const id = remembered(memory.groups.get(best.group));
    if (id !== null) return id;
  }
  if (memory && best.scope !== src.scope) {
    const id = remembered(memory.scopes.get(best.scope));
    if (id !== null) return id;
  }
  return best.id;
}

/**
 * The item to focus when nothing is: the autofocus candidate, else the top-left one outside
 * `avoidGroup` (the side rail, so a screen focuses its own content first), else the top-left.
 */
export function fallback<K>(pool: readonly Candidate<K>[], avoidGroup?: string): Candidate<K> | null {
  const auto = pool.find((c) => c.autofocus);
  if (auto) return auto;
  const topLeft = (cands: readonly Candidate<K>[]) => {
    let best: Candidate<K> | null = null;
    for (const c of cands) {
      if (!best || c.rect.y < best.rect.y || (c.rect.y === best.rect.y && c.rect.x < best.rect.x)) {
        best = c;
      }
    }
    return best;
  };
  return topLeft(pool.filter((c) => c.group !== avoidGroup)) ?? topLeft(pool);
}

/**
 * The next index in a list or grid of `count` items laid out in `columns` columns, or `null`
 * at the boundary (the caller then lets geometric navigation take over). Lists use
 * `columns = 1` and only handle up/down.
 */
export function gridStep(index: number, count: number, columns: number, dir: Dir): number | null {
  if (count <= 0) return null;
  const cols = Math.max(1, columns);
  const col = index % cols;
  switch (dir) {
    case "left":
      return col > 0 ? index - 1 : null;
    case "right":
      return col < cols - 1 && index + 1 < count ? index + 1 : null;
    case "up":
      return index - cols >= 0 ? index - cols : null;
    case "down": {
      if (index + cols < count) return index + cols;
      // Last, partial row: go to its last item if it's on a lower row.
      const lastRow = Math.floor((count - 1) / cols);
      return Math.floor(index / cols) < lastRow ? count - 1 : null;
    }
  }
}
