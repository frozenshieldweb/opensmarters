// SPDX-License-Identifier: GPL-3.0-or-later
// Pure helpers behind the screens: Continue point of a series, Xtream links pasted as playlists,
// the episodes that follow (next episode).
import { describe, expect, it } from "vitest";
import type { SeriesDetail } from "../api/types";
import { continuePoint, episodesAfter } from "./series";
import { detectXtream } from "./xtream";

const ep = (id: string, n: number, progress: { position: number; duration: number } | null = null) => ({
  id,
  number: n,
  title: `Episode ${n}`,
  plot: null,
  duration_secs: 2700,
  still: null,
  progress,
});

const series = (eps1: ReturnType<typeof ep>[], eps2: ReturnType<typeof ep>[] = []): SeriesDetail => ({
  id: "s1",
  name: "Harborlight Diaries",
  poster: null,
  backdrop: null,
  year: 2013,
  rating: 7.1,
  genre: null,
  plot: null,
  cast: null,
  seasons: [
    { number: 1, name: "Season 1", episodes: eps1 },
    ...(eps2.length ? [{ number: 2, name: "Season 2", episodes: eps2 }] : []),
  ],
});

describe("continuePoint", () => {
  it("starts at the first episode when nothing was watched", () => {
    const c = continuePoint(series([ep("a", 1), ep("b", 2)]))!;
    expect(c.episode.id).toBe("a");
    expect(c.resume).toBe(0);
    expect(c.label).toBe("Play S1 · E1");
  });

  it("resumes the episode in progress", () => {
    const c = continuePoint(series([ep("a", 1, { position: 2700, duration: 2700 }), ep("b", 2, { position: 600, duration: 2700 })]))!;
    expect(c.episode.id).toBe("b");
    expect(c.resume).toBe(600);
    expect(c.label).toBe("Continue S1 · E2");
  });

  it("goes on to the next episode, across seasons, after a finished one", () => {
    const c = continuePoint(series([ep("a", 1), ep("b", 2, { position: 2690, duration: 2700 })], [ep("c", 1)]))!;
    expect(c.episode.id).toBe("c");
    expect(c.label).toBe("Continue S2 · E1");
  });

  it("offers the last episode again when everything was watched", () => {
    const c = continuePoint(series([ep("a", 1, { position: 2700, duration: 2700 })]))!;
    expect(c.label).toBe("Watch S1 · E1 again");
  });

  it("is null for a series without episodes", () => {
    expect(continuePoint(series([]))).toBeNull();
  });
});

describe("episodesAfter", () => {
  it("lists the following episodes in order, across seasons", () => {
    const next = episodesAfter(series([ep("a", 1), ep("b", 2)], [ep("c", 1)]), "a");
    expect(next.map((e) => e.id)).toEqual(["b", "c"]);
    expect(next[1]!.subtitle).toBe("S2 · E1 · Episode 1");
  });
});

describe("detectXtream", () => {
  it("recognises an Xtream Codes link pasted as a playlist", () => {
    expect(detectXtream("http://panel.example:8080/get.php?username=mira&password=s3cret&type=m3u_plus")).toEqual({
      server: "http://panel.example:8080",
      username: "mira",
      password: "s3cret",
    });
  });

  it("ignores ordinary playlist links", () => {
    expect(detectXtream("https://lists.example/playlist.m3u")).toBeNull();
    expect(detectXtream("not a url")).toBeNull();
    expect(detectXtream("http://panel.example/get.php?username=mira")).toBeNull();
  });
});
