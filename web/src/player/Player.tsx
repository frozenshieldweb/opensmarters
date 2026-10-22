// SPDX-License-Identifier: GPL-3.0-or-later
// The player (DESIGN.md §6.10, §8.5): the shared video full-window with the OSD: title and
// clock on top; the scrubber (films, episodes) or the LIVE line; play/pause, ±10 s, channel
// up/down, audio and subtitle tracks, mute, fullscreen. It hides after 3 s; any key or pointer
// move shows it. Esc first hides the OSD, then leaves. Plain error messages; live streams
// reconnect by themselves.

import { useCallback, useEffect, useRef, useState } from "preact/hooks";
import { settings } from "../state/session";
import { epgVersion, nowNext, requestNowNext } from "../state/epg";
import { now, toast } from "../state/misc";
import { back, current } from "../state/router";
import { clock, fraction, timeRange, timecode } from "../lib/format";
import { cap } from "../lib/text";
import { Bar, Button, Icon, IconButton, Spinner, cx } from "../ui/kit";
import { LiveBadge } from "../ui/media";
import { useBack, useKeys } from "../ui/hooks";
import { focusEl, focusFallback, focusKey, isTextInput } from "../focus/dom";
import * as pb from "./playback";

const HIDE_AFTER = 3000;
const NUMBER_TIMEOUT = 1500;
const UP_NEXT_SECS = 10;

function isFullscreen(): boolean {
  return !!document.fullscreenElement;
}

function toggleFullscreen(): void {
  if (document.fullscreenElement) {
    void document.exitFullscreen().catch(() => {});
    return;
  }
  const root = document.documentElement;
  if (root.requestFullscreen) {
    void root.requestFullscreen({ navigationUI: "hide" }).catch(() => {});
    return;
  }
  const v = document.querySelector<HTMLVideoElement & { webkitEnterFullscreen?: () => void }>(".video-layer video");
  v?.webkitEnterFullscreen?.();
}

