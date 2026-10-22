// SPDX-License-Identifier: GPL-3.0-or-later
// The shell (DESIGN.md §6.1, §8): the side rail (expanded ≥ 1440 px, collapsed 72 px below
// and expanding over the content when it has the focus, a top bar with a menu below 960 px),
// the status cluster (clock), and the current screen. Only panels scroll, never the shell.

import type { ComponentChildren } from "preact";
import { account, accounts, selectAccount, settings } from "./state/session";
import { current, goSection, type Route, type RouteName, sectionOf, go } from "./state/router";
import { now } from "./state/misc";
import { clock } from "./lib/format";
import { cap, hue, monogram } from "./lib/text";
import { Icon, IconButton, Lockup, cx } from "./ui/kit";
import type { IconName } from "./ui/icons";
import { breakpoint } from "./ui/hooks";
import { openLayer, openMenu, type CloseLayer } from "./ui/overlay";
import { focusFallback, pushBack, rememberInGroup, RAIL_GROUP } from "./focus/dom";
import { useEffect, useLayoutEffect, useRef } from "preact/hooks";
import { invalidate } from "./state/cache";
import { clearEpg } from "./state/epg";
import { resetTo } from "./state/router";

interface NavItem {
  route: Route;
  name: RouteName;
  label: string;
  icon: IconName;
}

const NAV: NavItem[] = [
  { route: { name: "home" }, name: "home", label: "Home", icon: "house" },
  { route: { name: "live" }, name: "live", label: "Live TV", icon: "television-simple" },
  { route: { name: "guide" }, name: "guide", label: "Guide", icon: "rows" },
  { route: { name: "movies" }, name: "movies", label: "Movies", icon: "film-strip" },
  { route: { name: "series" }, name: "series", label: "Series", icon: "monitor-play" },
  { route: { name: "search" }, name: "search", label: "Search", icon: "magnifying-glass" },
  { route: { name: "favourites" }, name: "favourites", label: "Favourites", icon: "heart" },
];

const FILLABLE = new Set<IconName>(["house", "television-simple", "rows", "film-strip", "monitor-play", "magnifying-glass", "heart", "gear-six"]);

export const SECTION_TITLES: Partial<Record<RouteName, string>> = {
  home: "Home",
  live: "Live TV",
  guide: "Guide",
  movies: "Movies",
  series: "Series",
  search: "Search",
  favourites: "Favourites",
  settings: "Settings",
};

function RailItem(props: { item: NavItem; currentSection: RouteName | null; onPick?: () => void }) {
  const { item } = props;
  const on = props.currentSection === item.name;
  return (
    <button
      type="button"
      class={cx("rail-item", on && "is-current")}
      aria-current={on ? "page" : undefined}
      data-fk={`nav:${item.name}`}
      onClick={() => {
        props.onPick?.();
        goSection(item.route);
      }}
    >
      <Icon name={item.icon} fill={on && FILLABLE.has(item.icon)} class="rail-icon" />
      <span class="rail-label">{item.label}</span>
    </button>
  );
}

/** The account switcher: current provider name; a menu of the others. */
function AccountButton(props: { onPick?: () => void }) {
  const a = account.value;
  if (!a) return null;
  const pick = (e: MouseEvent) => {
    const list = accounts.value ?? [];
    void openMenu(
      e.currentTarget as HTMLElement,
      "Providers",
      [
        ...list.map((x) => ({
          label: cap(x.name, 60),
          hint: x.kind === "xtream" ? "Xtream Codes" : "M3U playlist",
          selected: x.id === a.id,
          onSelect: () => {
            props.onPick?.();
            if (x.id === a.id) return;
            selectAccount(x.id);
            invalidate();
            clearEpg();
            resetTo({ name: "home" });
          },
        })),
        {
          label: "Manage providers",
          icon: "gear-six" as const,
          onSelect: () => {
            props.onPick?.();
            go({ name: "settings", section: "providers" });
          },
        },
      ],
      "above",
    );
  };
  return (
    <button type="button" class="rail-item rail-account" data-fk="nav:account" onClick={pick} aria-label={`Provider: ${a.name}. Switch provider`}>
      <span class={cx("avatar", `hue-${hue(a.name)}`)} aria-hidden="true">
        {monogram(a.name) || "?"}
      </span>
      <span class="rail-label ellipsis">{cap(a.name, 40)}</span>
      <Icon name="caret-up" class="rail-caret" />
    </button>
  );
}

