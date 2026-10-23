// SPDX-License-Identifier: GPL-3.0-or-later
// Live TV (DESIGN.md §8.3, §6.8): categories (All, Favourites, then the provider's), a
// virtualised channel list with numbers, logos and now/next with progress, and an inline
// preview. Moving the focus never switches the stream: Enter plays a row in the preview, Enter
// on the playing row goes fullscreen. PageUp/PageDown and Ctrl+↑/↓ zap; digits pick a channel
// number (it zaps after 1.5 s or on Enter).

import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { Category, Channel } from "../api/types";
import { account, settings } from "../state/session";
import { useDelayed, useLoad } from "../state/cache";
import * as catalog from "../state/catalog";
import { epgVersion, nowNext, requestNowNext } from "../state/epg";
import { favourites, isFavourite, toggleFavourite } from "../state/favourites";
import { now, toast } from "../state/misc";
import { current, go, setParams, type Memo } from "../state/router";
import { fraction, plural, remaining, timeRange } from "../lib/format";
import { cap, fold } from "../lib/text";
import { Bar, Button, ErrorState, Empty, Icon, IconButton, Skeleton, cx } from "../ui/kit";
import { ChannelLogo, FavHeart } from "../ui/media";
import { VirtualList } from "../ui/virtual";
import { breakpoint, useInitialFocus, useKeys, useBack } from "../ui/hooks";
import { isTextInput } from "../focus/dom";
import * as pb from "../player/playback";
import { previewTarget } from "../player/VideoLayer";
import { ChipRow } from "./common";

const ROW_H = 68;
const NUMBER_TIMEOUT = 1500;

