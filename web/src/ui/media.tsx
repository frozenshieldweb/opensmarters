// SPDX-License-Identifier: GPL-3.0-or-later
// Artwork: channel logos on plates or monogram tiles (§6.4), posters and backdrop cards (§6.3),
// images that fall back to the placeholder art when the provider has none or it fails.

import type { ComponentChildren, JSX } from "preact";
import { useState } from "preact/hooks";
import type { Channel } from "../api/types";
import { cap, hue, monogram } from "../lib/text";
import { Bar, cx, Icon } from "./kit";

/** An image from the server (`/api/img/…`); `fallback` when there's none or it fails. */
export function Img(props: {
  src: string | null | undefined;
  fallback?: string;
  alt?: string;
  class?: string;
  eager?: boolean;
  onFail?: () => void;
}) {
  const [failed, setFailed] = useState<string | null>(null);
  const src = props.src && failed !== props.src ? props.src : props.fallback;
  if (!src) return null;
  return (
    <img
      class={props.class}
      src={src}
      alt={props.alt ?? ""}
      loading={props.eager ? "eager" : "lazy"}
      decoding="async"
      draggable={false}
      onError={() => {
        if (props.src && src === props.src) {
          setFailed(props.src);
          props.onFail?.();
        }
      }}
    />
  );
}

/** A channel logo on a plate, or a monogram tile in one of six muted hues. */
export function ChannelLogo(props: { channel: Pick<Channel, "name" | "logo">; size?: "sm" | "md" | "lg"; class?: string }) {
  const [failed, setFailed] = useState(false);
  const { name, logo } = props.channel;
  const cls = cx("logo", `logo-${props.size ?? "md"}`, props.class);
  if (logo && !failed) {
    return (
      <span class={cx(cls, "logo-plate")}>
        <img src={logo} alt="" loading="lazy" decoding="async" draggable={false} onError={() => setFailed(true)} />
      </span>
    );
  }
  const m = monogram(name);
  if (!m) {
    return (
      <span class={cx(cls, "logo-plate")}>
        <img src="/img/channel-logo.png" alt="" draggable={false} />
      </span>
    );
  }
  return (
    <span class={cx(cls, "logo-mono", `hue-${hue(name)}`)} aria-hidden="true">
      {m}
    </span>
  );
}

interface CardBase extends Omit<JSX.ButtonHTMLAttributes<HTMLButtonElement>, "title" | "type"> {
  fk?: string;
  autofocus?: boolean;
}

/** A 2:3 poster card: image, title (1 line) and meta caption. */
export function PosterCard({
  title,
  meta,
  image,
  progress,
  fk,
  autofocus,
  class: klass,
  badge,
  ...rest
}: CardBase & {
  title: string;
  meta?: string;
  image: string | null;
  progress?: number;
  badge?: ComponentChildren;
}) {
  const [failed, setFailed] = useState(false);
  const showTitle = !image || failed;
  return (
    <button
      type="button"
      {...rest}
      class={cx("card poster", klass as string)}
      data-fk={fk}
      data-autofocus={autofocus ? "" : undefined}
      aria-label={meta ? `${title}, ${meta}` : title}
    >
      <span class="card-art poster-art">
        <Img src={image} fallback="/img/poster.jpg" onFail={() => setFailed(true)} />
        {showTitle && <span class="poster-fallback-title">{cap(title, 80)}</span>}
        {badge}
        {progress !== undefined && progress > 0 && <Bar value={progress} class="card-progress" />}
      </span>
      <span class="card-title ellipsis">{cap(title, 120)}</span>
      {meta && <span class="card-meta t-caption c-3 ellipsis">{meta}</span>}
    </button>
  );
}

/** A 16:9 card with the title over a bottom scrim and an optional progress bar. */
export function BackdropCard({
  title,
  meta,
  image,
  progress,
  fk,
  autofocus,
  class: klass,
  ...rest
}: CardBase & { title: string; meta?: string; image: string | null; progress?: number }) {
  return (
    <button
      type="button"
      {...rest}
      class={cx("card backdrop-card", klass as string)}
      data-fk={fk}
      data-autofocus={autofocus ? "" : undefined}
      aria-label={meta ? `${title}, ${meta}` : title}
    >
      <span class="card-art backdrop-art">
        <Img src={image} fallback="/img/backdrop.jpg" />
        <span class="backdrop-text">
          <span class="t-body-strong ellipsis">{cap(title, 120)}</span>
          {meta && <span class="t-caption c-2 ellipsis">{meta}</span>}
        </span>
        {progress !== undefined && progress > 0 && <Bar value={progress} class="card-progress" />}
      </span>
    </button>
  );
}

/** The LIVE badge (live pill, overline). */
export function LiveBadge() {
  return (
    <span class="live-badge t-overline">
      <span class="sr">Live</span>
      <span aria-hidden="true">Live</span>
    </span>
  );
}

/** The small favourite heart shown on rows. */
export function FavHeart() {
  return <Icon name="heart" fill class="fav-heart" label="Favourite" />;
}
