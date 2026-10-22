// SPDX-License-Identifier: GPL-3.0-or-later
// Home (DESIGN.md §8.2): a hero band (the newest Continue watching item, else the newest film)
// and rails: Continue watching, Live favourites, Recently added films and series. Empty rails
// are hidden.

import type { ComponentChildren } from "preact";
import { useEffect, useMemo } from "preact/hooks";
import type { Channel, ContinueItem, Movie, MovieDetail, Series, SeriesDetail } from "../api/types";
import { account } from "../state/session";
import { useLoad } from "../state/cache";
import * as catalog from "../state/catalog";
import { favourites } from "../state/favourites";
import { epgVersion, nowNext, requestNowNext } from "../state/epg";
import { now } from "../state/misc";
import { go, type Memo } from "../state/router";
import { dot, fraction, rating as fmtRating, runtime, timecode } from "../lib/format";
import { cap } from "../lib/text";
import { Bar, Button, Empty, Skeleton } from "../ui/kit";
import { BackdropCard, ChannelLogo, Img, PosterCard } from "../ui/media";
import { useInitialFocus } from "../ui/hooks";
import { movieDetail, resumeItem, seriesDetail, watchChannel, watchMovie } from "../player/start";

type Featured =
  | { kind: "continue"; item: ContinueItem; movie?: MovieDetail; series?: SeriesDetail }
  | { kind: "new"; movie: MovieDetail };

export function Home(props: { memo: Memo }) {
  const a = account.value!;
  const cont = useLoad(`${a.id}:continue`, () => catalog.continueWatching(a.id));
  const movies = useLoad(catalog.keys.movies(a.id, "all"), () => catalog.movies(a.id, "all"));
  const series = useLoad(catalog.keys.series(a.id, "all"), () => catalog.series(a.id, "all"));
  const favs = favourites.value;

  const recentMovies = useMemo(
    () => (movies.data ? [...movies.data].sort((x, y) => (y.added ?? 0) - (x.added ?? 0)).slice(0, 20) : []),
    [movies.data],
  );
  const recentSeries = useMemo(
    () => (series.data ? [...series.data].sort((x, y) => (y.updated ?? 0) - (x.updated ?? 0)).slice(0, 20) : []),
    [series.data],
  );

  const top = cont.data?.[0];
  const featuredKey = top
    ? `${a.id}:featured:${top.kind}:${top.series_id ?? top.id}`
    : recentMovies[0]
      ? `${a.id}:featured:new:${recentMovies[0].id}`
      : null;
  const featured = useLoad<Featured | null>(featuredKey, async () => {
    if (top) {
      if (top.kind === "movie") return { kind: "continue", item: top, movie: await movieDetail(a.id, top.id) };
      if (top.series_id) return { kind: "continue", item: top, series: await seriesDetail(a.id, top.series_id) };
      return { kind: "continue", item: top };
    }
    const m = recentMovies[0];
    return m ? { kind: "new", movie: await movieDetail(a.id, m.id) } : null;
  });

  const liveFavs = favs?.channels ?? [];
  const loading = cont.loading || movies.loading || series.loading || !favs;
  const nothing =
    !loading && !(cont.data?.length || liveFavs.length || recentMovies.length || recentSeries.length);
  useInitialFocus(!loading && (!featuredKey || !featured.loading), props.memo);

  return (
    <div class="screen home" data-scroll="home">
      <Hero featured={featured.data ?? null} loading={!!featuredKey && featured.loading} account={a.id} />
      <div class="home-rails">
        {nothing && (
          <Empty
            art="empty-favourites"
            title="Nothing here yet"
            body="Start with Live TV. Films you start and channels you save will appear here."
            action={
              <Button variant="primary" icon="television-simple" onClick={() => go({ name: "live" })} autofocus>
                Browse Live TV
              </Button>
            }
          />
        )}
        {cont.data && cont.data.length > 0 && (
          <Rail title="Continue watching" scope="rail-continue">
            {cont.data.map((c) => (
              <BackdropCard
                key={`${c.kind}:${c.id}`}
                fk={`continue:${c.kind}:${c.id}`}
                title={cap(c.name, 120)}
                meta={`${timecode(Math.max(0, c.duration - c.position))} left`}
                image={c.poster}
                progress={fraction(0, c.duration, c.position)}
                onClick={() => void resumeItem(a.id, c)}
              />
            ))}
          </Rail>
        )}
        {liveFavs.length > 0 && <LiveRail account={a.id} channels={liveFavs} />}
        {movies.loading && !movies.data && <SkeletonRail />}
        {recentMovies.length > 0 && (
          <Rail title="Recently added" kicker="Films" scope="rail-movies" onSeeAll={() => go({ name: "movies" })}>
            {recentMovies.map((m) => (
              <PosterCard
                key={m.id}
                fk={`movie:${m.id}`}
                title={m.name}
                meta={metaOf(m)}
                image={m.poster}
                onClick={() => go({ name: "movie", id: m.id })}
              />
            ))}
          </Rail>
        )}
        {recentSeries.length > 0 && (
          <Rail title="Series" kicker="Recently updated" scope="rail-series" onSeeAll={() => go({ name: "series" })}>
            {recentSeries.map((s) => (
              <PosterCard
                key={s.id}
                fk={`series:${s.id}`}
                title={s.name}
                meta={metaOf(s)}
                image={s.poster}
                onClick={() => go({ name: "show", id: s.id })}
              />
            ))}
          </Rail>
        )}
      </div>
    </div>
  );
}

