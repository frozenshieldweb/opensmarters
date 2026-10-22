// SPDX-License-Identifier: GPL-3.0-or-later
// Settings (DESIGN.md §8.6; WEB_PROMPT.md §6): Providers, Playback, Account, Users (admin) and
// About, in two panes (a section list and the detail pane).

import type { ComponentChildren } from "preact";
import { useState } from "preact/hooks";
import { api, ApiError } from "../api/client";
import type { Account, AdminUser, Invite } from "../api/types";
import { account, accounts, dropAccount, putAccount, selectAccount, settings, signOut, updateSettings, user } from "../state/session";
import { errorText, invalidate, useLoad } from "../state/cache";
import { clearEpg } from "../state/epg";
import { toast } from "../state/misc";
import { go, resetTo, setParams, type Memo, type SettingsSection } from "../state/router";
import { ago, clock, date, dot } from "../lib/format";
import { cap, hue, monogram } from "../lib/text";
import { Button, Callout, Empty, ErrorState, Field, Icon, Lockup, Segmented, Skeleton, Spinner, Toggle, cx } from "../ui/kit";
import type { IconName } from "../ui/icons";
import { breakpoint, useInitialFocus } from "../ui/hooks";
import { confirmDialog } from "../ui/overlay";
import { ChipRow } from "./common";
import { SOURCE_URL } from "./auth";

interface SectionDef {
  id: SettingsSection;
  label: string;
  icon: IconName;
  admin?: boolean;
}

const SECTIONS: SectionDef[] = [
  { id: "providers", label: "Providers", icon: "television-simple" },
  { id: "playback", label: "Playback", icon: "play" },
  { id: "account", label: "Account", icon: "user-circle" },
  { id: "users", label: "Users", icon: "users", admin: true },
  { id: "about", label: "About", icon: "info" },
];

export function SettingsScreen(props: { memo: Memo; section?: SettingsSection }) {
  const admin = !!user.value?.is_admin;
  const sections = SECTIONS.filter((s) => !s.admin || admin);
  const sec = sections.find((s) => s.id === props.section) ?? sections[0]!;
  const bp = breakpoint.value;
  useInitialFocus(true, props.memo);
  const pick = (id: string) => setParams({ name: "settings", section: id as SettingsSection });
  return (
    <div class={cx("screen settings", `settings-${bp}`)}>
      {bp === "compact" ? (
        <div class="settings-chips">
          <h1 class="t-heading screen-pad-x">Settings</h1>
          <ChipRow scope="settings-chips" selected={sec.id} onSelect={pick} items={sections.map((s) => ({ id: s.id, label: s.label }))} label="Settings sections" />
        </div>
      ) : (
        <nav class="settings-nav" aria-label="Settings sections">
          <h1 class="t-heading settings-title">Settings</h1>
          <div data-scope="settings-nav" class="settings-nav-list">
            {sections.map((s) => (
              <button
                type="button"
                key={s.id}
                class={cx("cat-item settings-nav-item", s.id === sec.id && "is-selected")}
                aria-current={s.id === sec.id ? "page" : undefined}
                data-fk={`set:${s.id}`}
                data-autofocus={s.id === sec.id ? "" : undefined}
                onClick={() => pick(s.id)}
              >
                <Icon name={s.icon} />
                <span>{s.label}</span>
              </button>
            ))}
          </div>
        </nav>
      )}
      <div class="settings-pane" data-scope={`settings-${sec.id}`}>
        <div class="settings-pane-inner">
          <h2 class="t-heading settings-pane-title">{sec.label}</h2>
          {sec.id === "providers" && <Providers />}
          {sec.id === "playback" && <Playback />}
          {sec.id === "account" && <AccountSection />}
          {sec.id === "users" && admin && <Users />}
          {sec.id === "about" && <About />}
        </div>
      </div>
    </div>
  );
}

function Row(props: { label: string; desc?: ComponentChildren; children: ComponentChildren }) {
  return (
    <div class="set-row">
      <div class="set-row-text">
        <p class="t-body-strong">{props.label}</p>
        {props.desc && <p class="t-caption c-3">{props.desc}</p>}
      </div>
      <div class="set-row-control">{props.children}</div>
    </div>
  );
}

// ---- Providers

const STATUS: Record<Account["status"], { label: string; icon: IconName; cls: string }> = {
  active: { label: "Active", icon: "check-circle", cls: "st-success" },
  expired: { label: "Expired", icon: "warning", cls: "st-warning" },
  banned: { label: "Banned", icon: "prohibit", cls: "st-danger" },
  disabled: { label: "Disabled", icon: "prohibit", cls: "st-danger" },
  unknown: { label: "Status unknown", icon: "info", cls: "st-info" },
};

