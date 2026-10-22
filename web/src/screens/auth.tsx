// SPDX-License-Identifier: GPL-3.0-or-later
// Signed-out screens (WEB_PROMPT.md §2, §6; DESIGN.md §8.1): the landing page with the Sign in
// panel, first-run setup (the one-time code from the server log) and accepting an invitation.
// There is no public sign-up: the owner of the server invites people.

import type { ComponentChildren } from "preact";
import { useState } from "preact/hooks";
import { api, ApiError } from "../api/client";
import { signedIn, status } from "../state/session";
import { Button, Callout, Field, Lockup } from "../ui/kit";
import { useInitialFocus } from "../ui/hooks";

export const SOURCE_URL = "https://github.com/frozenshieldweb/opensmarters";
export const DOWNLOAD_URL = "https://frozenshieldweb.github.io/opensmarters/";

const NAME_RE = /^[A-Za-z0-9._-]{2,32}$/;

function errText(e: unknown): string {
  return e instanceof ApiError ? e.message : "Something went wrong. Try again.";
}

/** The invitation token in the address, if this is an invitation link. */
export function inviteToken(): string | null {
  const hash = location.hash.replace(/^#/, "");
  const params = new URLSearchParams(location.search);
  const hashParams = new URLSearchParams(hash);
  const path = /\/invite\/([A-Za-z0-9_\-.~]+)\/?$/.exec(location.pathname)?.[1];
  const fromHash = hashParams.get("invite") ?? hashParams.get("token") ?? (/^[A-Za-z0-9_\-.~]{16,}$/.test(hash) ? hash : null);
  const t = fromHash ?? params.get("invite") ?? params.get("token") ?? path ?? null;
  return t && /^[A-Za-z0-9_\-.~]{8,256}$/.test(t) ? t : null;
}

function forgetInvite(): void {
  history.replaceState(null, "", "/app/");
}

function Footer() {
  const v = status.value?.version;
  return (
    <footer class="auth-footer t-caption c-3">
      <p>
        opensmarters is free software (GPL-3.0-or-later). It ships with no channels, content or provider. Independent
        open-source project, not affiliated with IPTV Smarters or WHMCS Smarters.
      </p>
      <p class="auth-links">
        <a href={DOWNLOAD_URL} rel="noopener noreferrer" target="_blank">
          The Windows app
        </a>
        <a href={SOURCE_URL} rel="noopener noreferrer" target="_blank">
          Source code
        </a>
        {v && <span class="tnum">Version {v}</span>}
      </p>
    </footer>
  );
}

function AuthLayout(props: { headline: ComponentChildren; intro: ComponentChildren; children: ComponentChildren }) {
  return (
    <div class="auth" data-group="content">
      <picture class="auth-hero" aria-hidden="true">
        <source type="image/avif" srcset="/img/hero-1280.avif 1280w, /img/hero-1920.avif 1920w" sizes="100vw" />
        <source type="image/webp" srcset="/img/hero-768.webp 768w, /img/hero-1280.webp 1280w, /img/hero-1920.webp 1920w" sizes="100vw" />
        <img src="/img/hero-1920.jpg" alt="" />
      </picture>
      <div class="auth-scroll">
        <div class="auth-col">
          <Lockup class="auth-lockup" />
          <h1 class="t-hero auth-headline">{props.headline}</h1>
          <p class="auth-intro c-2">{props.intro}</p>
          {props.children}
        </div>
        <Footer />
      </div>
    </div>
  );
}

/** The landing page with the Sign in panel (signed out). */
export function Landing() {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fieldErr, setFieldErr] = useState<{ name?: string; password?: string }>({});
  useInitialFocus(true);

  const submit = async (e: Event) => {
    e.preventDefault();
    const fe: typeof fieldErr = {};
    if (!name.trim()) fe.name = "Enter your name.";
    if (!password) fe.password = "Enter your password.";
    setFieldErr(fe);
    if (fe.name || fe.password || busy) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.login(name.trim(), password);
      await signedIn(r.user);
    } catch (err) {
      setError(errText(err));
      setBusy(false);
    }
  };

  return (
    <AuthLayout
      headline={
        <>
          Make room
          <br />
          for good stories.
        </>
      }
      intro="Your channels, films and series, in one quiet place. This is a private server: sign in with the name and password its owner gave you."
    >
      <form class="auth-form" onSubmit={submit} data-scope="signin" aria-label="Sign in" noValidate>
        <Field label="Name" value={name} onInput={setName} autocomplete="username" autofocus error={fieldErr.name} maxLength={64} />
        <Field label="Password" type="password" value={password} onInput={setPassword} autocomplete="current-password" error={fieldErr.password} />
        {error && <Callout kind="danger">{error}</Callout>}
        <Button type="submit" variant="primary" size="lg" block busy={busy} iconRight={busy ? undefined : "caret-right"}>
          {busy ? "Signing in…" : "Sign in"}
        </Button>
        <p class="auth-note t-caption c-3">
          No public sign-up: the owner of this server invites people. Your provider's password stays on the server, encrypted, and never
          reaches this browser.
        </p>
      </form>
    </AuthLayout>
  );
}

