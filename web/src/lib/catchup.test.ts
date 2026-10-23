// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import type { Programme } from "../api/types";
import { canCatchUp, civil, dayLabel, groupByDay, minutesOf } from "./catchup";

const utc = (d: number, h: number, m = 0) => Date.UTC(2026, 9, d, h, m) / 1000;
const prog = (start: number, end: number, title = ""): Programme => ({ start, end, title: title || `${start}`, description: null });

/** `[label, ["DD HH:MM" on the viewer's clock, …]]` per day. */
function summary(days: ReturnType<typeof groupByDay>, offsetMin: number): [string, string[]][] {
  return days.map((d) => [
    d.label,
    d.programmes.map((p) => {
      const t = new Date((p.start + offsetMin * 60) * 1000);
      return `${String(t.getUTCDate()).padStart(2, "0")} ${String(t.getUTCHours()).padStart(2, "0")}:${String(t.getUTCMinutes()).padStart(2, "0")}`;
    }),
  ]);
}

describe("catch-up days", () => {
  it("labels today, yesterday, then weekday and date", () => {
    const today = civil(utc(20, 12), 0);
    expect(dayLabel(civil(utc(20, 1), 0), today)).toBe("Today");
    expect(dayLabel(civil(utc(19, 23), 0), today)).toBe("Yesterday");
    expect(dayLabel(civil(utc(21, 0), 0), today)).toBe("Tomorrow");
    expect(dayLabel(civil(utc(18, 9), 0), today)).toBe("Sun 18 Oct");
    expect(dayLabel(civil(utc(5, 9), 0), today)).toBe("Mon 5 Oct");
  });

  it("groups by the viewer's day, newest day and programme first", () => {
    // A viewer in Chicago in October (UTC−5); now: 20 Oct 15:00Z = 10:00 local.
    const off = -5 * 60;
    const now = utc(20, 15);
    const days = groupByDay(
      [
        prog(utc(19, 3), utc(19, 4)), // 18th 22:00 local
        prog(utc(20, 5), utc(20, 6)), // 20th 00:00 local
        prog(utc(19, 23), utc(20, 3)), // 19th 18:00 local
        prog(utc(20, 3), utc(20, 4, 30)), // 19th 22:00 local (the 20th in UTC)
        prog(utc(20, 13, 30), utc(20, 14, 30)), // 20th 08:30 local
      ],
      now,
      off,
    );
    expect(summary(days, off)).toEqual([
      ["Today", ["20 08:30", "20 00:00"]],
      ["Yesterday", ["19 22:00", "19 18:00"]],
      ["Sun 18 Oct", ["18 22:00"]],
    ]);
    expect(days.map((d) => d.key)).toEqual(["2026-10-20", "2026-10-19", "2026-10-18"]);
  });

  it("puts a programme near midnight on the viewer's day, not the UTC one", () => {
    const late = [prog(utc(24, 22, 30), utc(24, 23, 30))];
    expect(groupByDay(late, utc(25, 10), 120)[0]?.label).toBe("Today"); // 25 Oct 00:30 at UTC+2
    expect(groupByDay(late, utc(25, 10), 60)[0]?.label).toBe("Yesterday"); // 24 Oct 23:30 at UTC+1
    expect(groupByDay(late, utc(25, 10), 0)[0]?.label).toBe("Yesterday");
  });

  it("has nothing for an empty archive", () => {
    expect(groupByDay([], utc(20, 12), 0)).toEqual([]);
  });

  it("asks for whole minutes, rounded up", () => {
    expect(minutesOf({ start: 0, end: 3600 })).toBe(60);
    expect(minutesOf({ start: 0, end: 2730 })).toBe(46);
    expect(minutesOf({ start: 10, end: 10 })).toBe(1);
  });

  it("knows which guide programmes can be caught up", () => {
    const now = utc(20, 12);
    const archive = { archive_days: 2 };
    expect(canCatchUp(archive, prog(utc(20, 10), utc(20, 11)), now)).toBe(true);
    expect(canCatchUp(archive, prog(utc(20, 11, 30), utc(20, 12, 30)), now)).toBe(false); // on now
    expect(canCatchUp(archive, prog(utc(20, 13), utc(20, 14)), now)).toBe(false); // later
    expect(canCatchUp(archive, prog(utc(17, 10), utc(17, 11)), now)).toBe(false); // too old
    expect(canCatchUp({ archive_days: null }, prog(utc(20, 10), utc(20, 11)), now)).toBe(false);
    expect(canCatchUp({ archive_days: 0 }, prog(utc(20, 10), utc(20, 11)), now)).toBe(false);
  });
});
