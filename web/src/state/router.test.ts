// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { parentOf, parseUrl, sectionOf, urlOf, type Route } from "./router";

describe("router URLs", () => {
  const routes: Route[] = [
    { name: "home" },
    { name: "live" },
    { name: "live", category: "news & weather" },
    { name: "catchup", account: "a1", key: "1001" },
    { name: "catchup", account: "a/2", key: "m3u:0f3a#2" },
    { name: "guide", category: "7" },
    { name: "movies" },
    { name: "movies", category: "m-drama" },
    { name: "movie", id: "10 001" },
    { name: "series" },
    { name: "show", id: "50000" },
    { name: "search", q: "glacier live" },
    { name: "favourites" },
    { name: "settings" },
    { name: "settings", section: "users" },
    { name: "provider-new" },
    { name: "provider-edit", id: "a1" },
  ];

  it("round-trips every route through its URL", () => {
    for (const r of routes) {
      const u = new URL(urlOf(r), "http://x");
      expect(parseUrl(u.pathname, u.search)).toEqual(r);
    }
  });

  it("sends unknown and signed-out paths Home", () => {
    expect(parseUrl("/")).toEqual({ name: "home" });
    expect(parseUrl("/app/watch")).toEqual({ name: "home" });
    expect(parseUrl("/app/nonsense/1")).toEqual({ name: "home" });
    expect(parseUrl("/invite")).toEqual({ name: "home" });
    expect(parseUrl("/app/settings/secrets")).toEqual({ name: "settings" });
    expect(parseUrl("/app/catchup/a1")).toEqual({ name: "live" });
  });

  it("knows where Back goes from a screen opened directly", () => {
    expect(parentOf({ name: "movie", id: "1" })).toEqual({ name: "movies" });
    expect(parentOf({ name: "show", id: "1" })).toEqual({ name: "series" });
    expect(parentOf({ name: "catchup", account: "a", key: "1" })).toEqual({ name: "live" });
    expect(parentOf({ name: "provider-edit", id: "a" })).toEqual({ name: "settings", section: "providers" });
    expect(parentOf({ name: "live" })).toEqual({ name: "home" });
    expect(parentOf({ name: "home" })).toBeNull();
  });

  it("maps screens to their side-rail section", () => {
    expect(sectionOf({ name: "movie", id: "1" })).toBe("movies");
    expect(sectionOf({ name: "show", id: "1" })).toBe("series");
    expect(sectionOf({ name: "catchup", account: "a", key: "1" })).toBe("live");
    expect(sectionOf({ name: "provider-new" })).toBe("settings");
    expect(sectionOf({ name: "watch" })).toBeNull();
  });
});
