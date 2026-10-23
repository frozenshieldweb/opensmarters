// SPDX-License-Identifier: GPL-3.0-or-later
// Catch-up for one channel: its archived programmes from the provider, grouped by day on the
// viewer's clock (Today, Yesterday, then weekday and date), newest first. Enter plays a
// programme from its start in the player (a duration and seeking, like a film). Reached from
// Live TV (the preview's Catch-up button) and from the Guide.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import { api } from "../api/client";
import type { Programme } from "../api/types";
import { accounts, settings } from "../state/session";
import { useDelayed, useLoad } from "../state/cache";
import * as catalog from "../state/catalog";
import { now } from "../state/misc";
import { back, stack, type Memo } from "../state/router";
import { clock, dot, plural, runtime, timeRange } from "../lib/format";
import { groupByDay, type CatchupDay } from "../lib/catchup";
import { cap } from "../lib/text";
import { Button, Empty, ErrorState, Icon, Skeleton, cx } from "../ui/kit";
import { ChannelLogo } from "../ui/media";
import { breakpoint, useInitialFocus } from "../ui/hooks";
import { rememberInScope } from "../focus/dom";
import { watchCatchup } from "../player/start";
import { ChipRow } from "./common";

const dayMemo = "catchup:day";

export function CatchupScreen(props: { memo: Memo; account: string; channelKey: string }) {
  const acct = accounts.value?.find((a) => a.id === props.account) ?? null;
  const all = useLoad(acct ? catalog.keys.channels(acct.id, "all") : null, () => catalog.channels(props.account, "all"));
  const channel = all.data?.find((c) => c.key === props.channelKey) ?? null;
  const archive = !!channel?.archive_days;
  const list = useLoad(acct && archive ? `${acct.id}:catchup:${props.channelKey}` : null, () => api.catchup(props.account, props.channelKey), {
    ttlMs: 60_000,
  });
  const t = now.value;
  const days = useMemo(() => groupByDay(list.data ?? [], Math.floor(Date.now() / 1000)), [list.data, Math.floor(t / 60)]);
  const [dayKey, setDayKey] = useState<string | null>((props.memo[dayMemo] as string | undefined) ?? null);
  const day = days.find((d) => d.key === dayKey) ?? days[0] ?? null;
  const [focused, setFocused] = useState<number | null>(null);
  const bp = breakpoint.value;
  const fmt = settings.value.time_format;
  const loading = all.loading || (archive && list.loading);
  const showSkeleton = useDelayed(loading);

  useInitialFocus(!loading, props.memo);

  useEffect(() => {
    if (day) props.memo[dayMemo] = day.key;
  }, [day?.key]);

  // Moving into the days (left, or up to the chips) lands on the selected one.
  const rootRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root || !day) return;
    const k = CSS.escape(day.key);
    rememberInScope("cu-days", root.querySelector(`[data-fk="day:${k}"]`));
    rememberInScope("cu-chips", root.querySelector(`[data-fk="chip:${k}"]`));
  }, [day?.key, bp, days.length]);

  const pick = (key: string) => {
    if (key === day?.key) return;
    setDayKey(key);
    setFocused(null);
  };
  const play = (p: Programme) => {
    if (acct && channel) watchCatchup(acct.id, channel, p, fmt);
  };

  const below = stack.value[stack.value.length - 2]?.route.name;
  const backLabel = below === "guide" ? "Guide" : "Live TV";
  const info = (day?.programmes.find((p) => p.start === focused) ?? day?.programmes[0]) || null;

  let body;
  if (!acct) {
    body = <ErrorState title="This provider isn't here any more" message="It may have been removed in Settings." />;
  } else if (all.error && !all.data) {
    body = <ErrorState title="Couldn't load the channel" message={all.error} onRetry={all.reload} compact />;
  } else if (showSkeleton && (!all.data || list.loading)) {
    body = <ListSkeleton />;
  } else if (all.data && !channel) {
    body = <Empty title="This channel isn't available" body="Your provider no longer lists it." action={<BackAction label={backLabel} />} compact />;
  } else if (channel && !archive) {
    body = (
      <Empty
        title="No catch-up on this channel"
        body={`Your provider doesn't keep past programmes for ${cap(channel.name, 60)}.`}
        action={<BackAction label={backLabel} />}
        compact
      />
    );
  } else if (list.error && !list.data) {
    body = <ErrorState title="Couldn't load catch-up" message={list.error} onRetry={list.reload} compact />;
  } else if (list.data && days.length === 0) {
    body = (
      <Empty
        art="empty-guide"
        title="Nothing to catch up on yet"
        body={`${cap(channel?.name ?? "This channel", 60)} has no recorded programmes from the last ${plural(channel?.archive_days ?? 1, "day")}.`}
        action={<BackAction label={backLabel} />}
        compact
      />
    );
  } else if (day) {
    body = (
      <ProgrammeList
        day={day}
        fmt={fmt}
        compact={bp === "compact"}
        onPlay={play}
        onFocus={setFocused}
      />
    );
  } else {
    body = null;
  }

  const total = list.data?.length ?? 0;
  const dayItems = days.map((d) => ({ id: d.key, label: d.label }));

  return (
    <div ref={rootRef} class={cx("screen catchup", `catchup-${bp}`)}>
      {bp === "wide" && (
        <aside class="cats cu-days" aria-label="Days">
          <h2 class="t-heading cats-title">Catch-up</h2>
          <div class="cats-list" data-scope="cu-days">
            {days.map((d) => (
              <button
                type="button"
                key={d.key}
                class={cx("cat-item", d.key === day?.key && "is-selected")}
                aria-current={d.key === day?.key ? "true" : undefined}
                data-fk={`day:${d.key}`}
                onClick={() => pick(d.key)}
              >
                <span class="ellipsis">{d.label}</span>
                <span class="cat-count t-caption c-3 tnum">{d.programmes.length}</span>
              </button>
            ))}
            {loading && days.length === 0 && Array.from({ length: 5 }, (_, i) => <Skeleton key={i} class="sk-cat" />)}
          </div>
        </aside>
      )}
      <section class="cu-main" aria-label={channel ? `Catch-up: ${channel.name}` : "Catch-up"}>
        <header class="cu-head">
          <Button variant="ghost" icon="caret-left" class="back-btn cu-back" onClick={() => back()} fk="cu:back">
            {backLabel}
          </Button>
          <div class="cu-title-row">
            {channel && <ChannelLogo channel={channel} size="md" />}
            <div class="cu-titles">
              <h1 class="t-heading ellipsis">{channel ? cap(channel.name, 80) : "Catch-up"}</h1>
              <p class="t-caption c-3 tnum ellipsis">
                {channel && archive
                  ? dot(
                      bp === "wide" ? undefined : "Catch-up",
                      `${plural(channel.archive_days ?? 0, "day")} of programmes`,
                      list.data ? plural(total, "programme") : undefined,
                    )
                  : "Catch-up"}
              </p>
            </div>
          </div>
        </header>
        {bp !== "wide" && dayItems.length > 0 && (
          <ChipRow scope="cu-chips" selected={day?.key ?? ""} onSelect={pick} items={dayItems} label="Days" />
        )}
        <div class="cu-body">{body}</div>
        {bp !== "compact" && day && (
          <p class="live-hint t-caption c-3 cu-hint">
            <Icon name="info" />
            <span>Enter watches a programme from the start · Esc goes back</span>
          </p>
        )}
      </section>
      {bp !== "compact" && (
        <aside class="cu-info" aria-live="polite">
          {info && channel ? (
            <div class="now-card cu-card">
              <p class="t-overline c-3">
                {day?.label} · {clock(info.start, fmt)}
              </p>
              <h2 class="t-title clamp-2">{cap(info.title, 160)}</h2>
              <p class="t-caption c-3 tnum">{dot(timeRange(info.start, info.end, fmt), runtime(info.end - info.start))}</p>
              {info.description && <p class="c-2 cu-card-desc now-desc">{cap(info.description, 900)}</p>}
              <div class="cu-card-actions" data-scope="cu-card">
                <Button variant="primary" icon="play" iconFill onClick={() => play(info)} fk="cu:play">
                  Watch from the start
                </Button>
              </div>
            </div>
          ) : loading ? (
            <div class="now-card cu-card" aria-hidden="true">
              <Skeleton class="sk-line-short" />
              <Skeleton class="sk-line" />
            </div>
          ) : null}
        </aside>
      )}
    </div>
  );
}

