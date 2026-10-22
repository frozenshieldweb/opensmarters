// SPDX-License-Identifier: GPL-3.0-or-later
// The app root: which world the visitor is in (first-run setup, invitation, sign-in, no
// provider yet, or the app itself), the screen router, the shared video layer and the layers
// above it.

import { useEffect } from "preact/hooks";
import { accountId, accounts, accountsError, bootError, loadUserData, refreshStatus, status, user } from "./state/session";
import { current, onLeave, stack, startRouter, type Entry } from "./state/router";
import { currentKey } from "./focus/dom";
import { Shell } from "./shell";
import { Layers, Toasts, layerOpen } from "./ui/overlay";
import { BrandSymbol, ErrorState } from "./ui/kit";
import { VideoLayer } from "./player/VideoLayer";
import { Player } from "./player/Player";
import { item as playing, stop as stopPlayback } from "./player/playback";
import { loadFavourites } from "./state/favourites";
import { AcceptInvite, Landing, Setup, inviteToken } from "./screens/auth";
import { AddProvider } from "./screens/providers";
import { Home } from "./screens/home";
import { Live } from "./screens/live";
import { Guide } from "./screens/guide";
import { Browse } from "./screens/browse";
import { MovieDetailScreen, SeriesDetailScreen } from "./screens/detail";
import { Search } from "./screens/search";
import { FavouritesScreen } from "./screens/favourites";
import { SettingsScreen } from "./screens/settings";

function Boot(props: { message?: string | null; onRetry?: () => void }) {
  return (
    <div class="boot">
      <BrandSymbol class="boot-symbol" />
      {props.message ? (
        <div class="boot-error">
          <ErrorState title="Can't reach the server" message={props.message} onRetry={props.onRetry} compact />
        </div>
      ) : (
        <p class="sr" role="status">
          Loading opensmarters
        </p>
      )}
    </div>
  );
}

function ScreenFor(props: { entry: Entry }) {
  const e = props.entry;
  const r = e.route;
  const memo = e.memo;
  switch (r.name) {
    case "home":
      return <Home key={e.key} memo={memo} />;
    case "live":
      return <Live key={e.key} memo={memo} category={r.category} />;
    case "guide":
      return <Guide key={e.key} memo={memo} category={r.category} />;
    case "movies":
    case "series":
      return <Browse key={e.key} memo={memo} kind={r.name} category={r.category} />;
    case "movie":
      return <MovieDetailScreen key={e.key} memo={memo} id={r.id} />;
    case "show":
      return <SeriesDetailScreen key={e.key} memo={memo} id={r.id} />;
    case "search":
      return <Search key={e.key} memo={memo} q={r.q} />;
    case "favourites":
      return <FavouritesScreen key={e.key} memo={memo} />;
    case "settings":
      return <SettingsScreen key={e.key} memo={memo} section={r.section} />;
    case "provider-new":
      return <AddProvider key={e.key} />;
    case "provider-edit":
      return <AddProvider key={e.key} editId={r.id} />;
    case "watch":
      return null;
  }
}

/** The signed-in app: router, shell, player. */
function SignedIn() {
  useEffect(() => {
    const stopRouter = startRouter();
    const off = onLeave((from) => {
      from.memo.focus = currentKey() ?? undefined;
    });
    return () => {
      stopRouter();
      off();
      stopPlayback();
    };
  }, []);

  const aid = accountId.value;
  useEffect(() => {
    if (aid) void loadFavourites(aid);
  }, [aid]);

  const watching = current.value.route.name === "watch";
  // While the player is open, the screen that opened it stays mounted underneath.
  const s = stack.value;
  const shown = watching ? s[s.length - 2] : s[s.length - 1];
  return (
    <>
      <Shell inert={watching || layerOpen.value}>{shown && <ScreenFor entry={shown} />}</Shell>
      <VideoLayer />
      {watching && <Player inert={layerOpen.value} />}
    </>
  );
}

export function App() {
  useEffect(() => {
    void refreshStatus();
  }, []);

  const u = user.value;
  useEffect(() => {
    if (u) void loadUserData();
    else if (playing.value) stopPlayback();
  }, [u?.id]);

  const s = status.value;
  let body;
  if (!s) body = <Boot message={bootError.value} onRetry={() => void refreshStatus()} />;
  else if (s.setup_required) body = <Setup />;
  else if (!u) body = inviteToken() ? <AcceptInvite /> : <Landing />;
  else if (accounts.value === null)
    body = <Boot message={accountsError.value} onRetry={() => void loadUserData()} />;
  else if (accounts.value.length === 0) body = <AddProvider firstRun />;
  else body = <SignedIn />;

  return (
    <>
      {body}
      <Layers />
      <Toasts />
    </>
  );
}
