// SPDX-License-Identifier: GPL-3.0-or-later
// Detail pages (DESIGN.md §8.4): the provider's backdrop behind a scrim, title, meta, synopsis
// ("More" expands it), cast, and Play / Resume / Play from start / ♥. Series add season tabs
// and the episode list; playing goes on to the next episode at the end.

import type { ComponentChildren } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import type { Episode, MovieDetail, Season, SeriesDetail } from "../api/types";
import { account } from "../state/session";
import { useLoad } from "../state/cache";
import { isFavourite, toggleFavourite } from "../state/favourites";
import { back, current, stack, type Memo } from "../state/router";
import { dot, fraction, rating as fmtRating, runtime, timecode } from "../lib/format";
import { cap } from "../lib/text";
import { Bar, Button, ErrorState, Icon, IconButton, Skeleton, cx } from "../ui/kit";
import { Img } from "../ui/media";
import { useInitialFocus } from "../ui/hooks";
import { watchEpisode, watchMovie } from "../player/start";
import { api } from "../api/client";

const FINISHED = 0.95;
/** A film or episode watched for at least this long (seconds) offers Resume. */
const RESUME_AFTER = 10;

/** Reloads the page's data when the player above it closes (new progress). */
function useReloadAfterPlayer(watching: boolean, reload: () => void): void {
  const was = useRef(watching);
  useEffect(() => {
    const closed = was.current && !watching;
    was.current = watching;
    if (!closed) return;
    // Give the player's last progress save a moment to land first.
    const t = setTimeout(reload, 400);
    return () => clearTimeout(t);
  }, [watching]);
}

function BackButton(props: { label: string }) {
  return (
    <Button variant="ghost" icon="caret-left" class="back-btn detail-back" onClick={() => back()} fk="detail:back">
      {props.label}
    </Button>
  );
}

function parentLabel(fallback: string): string {
  const s = stack.value;
  const below = s[s.length - 2]?.route.name;
  const names: Record<string, string> = {
    home: "Home",
    movies: "Movies",
    series: "Series",
    search: "Search",
    favourites: "Favourites",
  };
  return (below && names[below]) || fallback;
}

function DetailLayout(props: { backdrop: string | null; children: ComponentChildren; back: string }) {
  return (
    <div class="screen detail">
      <div class="detail-bg" aria-hidden="true">
        <Img src={props.backdrop} fallback="/img/backdrop.jpg" eager class="detail-bg-img" />
        <div class="detail-scrim" />
      </div>
      <div class="detail-content">
        <BackButton label={props.back} />
        {props.children}
      </div>
    </div>
  );
}

function Synopsis(props: { text: string | null }) {
  const [open, setOpen] = useState(false);
  if (!props.text) return null;
  const long = props.text.length > 260;
  return (
    <div class="synopsis">
      <p class={cx("c-2", !open && "clamp-4")}>{cap(props.text, 2000)}</p>
      {long && (
        <Button variant="ghost" size="sm" class="synopsis-more" onClick={() => setOpen((o) => !o)} iconRight={open ? "caret-up" : "caret-down"}>
          {open ? "Less" : "More"}
        </Button>
      )}
    </div>
  );
}

function DetailSkeleton() {
  return (
    <div class="detail-skeleton" aria-hidden="true">
      <Skeleton class="sk-display" />
      <Skeleton class="sk-line-short" />
      <Skeleton class="sk-line" />
      <Skeleton class="sk-line" />
      <Skeleton class="sk-button" />
    </div>
  );
}