export function Live(props: { memo: Memo; category?: string }) {
  const a = account.value!;
  const cat = props.category ?? "all";
  const cats = useLoad(catalog.keys.liveCategories(a.id), () => catalog.liveCategories(a.id));
  const list = useLoad(catalog.keys.channels(a.id, cat), () => catalog.channels(a.id, cat), { keepPrevious: true });
  const channels = list.data ?? [];
  const playing = pb.item.value?.kind === "live" ? pb.item.value.channel : null;
  const [number, setNumber] = useState<string>("");
  const numberTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [reveal, setReveal] = useState<{ index: number; focus?: boolean; nonce?: number } | null>(null);
  const showSkeleton = useDelayed(list.loading && !list.data);
  const bp = breakpoint.value;

  useInitialFocus(!list.loading && !cats.loading, props.memo);

  // Leaving Live TV stops the preview (unless it's for the fullscreen player).
  useEffect(
    () => () => {
      if (current.value.route.name !== "watch" && pb.item.value?.kind === "live") pb.stop();
    },
    [],
  );

  const onRange = useCallback(
    (s: number, e: number) => requestNowNext(a.id, channels.slice(s, e).map((c) => c.key)),
    [a.id, channels],
  );

  const selectCategory = (id: string) => {
    if (id === cat) return;
    setParams({ name: "live", category: id === "all" ? undefined : id } as { name: "live"; category?: string });
    props.memo["list:scroll"] = 0;
  };

  const playInPreview = (c: Channel, list: Channel[] = channels) => {
    void pb.play({ kind: "live", account: a.id, channel: c, list });
  };
  const fullscreen = () => {
    if (pb.item.value) go({ name: "watch" });
  };
  const activate = (c: Channel) => {
    if (playing?.key === c.key && pb.phase.value !== "error") fullscreen();
    else playInPreview(c);
  };

  const zap = (step: number) => {
    const next = pb.zap(step);
    if (next) {
      const i = channels.findIndex((c) => c.key === next.key);
      if (i >= 0) setReveal({ index: i, nonce: Date.now() });
    }
  };

  const commitNumber = async (digits: string) => {
    setNumber("");
    const n = Number(digits);
    if (!digits || !Number.isFinite(n)) return;
    let pool = channels;
    let ch = pool.find((c) => c.number === n);
    if (!ch) {
      pool = await catalog.channels(a.id, "all").catch(() => []);
      ch = pool.find((c) => c.number === n);
    }
    if (!ch) {
      toast(`There's no channel ${n}`, "info");
      return;
    }
    playInPreview(ch, pool);
    const i = channels.findIndex((c) => c.key === ch!.key);
    if (i >= 0) setReveal({ index: i, focus: true, nonce: Date.now() });
  };

  useEffect(
    () => () => {
      if (numberTimer.current) clearTimeout(numberTimer.current);
    },
    [],
  );

  // While the fullscreen player covers this screen, its keys belong to the player.
  const covered = current.value.route.name === "watch";

  useBack(() => {
    if (!number) return false;
    setNumber("");
    if (numberTimer.current) clearTimeout(numberTimer.current);
    return true;
  }, !!number && !covered);

  useKeys((e) => {
    if (isTextInput(document.activeElement)) return false;
    if (/^[0-9]$/.test(e.key) && !e.ctrlKey) {
      const next = (number + e.key).slice(0, 5);
      setNumber(next);
      if (numberTimer.current) clearTimeout(numberTimer.current);
      numberTimer.current = setTimeout(() => void commitNumber(next), NUMBER_TIMEOUT);
      return true;
    }
    if (number && e.key === "Enter") {
      if (numberTimer.current) clearTimeout(numberTimer.current);
      void commitNumber(number);
      return true;
    }
    const live = pb.item.value?.kind === "live";
    if (e.ctrlKey && (e.key === "ArrowDown" || e.key === "ArrowUp") && live) {
      zap(e.key === "ArrowDown" ? 1 : -1);
      return true;
    }
    if ((e.key === "PageUp" || e.key === "PageDown") && !e.ctrlKey && live) {
      // The TV remote's channel keys: PageUp is "channel up", the next channel.
      zap(e.key === "PageUp" ? 1 : -1);
      return true;
    }
    if (!e.ctrlKey && (e.key === "m" || e.key === "M") && live) {
      toast(pb.toggleMute() ? "Muted" : "Sound on");
      return true;
    }
    if (!e.ctrlKey && (e.key === "f" || e.key === "F") && live) {
      fullscreen();
      return true;
    }
    return false;
  }, !covered);

  const allCount = cats.data?.reduce((n, c) => n + (c.count ?? 0), 0) ?? 0;
  const favCount = favourites.value?.channels.length ?? 0;
  const catName =
    cat === "all" ? "All channels" : cat === "favourites" ? "Favourites" : cap(cats.data?.find((c) => c.id === cat)?.name ?? "Channels", 60);

  const listEl =
    list.error && !list.data ? (
      <ErrorState message={list.error} onRetry={list.reload} compact />
    ) : showSkeleton ? (
      <div class="vl" aria-hidden="true">
        {Array.from({ length: 9 }, (_, i) => (
          <div class="channel-row is-skeleton" key={i}>
            <Skeleton class="sk-num" />
            <Skeleton class="sk-logo" />
            <span class="row-text">
              <Skeleton class="sk-line" />
              <Skeleton class="sk-line-short" />
            </span>
          </div>
        ))}
      </div>
    ) : !list.loading && channels.length === 0 ? (
      cat === "favourites" ? (
        <Empty art="empty-favourites" title="Nothing saved yet" body="Press ♥ on a channel to keep it here." compact />
      ) : (
        <Empty title="No channels here" body="This category is empty." compact />
      )
    ) : (
      <VirtualList
        items={channels}
        itemKey={(c) => c.key}
        rowHeight={ROW_H}
        scope="live-list"
        memo={props.memo}
        memoKey="list"
        label={catName}
        pageKeys={false}
        onRange={onRange}
        reveal={reveal}
        class="channel-list"
        render={(c, i, tab) => (
          <ChannelRow
            c={c}
            account={a.id}
            playing={playing?.key === c.key}
            autofocus={playing ? playing.key === c.key : i === 0}
            tabIndex={tab}
            onActivate={() => activate(c)}
          />
        )}
      />
    );

  return (
    <div class={cx("screen live", `live-${bp}`)}>
      {bp === "wide" && (
        <CategoryColumn
          categories={cats.data ?? null}
          error={cats.error}
          selected={cat}
          allCount={allCount}
          favCount={favCount}
          onSelect={selectCategory}
        />
      )}
      <section class="live-main" aria-label={catName}>
        <header class="live-head">
          <h1 class="t-heading ellipsis">{catName}</h1>
          {list.data && <span class="t-caption c-3 tnum">{plural(channels.length, "channel")}</span>}
        </header>
        {bp !== "wide" && (
          <ChipRow
            scope="live-chips"
            selected={cat}
            onSelect={selectCategory}
            items={[
              { id: "all", label: "All" },
              { id: "favourites", label: "Favourites" },
              ...(cats.data ?? []).map((c) => ({ id: c.id, label: cap(c.name, 40) })),
            ]}
          />
        )}
        {bp === "compact" && playing && <Preview account={a.id} channel={playing} compact onFullscreen={fullscreen} />}
        <div class="live-list-wrap">{listEl}</div>
        {bp !== "compact" && (
          <p class="live-hint t-caption c-3">
            <Icon name="info" />
            <span>Enter to watch · Ctrl+↑/↓ or PageUp/PageDown change channel · Type a number to jump</span>
          </p>
        )}
      </section>
      {bp !== "compact" && <Preview account={a.id} channel={playing} onFullscreen={fullscreen} />}
      {number && (
        <div class="number-entry" role="status" aria-live="assertive">
          <span class="sr">Channel </span>
          <span class="number-digits tnum">{number}</span>
        </div>
      )}
    </div>
  );
}

