// SPDX-License-Identifier: GPL-3.0-or-later
// A development mock of the opensmarters web server's API (docs/WEB_PROMPT.md §4), so the UI
// can be built while the real server (crates/web) is written. Dev-only: it is wired into the
// Vite dev and preview servers (vite.config.ts) and never part of the production bundle.
//
// State is in memory. `MOCK_STATE=fresh` starts with no users (first-run setup; the setup code
// is printed to the console); the default starts with an administrator "mira" (password
// "afterglow-demo") who has a demo provider. POST /api/__mock/reset {"state": "fresh"|"ready"}
// resets it (used by the screenshot script).
//
// It follows the real server's (crates/web) behaviour where the contract leaves room: error
// codes, `Retry-After` on 429, invite links `{origin}/invite#<token>`, epg/now leaving out
// channels whose guide isn't ready yet, PUT /api/settings taking a subset, M3U accounts (films
// from the playlist, no categories, no series), films as a growing EVENT playlist that starts at
// the requested `start`, and 409 `stream_replaced` on a stream replaced by a newer one.
//
// Scripted errors: sign-in failures (401, then 429 after 5), a provider that refuses (server
// containing "refuse", or username "wrong") or can't be reached (server containing
// "unreachable"), channel 13 (stream limit reached, 503), channel 14 (provider refused the
// stream, 502), channel 9 (the stream drops every 20 s: reconnects), and "Θάλασσα HD" (HEVC).

import { randomBytes } from "node:crypto";
import { createReadStream, existsSync } from "node:fs";
import type { IncomingMessage, ServerResponse } from "node:http";
import type {
  Account,
  AdminUser,
  Channel,
  ContinueItem,
  Episode,
  Movie,
  PlayRequest,
  Programme,
  Series,
  Settings,
  User,
} from "../src/api/types.ts";
import { buildCatalogue, schedule, type Catalogue, type MockChannel } from "./catalogue.ts";
import { imageFor } from "./images.ts";
import { ensureMedia, livePlaylist, segmentFile, subtitleSegment, vodMaster, vodPlaylist, SEGMENT_SECS, type MediaDirs } from "./media.ts";

type Next = (err?: unknown) => void;
type Req = IncomingMessage & { body?: unknown };

interface MockUser extends AdminUser {
  password: string;
}

interface MockAccount extends Account {
  owner: string;
}

interface Stream {
  sid: string;
  owner: string;
  live: boolean;
  duration: number;
  firstSeg: number;
  created: number;
  dropsAfter: number | null;
}

const VERSION = "0.3.0-dev (mock)";
const nowS = () => Math.floor(Date.now() / 1000);
const id = () => randomBytes(9).toString("base64url");

class ApiFail extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly retryAfter?: number,
  ) {
    super(message);
  }
}

const fail = (status: number, code: string, message: string) => new ApiFail(status, code, message);

export class MockApi {
  private cat: Catalogue;
  private users: MockUser[] = [];
  private sessions = new Map<string, string>();
  private setupCode = "";
  private invites = new Map<string, number>();
  private accounts: MockAccount[] = [];
  private favs = new Map<string, { channel: Set<string>; movie: Set<string>; series: Set<string> }>();
  private progress = new Map<string, Map<string, { kind: "movie" | "episode"; series_id: string | null; position: number; duration: number; updated: number }>>();
  private settings = new Map<string, Settings>();
  private streams = new Map<string, Stream>();
  private replaced = new Map<string, string>();
  private startedAt = Date.now();
  private failures = new Map<string, number>();
  private media: MediaDirs | null;

  constructor(mediaRoot: string, state = process.env["MOCK_STATE"] ?? "ready") {
    this.cat = buildCatalogue(nowS());
    this.media = ensureMedia(mediaRoot);
    this.reset(state === "fresh" ? "fresh" : "ready");
  }