function Providers() {
  const list = accounts.value ?? [];
  const cur = account.value;
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = async (a: Account) => {
    setBusy(a.id);
    try {
      const fresh = await api.refreshAccount(a.id);
      putAccount(fresh);
      invalidate(`${a.id}:`);
      clearEpg();
      toast(`${cap(a.name, 40)}: lists refreshed`, "success");
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setBusy(null);
    }
  };

  const remove = async (a: Account) => {
    const ok = await confirmDialog({
      title: `Remove ${cap(a.name, 40)}?`,
      body: "Its favourites and viewing progress on this server go too. You can add the provider again later.",
      confirm: "Remove",
      danger: true,
    });
    if (!ok) return;
    try {
      await api.removeAccount(a.id);
      dropAccount(a.id);
      invalidate(`${a.id}:`);
      toast(`${cap(a.name, 40)} removed`, "success");
      if (cur?.id === a.id) resetTo({ name: "settings", section: "providers" });
    } catch (e) {
      toast(errorText(e), "error");
    }
  };

  return (
    <div class="providers">
      <p class="c-2 set-intro">Each person on this server has their own providers. Their passwords stay on the server, encrypted.</p>
      <ul class="provider-list" data-scope="provider-list">
        {list.map((a) => {
          const st = STATUS[a.status] ?? STATUS.unknown;
          return (
            <li class="provider-card" key={a.id}>
              <div class="provider-card-head">
                <span class={cx("avatar avatar-lg", `hue-${hue(a.name)}`)} aria-hidden="true">
                  {monogram(a.name) || "?"}
                </span>
                <div class="provider-card-titles">
                  <p class="t-title-sm ellipsis">
                    {cap(a.name, 60)}
                    {cur?.id === a.id && <span class="badge-soft t-overline">In use</span>}
                  </p>
                  <p class="t-caption c-3 ellipsis">{dot(a.kind === "xtream" ? "Xtream Codes" : "M3U playlist", cap(a.host, 80))}</p>
                </div>
                <span class={cx("status-chip t-caption", st.cls)}>
                  <Icon name={st.icon} />
                  {st.label}
                </span>
              </div>
              <p class="t-caption c-2 provider-card-meta">
                {dot(
                  a.expires_at ? `${a.expires_at * 1000 < Date.now() ? "Expired" : "Expires"} ${date(a.expires_at)}` : "No expiry date",
                  a.max_connections !== null
                    ? `${a.active_connections ?? 0} of ${a.max_connections} ${a.max_connections === 1 ? "stream" : "streams"} in use`
                    : undefined,
                )}
              </p>
              <div class="provider-card-actions">
                {cur?.id !== a.id && (
                  <Button
                    variant="primary"
                    size="sm"
                    icon="check"
                    onClick={() => {
                      selectAccount(a.id);
                      invalidate();
                      clearEpg();
                      resetTo({ name: "home" });
                    }}
                  >
                    Use this provider
                  </Button>
                )}
                <Button variant="secondary" size="sm" icon="pencil-simple" onClick={() => go({ name: "provider-edit", id: a.id })}>
                  Edit
                </Button>
                <Button variant="secondary" size="sm" icon="arrows-clockwise" busy={busy === a.id} onClick={() => void refresh(a)}>
                  Refresh
                </Button>
                <Button variant="danger" size="sm" icon="trash" onClick={() => void remove(a)}>
                  Remove
                </Button>
              </div>
            </li>
          );
        })}
      </ul>
      <Button variant="secondary" icon="plus" onClick={() => go({ name: "provider-new" })} fk="providers:add">
        Add a provider
      </Button>
    </div>
  );
}

// ---- Playback

function Playback() {
  const s = settings.value;
  const save = (patch: Partial<typeof s>) => updateSettings(patch).catch((e: unknown) => toast(errorText(e), "error"));
  return (
    <div class="set-rows">
      <Row label="Live TV format" desc="How this server asks your provider for live channels. Try the other one if channels don't start.">
        <Segmented
          label="Live TV format"
          value={s.live_format}
          onChange={(v) => void save({ live_format: v })}
          options={[
            { value: "ts", label: "MPEG-TS" },
            { value: "m3u8", label: "HLS" },
          ]}
        />
      </Row>
      <Row label="Time format" desc={`Clocks and the guide. Now: ${clock(Date.now() / 1000, s.time_format)}`}>
        <Segmented
          label="Time format"
          value={s.time_format}
          onChange={(v) => void save({ time_format: v })}
          options={[
            { value: "24h", label: "24-hour" },
            { value: "12h", label: "12-hour" },
          ]}
        />
      </Row>
      <Row label="Play the next episode" desc="When an episode ends, the next one starts after 10 seconds.">
        <Toggle on={s.autoplay_next} onChange={(on) => void save({ autoplay_next: on })} label="Play the next episode" />
      </Row>
    </div>
  );
}

