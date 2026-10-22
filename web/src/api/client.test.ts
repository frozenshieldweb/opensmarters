// SPDX-License-Identifier: GPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, onUnauthorized } from "./client";

interface Call {
  url: string;
  init: RequestInit;
}

function fakeFetch(status: number, body: unknown): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal("fetch", async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return new Response(body === undefined ? "" : JSON.stringify(body), { status });
  });
  return calls;
}

afterEach(() => vi.unstubAllGlobals());

describe("api client", () => {
  it("sends X-Opensmarters: 1 and JSON on every non-GET request, never on GET", async () => {
    const calls = fakeFetch(200, {});
    await api.addFavourite("a1", "movie", "10 0/1");
    await api.status();
    await api.login("mira", "pw-long-enough");
    const [put, get, post] = calls;
    expect(put!.init.method).toBe("PUT");
    expect(put!.url).toBe("/api/accounts/a1/favourites/movie/10%200%2F1");
    expect((put!.init.headers as Record<string, string>)["X-Opensmarters"]).toBe("1");
    expect((get!.init.headers as Record<string, string>)["X-Opensmarters"]).toBeUndefined();
    expect((post!.init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
    expect(JSON.parse(post!.init.body as string)).toEqual({ name: "mira", password: "pw-long-enough" });
    expect(post!.init.credentials).toBe("same-origin");
  });

  it("turns error bodies into ApiError with the server's message", async () => {
    fakeFetch(503, { error: { code: "limit_reached", message: "The provider allows 1 stream." } });
    const e = await api.play({ account: "a1", kind: "live", id: "ch1", hevc: false }).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect((e as ApiError).status).toBe(503);
    expect((e as ApiError).code).toBe("limit_reached");
    expect((e as ApiError).message).toBe("The provider allows 1 stream.");
  });

  it("falls back to a plain message when the body isn't an error object", async () => {
    fakeFetch(502, "<html>bad gateway</html>");
    const e = (await api.accounts().catch((x: unknown) => x)) as ApiError;
    expect(e.code).toBe("http_502");
    expect(e.message).toMatch(/provider/i);
  });

  it("reports a 401 to the session, except from the sign-in forms", async () => {
    const seen: string[] = [];
    const off = onUnauthorized((e) => seen.push(e.code));
    fakeFetch(401, { error: { code: "not_signed_in", message: "Sign in again." } });
    await api.accounts().catch(() => {});
    fakeFetch(401, { error: { code: "bad_credentials", message: "No match." } });
    await api.login("x", "y").catch(() => {});
    off();
    expect(seen).toEqual(["not_signed_in"]);
  });

  it("joins epg keys and sends series_id for episodes", async () => {
    const calls = fakeFetch(200, {});
    await api.epgNow("a1", ["k 1", "k,2"]);
    await api.play({ account: "a1", kind: "episode", id: "e1", series_id: "s1", hevc: true });
    expect(calls[0]!.url).toBe("/api/accounts/a1/epg/now?keys=k%201,k%2C2");
    expect(JSON.parse(calls[1]!.init.body as string)).toMatchObject({ kind: "episode", series_id: "s1", hevc: true });
  });
});