  reset(state: "fresh" | "ready"): void {
    this.users = [];
    this.sessions.clear();
    this.invites.clear();
    this.accounts = [];
    this.favs.clear();
    this.progress.clear();
    this.settings.clear();
    this.streams.clear();
    this.replaced.clear();
    this.startedAt = Date.now();
    this.failures.clear();
    this.setupCode = randomBytes(4).toString("hex").toUpperCase().replace(/(.{4})/, "$1-");
    if (state === "fresh") {
      console.log(`[mock] first run: setup code ${this.setupCode}`);
      return;
    }
    const t = nowS();
    const mira: MockUser = { id: "u1", name: "mira", is_admin: true, disabled: false, created_at: t - 86400 * 40, last_seen_at: t - 120, password: "afterglow-demo" };
    this.users.push(
      mira,
      { id: "u2", name: "tobias", is_admin: false, disabled: false, created_at: t - 86400 * 21, last_seen_at: t - 86400 * 2, password: "tobias-password" },
      { id: "u3", name: "wren.k", is_admin: false, disabled: true, created_at: t - 86400 * 9, last_seen_at: null, password: "wren-password-1" },
    );
    this.accounts.push(
      { id: "a1", owner: "u1", name: "Demo provider", kind: "xtream", host: "panel.provider.example", status: "active", expires_at: t + 86400 * 143, max_connections: 2, active_connections: 0 },
      { id: "a2", owner: "u1", name: "Evening playlist", kind: "m3u", host: "lists.provider.example", status: "unknown", expires_at: null, max_connections: null, active_connections: null },
    );
    const f = this.favsOf("u1", "a1");
    for (const n of [1, 2, 3, 4, 5, 9, 31, 62]) f.channel.add(`ch${n}`);
    for (const m of this.cat.movies.slice(0, 600).filter((_, i) => i % 97 === 3)) f.movie.add(m.id);
    for (const s of this.cat.series.filter((_, i) => i % 41 === 2)) f.series.add(s.id);
    const p = this.progressOf("u1", "a1");
    const recent = [...this.cat.movies].sort((a, b) => (b.added ?? 0) - (a.added ?? 0));
    recent.slice(1, 5).forEach((m, i) => {
      const d = m.detail.duration_secs ?? 6000;
      p.set(`movie:${m.id}`, { kind: "movie", series_id: null, position: Math.floor(d * (0.2 + i * 0.15)), duration: d, updated: t - 3600 * (i * 7 + 1) });
    });
    const s0 = this.cat.series[0]!;
    const ep = s0.seasons[0]!.episodes[1]!;
    p.set(`episode:${ep.id}`, { kind: "episode", series_id: s0.id, position: 800, duration: ep.duration_secs ?? 2700, updated: t - 1800 });
    const ep0 = s0.seasons[0]!.episodes[0]!;
    p.set(`episode:${ep0.id}`, { kind: "episode", series_id: s0.id, position: (ep0.duration_secs ?? 2700) - 30, duration: ep0.duration_secs ?? 2700, updated: t - 86400 });
  }

  // ---- helpers

  private favsOf(user: string, account: string) {
    const k = `${user}:${account}`;
    let f = this.favs.get(k);
    if (!f) {
      f = { channel: new Set(), movie: new Set(), series: new Set() };
      this.favs.set(k, f);
    }
    return f;
  }

  private progressOf(user: string, account: string) {
    const k = `${user}:${account}`;
    let p = this.progress.get(k);
    if (!p) {
      p = new Map();
      this.progress.set(k, p);
    }
    return p;
  }

  private publicUser(u: MockUser): User {
    return { id: u.id, name: u.name, is_admin: u.is_admin };
  }

  private publicAccount(a: MockAccount): Account {
    const { owner: _owner, ...rest } = a;
    void _owner;
    return { ...rest, active_connections: a.max_connections === null ? null : [...this.streams.values()].filter((s) => s.owner === a.owner).length };
  }

  private cookieUser(req: Req): MockUser | null {
    const m = /(?:^|;\s*)os_session=([^;]+)/.exec(req.headers.cookie ?? "");
    const uid = m ? this.sessions.get(m[1]!) : undefined;
    const u = uid ? this.users.find((x) => x.id === uid) : undefined;
    if (!u || u.disabled) return null;
    u.last_seen_at = nowS();
    return u;
  }

  private startSession(res: ServerResponse, u: MockUser): void {
    const token = id();
    this.sessions.set(token, u.id);
    res.setHeader("Set-Cookie", `os_session=${token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`);
  }

  private account(u: MockUser, aid: string): MockAccount {
    const a = this.accounts.find((x) => x.id === aid && x.owner === u.id);
    if (!a) throw fail(404, "not_found", "This provider isn't available any more.");
    return a;
  }

  private channel(key: string): MockChannel {
    const c = this.cat.channels.find((x) => x.key === key);
    if (!c) throw fail(404, "not_found", "This channel isn't available any more.");
    return c;
  }

  private publicChannel(c: MockChannel): Channel {
    return { key: c.key, name: c.name, number: c.number, logo: c.logo, categories: c.categories, archive_days: c.archive_days };
  }

