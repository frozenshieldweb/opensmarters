// SPDX-License-Identifier: GPL-3.0-or-later
// Virtualised lists and grids: only the visible rows are in the DOM, so a provider with 10 000
// channels or 35 000 films scrolls smoothly. They handle their own arrow keys (the focus engine
// can't see rows that aren't rendered) and give the key back at their edges, so the spatial
// engine moves on to the next region (DESIGN.md §5, crates/app/src/focus "virtualised lists").

import type { ComponentChildren } from "preact";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { gridStep, type Dir } from "../focus/engine";
import type { Memo } from "../state/router";
import { cx } from "./kit";

const KEY_DIRS: Record<string, Dir> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };

interface Geometry {
  scrollTop: number;
  height: number;
  width: number;
}

function useScrollGeometry(ref: { current: HTMLDivElement | null }) {
  const [g, setG] = useState<Geometry>({ scrollTop: 0, height: 0, width: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    let frame = 0;
    const read = () => {
      frame = 0;
      setG((p) =>
        p.scrollTop === el.scrollTop && p.height === el.clientHeight && p.width === el.clientWidth
          ? p
          : { scrollTop: el.scrollTop, height: el.clientHeight, width: el.clientWidth },
      );
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(read);
    };
    read();
    el.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(read);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", onScroll);
      ro.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, [ref]);
  return [g, setG] as const;
}

function focusableIn(el: Element | null): HTMLElement | null {
  if (!el) return null;
  return el.matches("button, [data-f]") ? (el as HTMLElement) : el.querySelector<HTMLElement>("button, [data-f]");
}

interface Shared<T> {
  items: readonly T[];
  /** Stable key per item (DOM reuse and focus restoration). */
  itemKey: (item: T, index: number) => string;
  /** Focus scope name (spatial navigation). */
  scope: string;
  /** The route memo: the list keeps its scroll position there. */
  memo?: Memo;
  memoKey?: string;
  class?: string;
  /** Called with the visible range (inclusive start, exclusive end) when it changes. */
  onRange?: (start: number, end: number) => void;
  /** An index to scroll into view (and focus when `focus` is set) when it changes. */
  reveal?: { index: number; focus?: boolean; nonce?: number } | null;
  label?: string;
  footer?: ComponentChildren;
}

interface ListProps<T> extends Shared<T> {
  /** PageUp/PageDown page the list (default). Live TV turns this off: they zap there. */
  pageKeys?: boolean;
  rowHeight: number;
  render: (item: T, index: number, tabIndex: number) => ComponentChildren;
  overscan?: number;
}

/** A vertical virtualised list of fixed-height rows (each row renders one button). */
export function VirtualList<T>(p: ListProps<T>) {
  const ref = useRef<HTMLDivElement>(null);
  const [g, setG] = useScrollGeometry(ref);
  const [active, setActive] = useState(0);
  const pendingFocus = useRef<number | null>(null);
  const n = p.items.length;
  const rh = p.rowHeight;
  const memoKey = p.memoKey ?? "list";
  const overscan = p.overscan ?? 6;

  // Restore the scroll position once the rows exist.
  useLayoutEffect(() => {
    const el = ref.current;
    const saved = p.memo?.[`${memoKey}:scroll`];
    if (el && typeof saved === "number" && n > 0) {
      el.scrollTop = saved;
      setG((x) => ({ ...x, scrollTop: el.scrollTop }));
    }
    // Only when the list first gets its items.
  }, [n > 0]);

  const start = Math.max(0, Math.floor(g.scrollTop / rh) - overscan);
  const end = Math.min(n, Math.ceil((g.scrollTop + (g.height || 800)) / rh) + overscan);

  const onRange = p.onRange;
  const visStart = Math.max(0, Math.floor(g.scrollTop / rh));
  const visEnd = Math.min(n, Math.ceil((g.scrollTop + (g.height || 800)) / rh) + 1);
  useEffect(() => {
    if (n > 0) onRange?.(visStart, visEnd);
  }, [visStart, visEnd, n, onRange]);

  const scrollToIndex = useCallback(
    (i: number) => {
      const el = ref.current;
      if (!el) return;
      const look = rh; // one row of look-ahead (§5)
      const top = i * rh;
      let st = el.scrollTop;
      if (top - look < st) st = Math.max(0, top - look);
      else if (top + rh + look > st + el.clientHeight) st = top + rh + look - el.clientHeight;
      if (st !== el.scrollTop) {
        el.scrollTop = st;
        setG((x) => ({ ...x, scrollTop: el.scrollTop }));
      }
    },
    [rh, setG],
  );

  const focusIndex = useCallback(
    (i: number) => {
      scrollToIndex(i);
      const row = ref.current?.querySelector(`[data-index="${i}"]`);
      const btn = focusableIn(row ?? null);
      if (btn) btn.focus({ preventScroll: true });
      else pendingFocus.current = i;
    },
    [scrollToIndex],
  );

  useLayoutEffect(() => {
    const i = pendingFocus.current;
    if (i === null) return;
    const btn = focusableIn(ref.current?.querySelector(`[data-index="${i}"]`) ?? null);
    if (btn) {
      pendingFocus.current = null;
      btn.focus({ preventScroll: true });
    }
  });

  useEffect(() => {
    const r = p.reveal;
    if (!r || r.index < 0 || r.index >= n) return;
    if (r.focus) focusIndex(r.index);
    else scrollToIndex(r.index);
  }, [p.reveal?.index, p.reveal?.nonce, p.reveal?.focus, n]);

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.altKey || e.metaKey || e.ctrlKey || e.shiftKey || e.defaultPrevented) return;
    const t = e.target as HTMLElement;
    const rowEl = t.closest<HTMLElement>("[data-index]");
    if (!rowEl || !ref.current?.contains(rowEl)) return;
    const i = Number(rowEl.dataset["index"]);
    const page = Math.max(1, Math.floor((ref.current.clientHeight || rh) / rh) - 1);
    let next: number | null;
    const dir = KEY_DIRS[e.key];
    if (dir === "up" || dir === "down") next = gridStep(i, n, 1, dir);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else if (e.key === "PageDown" && p.pageKeys !== false) next = Math.min(n - 1, i + page);
    else if (e.key === "PageUp" && p.pageKeys !== false) next = Math.max(0, i - page);
    else return;
    if (next === null || next === i) return;
    e.preventDefault();
    focusIndex(next);
  };

  const onFocusIn = (e: FocusEvent) => {
    const rowEl = (e.target as HTMLElement).closest<HTMLElement>("[data-index]");
    if (!rowEl) return;
    const i = Number(rowEl.dataset["index"]);
    setActive(i);
    if (p.memo) p.memo[`${memoKey}:scroll`] = ref.current?.scrollTop ?? 0;
  };

  const onScroll = () => {
    if (p.memo) p.memo[`${memoKey}:scroll`] = ref.current?.scrollTop ?? 0;
  };

  const rows = [];
  const tabRow = Math.min(active, Math.max(0, n - 1));
  for (let i = start; i < end; i++) {
    const item = p.items[i]!;
    rows.push(
      <div class="vl-row" key={p.itemKey(item, i)} data-index={i} style={{ transform: `translateY(${i * rh}px)`, height: `${rh}px` }}>
        {p.render(item, i, i === tabRow ? 0 : -1)}
      </div>,
    );
  }

  return (
    <div
      ref={ref}
      class={cx("vl", p.class)}
      data-scope={p.scope}
      role="list"
      aria-label={p.label}
      onKeyDown={onKeyDown}
      onFocusIn={onFocusIn}
      onScroll={onScroll}
    >
      <div class="vl-sizer" style={{ height: `${n * rh}px` }}>
        {rows}
      </div>
      {p.footer}
    </div>
  );
}