function ChannelRow(props: { c: Channel; account: string; playing: boolean; autofocus: boolean; tabIndex: number; onActivate: () => void }) {
  const { c } = props;
  void epgVersion.value;
  const t = now.value;
  const nn = nowNext(props.account, c.key);
  const p = nn?.[0];
  const fav = isFavourite("channel", c.key);
  const fmt = settings.value.time_format;
  return (
    <button
      type="button"
      class={cx("channel-row", props.playing && "is-playing")}
      tabIndex={props.tabIndex}
      data-fk={`ch:${c.key}`}
      data-autofocus={props.autofocus ? "" : undefined}
      aria-current={props.playing ? "true" : undefined}
      aria-label={[c.number !== null ? `${c.number}` : "", c.name, p ? `now: ${p.title}` : "", props.playing ? "playing" : "", fav ? "favourite" : ""].filter(Boolean).join(", ")}
      onClick={props.onActivate}
    >
      <span class="row-num t-caption tnum">{c.number ?? ""}</span>
      <ChannelLogo channel={c} size="md" />
      <span class="row-text">
        <span class="row-name t-body-strong ellipsis">{cap(c.name, 120)}</span>
        <span class="row-now t-caption ellipsis">{p ? cap(p.title, 100) : nn ? "No guide" : " "}</span>
        <Bar value={p ? fraction(p.start, p.end, t) : 0} class={cx("row-bar", !p && "is-empty")} />
      </span>
      <span class="row-time t-caption c-3 tnum">{p ? timeRange(p.start, p.end, fmt) : ""}</span>
      <span class="row-icons">
        {fav && <FavHeart />}
        {c.archive_days ? <Icon name="clock-counter-clockwise" class="row-catchup" label="Catch-up available" /> : null}
      </span>
    </button>
  );
}

function CategoryColumn(props: {
  categories: Category[] | null;
  error: string | null;
  selected: string;
  allCount: number;
  favCount: number;
  onSelect: (id: string) => void;
}) {
  const [q, setQ] = useState("");
  const filtered = useMemo(() => {
    const f = fold(q.trim());
    return (props.categories ?? []).filter((c) => !f || fold(c.name).includes(f));
  }, [props.categories, q]);
  const item = (id: string, name: string, count: number | undefined) => (
    <button
      type="button"
      key={id}
      class={cx("cat-item", props.selected === id && "is-selected")}
      aria-current={props.selected === id ? "true" : undefined}
      data-fk={`cat:${id}`}
      onClick={() => props.onSelect(id)}
      title={name.length > 28 ? name : undefined}
    >
      <span class="ellipsis">{name}</span>
      {count !== undefined && <span class="cat-count t-caption c-3 tnum">{count.toLocaleString("en-GB")}</span>}
    </button>
  );
  return (
    <aside class="cats" aria-label="Categories">
      <h2 class="t-heading cats-title">Live TV</h2>
      <label class="cats-search">
        <Icon name="magnifying-glass" />
        <input
          type="search"
          class="cats-search-input"
          placeholder="Find a category"
          aria-label="Find a category"
          value={q}
          onInput={(e) => setQ((e.currentTarget as HTMLInputElement).value)}
          data-fk="cat-search"
        />
      </label>
      <div class="cats-list" data-scope="live-cats">
        {item("all", "All", props.allCount || undefined)}
        {item("favourites", "Favourites", props.favCount)}
        <hr class="cats-sep" />
        {props.categories === null && !props.error
          ? Array.from({ length: 8 }, (_, i) => <Skeleton key={i} class="sk-cat" />)
          : filtered.map((c) => item(c.id, cap(c.name, 80), c.count))}
        {props.error && <p class="t-caption c-3 cats-error">{props.error}</p>}
      </div>
    </aside>
  );
}