  /** The films of an M3U playlist (its VOD entries). */
  private playlistMovies(): Catalogue["movies"] {
    return this.cat.movies.filter((_, i) => i % 36 === 7);
  }

  private publicMovie(m: Catalogue["movies"][number]): Movie {
    return { id: m.id, name: m.name, poster: m.poster, year: m.year, rating: m.rating, added: m.added };
  }

  private publicSeries(s: Catalogue["series"][number]): Series {
    return { id: s.id, name: s.name, poster: s.poster, year: s.year, rating: s.rating, updated: s.updated };
  }

  private validName(name: unknown): string {
    if (typeof name !== "string" || !/^[A-Za-z0-9._-]{2,32}$/.test(name)) {
      throw fail(400, "invalid_name", "Use 2 to 32 letters, digits, dots, dashes or underscores.");
    }
    if (this.users.some((u) => u.name.toLowerCase() === name.toLowerCase())) {
      throw fail(409, "name_taken", "That name is already taken.");
    }
    return name;
  }

  private validPassword(pw: unknown): string {
    if (typeof pw !== "string" || [...pw].length < 10) throw fail(400, "weak_password", "Use at least 10 characters.");
    return pw;
  }

  private checkProvider(server: string | undefined, username: string | undefined): void {
    if (server !== undefined) {
      let u: URL;
      try {
        u = new URL(server);
      } catch {
        throw fail(400, "invalid_server", "Enter an address that starts with http:// or https://.");
      }
      if (u.protocol !== "http:" && u.protocol !== "https:") throw fail(400, "invalid_server", "Enter an address that starts with http:// or https://.");
      if (/unreachable/i.test(u.hostname)) throw fail(502, "provider_unreachable", "We couldn't reach this server. Check the address, or try again later.");
      if (/refuse/i.test(u.hostname) || username === "wrong") throw fail(400, "provider_rejected", "Your provider didn't accept this username and password.");
      if (/^(localhost|127\.|10\.|192\.168\.)/.test(u.hostname)) throw fail(400, "blocked_address", "This server can't use addresses on a private network.");
    }
  }

  // ---- routing

  handle = async (req: Req, res: ServerResponse, next: Next): Promise<void> => {
    const url = new URL(req.url ?? "/", "http://mock.local");
    if (!url.pathname.startsWith("/api/")) return next();
    try {
      if (req.method !== "GET" && req.method !== "HEAD" && req.headers["x-opensmarters"] !== "1") {
        throw fail(403, "csrf", "This request is missing its X-Opensmarters header.");
      }
      if (req.method === "POST" || req.method === "PUT" || req.method === "PATCH") req.body = await readJson(req);
      // A little latency makes loading states visible.
      if (!url.pathname.startsWith("/api/streams/") && !url.pathname.startsWith("/api/img/")) await sleep(80 + Math.random() * 120);
      await this.route(req, res, url);
    } catch (e) {
      if (e instanceof ApiFail) {
        if (e.retryAfter) res.setHeader("Retry-After", String(e.retryAfter));
        return json(res, e.status, { error: { code: e.code, message: e.message } });
      }
      console.error("[mock]", e);
      json(res, 500, { error: { code: "internal", message: "Something went wrong on the server." } });
    }
  };

