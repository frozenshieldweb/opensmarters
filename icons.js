// SPDX-License-Identifier: GPL-3.0-or-later
// Phosphor icons (MIT; licence in fonts/LICENSE-Phosphor.txt), the app's own icon set
// (DESIGN.md §4.8). Code points from crates/app/src/ui/icons/glyphs.rs. The two subset
// fonts hold exactly these glyphs; add a code point to tools/assets/launch.toml
// ("site assets" block) and rebuild before using a new one here.
import { h } from "./h.js";

export const GLYPH = {
  ARROW_CLOCKWISE: 0xe036,
  ARROW_COUNTER_CLOCKWISE: 0xe038,
  ARROW_UP_RIGHT: 0xe092,
  ARROWS_OUT: 0xe0a2,
  BACKSPACE: 0xe0ae,
  BROADCAST: 0xe0f2,
  CARET_DOWN: 0xe136,
  CARET_LEFT: 0xe138,
  CARET_RIGHT: 0xe13a,
  CARET_UP: 0xe13c,
  CHECK: 0xe182,
  CHECK_CIRCLE: 0xe184,
  CLOCK_COUNTER_CLOCKWISE: 0xe1a0,
  SUBTITLES: 0xe1a8,
  CODE: 0xe1bc,
  COPY: 0xe1ca,
  DOWNLOAD_SIMPLE: 0xe20c,
  EYE_SLASH: 0xe224,
  GEAR_SIX: 0xe272,
  HEART: 0xe2a8,
  HOUSE: 0xe2c2,
  INFO: 0xe2ce,
  KEY: 0xe2d6,
  KEYBOARD: 0xe2d8,
  LINK: 0xe2e2,
  LOCK_SIMPLE: 0xe308,
  MAGNIFYING_GLASS: 0xe30c,
  MOON: 0xe330,
  PAUSE: 0xe39e,
  PLAY: 0xe3d0,
  RECORD: 0xe3ee,
  SHIELD_CHECK: 0xe40c,
  SPEAKER_HIGH: 0xe44a,
  SPEAKER_SLASH: 0xe45a,
  SQUARES_FOUR: 0xe464,
  WARNING: 0xe4e0,
  X: 0xe4f6,
  MONITOR_PLAY: 0xe58c,
  ROWS: 0xe5a2,
  CPU: 0xe610,
  FRAME_CORNERS: 0xe626,
  GAUGE: 0xe628,
  REWIND: 0xe6a8,
  FILM_STRIP: 0xe792,
  FILE_ZIP: 0xe958,
  TELEVISION_SIMPLE: 0xeae6,
  TERMINAL_WINDOW: 0xeae8,
};

// Glyphs present in the Fill subset (selected / "on" states).
const FILLED = new Set([0xe2a8, 0xe308, 0xe39e, 0xe3d0, 0xe3ee, 0xe44a, 0xe5a2, 0xeae6]);

/** A decorative icon: `icon("PLAY", { fill: true, cls: "x" })`. */
export function icon(name, { fill = false, cls = "" } = {}) {
  const cp = GLYPH[name];
  if (cp === undefined) throw new Error(`unknown icon ${name}`);
  if (fill && !FILLED.has(cp)) throw new Error(`icon ${name} has no Fill glyph in the subset`);
  const c = `i${fill ? " i-fill" : ""}${cls ? ` ${cls}` : ""}`;
  return h("span", { class: c, "aria-hidden": "true" }, String.fromCodePoint(cp));
}

/** Changes an existing icon element's glyph (runtime). */
export function setIcon(el, name, fill = false) {
  el.textContent = String.fromCodePoint(GLYPH[name]);
  el.classList.toggle("i-fill", fill);
}
