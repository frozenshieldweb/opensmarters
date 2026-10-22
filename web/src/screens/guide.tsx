// SPDX-License-Identifier: GPL-3.0-or-later
// Guide (DESIGN.md §6.9, simplified): channels down the side, their programmes along a time
// line from half an hour ago to six hours ahead (6 px per minute), a now-line, and an info panel
// for the focused programme. Left/Right move by programme, Up/Down by channel keeping the time.
// Rows are virtualised; each row's programmes load when it comes into view.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import { api } from "../api/client";
import type { Channel, Programme } from "../api/types";
import { account, settings } from "../state/session";
import { useLoad } from "../state/cache";
import * as catalog from "../state/catalog";
import { favourites } from "../state/favourites";
import { now, toast } from "../state/misc";
import { setParams, type Memo } from "../state/router";
import { clock, dot, fraction, plural, timeRange } from "../lib/format";
import { cap } from "../lib/text";
import { Button, Empty, ErrorState, Icon, Skeleton, cx } from "../ui/kit";
import { ChannelLogo } from "../ui/media";
import { breakpoint, useInitialFocus } from "../ui/hooks";
import { watchChannel } from "../player/start";
import { ChipRow } from "./common";

const PX_PER_MIN = 6;
const ROW_H = 64;
const RULER_H = 40;
const BEFORE = 30 * 60;
const AFTER = 6 * 3600;

type RowState = { state: "loading" } | { state: "ok"; progs: Programme[] } | { state: "error" };

interface Focus {
  row: number;
  /** The time position kept when moving up and down. */
  at: number;
}

/** Loads programmes per channel as rows come into view (6 requests at a time). */
function useRows(account: string, from: number, to: number) {
  const rows = useRef(new Map<string, RowState>());
  const queue = useRef<string[]>([]);
  const active = useRef(0);
  const [version, setVersion] = useState(0);
  useEffect(() => {
    rows.current.clear();
    queue.current = [];
    setVersion((v) => v + 1);
  }, [account, from, to]);

  const pump = useCallback(() => {
    while (active.current < 6 && queue.current.length > 0) {
      const key = queue.current.shift()!;
      active.current++;
      api
        .epgChannel(account, key, from, to)
        .then(
          (progs) => rows.current.set(key, { state: "ok", progs: progs.slice().sort((a, b) => a.start - b.start) }),
          () => rows.current.set(key, { state: "error" }),
        )
        .finally(() => {
          active.current--;
          setVersion((v) => v + 1);
          pump();
        });
    }
  }, [account, from, to]);

  const request = useCallback(
    (keys: string[]) => {
      let added = false;
      for (const k of keys) {
        if (!rows.current.has(k)) {
          rows.current.set(k, { state: "loading" });
          queue.current.push(k);
          added = true;
        }
      }
      if (added) pump();
    },
    [pump],
  );
  return { rows: rows.current, request, version };
}