interface GridProps<T> extends Shared<T> {
  /** Minimum item width; columns stretch to fill the row. */
  minItemWidth: number;
  gap: number;
  rowGap: number;
  /** Item height for a given item width. */
  itemHeight: (width: number) => number;
  render: (item: T, index: number, tabIndex: number, width: number) => ComponentChildren;
  /** Padding inside the scroll area around the grid (room for the focus ring and lift). */
  pad: number;
}

/** A virtualised grid of equal cards (posters). */
export function VirtualGrid<T>(p: GridProps<T>) {
  const ref = useRef<HTMLDivElement>(null);
  const [g, setG] = useScrollGeometry(ref);
  const [active, setActive] = useState(0);
  const pendingFocus = useRef<number | null>(null);
  const n = p.items.length;
  const memoKey = p.memoKey ?? "grid";
  const inner = Math.max(0, g.width - p.pad * 2);
  const cols = Math.max(1, Math.floor((inner + p.gap) / (p.minItemWidth + p.gap)));
  const itemW = cols > 0 ? Math.floor((inner - p.gap * (cols - 1)) / cols) : p.minItemWidth;
  const itemH = p.itemHeight(itemW);
  const rowH = itemH + p.rowGap;
  const rowsN = Math.ceil(n / cols);
  const top0 = p.pad;

  useLayoutEffect(() => {
    const el = ref.current;
    const saved = p.memo?.[`${memoKey}:scroll`];
    if (el && typeof saved === "number" && n > 0) {
      el.scrollTop = saved;
      setG((x) => ({ ...x, scrollTop: el.scrollTop }));
    }
  }, [n > 0]);

  const firstRow = Math.max(0, Math.floor((g.scrollTop - top0) / rowH) - 2);
  const lastRow = Math.min(rowsN, Math.ceil((g.scrollTop - top0 + (g.height || 900)) / rowH) + 2);

  const onRange = p.onRange;
  useEffect(() => {
    if (n > 0 && g.width > 0) onRange?.(firstRow * cols, Math.min(n, lastRow * cols));
  }, [firstRow, lastRow, cols, n, onRange, g.width]);

  const scrollToIndex = useCallback(
    (i: number) => {
      const el = ref.current;
      if (!el) return;
      const row = Math.floor(i / cols);
      const top = top0 + row * rowH;
      const look = Math.min(rowH * 0.5, 120);
      let st = el.scrollTop;
      if (row === 0) st = 0;
      else if (top - look < st) st = Math.max(0, top - look);
      else if (top + itemH + look > st + el.clientHeight) st = top + itemH + look - el.clientHeight;
      if (st !== el.scrollTop) {
        el.scrollTop = st;
        setG((x) => ({ ...x, scrollTop: el.scrollTop }));
      }
    },
    [cols, rowH, itemH, top0, setG],
  );

  const focusIndex = useCallback(
    (i: number) => {
      scrollToIndex(i);
      const btn = focusableIn(ref.current?.querySelector(`[data-index="${i}"]`) ?? null);
      if (btn) btn.focus({ preventScroll: true });
      else pendingFocus.current = i;
    },
    [scrollToIndex],
  );

  useLayoutEffect(() => {
    const i = pendingFocus.current;
    if (i === null) return;
    const btn = focusableIn(ref.current?.querySelector(`[data-index="${i}"]`) ?? null);
    if (btn) {
      pendingFocus.current = null;
      btn.focus({ preventScroll: true });
    }
  });

  useEffect(() => {
    const r = p.reveal;
    if (!r || r.index < 0 || r.index >= n || g.width === 0) return;
    if (r.focus) focusIndex(r.index);
    else scrollToIndex(r.index);
  }, [p.reveal?.index, p.reveal?.nonce, p.reveal?.focus, n, g.width > 0]);

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.altKey || e.metaKey || e.ctrlKey || e.shiftKey || e.defaultPrevented) return;
    const cell = (e.target as HTMLElement).closest<HTMLElement>("[data-index]");
    if (!cell || !ref.current?.contains(cell)) return;
    const i = Number(cell.dataset["index"]);
    const pageRows = Math.max(1, Math.floor((ref.current.clientHeight || rowH) / rowH));
    let next: number | null;
    const dir = KEY_DIRS[e.key];
    if (dir) next = gridStep(i, n, cols, dir);
    else if (e.key === "PageDown") next = Math.min(n - 1, i + pageRows * cols);
    else if (e.key === "PageUp") next = Math.max(i % cols, i - pageRows * cols);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else return;
    if (next === null || next === i) return;
    e.preventDefault();
    focusIndex(next);
  };

  const onFocusIn = (e: FocusEvent) => {
    const cell = (e.target as HTMLElement).closest<HTMLElement>("[data-index]");
    if (!cell) return;
    setActive(Number(cell.dataset["index"]));
    if (p.memo) p.memo[`${memoKey}:scroll`] = ref.current?.scrollTop ?? 0;
  };

  const onScroll = () => {
    if (p.memo) p.memo[`${memoKey}:scroll`] = ref.current?.scrollTop ?? 0;
  };

  const cells = [];
  const tabCell = Math.min(active, Math.max(0, n - 1));
  if (g.width > 0) {
    for (let r = firstRow; r < lastRow; r++) {
      for (let c = 0; c < cols; c++) {
        const i = r * cols + c;
        if (i >= n) break;
        const item = p.items[i]!;
        cells.push(
          <div
            class="vg-cell"
            key={p.itemKey(item, i)}
            data-index={i}
            style={{
              transform: `translate(${p.pad + c * (itemW + p.gap)}px, ${top0 + r * rowH}px)`,
              width: `${itemW}px`,
              height: `${itemH}px`,
            }}
          >
            {p.render(item, i, i === tabCell ? 0 : -1, itemW)}
          </div>,
        );
      }
    }
  }

  return (
    <div ref={ref} class={cx("vg", p.class)} data-scope={p.scope} role="list" aria-label={p.label} onKeyDown={onKeyDown} onFocusIn={onFocusIn} onScroll={onScroll}>
      <div class="vg-sizer" style={{ height: `${top0 + rowsN * rowH + p.pad}px` }}>
        {cells}
      </div>
      {p.footer}
    </div>
  );
}
