// SPDX-License-Identifier: GPL-3.0-or-later
// Search (DESIGN.md §6.7) across channels, films and series: results as you type (200 ms
// debounce, 2 characters at least), in three rails.

import { useEffect, useRef, useState } from "preact/hooks";
import { api } from "../api/client";
import type { Channel, SearchResults } from "../api/types";
import { account } from "../state/session";
import { cached, errorText } from "../state/cache";
import { go, setParams, type Memo } from "../state/router";
import { cap } from "../lib/text";
import { Button, Empty, Icon, Spinner } from "../ui/kit";
import { ChannelLogo, PosterCard } from "../ui/media";
import { useInitialFocus } from "../ui/hooks";
import { watchChannel } from "../player/start";
import { metaOf, Rail } from "./home";

export function Search(props: { memo: Memo; q?: string }) {
  const a = account.value!;
  const [q, setQ] = useState(props.q ?? "");
  const [res, setRes] = useState<{ q: string; data: SearchResults } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  useInitialFocus(true, props.memo);

  useEffect(() => {
    const text = q.trim();
    if ([...text].length < 2) {
      setRes(null);
      setBusy(false);
      setError(null);
      return;
    }
    setBusy(true);
    const t = setTimeout(() => {
      setParams({ name: "search", q: text });
      cached(`${a.id}:search:${text.toLowerCase()}`, () => api.search(a.id, text), 60_000).then(
        (data) => {
          setRes({ q: text, data });
          setError(null);
          setBusy(false);
        },
        (e: unknown) => {
          setError(errorText(e));
          setBusy(false);
        },
      );
    }, 200);
    return () => clearTimeout(t);
  }, [q, a.id]);

  const data = res?.data;
  const total = data ? data.channels.length + data.movies.length + data.series.length : 0;
  const clear = () => {
    setQ("");
    setParams({ name: "search" });
    input.current?.focus();
  };

  return (
    <div class="screen search screen-pad">
      <header class="search-head">
        <h1 class="t-heading">Search</h1>
        <label class="search-field" data-scope="search-field">
          <Icon name="magnifying-glass" class="search-icon" />
          <input
            ref={input}
            type="search"
            class="search-input"
            value={q}
            placeholder="Channels, films and series"
            aria-label="Search channels, films and series"
            autocomplete="off"
            spellcheck={false}
            maxLength={100}
            data-autofocus=""
            data-fk="search:input"
            onInput={(e) => setQ((e.currentTarget as HTMLInputElement).value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                // Enter moves into the results.
                const first = document.querySelector<HTMLElement>(".search-results button");
                first?.focus();
              }
            }}
          />
          {busy && <Spinner small />}
          {q && !busy && (
            <button type="button" class="search-clear" aria-label="Clear search" title="Clear search" onClick={clear}>
              <Icon name="x" />
            </button>
          )}
        </label>
      </header>
      <div class="search-results">
        {error && <Empty art="error-offline" title="Search didn't work" body={error} compact />}
        {!error && !data && !busy && (
          <p class="search-hint c-3">{[...q.trim()].length === 1 ? "Type one more letter." : "Type at least 2 letters to search channels, films and series."}</p>
        )}
        {!error && data && total === 0 && (
          <Empty
            art="empty-search"
            title="No matches"
            body={`Nothing called “${cap(res!.q, 60)}”. Try a shorter word, or check the spelling.`}
            action={
              <Button variant="secondary" icon="x" onClick={clear}>
                Clear search
              </Button>
            }
          />
        )}
        {data && data.channels.length > 0 && (
          <Rail title="Channels" kicker={`${data.channels.length}`} scope="search-channels">
            {data.channels.map((c) => (
              <ChannelTile key={c.key} c={c} onClick={() => watchChannel(a.id, c, data.channels)} />
            ))}
          </Rail>
        )}
        {data && data.movies.length > 0 && (
          <Rail title="Films" kicker={`${data.movies.length}`} scope="search-movies">
            {data.movies.map((m) => (
              <PosterCard key={m.id} fk={`movie:${m.id}`} title={m.name} meta={metaOf(m)} image={m.poster} onClick={() => go({ name: "movie", id: m.id })} />
            ))}
          </Rail>
        )}
        {data && data.series.length > 0 && (
          <Rail title="Series" kicker={`${data.series.length}`} scope="search-series">
            {data.series.map((s) => (
              <PosterCard key={s.id} fk={`series:${s.id}`} title={s.name} meta={metaOf(s)} image={s.poster} onClick={() => go({ name: "show", id: s.id })} />
            ))}
          </Rail>
        )}
      </div>
    </div>
  );
}

export function ChannelTile(props: { c: Channel; onClick: () => void; autofocus?: boolean }) {
  const { c } = props;
  return (
    <button
      type="button"
      class="card tile tile-plain"
      data-fk={`tile:${c.key}`}
      data-autofocus={props.autofocus ? "" : undefined}
      aria-label={c.name}
      onClick={props.onClick}
    >
      <ChannelLogo channel={c} size="lg" />
      <span class="tile-text">
        <span class="t-body-strong ellipsis">{cap(c.name, 80)}</span>
        <span class="t-caption c-3 tnum">{c.number !== null ? `Channel ${c.number}` : "Live"}</span>
      </span>
    </button>
  );
}