export function Player(props: { inert: boolean }) {
  const it = pb.item.value;
  const phase = pb.phase.value;
  const err = pb.error.value;
  const live = it?.kind === "live";
  const [osd, setOsd] = useState(true);
  const [sheet, setSheet] = useState(false);
  const [number, setNumber] = useState("");
  const [fs, setFs] = useState(isFullscreen());
  const [seekFlash, setSeekFlash] = useState<string | null>(null);
  const [upNext, setUpNext] = useState<number | null>(null);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const numberTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const enteredFs = useRef(false);

  const playing = phase === "playing";
  const pinned = !playing || sheet || !!err; // the OSD stays while paused, buffering or on errors

  const poke = useCallback(() => {
    setOsd(true);
    if (hideTimer.current) clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => setOsd(false), HIDE_AFTER);
  }, []);

  useEffect(() => {
    poke();
    const onFs = () => setFs(isFullscreen());
    document.addEventListener("fullscreenchange", onFs);
    return () => {
      document.removeEventListener("fullscreenchange", onFs);
      if (hideTimer.current) clearTimeout(hideTimer.current);
      if (numberTimer.current) clearTimeout(numberTimer.current);
      if (flashTimer.current) clearTimeout(flashTimer.current);
      // Leaving the player: Live TV keeps the channel in its preview; anything else stops.
      const toLive = current.value.route.name === "live" && pb.item.value?.kind === "live";
      if (!toLive) pb.stop();
      if (enteredFs.current && document.fullscreenElement) void document.exitFullscreen().catch(() => {});
      // Back on the screen below: the focus returns to what opened the player.
      const below = current.value;
      requestAnimationFrame(() => {
        const content = document.querySelector('[data-group="content"]') ?? undefined;
        if (below.memo.focus && focusKey(below.memo.focus, content)) return;
        if (content) focusFallback(content);
      });
    };
  }, []);

  const visible = osd || pinned;

  // Subtitles move up while the OSD is shown, so the controls never cover them.
  useEffect(() => {
    pb.osdShown.value = visible;
    return () => {
      pb.osdShown.value = false;
    };
  }, [visible]);

  // When the OSD appears without focus inside it, put the focus on play/pause.
  useEffect(() => {
    if (!visible) {
      (document.activeElement as HTMLElement | null)?.blur?.();
      rootRef.current?.focus({ preventScroll: true });
      return;
    }
    const root = rootRef.current;
    if (!root) return;
    const active = document.activeElement;
    if (!root.contains(active) || active === root || (err && !active?.closest("[data-scope='osd-error']"))) {
      const target =
        root.querySelector<HTMLElement>(".osd-middle [data-autofocus]") ?? root.querySelector<HTMLElement>("[data-fk='osd:play']");
      if (target) focusEl(target, { scroll: false });
      else focusFallback(root);
    }
  }, [visible, !!err, phase === "ended", upNext === null]);

  // Up next: when an episode ends and there's another one.
  const next = pb.nextEpisode.value;
  useEffect(() => {
    if (phase === "ended" && next && settings.value.autoplay_next) setUpNext(UP_NEXT_SECS);
    else setUpNext(null);
  }, [phase, next?.id]);
  useEffect(() => {
    if (upNext === null) return;
    if (upNext <= 0) {
      setUpNext(null);
      pb.playNext();
      return;
    }
    const t = setTimeout(() => setUpNext((n) => (n === null ? null : n - 1)), 1000);
    return () => clearTimeout(t);
  }, [upNext]);

  const flash = (text: string) => {
    setSeekFlash(text);
    if (flashTimer.current) clearTimeout(flashTimer.current);
    flashTimer.current = setTimeout(() => setSeekFlash(null), 900);
  };

  const seek = (d: number) => {
    pb.seekBy(d);
    flash(d > 0 ? `+${Math.abs(d)} s` : `−${Math.abs(d)} s`);
  };

  const zap = (step: number) => {
    const ch = pb.zap(step);
    if (ch) poke();
  };

  const commitNumber = (digits: string) => {
    setNumber("");
    const n = Number(digits);
    if (!digits) return;
    const ch = pb.zapNumber(n);
    if (!ch) toast(`There's no channel ${n}`, "info");
    else poke();
  };

  useBack(() => {
    if (number) {
      setNumber("");
      return true;
    }
    if (sheet) {
      setSheet(false);
      return true;
    }
    if (upNext !== null) {
      setUpNext(null);
      return true;
    }
    if (osd && playing) {
      if (hideTimer.current) clearTimeout(hideTimer.current);
      setOsd(false);
      return true;
    }
    back();
    return true;
  });

  useKeys((e) => {
    if (isTextInput(document.activeElement)) return false;
    const k = e.key;
    const shown = visible;
    const onScrubber = (document.activeElement as HTMLElement | null)?.dataset["fk"] === "osd:scrubber";
    if (k === "Escape" || k === "Backspace") return false;
    // Channel keys: TV-remote convention (PageUp / Ctrl+↑ = channel up = next channel).
    if (live && ((k === "PageUp" && !e.ctrlKey) || (e.ctrlKey && k === "ArrowUp"))) return zap(1), true;
    if (live && ((k === "PageDown" && !e.ctrlKey) || (e.ctrlKey && k === "ArrowDown"))) return zap(-1), true;
    if (e.ctrlKey) return false;
    if (live && /^[0-9]$/.test(k)) {
      const n = (number + k).slice(0, 5);
      setNumber(n);
      if (numberTimer.current) clearTimeout(numberTimer.current);
      numberTimer.current = setTimeout(() => commitNumber(n), NUMBER_TIMEOUT);
      return true;
    }
    if (number && k === "Enter") {
      if (numberTimer.current) clearTimeout(numberTimer.current);
      commitNumber(number);
      return true;
    }
    if (k === " " && (!shown || document.activeElement === rootRef.current || onScrubber)) {
      pb.togglePause();
      poke();
      return true;
    }
    if (k === "m" || k === "M") {
      toast(pb.toggleMute() ? "Muted" : "Sound on");
      poke();
      return true;
    }
    if (k === "f" || k === "F") {
      enteredFs.current = !isFullscreen();
      toggleFullscreen();
      return true;
    }
    if (k === "i" || k === "I") {
      if (osd && !pinned) setOsd(false);
      else poke();
      return true;
    }
    if (!live && (k === "a" || k === "A") && pb.audioTracks.value.length > 1) {
      const list = pb.audioTracks.value;
      const i = list.findIndex((t) => t.id === pb.audioTrack.value);
      const nextT = list[(i + 1) % list.length]!;
      pb.setAudioTrack(nextT.id);
      toast(`Audio: ${nextT.label}`);
      return true;
    }
    if ((k === "s" || k === "S") && pb.subtitleTracks.value.length > 0) {
      const list = pb.subtitleTracks.value;
      const i = list.findIndex((t) => t.id === pb.subtitleTrack.value);
      const nextT = i + 1 < list.length ? list[i + 1]! : null;
      pb.setSubtitleTrack(nextT ? nextT.id : -1);
      toast(nextT ? `Subtitles: ${nextT.label}` : "Subtitles off");
      return true;
    }
    if (!shown) {
      // Direct transport with the OSD hidden.
      if (k === "Enter") {
        if (!live) pb.togglePause();
        poke();
        return true;
      }
      if (k === "ArrowLeft" || k === "ArrowRight") {
        const d = k === "ArrowLeft" ? -1 : 1;
        if (live) zap(d);
        else {
          seek(d * (e.shiftKey ? 60 : 10));
          poke();
          requestAnimationFrame(() => {
            const s = rootRef.current?.querySelector<HTMLElement>("[data-fk='osd:scrubber']");
            if (s) focusEl(s, { scroll: false });
          });
        }
        return true;
      }
      if (k === "ArrowUp" || k === "ArrowDown") {
        if (live) zap(k === "ArrowUp" ? 1 : -1);
        else poke();
        return true;
      }
      poke();
      return false;
    }
    if (onScrubber && !live && (k === "ArrowLeft" || k === "ArrowRight")) {
      seek((k === "ArrowLeft" ? -1 : 1) * (e.shiftKey ? 60 : 10));
      poke();
      return true;
    }
    poke();
    return false;
  });

  if (!it) {
    return (
      <div class="player is-idle" data-modal="" inert={props.inert}>
        <div class="player-idle">
          <p class="c-2">Nothing is playing.</p>
          <Button variant="secondary" icon="caret-left" onClick={() => back()} autofocus>
            Back
          </Button>
        </div>
      </div>
    );
  }

  const title = it.kind === "live" ? cap(it.channel.name, 120) : it.title;
  const subtitle = it.kind === "live" ? (it.channel.number !== null ? `Channel ${it.channel.number}` : "Live") : it.subtitle;
  const ended = phase === "ended";

  return (
    <div
      ref={rootRef}
      class={cx("player", visible ? "osd-on" : "osd-off", sheet && "has-sheet")}
      data-modal=""
      data-group="player"
      tabIndex={-1}
      inert={props.inert}
      onPointerMove={(e) => (e.movementX || e.movementY) && poke()}
      onPointerDown={(e) => {
        if (e.target === e.currentTarget || (e.target as HTMLElement).classList.contains("osd-middle")) {
          if (!visible) poke();
          else if (!err && !ended) {
            pb.togglePause();
            poke();
          }
        }
      }}
    >
      <div class="osd-top" data-scope="osd-top">
        <IconButton icon="caret-left" label="Back" size="osd" onClick={() => back()} fk="osd:back" />
        <div class="osd-titles">
          <h1 class="t-title ellipsis">{title}</h1>
          <p class="t-caption c-2 ellipsis">{subtitle}</p>
        </div>
        <span class="osd-clock t-title-sm tnum">{clock(now.value, settings.value.time_format)}</span>
      </div>

      <div class="osd-middle">
        {!err && (phase === "starting" || phase === "buffering") && (
          <div class="osd-status">
            <Spinner />
            <span class="t-caption">{pb.reconnecting.value ? `Reconnecting… (${pb.reconnecting.value})` : "Buffering…"}</span>
          </div>
        )}
        {seekFlash && <div class="osd-flash t-title tnum">{seekFlash}</div>}
        {err && (
          <div class="osd-panel" role="alert" data-scope="osd-error">
            <Icon name="warning-circle" class="osd-panel-icon" />
            <h2 class="t-title">{err.title}</h2>
            <p class="c-2">{err.message}</p>
            <div class="osd-panel-actions">
              <Button variant="secondary" icon="caret-left" onClick={() => back()}>
                Back
              </Button>
              {err.retry && (
                <Button variant="primary" icon="arrows-clockwise" onClick={() => pb.retry()} autofocus fk="osd:retry">
                  Try again
                </Button>
              )}
            </div>
          </div>
        )}
        {ended && !err && (
          <div class="osd-panel" data-scope="osd-ended">
            {next && upNext !== null ? (
              <>
                <p class="t-overline c-3">Up next in {upNext} s</p>
                <h2 class="t-title">{next.subtitle}</h2>
                <div class="osd-panel-actions">
                  <Button variant="secondary" onClick={() => setUpNext(null)}>
                    Cancel
                  </Button>
                  <Button variant="primary" icon="play" iconFill onClick={() => pb.playNext()} autofocus fk="osd:next">
                    Play now
                  </Button>
                </div>
              </>
            ) : (
              <>
                <h2 class="t-title">Finished</h2>
                <div class="osd-panel-actions">
                  <Button variant="secondary" icon="caret-left" onClick={() => back()}>
                    Back
                  </Button>
                  <Button variant="secondary" icon="arrow-counter-clockwise" onClick={() => pb.seekTo(0)}>
                    Replay
                  </Button>
                  {next && (
                    <Button variant="primary" icon="skip-forward" onClick={() => pb.playNext()} autofocus fk="osd:next">
                      Next episode
                    </Button>
                  )}
                </div>
              </>
            )}
          </div>
        )}
      </div>

      <div class="osd-bottom">
        {live ? <LiveLine /> : <Scrubber onSeek={(abs) => pb.seekTo(abs)} />}
        <div class="osd-controls">
          <div class="osd-left" data-scope="osd-left">
            {live && (
              <>
                <IconButton icon="caret-up" label="Next channel (PageUp)" size="osd" onClick={() => zap(1)} fk="osd:ch-up" />
                <IconButton icon="caret-down" label="Previous channel (PageDown)" size="osd" onClick={() => zap(-1)} fk="osd:ch-down" />
              </>
            )}
          </div>
          <div class="osd-center" data-scope="osd-center">
            {!live && (
              <button type="button" class="osd-seek" aria-label="Back 10 seconds" title="Back 10 seconds" onClick={() => seek(-10)} data-fk="osd:back10">
                <Icon name="arrow-counter-clockwise" />
                <span class="osd-seek-n tnum">10</span>
              </button>
            )}
            <button
              type="button"
              class="osd-play"
              aria-label={playing || phase === "buffering" ? "Pause" : "Play"}
              title={playing ? "Pause (Space)" : "Play (Space)"}
              data-fk="osd:play"
              onClick={() => {
                pb.togglePause();
                poke();
              }}
            >
              <Icon name={playing || phase === "buffering" ? "pause" : "play"} fill />
            </button>
            {!live && (
              <button type="button" class="osd-seek" aria-label="Forward 10 seconds" title="Forward 10 seconds" onClick={() => seek(10)} data-fk="osd:fwd10">
                <Icon name="arrow-clockwise" />
                <span class="osd-seek-n tnum">10</span>
              </button>
            )}
          </div>
          <div class="osd-right" data-scope="osd-right">
            {next && <IconButton icon="skip-forward" label="Next episode" size="osd" onClick={() => pb.playNext()} fk="osd:skip" />}
            {(pb.audioTracks.value.length > 1 || pb.subtitleTracks.value.length > 0) && (
              <IconButton icon="subtitles" label="Audio and subtitles" size="osd" onClick={() => setSheet(true)} fk="osd:tracks" active={sheet} />
            )}
            <IconButton
              icon={pb.muted.value ? "speaker-slash" : "speaker-high"}
              label={pb.muted.value ? "Sound on (M)" : "Mute (M)"}
              size="osd"
              onClick={() => toast(pb.toggleMute() ? "Muted" : "Sound on")}
              fk="osd:mute"
            />
            <IconButton
              icon={fs ? "arrows-in" : "arrows-out"}
              label={fs ? "Exit full screen (F)" : "Full screen (F)"}
              size="osd"
              onClick={() => {
                enteredFs.current = !isFullscreen();
                toggleFullscreen();
              }}
              fk="osd:fs"
            />
          </div>
        </div>
      </div>

      {sheet && <TrackSheet onClose={() => setSheet(false)} />}
      {number && (
        <div class="number-entry" role="status" aria-live="assertive">
          <span class="sr">Channel </span>
          <span class="number-digits tnum">{number}</span>
        </div>
      )}
    </div>
  );
}

