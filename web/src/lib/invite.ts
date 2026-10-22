// SPDX-License-Identifier: GPL-3.0-or-later
// The token of an invitation link. The server's links are `{origin}/invite#<token>` (the token
// in the fragment never reaches a server log); `?invite=`, `#invite=` and `/invite/<token>`
// are accepted too.

const TOKEN = /^[A-Za-z0-9_\-.~]{8,256}$/;

export function inviteTokenFrom(loc: { pathname: string; search: string; hash: string }): string | null {
  const hash = loc.hash.replace(/^#/, "");
  const params = new URLSearchParams(loc.search);
  const hashParams = new URLSearchParams(hash);
  const path = /\/invite\/([A-Za-z0-9_\-.~]+)\/?$/.exec(loc.pathname)?.[1];
  const onInvitePage = /^\/(app\/)?invite\/?$/.test(loc.pathname);
  const bare = onInvitePage && TOKEN.test(hash) ? hash : null;
  const t = hashParams.get("invite") ?? hashParams.get("token") ?? bare ?? params.get("invite") ?? params.get("token") ?? path ?? null;
  return t && TOKEN.test(t) ? t : null;
}