function RailBody(props: { onPick?: () => void }) {
  const section = sectionOf(current.value.route);
  const ref = useRef<HTMLSpanElement>(null);
  // Moving into the rail lands on the current section (like the app's rail memory), also after
  // the focus wandered through the rail and left it without choosing anything.
  useEffect(() => {
    const rail = ref.current?.parentElement;
    if (!rail) return;
    const remember = () => rememberInGroup(RAIL_GROUP, rail.querySelector(".rail-item.is-current"));
    remember();
    const onOut = (e: FocusEvent) => {
      if (!(e.relatedTarget instanceof Node) || !rail.contains(e.relatedTarget)) remember();
    };
    rail.addEventListener("focusout", onOut);
    return () => rail.removeEventListener("focusout", onOut);
  }, [section]);
  return (
    <>
      <span ref={ref} hidden />
      <div class="rail-top">
        <Lockup class="rail-lockup" />
      </div>
      <nav class="rail-items" aria-label="Main" data-scope="rail-nav">
        {NAV.filter((n) => n.name !== "series" || account.value?.kind !== "m3u").map((n) => (
          <RailItem key={n.name} item={n} currentSection={section} onPick={props.onPick} />
        ))}
      </nav>
      <div class="rail-bottom" data-scope="rail-bottom">
        <AccountButton onPick={props.onPick} />
        <RailItem
          item={{ route: { name: "settings" }, name: "settings", label: "Settings", icon: "gear-six" }}
          currentSection={section}
          onPick={props.onPick}
        />
      </div>
    </>
  );
}

/** The clock (and nothing noisy) in the top-right corner (§8.6 header status cluster). */
export function StatusCluster() {
  return (
    <div class="status" aria-hidden="true">
      <span class="status-clock t-title-sm tnum">{clock(now.value, settings.value.time_format)}</span>
    </div>
  );
}

function openDrawer(): void {
  void openLayer((close) => <Drawer onClose={close} />);
}

function Drawer(props: { onClose: CloseLayer }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    const cur = el?.querySelector<HTMLElement>(".rail-item.is-current") ?? null;
    if (cur) cur.focus({ preventScroll: true });
    else if (el) focusFallback(el);
  }, []);
  useEffect(() => pushBack(() => (props.onClose(), true)), []);
  useEffect(() => {
    // Leaving the compact layout closes the drawer.
    if (breakpoint.value !== "compact") props.onClose();
  }, [breakpoint.value]);
  return (
    <div class="layer layer-drawer" data-modal="" onPointerDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div ref={ref} class="rail rail-drawer" data-group="rail" role="dialog" aria-modal="true" aria-label="Menu">
        <RailBody onPick={() => props.onClose(false)} />
      </div>
    </div>
  );
}

export function Shell(props: { children: ComponentChildren; inert: boolean }) {
  const compact = breakpoint.value === "compact";
  const section = sectionOf(current.value.route);
  return (
    <div class={cx("shell", `bp-${breakpoint.value}`)} inert={props.inert}>
      {compact ? (
        <header class="topbar" data-group="rail" data-scope="topbar">
          <IconButton icon="list" label="Menu" onClick={openDrawer} fk="nav:menu" />
          <span class="topbar-title t-title-sm ellipsis">{(section && SECTION_TITLES[section]) ?? ""}</span>
          <StatusCluster />
        </header>
      ) : (
        <aside class="rail" data-group="rail" aria-label="Navigation">
          <RailBody />
        </aside>
      )}
      <main class="main" data-group="content">
        {!compact && <StatusCluster />}
        {props.children}
      </main>
    </div>
  );
}
