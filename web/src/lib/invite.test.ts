// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { inviteTokenFrom } from "./invite";

const at = (url: string) => {
  const u = new URL(url, "https://tv.example");
  return inviteTokenFrom({ pathname: u.pathname, search: u.search, hash: u.hash });
};

describe("inviteTokenFrom", () => {
  it("reads the server's /invite#<token> links", () => {
    expect(at("/invite#7H1xmlurJk2MD0mQ0bFvp-yThwaOnNipQHx7RWybaCE")).toBe("7H1xmlurJk2MD0mQ0bFvp-yThwaOnNipQHx7RWybaCE");
  });

  it("accepts the other spellings", () => {
    expect(at("/app/invite#invite=abcdefgh12")).toBe("abcdefgh12");
    expect(at("/?invite=abcdefgh12")).toBe("abcdefgh12");
    expect(at("/invite/abcdefgh12")).toBe("abcdefgh12");
  });

  it("ignores other pages' fragments and bad tokens", () => {
    expect(at("/app/live#abcdefghijklmnopqrstuvwxyz")).toBeNull();
    expect(at("/invite#short")).toBeNull();
    expect(at("/invite#has%20space")).toBeNull();
    expect(at("/")).toBeNull();
  });
});
