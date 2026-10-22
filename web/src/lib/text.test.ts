// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { cap, fold, hue, monogram } from "./text";
import { clock, dot, fraction, remaining, runtime, timecode, timeRange } from "./format";

describe("text", () => {
  it("caps by code point", () => {
    expect(cap("  hello  ")).toBe("hello");
    expect(cap("abcdef", 4)).toBe("abc…");
    expect(cap("🚀🚀🚀🚀🚀", 3)).toBe("🚀🚀…");
    expect(cap(null)).toBe("");
  });

  it("makes monograms from letters and digits only", () => {
    expect(monogram("Glacier Live")).toBe("GL");
    expect(monogram("Oakhaven")).toBe("OA");
    expect(monogram("🎬 Reel One")).toBe("RO");
    expect(monogram("Κύμα TV")).toBe("ΚT");
    expect(monogram("123 Numbers First")).toBe("1N");
    expect(monogram("  ")).toBe("");
  });

  it("picks a stable hue", () => {
    expect(hue("Glacier Live")).toBe(hue("Glacier Live"));
    expect(hue("x")).toBeGreaterThanOrEqual(0);
    expect(hue("x")).toBeLessThan(6);
  });

  it("folds case and accents", () => {
    expect(fold("Ünïcödé Çhännél")).toBe("unicode channel");
  });
});

describe("format", () => {
  it("formats positions and running times", () => {
    expect(timecode(0)).toBe("0:00");
    expect(timecode(65)).toBe("1:05");
    expect(timecode(3733)).toBe("1:02:13");
    expect(runtime(6720)).toBe("1 h 52");
    expect(runtime(2700)).toBe("45 min");
    expect(runtime(null)).toBe("");
    expect(remaining(39 * 60)).toBe("39 min left");
  });

  it("formats clocks in both formats", () => {
    const ts = new Date(2026, 9, 21, 15, 5).getTime() / 1000;
    expect(clock(ts)).toBe("15:05");
    expect(clock(ts, "12h")).toBe("3:05 pm");
    const midnight = new Date(2026, 9, 21, 0, 30).getTime() / 1000;
    expect(clock(midnight, "12h")).toBe("12:30 am");
    expect(timeRange(ts, ts + 2400)).toBe("15:05–15:45");
  });

  it("clamps fractions and joins parts", () => {
    expect(fraction(0, 100, 50)).toBe(0.5);
    expect(fraction(0, 100, 150)).toBe(1);
    expect(fraction(100, 100, 150)).toBe(0);
    expect(dot("2026", null, "", "Drama")).toBe("2026 · Drama");
  });
});