// ---- Account

function AccountSection() {
  const u = user.value!;
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fe, setFe] = useState<{ current?: string; next?: string; again?: string }>({});

  const submit = async (e: Event) => {
    e.preventDefault();
    const errs: typeof fe = {};
    if (!current) errs.current = "Enter your current password.";
    if ([...next].length < 10) errs.next = "Use at least 10 characters.";
    else if (next !== again) errs.again = "The two passwords don't match.";
    setFe(errs);
    if (errs.current || errs.next || errs.again || busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.changePassword(current, next);
      setCurrent("");
      setNext("");
      setAgain("");
      toast("Password changed. Other devices are signed out.", "success");
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Something went wrong. Try again.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="account-section">
      <div class="account-who">
        <span class={cx("avatar avatar-lg", `hue-${hue(u.name)}`)} aria-hidden="true">
          {monogram(u.name)}
        </span>
        <div>
          <p class="t-title-sm">{u.name}</p>
          <p class="t-caption c-3">{u.is_admin ? "Administrator" : "Member"}</p>
        </div>
        <Button variant="secondary" icon="sign-out" class="account-signout" fk="account:signout" onClick={() => void signOut()}>
          Sign out
        </Button>
      </div>
      <form class="set-form" onSubmit={submit} aria-label="Change password" data-scope="password-form" noValidate>
        <h3 class="t-title-sm">Change password</h3>
        <Field label="Current password" type="password" value={current} onInput={setCurrent} autocomplete="current-password" error={fe.current} />
        <Field label="New password" type="password" value={next} onInput={setNext} autocomplete="new-password" error={fe.next} hint="At least 10 characters." />
        <Field label="New password again" type="password" value={again} onInput={setAgain} autocomplete="new-password" error={fe.again} />
        {error && <Callout kind="danger">{error}</Callout>}
        <div>
          <Button type="submit" variant="primary" busy={busy}>
            Change password
          </Button>
        </div>
        <p class="t-caption c-3">Changing it signs you out everywhere else.</p>
      </form>
    </div>
  );
}

// ---- Users (admin)