export function metaOf(m: Pick<Movie | Series, "year" | "rating">): string {
  const r = fmtRating(m.rating);
  return dot(m.year ?? undefined, r ? `★ ${r}` : undefined);
}

function Hero(props: { featured: Featured | null; loading: boolean; account: string }) {
  const f = props.featured;
  if (!f) {
    return (
      <section class="hero hero-empty" aria-hidden={props.loading ? "true" : undefined}>
        <img class="hero-bg hero-ambient" src="/img/home-ambient.jpg" alt="" />
        <div class="hero-scrim" />
        {props.loading && (
          <div class="hero-text">
            <Skeleton class="sk-overline" />
            <Skeleton class="sk-display" />
            <Skeleton class="sk-line" />
          </div>
        )}
      </section>
    );
  }
  let overline: string;
  let title: string;
  let meta: string;
  let plot: string | null;
  let backdrop: string | null;
  let primary: ComponentChildren;
  let info: () => void;
  if (f.kind === "continue") {
    const c = f.item;
    overline = "Continue watching";
    title = c.kind === "episode" && f.series ? f.series.name : c.name;
    const d = f.movie ?? f.series;
    meta =
      c.kind === "episode"
        ? cap(c.name, 120)
        : dot(d?.year ?? undefined, runtime(f.movie?.duration_secs), d?.rating ? `★ ${fmtRating(d.rating)}` : undefined, cap(d?.genre ?? "", 60));
    plot = d?.plot ?? null;
    backdrop = d?.backdrop ?? null;
    primary = (
      <Button variant="primary" size="lg" icon="play" iconFill autofocus fk="hero:play" onClick={() => void resumeItem(props.account, c)}>
        Resume {timecode(c.position)}
      </Button>
    );
    info = () => (c.kind === "movie" ? go({ name: "movie", id: c.id }) : c.series_id && go({ name: "show", id: c.series_id }));
  } else {
    const m = f.movie;
    overline = "Recently added";
    title = m.name;
    meta = dot(m.year ?? undefined, runtime(m.duration_secs), m.rating ? `★ ${fmtRating(m.rating)}` : undefined, cap(m.genre ?? "", 60));
    plot = m.plot;
    backdrop = m.backdrop;
    primary = (
      <Button variant="primary" size="lg" icon="play" iconFill autofocus fk="hero:play" onClick={() => watchMovie(props.account, m)}>
        Play
      </Button>
    );
    info = () => go({ name: "movie", id: m.id });
  }
  return (
    <section class="hero" aria-label={overline}>
      <Img class="hero-bg" src={backdrop} fallback="/img/home-ambient.jpg" eager />
      <div class="hero-scrim" />
      <div class="hero-text" data-scope="hero">
        <p class="t-overline c-2">{overline}</p>
        <h1 class="t-display hero-title clamp-2">{cap(title, 160)}</h1>
        {meta && <p class="t-caption c-2 hero-meta">{meta}</p>}
        {plot && <p class="c-2 clamp-2 hero-plot">{cap(plot, 400)}</p>}
        <div class="hero-actions">
          {primary}
          <Button variant="secondary" size="lg" icon="info" fk="hero:info" onClick={info}>
            More info
          </Button>
        </div>
      </div>
    </section>
  );
}

export function Rail(props: { title: string; kicker?: string; scope: string; children: ComponentChildren; onSeeAll?: () => void }) {
  return (
    <section class="rail-row" aria-label={props.title}>
      <div class="rail-head">
        <h2 class="t-title">{props.title}</h2>
        {props.kicker && <span class="t-overline c-3">{props.kicker}</span>}
        {props.onSeeAll && (
          <Button variant="ghost" size="sm" iconRight="caret-right" class="rail-seeall" onClick={props.onSeeAll} data-nohover="">
            See all
          </Button>
        )}
      </div>
      <div class="rail-scroll" data-scope={props.scope}>
        {props.children}
      </div>
    </section>
  );
}

function SkeletonRail() {
  return (
    <section class="rail-row" aria-hidden="true">
      <div class="rail-head">
        <Skeleton class="sk-title" />
      </div>
      <div class="rail-scroll">
        {Array.from({ length: 8 }, (_, i) => (
          <Skeleton key={i} class="sk-poster" />
        ))}
      </div>
    </section>
  );
}

function LiveRail(props: { account: string; channels: Channel[] }) {
  const keys = props.channels.map((c) => c.key);
  useEffect(() => requestNowNext(props.account, keys), [props.account, keys.join(",")]);
  void epgVersion.value;
  const t = now.value;
  return (
    <Rail title="Live now" kicker="Favourites" scope="rail-live" onSeeAll={() => go({ name: "live", category: "favourites" })}>
      {props.channels.map((c) => {
        const p = nowNext(props.account, c.key)?.[0];
        return (
          <button
            type="button"
            key={c.key}
            class="card tile"
            data-fk={`tile:${c.key}`}
            aria-label={p ? `${c.name}, now: ${p.title}` : c.name}
            onClick={() => watchChannel(props.account, c, props.channels)}
          >
            <span class="tile-live t-overline">
              <span class="live-dot" aria-hidden="true" />
              Live
            </span>
            <ChannelLogo channel={c} size="lg" />
            <span class="tile-text">
              <span class="t-body-strong ellipsis">{cap(c.name, 80)}</span>
              <span class="t-caption c-2 ellipsis">{p ? cap(p.title, 80) : " "}</span>
            </span>
            <Bar value={p ? fraction(p.start, p.end, t) : 0} class="tile-bar" />
          </button>
        );
      })}
    </Rail>
  );
}