  private async route(req: Req, res: ServerResponse, url: URL): Promise<void> {
    const p = url.pathname;
    const m = req.method ?? "GET";
    const body = (req.body ?? {}) as Record<string, unknown>;
    const q = url.searchParams;

    // Dev only.
    if (p === "/api/__mock/reset" && m === "POST") {
      this.reset(body["state"] === "fresh" ? "fresh" : "ready");
      return json(res, 200, { setup_code: this.setupCode });
    }

    if (p === "/api/status" && m === "GET") {
      const u = this.cookieUser(req);
      return json(res, 200, { version: VERSION, setup_required: this.users.length === 0, user: u ? this.publicUser(u) : null });
    }
    if (p === "/api/setup" && m === "POST") {
      if (this.users.length > 0) throw fail(409, "already_set_up", "This server is already set up. Sign in instead.");
      if (String(body["setup_code"] ?? "").trim().toUpperCase() !== this.setupCode) {
        throw fail(403, "bad_setup_code", "That setup code isn't right. Copy it from the server log.");
      }
      const name = this.validName(body["name"]);
      const pw = this.validPassword(body["password"]);
      const u: MockUser = { id: id(), name, is_admin: true, disabled: false, created_at: nowS(), last_seen_at: nowS(), password: pw };
      this.users.push(u);
      this.startSession(res, u);
      return json(res, 200, { user: this.publicUser(u) });
    }
    if (p === "/api/login" && m === "POST") {
      const name = String(body["name"] ?? "");
      const n = this.failures.get(name.toLowerCase()) ?? 0;
      if (n >= 5) throw new ApiFail(429, "rate_limited", "Too many attempts. Wait a minute and try again.", 60);
      const u = this.users.find((x) => x.name.toLowerCase() === name.toLowerCase());
      if (!u || u.disabled || u.password !== body["password"]) {
        this.failures.set(name.toLowerCase(), n + 1);
        await sleep(400);
        throw fail(401, "bad_credentials", "That name and password don't match.");
      }
      this.failures.delete(name.toLowerCase());
      this.startSession(res, u);
      return json(res, 200, { user: this.publicUser(u) });
    }
    if (p === "/api/invites/accept" && m === "POST") {
      const token = String(body["token"] ?? "");
      const exp = this.invites.get(token);
      if (!exp || exp < nowS()) throw fail(400, "invalid_invite", "This invitation has expired or was already used. Ask for a new one.");
      const name = this.validName(body["name"]);
      const pw = this.validPassword(body["password"]);
      this.invites.delete(token);
      const u: MockUser = { id: id(), name, is_admin: false, disabled: false, created_at: nowS(), last_seen_at: nowS(), password: pw };
      this.users.push(u);
      this.startSession(res, u);
      return json(res, 200, { user: this.publicUser(u) });
    }
    if (p.startsWith("/api/img/") && m === "GET") return this.image(res, decodeURIComponent(p.slice(9)));

    const u = this.cookieUser(req);
    if (!u) throw fail(401, "not_signed_in", "Your session has ended. Sign in again.");

    if (p === "/api/logout" && m === "POST") {
      for (const [k, v] of this.sessions) if (v === u.id) this.sessions.delete(k);
      res.setHeader("Set-Cookie", "os_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
      return json(res, 200, {});
    }
    if (p === "/api/password" && m === "POST") {
      if (body["current"] !== u.password) throw fail(403, "wrong_password", "Your current password isn't right.");
      u.password = this.validPassword(body["new"]);
      for (const [k, v] of this.sessions) if (v === u.id) this.sessions.delete(k);
      this.startSession(res, u);
      return json(res, 200, {});
    }
    if (p.startsWith("/api/admin/")) return this.admin(u, req, res, p, m);
    if (p === "/api/settings") {
      if (m === "GET") return json(res, 200, this.settings.get(u.id) ?? { time_format: "24h", live_format: "ts", autoplay_next: true });
      if (m === "PUT") {
        const cur = this.settings.get(u.id) ?? { time_format: "24h", live_format: "ts", autoplay_next: true };
        const s: Settings = {
          time_format: body["time_format"] === undefined ? cur.time_format : body["time_format"] === "12h" ? "12h" : "24h",
          live_format: body["live_format"] === undefined ? cur.live_format : body["live_format"] === "m3u8" ? "m3u8" : "ts",
          autoplay_next: body["autoplay_next"] === undefined ? cur.autoplay_next : body["autoplay_next"] !== false,
        };
        this.settings.set(u.id, s);
        return json(res, 200, s);
      }
    }
    if (p === "/api/about" && m === "GET") {
      return json(res, 200, { version: VERSION, source_url: "https://github.com/frozenshieldweb/opensmarters", transcode_video: "never", streams_per_user: 1, max_streams: 4 });
    }
    if (p === "/api/accounts") {
      if (m === "GET") return json(res, 200, this.accounts.filter((a) => a.owner === u.id).map((a) => this.publicAccount(a)));
      if (m === "POST") return this.addAccount(u, res, body);
    }
    const am = /^\/api\/accounts\/([^/]+)(\/.*)?$/.exec(p);
    if (am) return this.accountRoute(u, req, res, decodeURIComponent(am[1]!), am[2] ?? "", m, body, q);
    if (p === "/api/play" && m === "POST") return this.play(u, res, body as unknown as PlayRequest);
    const sm = /^\/api\/streams\/([^/]+)(?:\/([^/]+))?$/.exec(p);
    if (sm) return this.streamRoute(u, res, sm[1]!, sm[2], m);
    throw fail(404, "not_found", "There's nothing here.");
  }

  private admin(u: MockUser, _req: Req, res: ServerResponse, p: string, m: string): void {
    if (!u.is_admin) throw fail(403, "forbidden", "Only an administrator can do this.");
    if (p === "/api/admin/users" && m === "GET") {
      return json(res, 200, this.users.map(({ password: _pw, ...rest }) => (void _pw, rest)));
    }
    if (p === "/api/admin/invites" && m === "POST") {
      const token = randomBytes(18).toString("base64url");
      const expires = nowS() + 72 * 3600;
      this.invites.set(token, expires);
      return json(res, 200, { url: `http://${_req.headers.host ?? "localhost"}/invite#${token}`, expires_at: expires });
    }
    const um = /^\/api\/admin\/users\/([^/]+)(?:\/(disable|enable))?$/.exec(p);
    if (um) {
      const target = this.users.find((x) => x.id === decodeURIComponent(um[1]!));
      if (!target) throw fail(404, "not_found", "This person isn't on the server any more.");
      if (target.id === u.id) throw fail(409, "cannot_change_self", "You can't do this to your own account.");
      if (um[2] === "disable" && m === "POST") {
        target.disabled = true;
        for (const [k, v] of this.sessions) if (v === target.id) this.sessions.delete(k);
        return json(res, 200, {});
      }
      if (um[2] === "enable" && m === "POST") {
        target.disabled = false;
        return json(res, 200, {});
      }
      if (!um[2] && m === "DELETE") {
        this.users = this.users.filter((x) => x.id !== target.id);
        return json(res, 200, {});
      }
    }
    throw fail(404, "not_found", "There's nothing here.");
  }

  private addAccount(u: MockUser, res: ServerResponse, b: Record<string, unknown>): void {
    const name = String(b["name"] ?? "").trim();
    if (!name) throw fail(400, "invalid_name", "Give this provider a name.");
    let host: string;
    if (b["kind"] === "xtream") {
      this.checkProvider(String(b["server"] ?? ""), String(b["username"] ?? ""));
      if (!b["username"] || !b["password"]) throw fail(400, "missing", "Enter your username and password.");
      host = new URL(String(b["server"])).hostname;
    } else if (b["kind"] === "m3u") {
      this.checkProvider(String(b["m3u_url"] ?? ""), undefined);
      if (b["epg_url"]) this.checkProvider(String(b["epg_url"]), undefined);
      host = new URL(String(b["m3u_url"])).hostname;
    } else {
      throw fail(400, "invalid_kind", "Choose Xtream Codes or M3U playlist.");
    }
    const a: MockAccount = {
      id: id(),
      owner: u.id,
      name: name.slice(0, 64),
      kind: b["kind"] as "xtream" | "m3u",
      host,
      status: b["kind"] === "xtream" ? "active" : "unknown",
      expires_at: b["kind"] === "xtream" ? nowS() + 86400 * 90 : null,
      max_connections: b["kind"] === "xtream" ? 1 : null,
      active_connections: b["kind"] === "xtream" ? 0 : null,
    };
    this.accounts.push(a);
    json(res, 200, this.publicAccount(a));
  }

  private accountRoute(u: MockUser, _req: Req, res: ServerResponse, aid: string, rest: string, m: string, b: Record<string, unknown>, q: URLSearchParams): void {
    const a = this.account(u, aid);
    if (rest === "") {
      if (m === "PATCH") {
        if (typeof b["name"] === "string" && b["name"].trim()) a.name = b["name"].trim().slice(0, 64);
        if (typeof b["server"] === "string") {
          this.checkProvider(b["server"], typeof b["username"] === "string" ? b["username"] : undefined);
          a.host = new URL(b["server"]).hostname;
        }
        if (typeof b["m3u_url"] === "string") {
          this.checkProvider(b["m3u_url"], undefined);
          a.host = new URL(b["m3u_url"]).hostname;
        }
        return json(res, 200, this.publicAccount(a));
      }
      if (m === "DELETE") {
        this.accounts = this.accounts.filter((x) => x !== a);
        return json(res, 200, {});
      }
    }
    if (rest === "/refresh" && m === "POST") return json(res, 200, this.publicAccount(a));
    if (m !== "GET" && !rest.startsWith("/favourites/") && rest !== "/progress") throw fail(405, "method", "Not allowed.");
    const c = this.cat;
    const fav = this.favsOf(u.id, a.id);
    const prog = this.progressOf(u.id, a.id);
    const progressFor = (key: string) => {
      const pr = prog.get(key);
      return pr ? { position: pr.position, duration: pr.duration } : null;
    };
    const xtream = a.kind === "xtream";
    switch (rest) {
      case "/live/categories":
        return json(res, 200, c.liveCategories);
      case "/live/channels": {
        const cat = q.get("category") ?? "all";
        const list =
          cat === "all"
            ? c.channels
            : cat === "favourites"
              ? c.channels.filter((ch) => fav.channel.has(ch.key))
              : c.channels.filter((ch) => ch.categories.includes(cat));
        return json(res, 200, list.map((ch) => this.publicChannel(ch)));
      }
      case "/epg/now": {
        const keys = (q.get("keys") ?? "").split(",").filter(Boolean).slice(0, 200);
        const t = nowS();
        const out: Record<string, Programme[]> = {};
        for (const k of keys) {
          const ch = c.channels.find((x) => x.key === k);
          if (!ch) continue;
          // The real server leaves out channels whose guide isn't ready yet ("ask again later").
          if (Date.now() - this.startedAt < 20_000 && (ch.number ?? 0) % 7 === 5) continue;
          const progs = schedule(ch, t, t + 6 * 3600);
          const i = progs.findIndex((pr) => pr.start <= t && pr.end > t);
          out[k] = i >= 0 ? progs.slice(i, i + 2) : [];
        }
        return json(res, 200, out);
      }
      case "/movies/categories":
        return json(res, 200, xtream ? c.movieCategories : []);
      case "/series/categories":
        return json(res, 200, xtream ? c.seriesCategories : []);
      case "/movies": {
        // M3U: the playlist's VOD entries, without categories.
        if (!xtream) return json(res, 200, this.playlistMovies().map((mv) => this.publicMovie(mv)));
        const cat = q.get("category") ?? "all";
        return json(res, 200, c.movies.filter((mv) => cat === "all" || mv.category === cat).map((mv) => this.publicMovie(mv)));
      }
      case "/series": {
        if (!xtream) return json(res, 200, []);
        const cat = q.get("category") ?? "all";
        return json(res, 200, c.series.filter((s) => cat === "all" || s.category === cat).map((s) => this.publicSeries(s)));
      }
      case "/search": {
        const text = (q.get("q") ?? "").trim();
        if ([...text].length < 2) throw fail(400, "query_too_short", "Type at least 2 characters.");
        const f = fold(text);
        return json(res, 200, {
          channels: c.channels.filter((x) => fold(x.name).includes(f)).slice(0, 50).map((x) => this.publicChannel(x)),
          movies: (xtream ? c.movies : this.playlistMovies()).filter((x) => fold(x.name).includes(f)).slice(0, 50).map((x) => this.publicMovie(x)),
          series: xtream ? c.series.filter((x) => fold(x.name).includes(f)).slice(0, 50).map((x) => this.publicSeries(x)) : [],
        });
      }
      case "/favourites":
        return json(res, 200, {
          channels: c.channels.filter((x) => fav.channel.has(x.key)).map((x) => this.publicChannel(x)),
          movies: c.movies.filter((x) => fav.movie.has(x.id)).map((x) => this.publicMovie(x)),
          series: c.series.filter((x) => fav.series.has(x.id)).map((x) => this.publicSeries(x)),
        });
      case "/continue": {
        const items: ContinueItem[] = [];
        for (const [key, pr] of prog) {
          if (pr.position >= pr.duration * 0.95) continue;
          const itemId = key.split(":")[1]!;
          if (pr.kind === "movie") {
            const mv = c.movies.find((x) => x.id === itemId);
            if (mv) items.push({ kind: "movie", id: mv.id, series_id: null, name: mv.name, poster: mv.poster, position: pr.position, duration: pr.duration, updated: pr.updated });
          } else {
            const ep = c.episodes.get(itemId);
            if (ep) {
              items.push({
                kind: "episode",
                id: itemId,
                series_id: ep.series.id,
                name: `${ep.series.name} · S${ep.season} E${ep.episode.number}`,
                poster: ep.series.poster,
                position: pr.position,
                duration: pr.duration,
                updated: pr.updated,
              });
            }
          }
        }
        items.sort((x, y) => y.updated - x.updated);
        return json(res, 200, items.slice(0, 20));
      }
      case "/progress": {
        if (m !== "PUT") throw fail(405, "method", "Not allowed.");
        const kind = b["kind"] === "episode" ? "episode" : "movie";
        const key = `${kind}:${String(b["id"])}`;
        prog.set(key, {
          kind,
          series_id: typeof b["series_id"] === "string" ? b["series_id"] : null,
          position: Math.max(0, Number(b["position"]) || 0),
          duration: Math.max(1, Number(b["duration"]) || 1),
          updated: nowS(),
        });
        return json(res, 200, {});
      }
    }
    const fm = /^\/favourites\/(channel|movie|series)\/(.+)$/.exec(rest);
    if (fm) {
      const set = fav[fm[1] as "channel" | "movie" | "series"];
      if (m === "PUT") set.add(decodeURIComponent(fm[2]!));
      else if (m === "DELETE") set.delete(decodeURIComponent(fm[2]!));
      else throw fail(405, "method", "Not allowed.");
      return json(res, 200, {});
    }
    const em = /^\/epg\/channel\/(.+)$/.exec(rest);
    if (em) {
      const ch = this.channel(decodeURIComponent(em[1]!));
      const from = q.has("from") ? Number(q.get("from")) : nowS() - 1800;
      const to = q.has("to") ? Number(q.get("to")) : nowS() + 12 * 3600;
      if (!Number.isFinite(from) || !Number.isFinite(to) || to <= from || to - from > 48 * 3600) {
        throw fail(400, "bad_range", "Ask for at most 48 hours of guide.");
      }
      return json(res, 200, schedule(ch, from, to));
    }
    const mm = /^\/movies\/(.+)$/.exec(rest);
    if (mm) {
      const mv = (xtream ? c.movies : this.playlistMovies()).find((x) => x.id === decodeURIComponent(mm[1]!));
      if (!mv) throw fail(404, "not_found", "This film isn't available any more.");
      return json(res, 200, { ...mv.detail, progress: progressFor(`movie:${mv.id}`) });
    }
    const sm = /^\/series\/(.+)$/.exec(rest);
    if (sm && xtream) {
      const s = c.series.find((x) => x.id === decodeURIComponent(sm[1]!));
      if (!s) throw fail(404, "not_found", "This series isn't available any more.");
      return json(res, 200, {
        ...s.detail,
        seasons: s.seasons.map((se) => ({
          number: se.number,
          name: se.name,
          episodes: se.episodes.map((e): Episode => ({ ...e, progress: progressFor(`episode:${e.id}`) })),
        })),
      });
    }
    throw fail(404, "not_found", "There's nothing here.");
  }

  private play(u: MockUser, res: ServerResponse, b: PlayRequest): void {
    const acct = this.account(u, String(b.account));
    if (!this.media) throw fail(503, "ffmpeg_missing", "The mock has no media: install ffmpeg and restart the dev server.");
    if (acct.kind === "m3u" && (b.kind === "episode" || b.kind === "catchup")) {
      throw fail(400, "invalid_request", "This provider is a playlist: it has no series or catch-up.");
    }
    // One stream per user: a new one replaces the previous one (which then answers 409).
    for (const [k, s] of this.streams) {
      if (s.owner === u.id) {
        this.streams.delete(k);
        this.replaced.set(k, u.id);
      }
    }
    const sid = id();
    if (b.kind === "live") {
      const ch = this.channel(String(b.id));
      if (ch.behaviour === "limit") throw fail(503, "limit_reached", "The provider allows 1 stream at a time. Stop watching on your other device, or wait a minute.");
      if (ch.behaviour === "refused") throw fail(502, "provider_error", "Your provider refused the stream. Too many connections may be open; wait a minute and try again.");
      if (ch.behaviour === "hevc" && !b.hevc) throw fail(415, "unsupported_format", "This channel uses a video format (HEVC) your browser can't play, and this server doesn't convert video.");
      this.streams.set(sid, { sid, owner: u.id, live: true, duration: 0, firstSeg: 0, created: Date.now(), dropsAfter: ch.behaviour === "drops" ? 20_000 : null });
      return json(res, 200, { stream: sid, url: `/api/streams/${sid}/index.m3u8`, live: true, duration: null, start: 0 });
    }
    let duration: number;
    if (b.kind === "movie") {
      const mv = this.cat.movies.find((x) => x.id === String(b.id));
      if (!mv) throw fail(404, "not_found", "This film isn't available any more.");
      duration = mv.detail.duration_secs ?? 5400;
    } else if (b.kind === "episode") {
      const ep = this.cat.episodes.get(String(b.id));
      if (!ep) throw fail(404, "not_found", "This episode isn't available any more.");
      duration = ep.episode.duration_secs ?? 2700;
    } else {
      throw fail(400, "unsupported", "Catch-up isn't available in this mock.");
    }
    const firstSeg = Math.max(0, Math.min(Math.floor((Number(b.start) || 0) / SEGMENT_SECS), Math.floor(duration / SEGMENT_SECS) - 1));
    this.streams.set(sid, { sid, owner: u.id, live: false, duration, firstSeg, created: Date.now(), dropsAfter: null });
    json(res, 200, { stream: sid, url: `/api/streams/${sid}/index.m3u8`, live: false, duration, start: firstSeg * SEGMENT_SECS });
  }

  private streamRoute(u: MockUser, res: ServerResponse, sid: string, file: string | undefined, m: string): void {
    const s = this.streams.get(sid);
    if (m === "DELETE" && !file) {
      if (s && s.owner === u.id) this.streams.delete(sid);
      return json(res, 200, {});
    }
    if (!s && this.replaced.get(sid) === u.id) {
      throw fail(409, "stream_replaced", "This stream stopped because a newer one started for your account.");
    }
    if (!s || s.owner !== u.id || !file || m !== "GET") throw fail(404, "not_found", "This stream has ended.");
    if (s.dropsAfter !== null && Date.now() - s.created > s.dropsAfter) {
      this.streams.delete(sid);
      throw fail(404, "not_found", "This stream has ended.");
    }
    res.setHeader("Cache-Control", "no-store");
    const playlists: Record<string, () => string> = s.live
      ? { "index.m3u8": () => livePlaylist(s.created, Date.now()) }
      : {
          "index.m3u8": () => vodMaster(),
          "video.m3u8": () => vodPlaylist(s.duration, s.firstSeg, "v", converted(s)),
          "audio-en.m3u8": () => vodPlaylist(s.duration, s.firstSeg, "a", converted(s)),
          "audio-fr.m3u8": () => vodPlaylist(s.duration, s.firstSeg, "f", converted(s)),
          "subs-en.m3u8": () => vodPlaylist(s.duration, s.firstSeg, "s", converted(s)),
        };
    const pl = playlists[file];
    if (pl) {
      res.statusCode = 200;
      res.setHeader("Content-Type", "application/vnd.apple.mpegurl");
      res.end(pl());
      return;
    }
    const vtt = /^s(\d+)\.vtt$/.exec(file);
    if (vtt && !s.live) {
      res.statusCode = 200;
      res.setHeader("Content-Type", "text/vtt; charset=utf-8");
      res.end(subtitleSegment(Number(vtt[1])));
      return;
    }
    const path = this.media ? segmentFile(this.media, file) : null;
    if (!path || !existsSync(path)) throw fail(404, "not_found", "No such segment.");
    res.statusCode = 200;
    res.setHeader("Content-Type", "video/mp2t");
    createReadStream(path).pipe(res);
  }

  private image(res: ServerResponse, token: string): void {
    const c = this.cat;
    const svg = imageFor(token, (kind, iid) => {
      if (kind === "l") return c.channels.find((x) => x.key === iid)?.name ?? null;
      if (kind === "p" || kind === "b") return c.movies.find((x) => x.id === iid)?.name ?? null;
      if (kind === "q" || kind === "c") return c.series.find((x) => x.id === iid)?.name ?? null;
      if (kind === "s") return c.episodes.get(iid)?.episode.title ?? null;
      return null;
    });
    if (!svg) throw fail(404, "not_found", "No such image.");
    res.statusCode = 200;
    res.setHeader("Content-Type", "image/svg+xml");
    res.setHeader("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'");
    res.setHeader("Cache-Control", "private, max-age=3600");
    res.end(svg);
  }
}

/** Segments "converted" so far: the real server's ffmpeg runs ahead of playback; here 4× real time. */
function converted(s: Stream): number {
  return 6 + Math.floor(((Date.now() - s.created) / 1000) * 4 / SEGMENT_SECS);
}

function fold(s: string): string {
  return s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

function json(res: ServerResponse, status: number, data: unknown): void {
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.setHeader("Cache-Control", "no-store");
  res.end(JSON.stringify(data));
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const c of req) {
    size += (c as Buffer).length;
    if (size > 64 * 1024) throw fail(413, "too_large", "That request is too large.");
    chunks.push(c as Buffer);
  }
  if (!chunks.length) return {};
  try {
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    throw fail(400, "bad_json", "That request isn't valid JSON.");
  }
}
