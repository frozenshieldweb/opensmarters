// SPDX-License-Identifier: GPL-3.0-or-later
// Movies and Series browse (DESIGN.md §8.4): categories (a column on wide windows, chips
// below), a sort menu (provider order, name, recently added, rating) and a virtualised poster
// grid. Enter opens the detail page.

import { useCallback, useMemo, useState } from "preact/hooks";
import type { Category, Movie, Series } from "../api/types";
import { account } from "../state/session";
import { useDelayed, useLoad } from "../state/cache";
import * as catalog from "../state/catalog";
import { go, setParams, type Memo } from "../state/router";
import { plural } from "../lib/format";
import { byName, cap } from "../lib/text";
import { load, save } from "../lib/storage";
import { Button, Empty, ErrorState, Skeleton, cx } from "../ui/kit";
import { PosterCard } from "../ui/media";
import { VirtualGrid } from "../ui/virtual";
import { breakpoint, useInitialFocus } from "../ui/hooks";
import { openMenu } from "../ui/overlay";
import { ChipRow } from "./common";
import { metaOf } from "./home";

type Kind = "movies" | "series";
type Item = Movie | Series;

function sortItems(items: readonly Item[], order: catalog.SortOrder): Item[] {
  const list = [...items];
  switch (order) {
    case "name":
      return list.sort((a, b) => byName(a.name, b.name));
    case "added":
      return list.sort((a, b) => stamp(b) - stamp(a));
    case "rating":
      return list.sort((a, b) => (b.rating ?? -1) - (a.rating ?? -1));
    default:
      return list;
  }
}

const stamp = (x: Item) => ("added" in x ? x.added : x.updated) ?? 0;

