// SPDX-License-Identifier: GPL-3.0-or-later
// Add or edit a provider (WEB_PROMPT.md §6; DESIGN.md §8.1): Xtream Codes or an M3U playlist,
// plain explanations, and the unencrypted-HTTP warning as in the Windows app. The server checks
// the details with the provider before saving them (encrypted, never sent back).

import type { ComponentChildren } from "preact";
import { useState } from "preact/hooks";
import { api, ApiError } from "../api/client";
import type { AccountPatch, NewAccount } from "../api/types";
import { accounts, putAccount, selectAccount, signOut } from "../state/session";
import { back, go } from "../state/router";
import { toast } from "../state/misc";
import { invalidate } from "../state/cache";
import { Button, Callout, Field, Lockup, Segmented } from "../ui/kit";
import { useInitialFocus } from "../ui/hooks";
import { cap } from "../lib/text";
import { detectXtream } from "../lib/xtream";

type Kind = "xtream" | "m3u";


const isHttp = (s: string) => /^\s*http:\/\//i.test(s);
const isUrl = (s: string) => /^\s*https?:\/\/[^\s/]+/i.test(s);

export function AddProvider(props: { firstRun?: boolean; editId?: string }) {
  const editing = props.editId ? (accounts.value ?? []).find((a) => a.id === props.editId) ?? null : null;
  const [kind, setKind] = useState<Kind>(editing?.kind ?? "xtream");
  const [name, setName] = useState(editing?.name ?? "");
  const [server, setServer] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [m3u, setM3u] = useState("");
  const [epg, setEpg] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fe, setFe] = useState<Record<string, string | undefined>>({});
  useInitialFocus(true);

  const xtreamLink = kind === "m3u" ? detectXtream(m3u) : null;
  const http = kind === "xtream" ? isHttp(server) : isHttp(m3u) || isHttp(epg);

  const convert = () => {
    if (!xtreamLink) return;
    setKind("xtream");
    setServer(xtreamLink.server);
    setUsername(xtreamLink.username);
    setPassword(xtreamLink.password);
    setM3u("");
  };

  const submit = async (e: Event) => {
    e.preventDefault();
    if (busy) return;
    const errs: Record<string, string | undefined> = {};
    if (!name.trim()) errs["name"] = "Give this provider a name, like “Living room”.";
    if (kind === "xtream") {
      if (!editing || server) {
        if (!server.trim()) errs["server"] = "Enter the server address your provider gave you.";
        else if (!isUrl(server)) errs["server"] = "Use an address that starts with http:// or https://.";
      }
      if (!editing && !username.trim()) errs["username"] = "Enter your username.";
      if (!editing && !password) errs["password"] = "Enter your password.";
    } else {
      if (!editing || m3u) {
        if (!m3u.trim()) errs["m3u"] = "Enter the playlist address.";
        else if (!isUrl(m3u)) errs["m3u"] = "Use an address that starts with http:// or https://.";
      }
      if (epg.trim() && !isUrl(epg)) errs["epg"] = "Use an address that starts with http:// or https://.";
    }
    setFe(errs);
    if (Object.values(errs).some(Boolean)) return;
    setBusy(true);
    setError(null);
    try {
      if (editing) {
        const patch: AccountPatch = {};
        if (name.trim() !== editing.name) patch.name = name.trim();
        if (kind === "xtream") {
          if (server.trim()) patch.server = server.trim();
          if (username.trim()) patch.username = username.trim();
          if (password) patch.password = password;
        } else {
          if (m3u.trim()) patch.m3u_url = m3u.trim();
          if (epg.trim()) patch.epg_url = epg.trim();
        }
        const a = await api.editAccount(editing.id, patch);
        putAccount(a);
        invalidate(`${a.id}:`);
        toast("Provider saved", "success");
        back();
      } else {
        const body: NewAccount =
          kind === "xtream"
            ? { name: name.trim(), kind, server: server.trim(), username: username.trim(), password }
            : { name: name.trim(), kind, m3u_url: m3u.trim(), ...(epg.trim() ? { epg_url: epg.trim() } : {}) };
        const a = await api.addAccount(body);
        const first = (accounts.value ?? []).length === 0;
        putAccount(a);
        if (first) selectAccount(a.id);
        if (!props.firstRun) {
          toast(`${cap(a.name, 40)} added`, "success");
          go({ name: "settings", section: "providers" }, { replace: true });
        }
      }
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Something went wrong. Try again.");
      setBusy(false);
    }
  };

  const form = (
    <form class="provider-form" onSubmit={submit} data-scope="provider-form" aria-label={editing ? "Edit provider" : "Add a provider"} noValidate>
      {!editing && (
        <Segmented<Kind>
          label="Provider type"
          value={kind}
          onChange={(k) => {
            setKind(k);
            setError(null);
            setFe({});
          }}
          options={[
            { value: "xtream", label: "Xtream Codes" },
            { value: "m3u", label: "M3U playlist" },
          ]}
        />
      )}
      <p class="provider-explain c-2">
        {kind === "xtream"
          ? "Your provider gave you a server address, a username and a password. You get live TV with its guide, films and series."
          : "A playlist link usually ends in .m3u or .m3u8. Add your provider's guide link (XMLTV) too if you have one. Playlists have live TV, and films when the playlist lists them; no series."}
      </p>
      <Field label="Name" value={name} onInput={setName} placeholder="Living room" error={fe["name"]} autofocus maxLength={64} />
      {kind === "xtream" ? (
        <>
          <Field
            label="Server address"
            type="url"
            inputMode="url"
            value={server}
            onInput={setServer}
            placeholder={editing ? `Unchanged (${editing.host})` : "http://provider.example:8080"}
            error={fe["server"]}
          />
          <Field label="Username" value={username} onInput={setUsername} placeholder={editing ? "Unchanged" : ""} error={fe["username"]} autocomplete="off" />
          <Field label="Password" type="password" value={password} onInput={setPassword} placeholder={editing ? "Unchanged" : ""} error={fe["password"]} autocomplete="off" />
        </>
      ) : (
        <>
          <Field
            label="Playlist address"
            type="url"
            inputMode="url"
            value={m3u}
            onInput={setM3u}
            placeholder={editing ? "Unchanged" : "https://provider.example/playlist.m3u"}
            error={fe["m3u"]}
          />
          {xtreamLink && (
            <div class="provider-convert">
              <Callout kind="info" lead="This is an Xtream Codes link.">
                Sign in with Xtream Codes instead to get films, series and the guide.
              </Callout>
              <Button variant="secondary" icon="arrows-clockwise" onClick={convert}>
                Use Xtream Codes
              </Button>
            </div>
          )}
          <Field
            label="Guide address (optional)"
            type="url"
            inputMode="url"
            value={epg}
            onInput={setEpg}
            placeholder={editing ? "Unchanged" : "https://provider.example/guide.xml"}
            error={fe["epg"]}
          />
        </>
      )}
      {http && (
        <Callout kind="warning" lead="Unencrypted connection:">
          your password is visible on the network between this server and your provider.
        </Callout>
      )}
      {error && <Callout kind="danger">{error}</Callout>}
      <div class="provider-actions">
        {!props.firstRun && (
          <Button variant="secondary" onClick={() => back()}>
            Cancel
          </Button>
        )}
        <Button type="submit" variant="primary" size={props.firstRun ? "lg" : "md"} block={props.firstRun} busy={busy}>
          {busy ? "Checking with your provider…" : editing ? "Save" : "Add provider"}
        </Button>
      </div>
      <p class="auth-note t-caption c-3">
        This server checks the details with your provider, then keeps the password encrypted. It never comes back to your browser.
      </p>
    </form>
  );

  if (props.firstRun) {
    return (
      <FirstRunLayout>
        <Lockup class="auth-lockup" />
        <h1 class="t-hero auth-headline">
          Add your
          <br />
          TV provider.
        </h1>
        <p class="auth-intro c-2">Use the details from your own subscription. opensmarters comes with no channels of its own.</p>
        {form}
        <Button variant="ghost" icon="sign-out" class="first-run-signout" onClick={() => void signOut()}>
          Sign out
        </Button>
      </FirstRunLayout>
    );
  }
  return (
    <div class="screen screen-pad provider-screen">
      <header class="screen-head">
        <Button variant="ghost" icon="caret-left" class="back-btn" onClick={() => back()}>
          {editing ? "Providers" : "Back"}
        </Button>
        <h1 class="t-heading">{editing ? `Edit ${cap(editing.name, 40)}` : "Add a provider"}</h1>
      </header>
      <div class="provider-col">{form}</div>
    </div>
  );
}

function FirstRunLayout(props: { children: ComponentChildren }) {
  return (
    <div class="auth" data-group="content">
      <picture class="auth-hero" aria-hidden="true">
        <source type="image/avif" srcset="/img/hero-1280.avif 1280w, /img/hero-1920.avif 1920w" sizes="100vw" />
        <source type="image/webp" srcset="/img/hero-768.webp 768w, /img/hero-1280.webp 1280w, /img/hero-1920.webp 1920w" sizes="100vw" />
        <img src="/img/hero-1920.jpg" alt="" />
      </picture>
      <div class="auth-scroll">
        <div class="auth-col">{props.children}</div>
      </div>
    </div>
  );
}