export function MovieDetailScreen(props: { memo: Memo; id: string }) {
  const a = account.value!;
  const d = useLoad<MovieDetail>(`${a.id}:movie:${props.id}`, () => api.movie(a.id, props.id), { ttlMs: 30_000, keepPrevious: true });
  useInitialFocus(!d.loading, props.memo);
  // Back from the player: fetch the new progress.
  const top = current.value.route.name;
  useReloadAfterPlayer(top === "watch", d.reload);

  const m = d.data;
  if (d.error && !m) {
    return (
      <DetailLayout backdrop={null} back={parentLabel("Movies")}>
        <ErrorState message={d.error} onRetry={d.reload} compact />
      </DetailLayout>
    );
  }
  if (!m) {
    return (
      <DetailLayout backdrop={null} back={parentLabel("Movies")}>
        <DetailSkeleton />
      </DetailLayout>
    );
  }
  const p = m.progress;
  const resumable = !!p && p.position >= RESUME_AFTER && p.position < p.duration * FINISHED;
  const fav = isFavourite("movie", m.id);
  const genres = cap(m.genre ?? "", 80).split(/\s*,\s*/).filter(Boolean).join(" · ");
  return (
    <DetailLayout backdrop={m.backdrop} back={parentLabel("Movies")}>
      <div class="detail-col">
        <h1 class="t-display clamp-2 detail-title">{cap(m.name, 200)}</h1>
        <p class="detail-meta t-caption c-2">
          {dot(m.year ?? undefined, runtime(m.duration_secs), m.rating ? `★ ${fmtRating(m.rating)}` : undefined, genres)}
        </p>
        <Synopsis text={m.plot} />
        {(m.cast || m.director) && (
          <p class="detail-cast t-caption c-3 clamp-2">
            {dot(m.cast ? `Cast: ${cap(m.cast, 300)}` : undefined, m.director ? `Directed by ${cap(m.director, 80)}` : undefined)}
          </p>
        )}
        {resumable && (
          <div class="detail-progress">
            <Bar value={fraction(0, p!.duration, p!.position)} label="Watched" />
            <span class="t-caption c-3 tnum">{timecode(p!.duration - p!.position)} left</span>
          </div>
        )}
        <div class="detail-actions" data-scope="detail-actions">
          <Button variant="primary" size="lg" icon="play" iconFill autofocus fk="detail:play" onClick={() => watchMovie(a.id, m, resumable ? p!.position : 0)}>
            {resumable ? `Resume ${timecode(p!.position)}` : "Play"}
          </Button>
          {resumable && (
            <Button variant="secondary" size="lg" icon="arrow-counter-clockwise" fk="detail:restart" onClick={() => watchMovie(a.id, m, 0)}>
              Play from start
            </Button>
          )}
          <IconButton
            icon="heart"
            fill={fav}
            active={fav}
            size="lg"
            label={fav ? "Remove from Favourites" : "Add to Favourites"}
            fk="detail:fav"
            onClick={() =>
              void toggleFavourite(a.id, { kind: "movie", item: { id: m.id, name: m.name, poster: m.poster, year: m.year, rating: m.rating, added: null } })
            }
          />
        </div>
      </div>
    </DetailLayout>
  );
}

/** Where "Continue" starts: the episode in progress, else the one after the last watched. */
export function continuePoint(s: SeriesDetail): { season: Season; episode: Episode; resume: number; label: string } | null {
  const all = s.seasons.flatMap((season) => season.episodes.map((episode) => ({ season, episode })));
  if (all.length === 0) return null;
  let lastIdx = -1;
  all.forEach((x, i) => {
    if (x.episode.progress && x.episode.progress.position > 0) lastIdx = i;
  });
  if (lastIdx < 0) {
    const f = all[0]!;
    return { ...f, resume: 0, label: `Play S${f.season.number} · E${f.episode.number}` };
  }
  const last = all[lastIdx]!;
  const pr = last.episode.progress!;
  if (pr.position < pr.duration * FINISHED) {
    return { ...last, resume: pr.position, label: `Continue S${last.season.number} · E${last.episode.number}` };
  }
  const n = all[lastIdx + 1];
  if (!n) return { ...last, resume: 0, label: `Watch S${last.season.number} · E${last.episode.number} again` };
  return { ...n, resume: 0, label: `Continue S${n.season.number} · E${n.episode.number}` };
}