function Scrubber(props: { onSeek: (abs: number) => void }) {
  const pos = pb.position.value;
  const dur = pb.duration.value ?? 0;
  const avail = pb.availableEnd.value;
  const ref = useRef<HTMLDivElement>(null);
  const seekAt = (clientX: number) => {
    const el = ref.current;
    if (!el || !dur) return;
    const r = el.getBoundingClientRect();
    props.onSeek(((clientX - r.left) / r.width) * dur);
  };
  return (
    <div class="scrubber-row">
      <span class="t-body tnum scrub-time">{timecode(pos)}</span>
      <div
        ref={ref}
        class="scrubber"
        role="slider"
        tabIndex={0}
        data-f=""
        data-fk="osd:scrubber"
        aria-label="Position"
        aria-valuemin={0}
        aria-valuemax={Math.round(dur)}
        aria-valuenow={Math.round(pos)}
        aria-valuetext={`${timecode(pos)} of ${timecode(dur)}`}
        onPointerDown={(e) => seekAt(e.clientX)}
      >
        <span class="scrub-track">
          <span class="scrub-avail" style={{ width: `${dur ? fraction(0, dur, avail) * 100 : 0}%` }} />
          <span class="scrub-fill" style={{ width: `${dur ? fraction(0, dur, pos) * 100 : 0}%` }} />
          <span class="scrub-thumb" style={{ left: `${dur ? fraction(0, dur, pos) * 100 : 0}%` }} />
        </span>
      </div>
      <span class="t-body tnum scrub-time scrub-remaining">−{timecode(Math.max(0, dur - pos))}</span>
    </div>
  );
}

