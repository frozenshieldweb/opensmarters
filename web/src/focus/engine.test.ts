// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from the tests of crates/app/src/focus/engine.rs, plus the grid stepping used by the
// virtualised lists.
import { describe, expect, it } from "vitest";
import {
  FocusMemory,
  bestInDirection,
  fallback,
  gridStep,
  isInDirection,
  navigate,
  rect as r,
  type Candidate,
} from "./engine";

const cand = (id: string, rect = r(0, 0, 10, 10), scope = "s", group = "content"): Candidate<string> => ({
  id,
  rect,
  scope,
  group,
});

describe("geometry", () => {
  it("grid prefers the aligned neighbour", () => {
    const cs: Candidate<string>[] = [];
    for (let row = 0; row < 2; row++)
      for (let col = 0; col < 3; col++) cs.push(cand(`${row}-${col}`, r(col * 116, row * 166, 100, 150), "grid"));
    const from = cs[1]!.rect;
    expect(bestInDirection(from, "down", cs)?.id).toBe("1-1");
    expect(bestInDirection(from, "right", cs)?.id).toBe("0-2");
    expect(bestInDirection(from, "left", cs)?.id).toBe("0-0");
    expect(bestInDirection(from, "up", cs)).toBeNull();
  });

  it("an item in the beam beats a closer diagonal one", () => {
    const from = r(0, 0, 100, 40);
    const cs = [cand("diag", r(120, 200, 100, 40)), cand("far", r(400, 10, 100, 40))];
    expect(bestInDirection(from, "right", cs)?.id).toBe("far");
  });

  it("a sideways overlap is not up", () => {
    const field = r(256, 106, 200, 52);
    const railItem = r(12, 87, 208, 48);
    expect(isInDirection(field, railItem, "up")).toBe(false);
    expect(isInDirection(field, railItem, "left")).toBe(true);
  });

  it("overlapping items are not in a direction", () => {
    const from = r(0, 0, 100, 100);
    expect(isInDirection(from, r(10, 10, 50, 50), "right")).toBe(false);
    expect(isInDirection(from, r(60, 0, 100, 100), "right")).toBe(true);
    expect(isInDirection(from, r(60, 0, 100, 100), "left")).toBe(false);
  });
});

describe("navigate", () => {
  it("moves inside the scope first", () => {
    const a = cand("a", r(0, 0, 100, 40));
    const b = cand("b", r(0, 60, 100, 40));
    const other = cand("o", r(0, 50, 100, 40), "other");
    expect(navigate(a, "down", [a, other, b])).toBe("b");
  });

  it("crossing scopes restores the scope's memory", () => {
    const top = [0, 1, 2, 3].map((i) => cand(`t${i}`, r(i * 120, 0, 100, 60), "rail-top"));
    const bot = [0, 1, 2, 3].map((i) => cand(`b${i}`, r(i * 120, 100, 100, 60), "rail-bot"));
    const all = [...top, ...bot];
    const mem = new FocusMemory<string>();
    mem.record(bot[2]!);
    mem.record(top[0]!);
    expect(navigate(top[0]!, "down", all, mem)).toBe("b2");
  });

  it("crossing groups restores the group's memory", () => {
    const rail = cand("nav", r(0, 0, 200, 48), "nav", "rail");
    const near = cand("near", r(250, 0, 100, 48), "cats");
    const far = cand("far", r(400, 300, 100, 48), "list");
    const mem = new FocusMemory<string>();
    mem.record(far);
    mem.record(rail);
    expect(navigate(rail, "right", [rail, near, far], mem)).toBe("far");
  });

  it("ignores a remembered item that isn't in the pressed direction", () => {
    const rail = cand("nav", r(300, 0, 200, 48), "nav", "rail");
    const left = cand("left", r(0, 0, 100, 48), "cats");
    const mem = new FocusMemory<string>();
    mem.record(left);
    // The memory of "content" points left of the rail; pressing right finds nothing there.
    expect(navigate(rail, "right", [rail, left], mem)).toBeNull();
  });

  it("up and down never jump between the rail and the page", () => {
    const railHome = cand("nav-home", r(0, 100, 200, 48), "rail", "rail");
    const railSettings = cand("nav-settings", r(0, 900, 200, 48), "rail", "rail");
    const last = cand("last", r(300, 600, 120, 44), "page");
    const all = [railHome, railSettings, last];
    expect(navigate(last, "down", all)).toBeNull();
    expect(["nav-home", "nav-settings"]).toContain(navigate(last, "left", all));
  });

  it("up reaches a top bar above the page (compact layout), never a rail beside it", () => {
    const menu = cand("menu", r(4, 6, 44, 44), "topbar", "rail");
    const first = cand("first", r(16, 120, 358, 64), "list");
    expect(navigate(first, "up", [menu, first])).toBe("menu");
    const railItem = cand("nav", r(12, 40, 200, 48), "rail-nav", "rail");
    const card = cand("card", r(280, 300, 168, 252), "posters");
    expect(navigate(card, "up", [railItem, card])).toBeNull();
    // A chip far right of the menu button still reaches it: the top bar spans the page.
    const chip = cand("chip", r(240, 70, 80, 36), "chips");
    const barMenu = { ...menu, groupRect: r(0, 0, 390, 56) };
    expect(navigate(chip, "up", [menu, chip])).toBeNull();
    expect(navigate(chip, "up", [barMenu, chip])).toBe("menu");
  });

  it("returns null at an edge", () => {
    const a = cand("a", r(0, 0, 100, 40));
    expect(navigate(a, "up", [a])).toBeNull();
  });
});