export function SeriesDetailScreen(props: { memo: Memo; id: string }) {
  const a = account.value!;
  const d = useLoad<SeriesDetail>(`${a.id}:series:${props.id}`, () => api.seriesDetail(a.id, props.id), { ttlMs: 30_000, keepPrevious: true });
  const s = d.data;
  const [seasonNo, setSeasonNo] = useState<number | null>((props.memo["season"] as number | undefined) ?? null);
  useInitialFocus(!d.loading, props.memo);
  const top = current.value.route.name;
  useReloadAfterPlayer(top === "watch", d.reload);

  if (d.error && !s) {
    return (
      <DetailLayout backdrop={null} back={parentLabel("Series")}>
        <ErrorState message={d.error} onRetry={d.reload} compact />
      </DetailLayout>
    );
  }
  if (!s) {
    return (
      <DetailLayout backdrop={null} back={parentLabel("Series")}>
        <DetailSkeleton />
      </DetailLayout>
    );
  }
  const cp = continuePoint(s);
  const season = s.seasons.find((x) => x.number === (seasonNo ?? cp?.season.number)) ?? s.seasons[0];
  const fav = isFavourite("series", s.id);
  const genres = cap(s.genre ?? "", 80).split(/\s*,\s*/).filter(Boolean).join(" · ");
  const pickSeason = (n: number) => {
    setSeasonNo(n);
    props.memo["season"] = n;
  };
  return (
    <DetailLayout backdrop={s.backdrop} back={parentLabel("Series")}>
      <div class="detail-col">
        <h1 class="t-display clamp-2 detail-title">{cap(s.name, 200)}</h1>
        <p class="detail-meta t-caption c-2">
          {dot(
            s.year ?? undefined,
            s.seasons.length === 1 ? "1 season" : `${s.seasons.length} seasons`,
            s.rating ? `★ ${fmtRating(s.rating)}` : undefined,
            genres,
          )}
        </p>
        <Synopsis text={s.plot} />
        {s.cast && <p class="detail-cast t-caption c-3 clamp-2">Cast: {cap(s.cast, 300)}</p>}
        <div class="detail-actions" data-scope="detail-actions">
          {cp && (
            <Button
              variant="primary"
              size="lg"
              icon="play"
              iconFill
              autofocus
              fk="detail:play"
              onClick={() => watchEpisode(a.id, s, cp.episode.id, cp.resume)}
            >
              {cp.resume > 0 ? `${cp.label} · ${timecode(cp.resume)}` : cp.label}
            </Button>
          )}
          <IconButton
            icon="heart"
            fill={fav}
            active={fav}
            size="lg"
            label={fav ? "Remove from Favourites" : "Add to Favourites"}
            fk="detail:fav"
            onClick={() =>
              void toggleFavourite(a.id, { kind: "series", item: { id: s.id, name: s.name, poster: s.poster, year: s.year, rating: s.rating, updated: null } })
            }
          />
        </div>
      </div>
      {s.seasons.length > 0 && season && (
        <section class="episodes" aria-label="Episodes">
          <div class="season-tabs" role="tablist" aria-label="Seasons" data-scope="season-tabs">
            {s.seasons.map((x) => (
              <button
                type="button"
                role="tab"
                key={x.number}
                aria-selected={x.number === season.number ? "true" : "false"}
                class={cx("tab", x.number === season.number && "is-selected")}
                data-fk={`season:${x.number}`}
                onClick={() => pickSeason(x.number)}
              >
                {cap(x.name || `Season ${x.number}`, 40)}
              </button>
            ))}
            <span class="t-caption c-3 season-count">{season.episodes.length === 1 ? "1 episode" : `${season.episodes.length} episodes`}</span>
          </div>
          <div class="episode-list" data-scope={`episodes-${season.number}`}>
            {season.episodes.map((e) => {
              const pr = e.progress;
              const watched = !!pr && pr.position >= pr.duration * FINISHED;
              const resume = pr && !watched && pr.position >= RESUME_AFTER ? pr.position : 0;
              return (
                <button
                  type="button"
                  key={e.id}
                  class="episode"
                  data-fk={`ep:${e.id}`}
                  aria-label={`Episode ${e.number}: ${e.title}${watched ? ", watched" : resume ? `, resume at ${timecode(resume)}` : ""}`}
                  onClick={() => watchEpisode(a.id, s, e.id, resume)}
                >
                  <span class="episode-thumb">
                    <Img src={e.still} fallback="/img/backdrop.jpg" />
                    <span class="episode-play" aria-hidden="true">
                      <Icon name="play" fill />
                    </span>
                    {pr && <Bar value={fraction(0, pr.duration, pr.position)} class="card-progress" />}
                  </span>
                  <span class="episode-text">
                    <span class="t-body-strong ellipsis">
                      {e.number} · {cap(e.title, 120)}
                    </span>
                    <span class="t-caption c-3">
                      {dot(runtime(e.duration_secs), watched ? "Watched" : resume ? `${timecode(pr!.duration - pr!.position)} left` : undefined)}
                    </span>
                    {e.plot && <span class="c-2 clamp-2 episode-plot">{cap(e.plot, 400)}</span>}
                  </span>
                  {watched && <Icon name="check-circle" class="episode-watched" label="Watched" />}
                </button>
              );
            })}
          </div>
        </section>
      )}
    </DetailLayout>
  );
}