function LiveLine() {
  const it = pb.item.value;
  const key = it?.kind === "live" ? it.channel.key : null;
  const acc = it?.account ?? null;
  useEffect(() => {
    if (key && acc) requestNowNext(acc, [key]);
  }, [key, acc]);
  void epgVersion.value;
  const t = now.value;
  if (!it || it.kind !== "live") return null;
  const c = it.channel;
  const nn = nowNext(it.account, c.key);
  const p = nn?.[0];
  const next = nn?.[1];
  const fmt = settings.value.time_format;
  return (
    <div class="live-line">
      <LiveBadge />
      <span class="t-body-strong ellipsis live-line-name">
        {c.number !== null ? `${c.number} · ` : ""}
        {cap(c.name, 80)}
      </span>
      {p && (
        <span class="live-line-now">
          <span class="t-body ellipsis">
            {cap(p.title, 100)} <span class="c-3 t-caption tnum">{timeRange(p.start, p.end, fmt)}</span>
          </span>
          <Bar value={fraction(p.start, p.end, t)} class="live-line-bar" />
        </span>
      )}
      {next && (
        <span class="t-caption c-2 ellipsis live-line-next">
          Next: {clock(next.start, fmt)} {cap(next.title, 80)}
        </span>
      )}
    </div>
  );
}