function passwordErrors(pw: string, confirm: string): { password?: string; confirm?: string } {
  const out: { password?: string; confirm?: string } = {};
  if ([...pw].length < 10) out.password = "Use at least 10 characters.";
  else if (pw !== confirm) out.confirm = "The two passwords don't match.";
  return out;
}

function nameError(name: string): string | undefined {
  return NAME_RE.test(name) ? undefined : "Use 2 to 32 letters, digits, dots, dashes or underscores.";
}

/** First run: create the administrator with the one-time setup code from the server log. */
export function Setup() {
  const [code, setCode] = useState("");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fe, setFe] = useState<{ code?: string; name?: string; password?: string; confirm?: string }>({});
  useInitialFocus(true);

  const submit = async (e: Event) => {
    e.preventDefault();
    const errs = { ...passwordErrors(password, confirm), name: nameError(name.trim()), code: code.trim() ? undefined : "Enter the setup code." };
    setFe(errs);
    if (errs.code || errs.name || errs.password || errs.confirm || busy) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.setup(code.trim(), name.trim(), password);
      await signedIn(r.user);
    } catch (err) {
      setError(errText(err));
      setBusy(false);
    }
  };

  return (
    <AuthLayout
      headline={
        <>
          Welcome to
          <br />
          your own server.
        </>
      }
      intro="First, create the administrator. The setup code is printed once in the server's log; it works only once."
    >
      <form class="auth-form" onSubmit={submit} data-scope="setup" aria-label="Set up" noValidate>
        <Field label="Setup code" value={code} onInput={setCode} autofocus error={fe.code} maxLength={64} />
        <Field label="Your name" value={name} onInput={setName} autocomplete="username" error={fe.name} hint="2 to 32 letters, digits, dots, dashes or underscores." maxLength={32} />
        <Field label="Password" type="password" value={password} onInput={setPassword} autocomplete="new-password" error={fe.password} hint="At least 10 characters." />
        <Field label="Password again" type="password" value={confirm} onInput={setConfirm} autocomplete="new-password" error={fe.confirm} />
        {error && <Callout kind="danger">{error}</Callout>}
        <Button type="submit" variant="primary" size="lg" block busy={busy}>
          {busy ? "Creating…" : "Create administrator"}
        </Button>
        <p class="auth-note t-caption c-3">You can invite the rest of your household afterwards, from Settings › Users.</p>
      </form>
    </AuthLayout>
  );
}

/** Accept an invitation: choose a name and a password. */
export function AcceptInvite() {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fe, setFe] = useState<{ name?: string; password?: string; confirm?: string }>({});
  useInitialFocus(true);

  const submit = async (e: Event) => {
    e.preventDefault();
    const token = inviteToken();
    const errs = { ...passwordErrors(password, confirm), name: nameError(name.trim()) };
    setFe(errs);
    if (!token || errs.name || errs.password || errs.confirm || busy) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.acceptInvite(token, name.trim(), password);
      forgetInvite();
      await signedIn(r.user);
    } catch (err) {
      setError(errText(err));
      setBusy(false);
    }
  };

  return (
    <AuthLayout
      headline={
        <>
          You're
          <br />
          invited.
        </>
      }
      intro="Choose a name and a password for this server. You'll add your own TV provider next."
    >
      <form class="auth-form" onSubmit={submit} data-scope="invite" aria-label="Accept the invitation" noValidate>
        <Field label="Your name" value={name} onInput={setName} autocomplete="username" autofocus error={fe.name} hint="2 to 32 letters, digits, dots, dashes or underscores." maxLength={32} />
        <Field label="Password" type="password" value={password} onInput={setPassword} autocomplete="new-password" error={fe.password} hint="At least 10 characters." />
        <Field label="Password again" type="password" value={confirm} onInput={setConfirm} autocomplete="new-password" error={fe.confirm} />
        {error && <Callout kind="danger">{error}</Callout>}
        <Button type="submit" variant="primary" size="lg" block busy={busy}>
          {busy ? "Joining…" : "Join"}
        </Button>
        <Button
          variant="ghost"
          block
          onClick={() => {
            forgetInvite();
            location.reload();
          }}
        >
          I already have an account
        </Button>
      </form>
    </AuthLayout>
  );
}