/** The inline preview: the video (drawn by the shared video layer) and the Now/Next card. */
function Preview(props: { account: string; channel: Channel | null; compact?: boolean; onFullscreen: () => void }) {
  const ref = useRef<HTMLButtonElement>(null);
  const c = props.channel;
  const phase = pb.phase.value;
  const err = pb.error.value;
  void epgVersion.value;
  const t = now.value;
  const nn = c ? nowNext(props.account, c.key) : undefined;
  const p = nn?.[0];
  const next = nn?.[1];
  const fmt = settings.value.time_format;
  const fav = c ? isFavourite("channel", c.key) : false;

  useEffect(() => {
    previewTarget.value = ref.current;
    return () => {
      if (previewTarget.value === ref.current) previewTarget.value = null;
    };
  }, []);
  useEffect(() => {
    if (c) requestNowNext(props.account, [c.key]);
  }, [c?.key]);

  return (
    <section class={cx("preview", props.compact && "preview-compact")} aria-label="Preview">
      {!props.compact && (
        <div class="preview-head">
          <span class="t-overline c-3 preview-label">
            {c && <span class="live-dot" aria-hidden="true" />}
            {c ? "Playing in preview" : "Preview"}
          </span>
          {c && (
            <span class="t-caption c-3 ellipsis tnum">
              {c.number !== null ? `${c.number} · ` : ""}
              {cap(c.name, 60)}
            </span>
          )}
        </div>
      )}
      <button
        type="button"
        ref={ref}
        class={cx("preview-video", !c && "is-idle")}
        aria-label={c ? `Watch ${c.name} fullscreen` : "Nothing playing"}
        data-fk="preview"
        disabled={!c}
        onClick={props.onFullscreen}
      >
        {!c && (
          <span class="preview-idle">
            <span class="preview-idle-text c-2">Choose a channel and press Enter</span>
          </span>
        )}
        {c && (phase === "starting" || phase === "buffering") && !err && (
          <span class="preview-status">
            <span class="spinner" aria-hidden="true">
              <svg viewBox="0 0 48 48">
                <circle cx="24" cy="24" r="20" />
              </svg>
            </span>
            <span class="t-caption">{pb.reconnecting.value ? `Reconnecting… (${pb.reconnecting.value})` : "Buffering…"}</span>
          </span>
        )}
        {c && err && (
          <span class="preview-error">
            <Icon name="warning" />
            <span class="t-body-strong">{err.title}</span>
            <span class="t-caption c-2">{err.message}</span>
          </span>
        )}
      </button>
      {!props.compact && (
        <div class="now-card">
          {p ? (
            <>
              <p class="t-overline c-3">Now</p>
              <h2 class="t-title clamp-2">{cap(p.title, 120)}</h2>
              <p class="now-time t-caption c-3 tnum">
                <span>{timeRange(p.start, p.end, fmt)}</span>
                <span>{remaining(p.end - t)}</span>
              </p>
              <Bar value={fraction(p.start, p.end, t)} class="now-bar" />
              {p.description && <p class="c-2 clamp-3 now-desc">{cap(p.description, 500)}</p>}
              {next && (
                <p class="now-next t-caption">
                  <span class="t-overline c-3">Next</span>
                  <span class="c-2 ellipsis">
                    {timeRange(next.start, next.end, fmt).split("–")[0]} · {cap(next.title, 80)}
                  </span>
                </p>
              )}
            </>
          ) : c ? (
            <>
              <p class="t-overline c-3">Now</p>
              <h2 class="t-title clamp-2">{cap(c.name, 120)}</h2>
              <p class="c-3 t-caption">{nn ? "No guide for this channel." : "Loading the guide…"}</p>
            </>
          ) : (
            <p class="c-3">The programme guide for the playing channel appears here.</p>
          )}
        </div>
      )}
      {props.compact && p && (
        <p class="preview-compact-now t-caption ellipsis">
          <span class="c-3">Now </span>
          {cap(p.title, 100)}
        </p>
      )}
      <div class="preview-actions" data-scope="preview-actions">
        <Button variant="primary" icon="arrows-out" onClick={props.onFullscreen} disabled={!c} fk="pv:full">
          Fullscreen
        </Button>
        {c?.archive_days ? (
          <Button
            variant="secondary"
            icon="clock-counter-clockwise"
            onClick={() => go({ name: "catchup", account: props.account, key: c.key })}
            fk="pv:catchup"
            title={`Past programmes of ${c.name} (${plural(c.archive_days, "day")})`}
          >
            Catch-up
          </Button>
        ) : null}
        {!props.compact &&
          (breakpoint.value === "wide" ? (
            <Button variant="secondary" icon="rows" onClick={() => go({ name: "guide" })} fk="pv:guide">
              Guide
            </Button>
          ) : (
            // Narrower columns: the Guide (also in the side rail) as an icon, so the buttons stay on one line.
            <IconButton icon="rows" label="Guide" onClick={() => go({ name: "guide" })} fk="pv:guide" />
          ))}
        <IconButton
          icon="heart"
          fill={fav}
          active={fav}
          label={fav ? "Remove from Favourites" : "Add to Favourites"}
          disabled={!c}
          onClick={() => c && void toggleFavourite(props.account, { kind: "channel", item: c })}
          fk="pv:fav"
        />
      </div>
    </section>
  );
}