function Users() {
  const me = user.value!;
  const list = useLoad<AdminUser[]>("admin:users", () => api.users(), { ttlMs: 0, keepPrevious: true });
  const [invite, setInvite] = useState<Invite | null>(null);
  const [inviting, setInviting] = useState(false);
  const [copied, setCopied] = useState(false);

  const createInvite = async () => {
    setInviting(true);
    try {
      setInvite(await api.invite());
      setCopied(false);
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setInviting(false);
    }
  };

  const copy = async () => {
    if (!invite) return;
    try {
      await navigator.clipboard.writeText(invite.url);
      setCopied(true);
      toast("Invitation link copied", "success");
    } catch {
      toast("Couldn't copy: select the link and copy it.", "warning");
    }
  };

  const act = async (u: AdminUser, what: "disable" | "enable" | "remove") => {
    if (what === "remove") {
      const ok = await confirmDialog({
        title: `Remove ${u.name}?`,
        body: "Their providers, favourites and progress on this server are deleted. This can't be undone.",
        confirm: "Remove",
        danger: true,
      });
      if (!ok) return;
    }
    try {
      if (what === "disable") await api.disableUser(u.id);
      else if (what === "enable") await api.enableUser(u.id);
      else await api.deleteUser(u.id);
      toast(what === "remove" ? `${u.name} removed` : what === "disable" ? `${u.name} can't sign in now` : `${u.name} can sign in again`, "success");
      list.reload();
    } catch (e) {
      toast(errorText(e), "error");
    }
  };

  return (
    <div class="users">
      <p class="c-2 set-intro">This server is private: people join only with an invitation link. Each link works once, for 72 hours.</p>
      <div class="invite-box" data-scope="invite">
        <Button variant="primary" icon="user-plus" busy={inviting} onClick={() => void createInvite()} fk="users:invite">
          {invite ? "Create another link" : "Invite someone"}
        </Button>
        {invite && (
          <div class="invite-link">
            <label class="field-label t-caption" for="invite-url">
              Invitation link
            </label>
            <div class="invite-row">
              <input id="invite-url" class="field-input invite-input" readOnly value={invite.url} onFocus={(e) => (e.currentTarget as HTMLInputElement).select()} />
              <Button variant="secondary" icon={copied ? "check" : "copy"} onClick={() => void copy()}>
                {copied ? "Copied" : "Copy"}
              </Button>
            </div>
            <p class="t-caption c-3">
              Works once, until {date(invite.expires_at)}, {clock(invite.expires_at)}. Send it privately: anyone with the link can join.
            </p>
          </div>
        )}
      </div>
      <h3 class="t-title-sm users-title">People</h3>
      {list.error && !list.data && <ErrorState message={list.error} onRetry={list.reload} compact />}
      {!list.data && !list.error && <Skeleton class="sk-line" />}
      {list.data && (
        <ul class="user-list" data-scope="user-list">
          {list.data.map((u) => {
            const self = u.id === me.id;
            return (
              <li key={u.id} class={cx("user-row", u.disabled && "is-disabled")}>
                <span class={cx("avatar", `hue-${hue(u.name)}`)} aria-hidden="true">
                  {monogram(u.name)}
                </span>
                <div class="user-text">
                  <p class="t-body-strong">
                    {u.name}
                    {self && <span class="badge-soft t-overline">You</span>}
                  </p>
                  <p class="t-caption c-3">
                    {dot(
                      u.is_admin ? "Administrator" : "Member",
                      u.disabled ? "Can't sign in" : undefined,
                      `joined ${ago(u.created_at)}`,
                      u.last_seen_at ? `last seen ${ago(u.last_seen_at)}` : "never signed in",
                    )}
                  </p>
                </div>
                {!self && (
                  <div class="user-actions">
                    {u.disabled ? (
                      <Button variant="secondary" size="sm" icon="check" onClick={() => void act(u, "enable")}>
                        Allow
                      </Button>
                    ) : (
                      <Button variant="secondary" size="sm" icon="prohibit" onClick={() => void act(u, "disable")}>
                        Disable
                      </Button>
                    )}
                    <Button variant="danger" size="sm" icon="trash" onClick={() => void act(u, "remove")}>
                      Remove
                    </Button>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {list.data && list.data.length === 1 && <Empty compact title="Just you so far" body="Invite the rest of your household with a link." />}
    </div>
  );
}

// ---- About

function About() {
  const about = useLoad("about", () => api.about(), { ttlMs: 60_000 });
  const a = about.data;
  const transcode =
    a?.transcode_video === "always"
      ? "converts video the browser can't play to H.264 (uses a lot of processor time)"
      : a?.transcode_video === "hevc"
        ? "converts HEVC video to H.264 for browsers that can't play HEVC"
        : "never converts video: a channel in a format your browser can't play shows a message";
  return (
    <div class="about">
      <Lockup class="about-lockup" />
      {!a && !about.error && <Spinner />}
      {about.error && <Callout kind="danger">{about.error}</Callout>}
      {a && (
        <>
          <p class="t-body tnum">Version {a.version}</p>
          <div class="about-links">
            <a class="btn btn-secondary" href={a.source_url || SOURCE_URL} target="_blank" rel="noopener noreferrer">
              <Icon name="code" />
              <span class="btn-label">Source code</span>
              <Icon name="arrow-up-right" />
            </a>
          </div>
          <section class="about-block">
            <h3 class="t-title-sm">What this server sends where</h3>
            <ul class="about-list c-2">
              <li>Your browser talks only to this server. No other site is contacted, and there's no tracking or analytics.</li>
              <li>This server talks to your provider: it fetches the channel lists, the guide, artwork and the streams, and passes them on to you.</li>
              <li>Provider passwords and playlist links are stored on this server, encrypted, and never sent back to a browser.</li>
              <li>
                Every stream is repackaged for the browser; audio is converted when needed. Video: this server {transcode}.
              </li>
              <li>
                Each person can watch {a.streams_per_user === 1 ? "one stream" : `${a.streams_per_user} streams`} at a time, and the server plays at most{" "}
                {a.max_streams} at once. Each stream also uses one of your provider's connections.
              </li>
            </ul>
          </section>
          <section class="about-block">
            <h3 class="t-title-sm">Licences</h3>
            <p class="c-2">
              opensmarters is free software under the GNU General Public License, version 3 or later. Fonts: Fraunces and Source Sans 3 (as
              “Afterglow Sans”), SIL Open Font License 1.1. Icons: Phosphor, MIT licence. Video: hls.js, Apache 2.0. Interface: Preact, MIT
              licence.{" "}
              <a href="/licences/README.txt" target="_blank" rel="noopener noreferrer">
                Licence texts
              </a>
            </p>
            <p class="c-3 t-caption">
              Independent open-source project. It ships with no channels, content or provider: use it with a subscription you're entitled to.
            </p>
          </section>
        </>
      )}
    </div>
  );
}