export function Guide(props: { memo: Memo; category?: string }) {
  const a = account.value!;
  const cat = props.category ?? "all";
  const cats = useLoad(catalog.keys.liveCategories(a.id), () => catalog.liveCategories(a.id));
  const list = useLoad(catalog.keys.channels(a.id, cat), () => catalog.channels(a.id, cat), { keepPrevious: true });
  const channels = list.data ?? [];
  const bp = breakpoint.value;
  const fmt = settings.value.time_format;

  // The window: from the last half hour, minus 30 minutes, to 6 hours ahead (fixed while open).
  const [from] = useState(() => Math.floor(Date.now() / 1000 / 1800) * 1800 - BEFORE);
  const to = from + BEFORE + AFTER + 1800;
  const width = ((to - from) / 60) * PX_PER_MIN;
  const chanW = bp === "compact" ? 104 : 240;

  const { rows, request, version } = useRows(a.id, from, to);
  const scroller = useRef<HTMLDivElement>(null);
  const [geo, setGeo] = useState({ top: 0, left: 0, h: 600, w: 1000 });
  const [focus, setFocus] = useState<Focus | null>(null);
  const pending = useRef<{ row: number; start: number } | null>(null);

  const firstRow = channels[0] ? rows.get(channels[0].key) : undefined;
  useInitialFocus(!list.loading && !!firstRow && firstRow.state !== "loading" && version > 0, props.memo);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    let frame = 0;
    const read = () => {
      frame = 0;
      setGeo({ top: el.scrollTop, left: el.scrollLeft, h: el.clientHeight, w: el.clientWidth });
    };
    const onScroll = () => {
      if (!frame) frame = requestAnimationFrame(read);
    };
    read();
    el.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(read);
    ro.observe(el);
    // Start at "now minus 15 minutes".
    if (typeof props.memo["guide:left"] === "number") {
      el.scrollLeft = props.memo["guide:left"];
      el.scrollTop = (props.memo["guide:top"] as number) ?? 0;
    } else {
      el.scrollLeft = Math.max(0, ((Date.now() / 1000 - from - 900) / 60) * PX_PER_MIN);
    }
    read();
    return () => {
      el.removeEventListener("scroll", onScroll);
      ro.disconnect();
      if (frame) cancelAnimationFrame(frame);
      props.memo["guide:left"] = el.scrollLeft;
      props.memo["guide:top"] = el.scrollTop;
    };
  }, [list.data !== undefined]);

  const first = Math.max(0, Math.floor((geo.top - RULER_H) / ROW_H) - 3);
  const last = Math.min(channels.length, Math.ceil((geo.top + geo.h) / ROW_H) + 3);
  useEffect(() => {
    if (channels.length) request(channels.slice(first, last).map((c) => c.key));
  }, [first, last, channels, request]);

  const xOf = (t: number) => ((Math.max(from, Math.min(to, t)) - from) / 60) * PX_PER_MIN;

  const cellsOf = (key: string): Programme[] | null => {
    const r = rows.get(key);
    return r?.state === "ok" ? r.progs.filter((p) => p.end > from && p.start < to) : null;
  };

  const ensureVisible = (row: number, x0: number, x1: number) => {
    const el = scroller.current;
    if (!el) return;
    const top = RULER_H + row * ROW_H;
    if (top - ROW_H < el.scrollTop + RULER_H) el.scrollTop = Math.max(0, top - ROW_H - RULER_H);
    else if (top + 2 * ROW_H > el.scrollTop + el.clientHeight) el.scrollTop = top + 2 * ROW_H - el.clientHeight;
    const viewL = el.scrollLeft;
    const viewW = el.clientWidth - chanW;
    if (x0 < viewL + 24) el.scrollLeft = Math.max(0, x0 - 24);
    else if (x0 > viewL + viewW - 120) el.scrollLeft = Math.min(x0 - 24, Math.max(0, x1 - viewW + 24));
  };

  const focusCell = (row: number, start: number | null) => {
    const ch = channels[row];
    if (!ch) return;
    const progs = cellsOf(ch.key);
    const p = start === null ? null : progs?.find((x) => x.start === start);
    ensureVisible(row, p ? xOf(p.start) : scroller.current?.scrollLeft ?? 0, p ? xOf(p.end) : (scroller.current?.scrollLeft ?? 0) + 200);
    const sel = p ? `[data-fk="epg:${CSS.escape(ch.key)}:${p.start}"]` : `[data-fk="epgrow:${CSS.escape(ch.key)}"]`;
    const el = scroller.current?.querySelector<HTMLElement>(sel);
    if (el) el.focus({ preventScroll: true });
    else pending.current = { row, start: p?.start ?? -1 };
  };

  useLayoutEffect(() => {
    const p = pending.current;
    if (!p) return;
    const ch = channels[p.row];
    if (!ch) return;
    const sel = p.start >= 0 ? `[data-fk="epg:${CSS.escape(ch.key)}:${p.start}"]` : `[data-fk="epgrow:${CSS.escape(ch.key)}"]`;
    const el = scroller.current?.querySelector<HTMLElement>(sel);
    if (el) {
      pending.current = null;
      el.focus({ preventScroll: true });
    }
  });

  const pickAt = (row: number, at: number): number | null => {
    const ch = channels[row];
    const progs = ch ? cellsOf(ch.key) : null;
    if (!progs || progs.length === 0) return null;
    const hit = progs.find((p) => p.start <= at && p.end > at) ?? progs.reduce((b, p) => (Math.abs(p.start - at) < Math.abs(b.start - at) ? p : b));
    return hit.start;
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || e.defaultPrevented || !focus) return;
    const ch = channels[focus.row];
    if (!ch) return;
    const progs = cellsOf(ch.key) ?? [];
    const curStart = Number((e.target as HTMLElement).dataset["start"] ?? NaN);
    const idx = progs.findIndex((p) => p.start === curStart);
    let handled = false;
    if (e.key === "ArrowRight" && idx >= 0 && idx < progs.length - 1) {
      const n = progs[idx + 1]!;
      setFocus({ row: focus.row, at: Math.max(n.start, from) + 60 });
      focusCell(focus.row, n.start);
      handled = true;
    } else if (e.key === "ArrowLeft" && idx > 0) {
      const n = progs[idx - 1]!;
      setFocus({ row: focus.row, at: Math.max(n.start, from) + 60 });
      focusCell(focus.row, n.start);
      handled = true;
    } else if ((e.key === "ArrowDown" && focus.row < channels.length - 1) || (e.key === "ArrowUp" && focus.row > 0)) {
      const row = focus.row + (e.key === "ArrowDown" ? 1 : -1);
      focusCell(row, pickAt(row, focus.at));
      setFocus({ row, at: focus.at });
      handled = true;
    } else if (e.key === "PageDown" || e.key === "PageUp") {
      const page = Math.max(1, Math.floor((geo.h - RULER_H) / ROW_H) - 1);
      const row = Math.max(0, Math.min(channels.length - 1, focus.row + (e.key === "PageDown" ? page : -page)));
      focusCell(row, pickAt(row, focus.at));
      setFocus({ row, at: focus.at });
      handled = true;
    }
    if (handled) e.preventDefault();
  };

  const onFocusIn = (e: FocusEvent) => {
    const t = e.target as HTMLElement;
    const row = Number(t.closest<HTMLElement>("[data-row]")?.dataset["row"] ?? NaN);
    if (!Number.isFinite(row)) return;
    const start = Number(t.dataset["start"] ?? NaN);
    setFocus((f) => {
      if (f && f.row === row && Number.isFinite(start) && f.at >= start) return f;
      return { row, at: Number.isFinite(start) ? Math.max(start, from) + 60 : Math.max(Date.now() / 1000, from) };
    });
  };

  const jumpNow = () => {
    const el = scroller.current;
    if (el) el.scrollLeft = Math.max(0, ((Date.now() / 1000 - from - 900) / 60) * PX_PER_MIN);
    const row = focus?.row ?? 0;
    const at = Date.now() / 1000;
    focusCell(row, pickAt(row, at));
    setFocus({ row, at });
  };

  const activate = (c: Channel, p: Programme) => {
    const t = Date.now() / 1000;
    if (p.start <= t && p.end > t) watchChannel(a.id, c, channels);
    else if (p.start > t) toast(`${cap(p.title, 60)} starts at ${clock(p.start, fmt)}.`, "info");
    else toast("This programme has finished. Catch-up isn't in the web edition yet.", "info");
  };

  // Info panel: the focused programme.
  const fch = focus ? channels[focus.row] : undefined;
  const fprogs = fch ? cellsOf(fch.key) : null;
  const fprog = fprogs?.find((p) => p.start <= (focus?.at ?? 0) && p.end > (focus?.at ?? 0)) ?? null;
  const t = now.value;

  const ticks = useMemo(() => {
    const out: number[] = [];
    for (let x = from; x < to; x += 1800) out.push(x);
    return out;
  }, [from, to]);

  const catItems = [
    { id: "all", label: "All" },
    { id: "favourites", label: "Favourites" },
    ...(cats.data ?? []).map((c) => ({ id: c.id, label: cap(c.name, 40) })),
  ];
  void favourites.value;

  const nowX = xOf(t);
  const rowsEls = [];
  for (let i = first; i < last; i++) {
    const c = channels[i]!;
    const r = rows.get(c.key);
    const progs = cellsOf(c.key);
    rowsEls.push(
      <div class="g-row" key={c.key} data-row={i} style={{ transform: `translateY(${RULER_H + i * ROW_H}px)`, width: `${chanW + width}px` }}>
        <div class="g-chan" style={{ width: `${chanW}px` }}>
          <span class="g-num t-caption c-3 tnum">{c.number ?? ""}</span>
          <ChannelLogo channel={c} size="sm" />
          {bp !== "compact" && <span class="g-name t-body-strong ellipsis">{cap(c.name, 60)}</span>}
        </div>
        <div class="g-cells" style={{ width: `${width}px` }} data-scope="guide-grid">
          {progs && progs.length > 0 ? (
            progs.map((p) => {
              const x0 = xOf(p.start);
              const w = xOf(p.end) - x0;
              const past = p.end <= t;
              const live = p.start <= t && p.end > t;
              return (
                <button
                  type="button"
                  key={p.start}
                  class={cx("g-cell", past && "is-past", live && "is-now")}
                  data-fk={`epg:${c.key}:${p.start}`}
                  data-autofocus={i === 0 && live ? "" : undefined}
                  data-start={p.start}
                  tabIndex={-1}
                  style={{ transform: `translateX(${x0}px)`, width: `${Math.max(4, w - 2)}px` }}
                  aria-label={`${c.name}: ${p.title}, ${timeRange(p.start, p.end, fmt)}${live ? ", on now" : ""}`}
                  onClick={() => activate(c, p)}
                >
                  {live && <span class="g-elapsed" style={{ width: `${fraction(p.start, p.end, t) * 100}%` }} />}
                  {w >= 48 && (
                    <span
                      class="g-cell-text"
                      style={{ paddingLeft: `${Math.max(0, Math.min(geo.left - x0, w - 72))}px` }}
                    >
                      <span class="t-body-strong ellipsis">{cap(p.title, 80)}</span>
                      <span class="t-caption c-3 ellipsis tnum">{timeRange(p.start, p.end, fmt)}</span>
                    </span>
                  )}
                </button>
              );
            })
          ) : (
            <button
              type="button"
              class="g-cell g-empty"
              data-fk={`epgrow:${c.key}`}
              tabIndex={-1}
              style={{ transform: `translateX(${Math.max(0, geo.left)}px)`, width: `${Math.max(200, geo.w - chanW - 24)}px` }}
              onClick={() => watchChannel(a.id, c, channels)}
              aria-label={`${c.name}: ${r?.state === "loading" || !r ? "loading the guide" : "no guide"}. Press Enter to watch.`}
            >
              <span class="t-caption c-3">{!r || r.state === "loading" ? "Loading…" : "No guide for this channel · Enter to watch"}</span>
            </button>
          )}
        </div>
      </div>,
    );
  }

  return (
    <div class={cx("screen guide", `guide-${bp}`)}>
      <header class="guide-head">
        <h1 class="t-heading">Guide</h1>
        {list.data && <span class="t-caption c-3 tnum">{plural(channels.length, "channel")}</span>}
      </header>
      <ChipRow
        scope="guide-chips"
        selected={cat}
        onSelect={(id) => {
          props.memo["guide:top"] = 0;
          setFocus(null);
          setParams({ name: "guide", category: id === "all" ? undefined : id });
        }}
        items={catItems}
        trailing={
          <Button variant="secondary" icon="clock" onClick={jumpNow} class="guide-now" fk="guide:now">
            Now
          </Button>
        }
      />
      <section class="guide-info" aria-live="polite">
        {fch && fprog ? (
          <>
            <p class="t-overline c-3 ellipsis">
              {fch.number !== null ? `${fch.number} · ` : ""}
              {cap(fch.name, 60)}
            </p>
            <h2 class="t-title ellipsis">{cap(fprog.title, 120)}</h2>
            <p class="t-caption c-3 tnum">
              {dot(
                timeRange(fprog.start, fprog.end, fmt),
                `${Math.round((fprog.end - fprog.start) / 60)} min`,
                fprog.start > t
                  ? fprog.start - t < 3600
                    ? `starts in ${Math.max(1, Math.round((fprog.start - t) / 60))} min`
                    : `starts at ${clock(fprog.start, fmt)}`
                  : fprog.end > t
                    ? "on now"
                    : "finished",
              )}
            </p>
            {fprog.description && bp !== "compact" && <p class="c-2 ellipsis guide-desc">{cap(fprog.description, 300)}</p>}
          </>
        ) : (
          <p class="c-3 t-caption">{channels.length ? "Move through the guide with the arrow keys. Enter on a programme that's on now to watch it." : " "}</p>
        )}
      </section>
      <div class="guide-body">
        {list.error && !list.data ? (
          <ErrorState message={list.error} onRetry={list.reload} compact />
        ) : list.loading && !list.data ? (
          <div class="guide-skeleton" aria-hidden="true">
            {Array.from({ length: 8 }, (_, i) => (
              <Skeleton key={i} class="sk-guide-row" />
            ))}
          </div>
        ) : channels.length === 0 ? (
          <Empty art="empty-guide" title={cat === "favourites" ? "No favourite channels yet" : "No channels here"} body="Choose another category." compact />
        ) : (
          <div
            ref={scroller}
            class="guide-scroll"
            onKeyDown={onKeyDown}
            onFocusIn={onFocusIn}
            role="grid"
            aria-label="Programme guide"
            style={{ scrollPaddingLeft: `${chanW + 16}px`, scrollPaddingTop: `${RULER_H + 8}px` }}
          >
            <div class="g-sizer" style={{ width: `${chanW + width}px`, height: `${RULER_H + channels.length * ROW_H}px` }}>
              <div class="g-ruler" style={{ width: `${chanW + width}px` }} aria-hidden="true">
                <div class="g-ruler-corner t-caption" style={{ width: `${chanW}px` }}>
                  Today
                </div>
                <div class="g-ticks" style={{ width: `${width}px` }}>
                  {ticks.map((x) => (
                    <span key={x} class="g-tick t-caption c-3 tnum" style={{ transform: `translateX(${xOf(x)}px)` }}>
                      {clock(x, fmt)}
                    </span>
                  ))}
                  <span class="g-now-dot" style={{ transform: `translateX(${nowX}px)` }} />
                </div>
              </div>
              {rowsEls}
              <span
                class="g-now-line"
                aria-hidden="true"
                style={{ transform: `translateX(${chanW + nowX}px)`, height: `${channels.length * ROW_H}px` }}
              />
            </div>
          </div>
        )}
      </div>
      {bp !== "compact" && channels.length > 0 && (
        <p class="guide-hint t-caption c-3">
          <Icon name="info" />
          <span>← → programmes · ↑ ↓ channels · Enter watches what's on now · PageUp/PageDown scroll</span>
        </p>
      )}
    </div>
  );
}