export function Browse(props: { memo: Memo; kind: Kind; category?: string }) {
  const a = account.value!;
  const k = props.kind;
  const cat = props.category ?? "all";
  const cats = useLoad(k === "movies" ? catalog.keys.movieCategories(a.id) : catalog.keys.seriesCategories(a.id), () =>
    k === "movies" ? catalog.movieCategories(a.id) : catalog.seriesCategories(a.id),
  );
  const list = useLoad<Item[]>(k === "movies" ? catalog.keys.movies(a.id, cat) : catalog.keys.series(a.id, cat), () =>
    k === "movies" ? catalog.movies(a.id, cat) : catalog.series(a.id, cat),
  );
  const [order, setOrder] = useState<catalog.SortOrder>(() => (load(`sort:${k}`) as catalog.SortOrder | null) ?? "provider");
  const sorted = useMemo(() => sortItems(list.data ?? [], order), [list.data, order]);
  const showSkeleton = useDelayed(list.loading);
  const bp = breakpoint.value;
  const title = k === "movies" ? "Movies" : "Series";
  // M3U playlists have films (their VOD entries, without categories) but no series.
  const m3u = a.kind === "m3u" && k === "series";

  // The grid lays itself out after it has measured its width: focus its first poster then.
  const [gridReady, setGridReady] = useState(false);
  const onGridRange = useCallback(() => setGridReady(true), []);
  useInitialFocus(!list.loading && !cats.loading && (m3u || sorted.length === 0 || gridReady), props.memo);

  const select = (id: string) => {
    if (id === cat) return;
    props.memo["grid:scroll"] = 0;
    setParams({ name: k, category: id === "all" ? undefined : id });
  };

  const sortLabel = catalog.SORTS.find((s) => s.value === order)?.label ?? "";
  const openSort = (e: MouseEvent) =>
    void openMenu(
      e.currentTarget as HTMLElement,
      "Sort by",
      catalog.SORTS.map((s) => ({
        label: s.value === "added" && k === "series" ? "Recently updated" : s.label,
        selected: s.value === order,
        onSelect: () => {
          setOrder(s.value);
          save(`sort:${k}`, s.value);
          props.memo["grid:scroll"] = 0;
        },
      })),
    );

  const catList: Category[] = cats.data ?? [];
  const column = bp === "wide" && !m3u && (cats.loading || catList.length > 0);
  const catName = cat === "all" ? `All ${k === "movies" ? "films" : "series"}` : cap(catList.find((c) => c.id === cat)?.name ?? title, 60);
  const posterW = bp === "wide" ? 168 : bp === "medium" ? 148 : window.innerWidth < 600 ? 104 : 132;

  let body;
  if (m3u) {
    body = (
      <Empty
        title="No series with a playlist"
        body="Series come with Xtream Codes providers. This provider is an M3U playlist: its films are in Movies."
        action={
          <Button variant="primary" icon="film-strip" onClick={() => go({ name: "movies" })} autofocus>
            Go to Movies
          </Button>
        }
      />
    );
  } else if (list.error) {
    body = <ErrorState message={list.error} onRetry={list.reload} compact />;
  } else if (list.loading) {
    body = showSkeleton ? (
      <div class="grid-skeleton" aria-hidden="true">
        {Array.from({ length: 18 }, (_, i) => (
          <span class="sk-card" key={i}>
            <Skeleton class="sk-poster" />
            <Skeleton class="sk-line" />
          </span>
        ))}
      </div>
    ) : null;
  } else if (sorted.length === 0) {
    body = <Empty title="Nothing in this category" body="Try another category." compact />;
  } else {
    body = (
      <VirtualGrid<Item>
        items={sorted}
        itemKey={(x) => x.id}
        scope={`${k}-grid`}
        memo={props.memo}
        memoKey="grid"
        label={catName}
        minItemWidth={posterW}
        gap={bp === "compact" ? 12 : 16}
        rowGap={bp === "compact" ? 16 : 24}
        pad={bp === "wide" ? 48 : bp === "medium" ? 40 : window.innerWidth < 600 ? 16 : 24}
        padTop={16}
        itemHeight={(w) => Math.round(w * 1.5) + 52}
        onRange={onGridRange}
        class="poster-grid"
        render={(x, i, tab) => (
          <PosterCard
            tabIndex={tab}
            autofocus={i === 0}
            fk={`${k}:${x.id}`}
            title={x.name}
            meta={metaOf(x)}
            image={x.poster}
            onClick={() => go(k === "movies" ? { name: "movie", id: x.id } : { name: "show", id: x.id })}
          />
        )}
      />
    );
  }

  return (
    <div class={cx("screen browse", `browse-${bp}`, !column && "browse-nocats")}>
      {column && (
        <aside class="cats" aria-label="Categories">
          <h2 class="t-heading cats-title">{title}</h2>
          <div class="cats-list" data-scope={`${k}-cats`}>
            {[{ id: "all", name: "All" }, ...catList].map((c) => (
              <button
                type="button"
                key={c.id}
                class={cx("cat-item", cat === c.id && "is-selected")}
                aria-current={cat === c.id ? "true" : undefined}
                data-fk={`cat:${c.id}`}
                onClick={() => select(c.id)}
              >
                <span class="ellipsis">{cap(c.name, 80)}</span>
              </button>
            ))}
            {cats.loading && Array.from({ length: 6 }, (_, i) => <Skeleton key={i} class="sk-cat" />)}
          </div>
        </aside>
      )}
      <section class="browse-main" aria-label={catName}>
        <header class="browse-head">
          <div class="browse-titles">
            <h1 class="t-heading ellipsis">{m3u || bp === "medium" ? title : catName}</h1>
            {list.data && !m3u && <span class="t-caption c-3 tnum">{plural(sorted.length, k === "movies" ? "film" : "series", k === "movies" ? "films" : "series")}</span>}
          </div>
          {!m3u && (
            <Button variant="secondary" icon="sort-ascending" iconRight="caret-down" onClick={openSort} fk={`${k}:sort`} class="sort-btn">
              {sortLabel === "Recently added" && k === "series" ? "Recently updated" : sortLabel}
            </Button>
          )}
        </header>
        {bp !== "wide" && !m3u && catList.length > 0 && (
          <ChipRow scope={`${k}-chips`} selected={cat} onSelect={select} items={[{ id: "all", label: "All" }, ...catList.map((c) => ({ id: c.id, label: cap(c.name, 40) }))]} />
        )}
        <div class="browse-body">{body}</div>
      </section>
    </div>
  );
}