describe("fallback", () => {
  it("prefers autofocus, then top-left outside the avoided group", () => {
    const rail = cand("nav-home", r(0, 0, 200, 48), "rail", "rail");
    const a = cand("a", r(300, 100, 100, 40), "body");
    const b = cand("b", r(300, 20, 100, 40), "body");
    expect(fallback([rail, a, b], "rail")?.id).toBe("b");
    const auto = { ...cand("auto", r(300, 500, 100, 40), "body"), autofocus: true };
    expect(fallback([a, b, auto], "rail")?.id).toBe("auto");
  });

  it("uses the avoided group when nothing else is focusable", () => {
    const rail = cand("nav-home", r(0, 0, 200, 48), "rail", "rail");
    expect(fallback([rail], "rail")?.id).toBe("nav-home");
    expect(fallback([], "rail")).toBeNull();
  });
});

describe("memory", () => {
  it("forgets removed items", () => {
    const m = new FocusMemory<string>();
    const a = cand("a");
    m.record(a);
    expect(m.scopes.get("s")).toBe("a");
    m.forget("a");
    expect(m.scopes.size).toBe(0);
    expect(m.groups.size).toBe(0);
  });
});

describe("gridStep", () => {
  it("steps through a list", () => {
    expect(gridStep(0, 5, 1, "down")).toBe(1);
    expect(gridStep(4, 5, 1, "down")).toBeNull();
    expect(gridStep(0, 5, 1, "up")).toBeNull();
    expect(gridStep(2, 5, 1, "left")).toBeNull();
    expect(gridStep(0, 0, 1, "down")).toBeNull();
  });

  it("steps through a grid with a partial last row", () => {
    // 4 columns, 10 items: rows [0..3], [4..7], [8, 9].
    expect(gridStep(1, 10, 4, "right")).toBe(2);
    expect(gridStep(3, 10, 4, "right")).toBeNull();
    expect(gridStep(4, 10, 4, "left")).toBeNull();
    expect(gridStep(5, 10, 4, "up")).toBe(1);
    expect(gridStep(5, 10, 4, "down")).toBe(9);
    expect(gridStep(6, 10, 4, "down")).toBe(9);
    expect(gridStep(9, 10, 4, "right")).toBeNull();
    expect(gridStep(9, 10, 4, "down")).toBeNull();
  });
});