function TrackSheet(props: { onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const first = ref.current?.querySelector<HTMLElement>(".sheet-row.is-selected") ?? ref.current?.querySelector<HTMLElement>(".sheet-row");
    if (first) focusEl(first, { scroll: false });
  }, []);
  const audio = pb.audioTracks.value;
  const subs = pb.subtitleTracks.value;
  return (
    <aside class="sheet" ref={ref} aria-label="Playback options" data-scope="sheet">
      <div class="sheet-head">
        <h2 class="t-title">Playback options</h2>
        <IconButton icon="x" label="Close" onClick={props.onClose} />
      </div>
      {audio.length > 1 && (
        <section>
          <h3 class="t-overline c-3 sheet-section">Audio</h3>
          {audio.map((t) => (
            <SheetRow key={t.id} label={t.label} detail={t.detail} selected={pb.audioTrack.value === t.id} onClick={() => pb.setAudioTrack(t.id)} />
          ))}
        </section>
      )}
      {subs.length > 0 && (
        <section>
          <h3 class="t-overline c-3 sheet-section">Subtitles</h3>
          <SheetRow label="Off" selected={pb.subtitleTrack.value < 0} onClick={() => pb.setSubtitleTrack(-1)} />
          {subs.map((t) => (
            <SheetRow key={t.id} label={t.label} detail={t.detail} selected={pb.subtitleTrack.value === t.id} onClick={() => pb.setSubtitleTrack(t.id)} />
          ))}
        </section>
      )}
    </aside>
  );
}

function SheetRow(props: { label: string; detail?: string; selected: boolean; onClick: () => void }) {
  return (
    <button type="button" class={cx("sheet-row", props.selected && "is-selected")} role="menuitemradio" aria-checked={props.selected ? "true" : "false"} onClick={props.onClick}>
      <span class="ellipsis">{cap(props.label, 60)}</span>
      {props.detail && props.detail !== props.label && <span class="t-caption c-3">{cap(props.detail, 20)}</span>}
      {props.selected && <Icon name="check" class="sheet-check" />}
    </button>
  );
}
