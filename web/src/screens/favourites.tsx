// SPDX-License-Identifier: GPL-3.0-or-later
// Favourites: the channels, films and series saved with ♥ for the current provider.

import { useEffect } from "preact/hooks";
import { account } from "../state/session";
import { favourites, loadFavourites } from "../state/favourites";
import { go, type Memo } from "../state/router";
import { plural } from "../lib/format";
import { Button, Empty, Skeleton } from "../ui/kit";
import { PosterCard } from "../ui/media";
import { useInitialFocus } from "../ui/hooks";
import { watchChannel } from "../player/start";
import { metaOf, Rail } from "./home";
import { ChannelTile } from "./search";

export function FavouritesScreen(props: { memo: Memo }) {
  const a = account.value!;
  useEffect(() => {
    void loadFavourites(a.id, true);
  }, [a.id]);
  const f = favourites.value;
  const total = f ? f.channels.length + f.movies.length + f.series.length : 0;
  useInitialFocus(!!f, props.memo);
  return (
    <div class="screen favourites">
      <header class="screen-head screen-pad-x">
        <h1 class="t-heading">Favourites</h1>
        {f && total > 0 && <span class="t-caption c-3 tnum">{plural(total, "item")}</span>}
      </header>
      {!f && (
        <div class="screen-pad-x" aria-hidden="true">
          <Skeleton class="sk-title" />
        </div>
      )}
      {f && total === 0 && (
        <Empty
          art="empty-favourites"
          title="Nothing saved yet"
          body="Press ♥ on any channel, film or series to keep it here."
          action={
            <Button variant="primary" icon="television-simple" onClick={() => go({ name: "live" })} autofocus>
              Browse Live TV
            </Button>
          }
        />
      )}
      {f && f.channels.length > 0 && (
        <Rail title="Channels" kicker={`${f.channels.length}`} scope="fav-channels" onSeeAll={() => go({ name: "live", category: "favourites" })}>
          {f.channels.map((c) => (
            <ChannelTile key={c.key} c={c} onClick={() => watchChannel(a.id, c, f.channels)} />
          ))}
        </Rail>
      )}
      {f && f.movies.length > 0 && (
        <Rail title="Films" kicker={`${f.movies.length}`} scope="fav-movies">
          {f.movies.map((m) => (
            <PosterCard key={m.id} fk={`movie:${m.id}`} title={m.name} meta={metaOf(m)} image={m.poster} onClick={() => go({ name: "movie", id: m.id })} />
          ))}
        </Rail>
      )}
      {f && f.series.length > 0 && (
        <Rail title="Series" kicker={`${f.series.length}`} scope="fav-series">
          {f.series.map((s) => (
            <PosterCard key={s.id} fk={`series:${s.id}`} title={s.name} meta={metaOf(s)} image={s.poster} onClick={() => go({ name: "show", id: s.id })} />
          ))}
        </Rail>
      )}
    </div>
  );
}