function BackAction(props: { label: string }) {
  return (
    <Button variant="primary" icon="caret-left" onClick={() => back()} autofocus>
      Back to {props.label}
    </Button>
  );
}

function ListSkeleton() {
  return (
    <div class="cu-list" aria-hidden="true">
      {Array.from({ length: 7 }, (_, i) => (
        <div class="cu-row is-skeleton" key={i}>
          <Skeleton class="sk-line-short" />
          <span class="cu-text">
            <Skeleton class="sk-line" />
            <Skeleton class="sk-line-short" />
          </span>
        </div>
      ))}
    </div>
  );
}

function ProgrammeList(props: {
  day: CatchupDay;
  fmt: "24h" | "12h";
  compact: boolean;
  onPlay: (p: Programme) => void;
  onFocus: (start: number) => void;
}) {
  return (
    <div
      class="cu-list"
      data-scope="cu-list"
      aria-label={`${props.day.label}: ${plural(props.day.programmes.length, "programme")}`}
      onFocusIn={(e) => {
        const s = Number((e.target as HTMLElement).dataset["start"] ?? NaN);
        if (Number.isFinite(s)) props.onFocus(s);
      }}
    >
      {props.day.programmes.map((p, i) => (
        <ProgrammeRow key={`${props.day.key}:${p.start}`} p={p} fmt={props.fmt} first={i === 0} compact={props.compact} onPlay={props.onPlay} />
      ))}
    </div>
  );
}

function ProgrammeRow(props: { p: Programme; fmt: "24h" | "12h"; first: boolean; compact: boolean; onPlay: (p: Programme) => void }) {
  const { p } = props;
  return (
    <button
      type="button"
      class="cu-row"
      data-fk={`cu:${p.start}`}
      data-start={p.start}
      data-autofocus={props.first ? "" : undefined}
      aria-label={`${p.title}, ${timeRange(p.start, p.end, props.fmt)}, ${runtime(p.end - p.start)}. Watch from the start`}
      onClick={() => props.onPlay(p)}
    >
      <span class="cu-time t-body-strong tnum">{clock(p.start, props.fmt)}</span>
      <span class="cu-text">
        <span class="cu-name t-body-strong ellipsis">{cap(p.title, 140)}</span>
        {p.description && <span class={cx("cu-desc t-caption c-3", props.compact ? "clamp-2" : "ellipsis")}>{cap(p.description, 240)}</span>}
      </span>
      <span class="cu-dur t-caption c-3 tnum">{runtime(p.end - p.start)}</span>
      <Icon name="play" fill class="cu-play" />
    </button>
  );
}
