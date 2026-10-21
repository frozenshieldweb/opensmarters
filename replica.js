// SPDX-License-Identifier: GPL-3.0-or-later
// The interactive replica (docs/LAUNCH_PROMPT.md §A4): a small HTML/CSS recreation of the
// app's Live TV, Guide and Player screens, driven by the keyboard (one focus, moved with the
// arrows) or the pointer (hover sets focus). No screenshots, no video, no sound: the
// picture is a generated warm gradient that moves only after the visitor presses Play.
// All strings come from content.js; demo names are the app's synthetic demo data.
import { h, toDOM } from "./h.js";
import { icon, setIcon } from "./icons.js";
import { fmt } from "./util.js";

const HUES = ["#4A3A43", "#3E4A44", "#3C4456", "#4B3F35", "#45394F", "#4A3838"]; // DESIGN §6.4
// Light for the generated picture, per monogram hue: [warm key light, tint, depth]. The key
// light is always warm (Afterglow); the channel's hue only tints it.
const LIGHTS = [
  ["#E6BE91", "#C9868A", "#4A2A36"],
  ["#E6BE91", "#A4B6A4", "#28342E"],
  ["#E6BE91", "#9FB4D6", "#262B42"],
  ["#F0A848", "#E6BE91", "#43301E"],
  ["#E6BE91", "#B9A3D6", "#33283F"],
  ["#F28B82", "#E6BE91", "#432626"],
];
const CATCHUP = new Set([0, 1, 5]); // channels with an archive (sage icon)
const DEMO_CH = 1; // the channel demos show when nothing was played yet

/** The app's stable hue: FNV-1a 64 of the name, mod 6 (crates/app services::stable_hash). */
function hueIndex(name) {
  let x = 0xcbf29ce484222325n;
  for (const b of new TextEncoder().encode(name)) {
    x ^= BigInt(b);
    x = (x * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return Number(x % 6n);
}
const monogram = (name) => [...name].filter((c) => /[\p{L}\p{N}]/u.test(c)).slice(0, 2).join("").toUpperCase();
const pad2 = (n) => String(n).padStart(2, "0");
const hhmm = (t) => `${pad2(new Date(t).getHours())}:${pad2(new Date(t).getMinutes())}`;
const num3 = (i) => String(i + 1).padStart(3, "0");
const MIN = 60000;
const clockShift = (s) => `−${pad2(Math.floor(s / 60))}:${pad2(s % 60)}`;

// ---------------------------------------------------------------------------- schedule

const PATTERNS = [
  [60, 45, 30, 45, 60, 30, 90, 45, 30],
  [45, 30, 60, 45, 30, 60, 45, 60, 30],
  [30, 60, 45, 90, 30, 45, 60, 30, 60],
  [90, 120, 60, 30, 45, 60, 90, 30, 45],
  [45, 45, 30, 60, 45, 30, 60, 90, 30],
  [60, 90, 30, 45, 60, 45, 30, 60, 45],
  [30, 45, 60, 30, 45, 90, 30, 45, 60],
  [45, 60, 90, 30, 45, 30, 60, 45, 30],
];

/** Deterministic programmes for one channel, from local midnight to well past now. */
function schedule(ch, now, R) {
  const day = new Date(now);
  day.setHours(0, 0, 0, 0);
  const pat = PATTERNS[ch % PATTERNS.length];
  const out = [];
  let t = day.getTime() + ((ch * 15) % 30) * MIN;
  for (let k = 0; t < now + 6 * 60 * MIN; k++) {
    const d = pat[k % pat.length] * MIN;
    const base = R.programmes[(ch * 3 + k) % R.programmes.length];
    const title = (k + ch) % 4 === 3 ? fmt(R.repeat, { title: base }) : base;
    out.push({ start: t, end: t + d, title, seg: k + 1 });
    t += d;
  }
  return out;
}

// ---------------------------------------------------------------------------- picture

/** A generated, silent picture: soft warm lights drifting slowly (never real footage). */
class Picture {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { alpha: false });
    this.channels = [];
    this.t = 0;
    this.w = 0;
    this.h = 0;
  }
  resize() {
    const r = this.canvas.getBoundingClientRect();
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const w = Math.max(1, Math.round(r.width * dpr));
    const h = Math.max(1, Math.round(r.height * dpr));
    if (w !== this.w || h !== this.h) {
      this.canvas.width = this.w = w;
      this.canvas.height = this.h = h;
    }
    this.draw();
  }
  set(channels) {
    this.channels = channels;
    this.draw();
  }
  draw() {
    const { ctx, w, h } = this;
    if (!w || !h) return;
    const n = this.channels.length;
    const cols = n > 1 ? 2 : 1;
    const rows = n > 2 ? 2 : 1;
    const gap = n > 1 ? Math.round(w * 0.006) : 0;
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, w, h);
    this.channels.forEach((ch, i) => {
      const tw = (w - gap * (cols - 1)) / cols;
      const th = (h - gap * (rows - 1)) / rows;
      const x = (i % cols) * (tw + gap);
      const y = Math.floor(i / cols) * (th + gap);
      ctx.save();
      ctx.beginPath();
      ctx.rect(x, y, tw, th);
      ctx.clip();
      this.paint(ch.hue, ch.seed, x, y, tw, th);
      ctx.restore();
    });
  }
  paint(hue, seed, x, y, w, h) {
    const { ctx } = this;
    const t = this.t + seed * 7.3;
    const [glow, second, depth] = LIGHTS[hue];
    const base = ctx.createLinearGradient(x, y, x + w, y + h);
    base.addColorStop(0, "#0D0A0C");
    base.addColorStop(1, depth);
    ctx.fillStyle = base;
    ctx.fillRect(x, y, w, h);
    ctx.globalCompositeOperation = "lighter";
    // Soft pools of light drifting slowly.
    const pools = [
      [glow, 0.66, 0.42, 0.5, 0.3, 23],
      [second, 0.28, 0.62, 0.42, 0.2, 31],
    ];
    for (const [col, cx, cy, r, a, period] of pools) {
      const p = (t / period) * Math.PI * 2;
      const px = x + w * (cx + 0.07 * Math.sin(p + seed));
      const py = y + h * (cy + 0.05 * Math.cos(p * 0.8 + seed));
      const rad = Math.max(w, h) * r;
      const g = ctx.createRadialGradient(px, py, 0, px, py, rad);
      g.addColorStop(0, hexA(col, a));
      g.addColorStop(0.5, hexA(col, a * 0.32));
      g.addColorStop(1, hexA(col, 0));
      ctx.fillStyle = g;
      ctx.fillRect(x, y, w, h);
    }
    // Softly bowed planes of light with bright rims: the Afterglow motif. Each is a ring
    // gradient centred far below-right, so only a gentle arc crosses the frame.
    const planes = [
      [glow, 1.25, 1.9, 1.62, 0.5, 29],
      [second, 0.95, 2.3, 2.02, 0.32, 37],
      [glow, 1.55, 1.6, 1.22, 0.26, 43],
    ];
    for (const [col, cx, cy, r, a, period] of planes) {
      const p = (t / period) * Math.PI * 2;
      const px = x + w * (cx + 0.05 * Math.sin(p + seed * 0.5));
      const py = y + h * (cy + 0.04 * Math.cos(p + seed));
      const rad = h * r;
      const g = ctx.createRadialGradient(px, py, rad * 0.86, px, py, rad);
      g.addColorStop(0, hexA(col, 0));
      g.addColorStop(0.72, hexA(col, a * 0.22));
      g.addColorStop(0.97, hexA(col, a));
      g.addColorStop(1, hexA(col, 0));
      ctx.fillStyle = g;
      ctx.fillRect(x, y, w, h);
    }
    ctx.globalCompositeOperation = "source-over";
    // Vignette, so overlays always sit on a dark edge.
    const v = ctx.createRadialGradient(x + w / 2, y + h / 2, Math.min(w, h) * 0.35, x + w / 2, y + h / 2, Math.max(w, h) * 0.75);
    v.addColorStop(0, "rgba(0,0,0,0)");
    v.addColorStop(1, "rgba(0,0,0,0.45)");
    ctx.fillStyle = v;
    ctx.fillRect(x, y, w, h);
  }
}
function hexA(hex, a) {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${n >> 16},${(n >> 8) & 255},${n & 255},${a.toFixed(3)})`;
}

// ---------------------------------------------------------------------------- replica

export class Replica {
  /**
   * @param {HTMLElement} root   the role=application element (tabindex=0)
   * @param {object} R           content.replica
   * @param {Set<string>} features  feature ids this release ships
   * @param {{ reducedMotion: () => boolean, onFocusMove?: (el: Element) => void, onLeave?: () => void }} opts
   */
  constructor(root, R, features, opts) {
    this.root = root;
    this.screen = root.querySelector(".rp-screen");
    this.R = R;
    this.features = features;
    this.opts = opts;
    this.now = Date.now();
    this.channels = R.channels.map((name, i) => ({
      i, name, hue: hueIndex(name), mono: monogram(name), seed: i * 1.7,
      progs: schedule(i, this.now, R), catchup: CATCHUP.has(i),
    }));
    this.state = {
      scene: "live", focus: {}, memory: {}, tuned: 0, started: false, paused: false, shift: 0,
      muted: false, vod: false, multi: false, recording: false, locked: false, flash: null,
    };
    this.demoSaved = null;
    this.timers = {};
    this.digits = "";
    this.active = false;
    this.build();
    this.bind();
    this.render();
  }

  has(id) {
    return this.features.has(id);
  }

  // -------------------------------------------------------------------------- build
  build() {
    const R = this.R;
    const rail = (current) => h("div", { class: "rail", "aria-hidden": "true" },
      h("svg", { class: "rail-sym", viewBox: "0 0 5 4" }, h("use", { href: "#sym", width: "5", height: "4" })),
      h("div", { class: "rail-items" },
        ["MAGNIFYING_GLASS", "HOUSE", "TELEVISION_SIMPLE", "ROWS", "FILM_STRIP", "MONITOR_PLAY", "CLOCK_COUNTER_CLOCKWISE", "HEART"]
          .map((g) => h("span", { class: g === current ? "rail-item is-current" : "rail-item" }, icon(g, { fill: g === current })))),
      h("span", { class: "rail-item rail-bottom" }, icon("GEAR_SIX")));
    const tile = (c, cls = "tile") => h("span", { class: `${cls} hue-${c.hue}` }, c.mono);
    const clock = () => h("span", { class: "clock" }, hhmm(this.now));

    // Live TV (DESIGN §8.3, §6.8)
    const rows = this.channels.map((c) => h("div", { class: "row", "data-nav": `row-${c.i}`, "data-region": "list" },
      h("span", { class: "num" }, num3(c.i)),
      tile(c),
      h("span", { class: "txt" },
        h("span", { class: "name" }, c.name),
        h("span", { class: "now" }, ""),
        h("span", { class: "bar" }, h("i", {}))),
      h("span", { class: "time" }, ""),
      c.catchup ? h("span", { class: "cu" }, icon("CLOCK_COUNTER_CLOCKWISE")) : h("span", { class: "cu" })));
    const live = h("div", { class: "sc sc-live", "data-scene": "live" },
      rail("TELEVISION_SIMPLE"),
      h("div", { class: "main" },
        h("div", { class: "head" },
          h("span", { class: "title" }, R.allChannels, h("span", { class: "count" }, fmt(R.channelsCount, { n: this.channels.length }))),
          clock()),
        h("div", { class: "cols" },
          h("div", { class: "list" },
            h("span", { class: "over" }, R.onNow),
            h("div", { class: "clip", "data-scroll": "" }, h("div", { class: "rows", "data-scroll-inner": "" }, rows))),
          h("div", { class: "side" },
            h("div", { class: "over-row" },
              h("span", { class: "over pv-label" }, h("i", { class: "dot" }), R.playingInPreview),
              h("span", { class: "pv-ch" }, "")),
            h("div", { class: "video", "data-nav": "video", "data-region": "side" },
              h("canvas", { class: "pic" }),
              h("div", { class: "idle" }, h("span", { class: "idle-text" }, R.pressEnter)),
              h("div", { class: "mini" }, h("span", { class: "badge" }, R.live), h("span", { class: "mini-name" }, ""),
                h("span", { class: "mini-shift" }, "")),
              h("div", { class: "zap zap-compact", "aria-live": "polite", "aria-atomic": "true" })),
            h("div", { class: "card" },
              h("span", { class: "over" }, R.now),
              h("span", { class: "card-title" }, ""),
              h("span", { class: "card-time" }, h("span", { class: "ct-range" }, ""), h("span", { class: "ct-left" }, "")),
              h("span", { class: "bar wide" }, h("i", {})),
              h("span", { class: "card-syn" }, ""),
              h("span", { class: "card-next" }, h("span", { class: "over" }, R.next), h("span", { class: "cn" }, ""))),
            h("div", { class: "btns" },
              h("span", { class: "rbtn primary", "data-nav": "btn-full", "data-region": "side" }, icon("ARROWS_OUT"), R.fullscreen),
              h("span", { class: "rbtn", "data-nav": "btn-guide", "data-region": "side" }, icon("ROWS"), R.guide))))));

    // Guide (DESIGN §6.9)
    const gRows = this.channels.map((c) => h("div", { class: "g-row", "data-ch": String(c.i) },
      h("span", { class: "g-ch" }, h("span", { class: "num" }, String(c.i + 1)), tile(c, "tile sm"), h("span", { class: "g-name" }, c.name)),
      h("span", { class: "g-cells" })));
    const guide = h("div", { class: "sc sc-guide", "data-scene": "guide" },
      rail("ROWS"),
      h("div", { class: "main" },
        h("div", { class: "head" }, h("span", { class: "title" }, R.guide), clock()),
        h("div", { class: "info" },
          h("span", { class: "over g-over" }, ""),
          h("span", { class: "g-title" }, ""),
          h("span", { class: "g-meta" }, ""),
          h("span", { class: "g-syn" }, "")),
        h("div", { class: "grid" },
          h("div", { class: "ruler" }, h("span", { class: "day" }, R.today), h("span", { class: "ticks" }), h("i", { class: "now-dot" })),
          h("div", { class: "g-clip", "data-scroll": "" },
            h("div", { class: "g-body", "data-scroll-inner": "" }, gRows),
            h("i", { class: "now-line" })))));

    // Player (DESIGN §6.10)
    const pbtn = (nav, g, label, extra = {}) => h("span", { class: "pbtn", "data-nav": nav, "data-region": "osd", "aria-label": label, ...extra }, icon(g));
    const player = h("div", { class: "sc sc-player", "data-scene": "player" },
      h("canvas", { class: "pic" }),
      h("div", { class: "multi-labels" }),
      h("div", { class: "scrim-top" }), h("div", { class: "scrim-bottom" }),
      h("div", { class: "p-top" },
        pbtn("p-back", "CARET_LEFT", R.back),
        h("span", { class: "p-titles" }, h("span", { class: "p-title" }, ""), h("span", { class: "p-sub" }, "")),
        h("span", { class: "p-pills" },
          h("span", { class: "pill rec" }, icon("RECORD", { fill: true }), R.recording),
          h("span", { class: "pill multi" }, icon("SQUARES_FOUR"), R.multiscreen)),
        clock()),
      h("div", { class: "p-bottom" },
        h("div", { class: "p-info" },
          h("span", { class: "badge" }, R.live),
          h("span", { class: "p-ch" }, ""),
          h("span", { class: "p-prog" }, "")),
        h("div", { class: "p-bar" },
          h("span", { class: "p-elapsed" }, ""),
          h("span", { class: "track" }, h("i", { class: "buf" }), h("i", { class: "fill" }), h("i", { class: "thumb" })),
          h("span", { class: "p-remain" }, ""),
          h("span", { class: "p-shift" }, ""),
          h("span", { class: "rbtn ghost golive", "data-nav": "p-golive", "data-region": "osd" }, icon("BROADCAST"), R.goLive)),
        h("div", { class: "p-ctrls" },
          h("span", { class: "seek", "data-nav": "p-back10", "data-region": "osd", "aria-label": R.back10 }, icon("ARROW_COUNTER_CLOCKWISE"), h("span", { class: "ten" }, R.ten)),
          h("span", { class: "pp", "data-nav": "p-pp", "data-region": "osd", "aria-label": R.playPause }, icon("PAUSE", { fill: true })),
          h("span", { class: "seek", "data-nav": "p-fwd10", "data-region": "osd", "aria-label": R.fwd10 }, icon("ARROW_CLOCKWISE"), h("span", { class: "ten" }, R.ten)),
          h("span", { class: "spacer" }),
          pbtn("p-tracks", "SUBTITLES", R.tracks),
          pbtn("p-aspect", "FRAME_CORNERS", R.aspect),
          pbtn("p-vol", "SPEAKER_HIGH", R.volume),
          pbtn("p-full", "ARROWS_OUT", R.fullscreen))));

    // Overlays shared by the scenes.
    const overlays = [
      h("div", { class: "ov-zap zap", "aria-live": "polite", "aria-atomic": "true" }),
      h("div", { class: "ov-number", "aria-live": "polite" }, ""),
      h("div", { class: "ov-flash", "aria-live": "polite" }, icon("MOON"), h("span", {}, "")),
      h("div", { class: "ov-pin" },
        h("div", { class: "pin" },
          h("span", { class: "pin-cat" }, icon("LOCK_SIMPLE", { fill: true }), R.lockedCategory),
          h("span", { class: "pin-title" }, R.lockedTitle),
          h("span", { class: "pin-body" }, R.lockedBody),
          h("span", { class: "dots" }, [0, 1, 2, 3].map(() => h("i", {}))),
          h("span", { class: "keys" },
            ["1", "2", "3", "4", "5", "6", "7", "8", "9"].map((d) => h("span", { class: "key" }, d)),
            h("span", { class: "key fn", "aria-label": R.pinClear }, icon("X")),
            h("span", { class: "key" }, "0"),
            h("span", { class: "key fn", "aria-label": R.pinDelete }, icon("BACKSPACE"))))),
      h("p", { class: "sr rp-say", "aria-live": "polite" }, ""),
    ];

    this.screen.replaceChildren(toDOM([live, guide, player, ...overlays]));
    const q = (s) => this.screen.querySelector(s);
    this.el = {
      live: q(".sc-live"), guide: q(".sc-guide"), player: q(".sc-player"),
      rows: [...this.screen.querySelectorAll(".sc-live .row")],
      gRows: [...this.screen.querySelectorAll(".g-row")],
      video: q(".sc-live .video"), zapCompact: q(".zap-compact"), zap: q(".ov-zap"),
      number: q(".ov-number"), flash: q(".ov-flash"), flashText: q(".ov-flash span:last-child"),
      say: q(".rp-say"),
    };
    this.pics = {
      live: new Picture(q(".sc-live .pic")),
      player: new Picture(q(".sc-player .pic")),
    };
    this.buildGuideCells();
    this.state.focus = { live: "row-0", guide: this.nowCellKey(0), player: "p-pp" };
  }

  /** Guide window: from the last half hour before (now − 40 min), 150 minutes wide. */
  buildGuideCells() {
    const start = new Date(this.now - 40 * MIN);
    start.setMinutes(start.getMinutes() < 30 ? 0 : 30, 0, 0);
    this.win = { start: start.getTime(), end: start.getTime() + 150 * MIN };
    const span = this.win.end - this.win.start;
    const pct = (t) => ((t - this.win.start) / span) * 100;
    const ticks = this.screen.querySelector(".sc-guide .ticks");
    for (let t = this.win.start; t < this.win.end; t += 30 * MIN) {
      const s = toDOM(h("span", { class: "tick" }, hhmm(t)));
      s.style.setProperty("--x", `${pct(t)}%`);
      ticks.append(s);
    }
    this.screen.querySelector(".sc-guide .grid").style.setProperty("--now", (pct(this.now) / 100).toFixed(4));
    this.cells = [];
    this.channels.forEach((c, ci) => {
      const host = this.el.gRows[ci].querySelector(".g-cells");
      c.progs.forEach((p, k) => {
        if (p.end <= this.win.start || p.start >= this.win.end) return;
        const past = p.end <= this.now;
        const onNow = p.start <= this.now && this.now < p.end;
        const key = `cell-${ci}-${k}`;
        const cell = toDOM(h("span", {
          class: `cell${past ? " past" : ""}${onNow ? " on-now" : ""}`, "data-nav": key, "data-region": "grid",
        },
        h("span", { class: "c-title" }, p.title),
        h("span", { class: "c-time" }, `${hhmm(p.start)}–${hhmm(p.end)}`),
        past && c.catchup ? h("span", { class: "c-cu" }, icon("CLOCK_COUNTER_CLOCKWISE")) : null));
        const l = Math.max(0, pct(p.start));
        const r = Math.min(100, pct(p.end));
        cell.style.setProperty("--l", `${l}%`);
        cell.style.setProperty("--w", `${r - l}%`);
        if (onNow) cell.style.setProperty("--elapsed", `${((this.now - Math.max(p.start, this.win.start)) / (Math.min(p.end, this.win.end) - Math.max(p.start, this.win.start))) * 100}%`);
        if (p.start < this.win.start) cell.classList.add("cut-left");
        host.append(cell);
        this.cells.push({ key, ch: ci, k, p, past, onNow });
      });
    });
  }

  nowCellKey(ch) {
    const c = this.cells.find((x) => x.ch === ch && x.onNow);
    return c ? c.key : this.cells.find((x) => x.ch === ch).key;
  }

  // -------------------------------------------------------------------------- events
  bind() {
    const root = this.root;
    root.addEventListener("keydown", (e) => this.onKey(e));
    root.addEventListener("focus", () => {
      this.setActive(true);
      this.showFocus();
    });
    root.addEventListener("blur", () => {
      if (!this.hovered) this.setActive(false);
    });
    root.addEventListener("pointerenter", (e) => {
      if (e.pointerType !== "mouse") return;
      this.hovered = true;
      this.setActive(true);
    });
    root.addEventListener("pointerleave", (e) => {
      if (e.pointerType !== "mouse") return;
      this.hovered = false;
      if (document.activeElement !== root) this.setActive(false);
    });
    // Hover sets focus (no separate hover style).
    root.addEventListener("pointerover", (e) => {
      if (e.pointerType !== "mouse") return;
      const t = e.target.closest("[data-nav]");
      if (t && this.visible(t)) this.focusKey(t.dataset.nav, { quiet: true });
    });
    // Mouse: click activates (hover already focused). Touch/pen: tap focuses, a second tap
    // on the focused item activates.
    root.addEventListener("pointerdown", (e) => {
      this.pointerType = e.pointerType;
    });
    root.addEventListener("click", (e) => {
      const t = e.target.closest("[data-nav]");
      if (!t || !this.visible(t)) return;
      const key = t.dataset.nav;
      if (this.pointerType === "mouse" || this.state.focus[this.state.scene] === key) {
        this.focusKey(key);
        this.activate(key);
      } else {
        this.focusKey(key);
      }
    });
    const ro = new ResizeObserver(() => {
      this.pics.live.resize();
      this.pics.player.resize();
      this.scrollIntoView();
    });
    ro.observe(this.screen);
    document.addEventListener("visibilitychange", () => this.loop());
    this.visibleOnScreen = true;
    new IntersectionObserver((entries) => {
      this.visibleOnScreen = entries.some((x) => x.isIntersecting);
      this.loop();
    }).observe(this.screen);
  }

  setActive(on) {
    this.active = on;
    this.root.classList.toggle("is-active", on);
    document.getElementById("rp-hint")?.classList.toggle("is-active", on && document.activeElement === this.root);
  }

  onKey(e) {
    if (e.ctrlKey || e.altKey || e.metaKey) return;
    const k = e.key;
    const dir = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" }[k];
    if (dir) this.move(dir);
    else if (k === "Enter") (this.digits ? this.commitDigits() : this.activate(this.state.focus[this.state.scene]));
    else if (k === " " || k === "Spacebar") this.space();
    else if (k === "PageUp") this.zap(1);
    else if (k === "PageDown") this.zap(-1);
    else if (/^[0-9]$/.test(k)) this.digit(k);
    else if (k === "z" || k === "Z") this.sleep();
    else if (k === "Backspace") this.back();
    else if (k === "Escape") {
      this.leave();
    } else return;
    e.preventDefault();
  }

  // -------------------------------------------------------------------------- focus
  navItems() {
    const scope = this.state.locked ? this.screen.querySelector(".ov-pin") : this.el[this.state.scene];
    return [...scope.querySelectorAll("[data-nav]")].filter((n) => this.visible(n));
  }

  visible(n) {
    return n.offsetParent !== null && !n.closest("[hidden]") && getComputedStyle(n).visibility !== "hidden";
  }

  focused() {
    const key = this.state.focus[this.state.scene];
    return key ? this.screen.querySelector(`[data-nav="${key}"]`) : null;
  }

  focusKey(key, { quiet = false } = {}) {
    const s = this.state;
    const prev = this.focused();
    if (prev && prev.dataset.region && prev.dataset.nav !== key) s.memory[`${s.scene}:${prev.dataset.region}`] = prev.dataset.nav;
    s.focus[s.scene] = key;
    this.showFocus();
    if (!quiet) this.announceFocus();
  }

  showFocus() {
    for (const n of this.screen.querySelectorAll(".is-focus")) n.classList.remove("is-focus");
    const el = this.focused();
    if (!el) return;
    el.classList.add("is-focus");
    this.scrollIntoView();
    this.renderGuideInfo();
    if (this.opts.onFocusMove && document.activeElement === this.root) this.opts.onFocusMove(el);
  }

  announceFocus() {
    const el = this.focused();
    if (!el) return;
    const key = el.dataset.nav;
    let text = el.getAttribute("aria-label") || el.textContent.trim();
    if (key.startsWith("row-")) {
      const c = this.channels[Number(key.slice(4))];
      text = fmt(this.R.sayRow, { num: c.i + 1, name: c.name, title: this.progNow(c).title });
    } else if (key.startsWith("cell-")) {
      const cell = this.cells.find((x) => x.key === key);
      text = `${this.channels[cell.ch].name}, ${cell.p.title}, ${hhmm(cell.p.start)}–${hhmm(cell.p.end)}`;
    }
    this.say(text);
  }

  say(text) {
    this.el.say.textContent = text;
  }

  /** Spatial navigation: nearest item in that direction, preferring the same band. */
  move(dir) {
    const cur = this.focused();
    const items = this.navItems();
    if (!cur || !items.includes(cur)) {
      if (items[0]) this.focusKey(items[0].dataset.nav);
      return;
    }
    const a = cur.getBoundingClientRect();
    const horiz = dir === "left" || dir === "right";
    if (!horiz && this.anchorKey !== cur.dataset.nav) this.anchorX = (a.left + a.right) / 2;
    const ax = this.anchorX ?? (a.left + a.right) / 2;
    const cands = [];
    for (const n of items) {
      if (n === cur) continue;
      const b = n.getBoundingClientRect();
      const e = 2;
      let primary;
      if (dir === "up") primary = b.bottom <= a.top + e ? a.top - b.bottom : null;
      if (dir === "down") primary = b.top >= a.bottom - e ? b.top - a.bottom : null;
      if (dir === "left") primary = b.right <= a.left + e ? a.left - b.right : null;
      if (dir === "right") primary = b.left >= a.right - e ? b.left - a.right : null;
      if (primary === null) continue;
      const orth = horiz
        ? Math.max(0, Math.max(b.top, a.top) - Math.min(b.bottom, a.bottom)) + Math.abs((b.top + b.bottom) / 2 - (a.top + a.bottom) / 2) * 0.1
        : ax < b.left ? b.left - ax : ax > b.right ? ax - b.right : 0;
      cands.push({ n, primary: Math.max(0, primary), orth });
    }
    if (!cands.length) return;
    const best = Math.min(...cands.map((c) => c.primary));
    const band = cands.filter((c) => c.primary <= best + Math.max(8, (horiz ? a.width : a.height) * 0.5));
    band.sort((x, y) => x.orth - y.orth || x.primary - y.primary);
    let target = band[0].n;
    // Entering another region restores its last focused item (focus memory per region).
    const region = target.dataset.region;
    if (region && region !== cur.dataset.region) {
      const remembered = this.state.memory[`${this.state.scene}:${region}`];
      const r = remembered && this.screen.querySelector(`[data-nav="${remembered}"]`);
      if (r && this.visible(r)) target = r;
    }
    this.focusKey(target.dataset.nav);
    if (!horiz) this.anchorKey = target.dataset.nav;
    else this.anchorX = null;
  }

  /** Keeps the focused row/cell inside its scrolling clip, with one row of look-ahead. */
  scrollIntoView() {
    const el = this.focused();
    if (!el) return;
    const clip = el.closest("[data-scroll]");
    if (!clip) return;
    const inner = clip.querySelector("[data-scroll-inner]");
    const er = el.getBoundingClientRect();
    const ir = inner.getBoundingClientRect();
    // Rects are in page pixels; offsets in layout pixels (the same here: nothing scales).
    const rowH = er.height;
    const top = er.top - ir.top;
    const view = clip.clientHeight;
    const max = Math.max(0, inner.scrollHeight - view);
    let y = this.scrollY?.get(clip) ?? 0;
    if (top - rowH * 0.6 < y) y = top - rowH * 0.6;
    if (top + rowH * 1.6 > y + view) y = top + rowH * 1.6 - view;
    y = Math.min(max, Math.max(0, y));
    (this.scrollY ??= new Map()).set(clip, y);
    inner.style.setProperty("--scroll", `${-y}px`);
  }

  // -------------------------------------------------------------------------- actions
  activate(key) {
    if (!key) return;
    const s = this.state;
    if (key.startsWith("row-")) {
      const i = Number(key.slice(4));
      if (s.started && s.tuned === i) this.go("player");
      else this.play(i);
    } else if (key === "video") {
      if (s.started) this.go("player");
      else this.play(s.tuned);
    } else if (key === "btn-full") {
      if (!s.started) this.play(s.tuned);
      this.go("player");
    } else if (key === "btn-guide") this.go("guide");
    else if (key.startsWith("cell-")) {
      const cell = this.cells.find((x) => x.key === key);
      if (cell.p.start > this.now) this.flash(fmt(this.R.startsAt, { time: hhmm(cell.p.start) }), "INFO");
      else {
        this.play(cell.ch);
        this.go("player");
      }
    } else if (key === "p-back" || key === "p-full") this.go("live");
    else if (key === "p-pp") this.togglePause();
    else if (key === "p-back10") this.seek(10);
    else if (key === "p-fwd10") this.seek(-10);
    else if (key === "p-golive") this.goLive();
    else if (key === "p-vol") {
      s.muted = !s.muted;
      this.render();
    }
  }

  play(i) {
    const s = this.state;
    s.tuned = i;
    s.started = true;
    s.paused = false;
    s.shift = 0;
    s.vod = false;
    this.say(fmt(this.R.sayPlaying, { name: this.channels[i].name }));
    this.render();
  }

  go(scene) {
    const s = this.state;
    if (s.scene === scene) return;
    s.scene = scene;
    if (scene === "guide" && !this.cells.some((c) => c.key === s.focus.guide && c.ch === s.tuned)) s.focus.guide = this.nowCellKey(s.tuned);
    if (scene === "live") s.focus.live = `row-${s.tuned}`;
    this.render();
    this.showFocus();
    this.say(fmt(this.R.sayScene, { scene: this.opts.sceneName(scene) }));
  }

  space() {
    const s = this.state;
    if (!s.started || s.scene === "guide") return;
    if (this.has("rewind")) {
      if (s.paused) s.paused = false;
      else {
        s.paused = true;
        s.shift = Math.max(s.shift, 12);
      }
      this.render();
    } else if (s.scene === "player") this.togglePause();
  }

  togglePause() {
    const s = this.state;
    if (!s.started) {
      this.play(s.tuned);
      return;
    }
    s.paused = !s.paused;
    if (s.paused && this.has("rewind") && !s.vod) s.shift = Math.max(s.shift, 12);
    this.render();
  }

  seek(back) {
    const s = this.state;
    if (s.vod || !this.has("rewind") || !s.started) return;
    s.shift = Math.max(0, s.shift + back);
    this.render();
  }

  goLive() {
    this.state.shift = 0;
    this.state.paused = false;
    this.render();
  }

  zap(step) {
    const s = this.state;
    const n = this.channels.length;
    const to = s.tuned + step;
    let edge = null;
    if (to < 0) edge = this.R.firstChannel;
    else if (to >= n) edge = this.R.lastChannel;
    else {
      s.tuned = to;
      s.started = true;
      s.paused = false;
      s.shift = 0;
      s.vod = false;
      if (s.scene === "live") s.focus.live = `row-${to}`;
    }
    this.render();
    this.showFocus();
    this.showZap(edge);
  }

  digit(d) {
    if (this.digits.length >= 3) this.digits = "";
    this.digits += d;
    this.el.number.textContent = this.digits;
    this.el.number.classList.add("show");
    clearTimeout(this.timers.digits);
    this.timers.digits = setTimeout(() => this.commitDigits(), 1500);
  }

  commitDigits() {
    clearTimeout(this.timers.digits);
    const n = parseInt(this.digits, 10);
    this.digits = "";
    this.el.number.classList.remove("show");
    if (n >= 1 && n <= this.channels.length) this.zap(n - 1 - this.state.tuned || 0);
    else this.flash(fmt(this.R.noChannel, { n }), "INFO");
  }

  sleep() {
    if (!this.has("sleep")) return;
    this.flash(this.R.sleepIn, "MOON");
  }

  back() {
    const s = this.state;
    if (s.scene !== "live") this.go("live");
  }

  leave() {
    clearTimeout(this.timers.digits);
    this.digits = "";
    this.el.number.classList.remove("show");
    this.opts.onLeave?.();
  }

  flash(text, glyph, ms = 3000) {
    setIcon(this.el.flash.querySelector(".i"), glyph);
    this.el.flashText.textContent = text;
    this.el.flash.classList.add("show");
    clearTimeout(this.timers.flash);
    this.timers.flash = ms
      ? setTimeout(() => {
        this.timers.flash = null;
        this.el.flash.classList.remove("show");
      }, ms)
      : null;
  }

  showZap(edge) {
    const s = this.state;
    const c = this.channels[s.tuned];
    const p = this.progNow(c);
    const next = this.progNext(c);
    const host = s.scene === "live" ? this.el.zapCompact : this.el.zap;
    const other = host === this.el.zap ? this.el.zapCompact : this.el.zap;
    other.classList.remove("show");
    host.replaceChildren(toDOM([
      h("span", { class: "z-num" }, String(c.i + 1)),
      h("span", { class: `tile hue-${c.hue}` }, c.mono),
      h("span", { class: "z-txt" },
        h("span", { class: "z-name" }, c.name, edge ? h("span", { class: "z-edge" }, edge) : null),
        h("span", { class: "z-now" }, h("span", {}, p.title), h("span", { class: "z-range" }, `${hhmm(p.start)}–${hhmm(p.end)}`)),
        h("span", { class: "bar" }, h("i", {})),
        h("span", { class: "z-next" }, `${this.R.next}: ${hhmm(next.start)} · ${next.title}`)),
    ]));
    host.querySelector(".bar i").style.setProperty("--p", this.progress(p));
    host.classList.add("show");
    this.root.classList.add("zapping");
    clearTimeout(this.timers.zap);
    this.timers.zap = setTimeout(() => {
      host.classList.remove("show");
      this.root.classList.remove("zapping");
    }, 3000);
  }

  // -------------------------------------------------------------------------- demos
  /** Linked storytelling: a feature beat shows its state; restore() brings the visitor's back. */
  demo(id) {
    if (!this.demoSaved) this.demoSaved = structuredClone(this.state);
    const s = (this.state = structuredClone(this.demoSaved));
    this.root.classList.add("demo");
    if (!s.started) s.tuned = DEMO_CH;
    s.flash = null;
    if (id === "live") {
      s.scene = "guide";
      s.focus.guide = this.nowCellKey(s.tuned);
    } else if (id === "catchup") {
      s.scene = "guide";
      const ch = this.channels.find((c) => c.catchup && c.i === s.tuned) ? s.tuned : DEMO_CH;
      const past = this.cells.filter((c) => c.ch === ch && c.past);
      s.focus.guide = (past[past.length - 1] || this.cells.find((c) => c.ch === ch)).key;
    } else if (id === "parental") {
      s.scene = "live";
      s.locked = true;
    } else if (id === "vod") {
      s.scene = "player";
      s.vod = true;
      s.paused = false;
    } else if (id === "rewind") {
      s.scene = "player";
      s.paused = true;
      s.shift = 12;
    } else if (id === "record") {
      s.scene = "player";
      s.recording = true;
    } else if (id === "multi") {
      s.scene = "player";
      s.multi = true;
    } else if (id === "sleep") {
      s.scene = "player";
      s.flash = this.R.sleepIn;
    }
    this.render();
    this.showFocus();
  }

  restore() {
    if (!this.demoSaved) return;
    this.root.classList.remove("demo");
    this.state = this.demoSaved;
    this.demoSaved = null;
    this.render();
    this.showFocus();
  }

  /** Keeps the demo as the visitor's own state (a click on a beat). */
  keep() {
    this.root.classList.remove("demo");
    this.demoSaved = null;
    this.state.locked = false;
    this.state.flash = null;
    this.render();
  }

  setScene(scene) {
    this.restoreIfDemo();
    this.go(scene);
    this.showFocus();
  }

  restoreIfDemo() {
    if (this.demoSaved) this.restore();
  }

  // -------------------------------------------------------------------------- render
  progNow(c) {
    return c.progs.find((p) => p.start <= this.now && this.now < p.end) || c.progs[0];
  }
  progNext(c) {
    return c.progs.find((p) => p.start > this.now) || c.progs[c.progs.length - 1];
  }
  progress(p) {
    return `${Math.min(100, Math.max(0, ((this.now - p.start) / (p.end - p.start)) * 100)).toFixed(1)}%`;
  }

  render() {
    const s = this.state;
    const R = this.R;
    const ch = this.channels[s.tuned];
    const prog = this.progNow(ch);
    for (const sc of ["live", "guide", "player"]) this.el[sc].hidden = s.scene !== sc;
    this.root.dataset.scene = s.scene;
    if (this.lastScene !== s.scene) {
      this.lastScene = s.scene;
      this.pics.live.resize();
      this.pics.player.resize();
      this.opts.onScene?.(s.scene);
    }
    this.root.classList.toggle("started", s.started);
    this.root.classList.toggle("paused", s.paused);
    this.root.classList.toggle("shifted", s.shift > 0 && !s.vod);
    this.root.classList.toggle("vod", s.vod);
    this.root.classList.toggle("multi", s.multi);
    this.root.classList.toggle("recording", s.recording);
    this.root.classList.toggle("locked", s.locked);
    this.root.classList.toggle("has-rewind", this.has("rewind"));

    // Live TV rows.
    this.el.rows.forEach((row, i) => {
      const c = this.channels[i];
      const p = this.progNow(c);
      row.querySelector(".now").textContent = p.title;
      row.querySelector(".time").textContent = `${hhmm(p.start)}–${hhmm(p.end)}`;
      row.querySelector(".bar i").style.setProperty("--p", this.progress(p));
      row.classList.toggle("playing", s.started && s.tuned === i);
    });
    const q = (sel) => this.screen.querySelector(sel);
    q(".pv-ch").textContent = `${num3(ch.i)} · ${ch.name}`;
    q(".mini-name").textContent = ch.name;
    q(".mini-shift").textContent = s.shift > 0 ? clockShift(s.shift) : "";
    q(".card-title").textContent = prog.title;
    q(".ct-range").textContent = `${hhmm(prog.start)}–${hhmm(prog.end)}`;
    q(".ct-left").textContent = fmt(R.minRemaining, { n: Math.max(1, Math.round((prog.end - this.now) / MIN)) });
    q(".card .bar i").style.setProperty("--p", this.progress(prog));
    q(".card-syn").textContent = fmt(R.synopsis, { title: prog.title, channel: ch.name });
    const nx = this.progNext(ch);
    q(".cn").textContent = `${hhmm(nx.start)} · ${nx.title}`;
    q(".idle-text").textContent = this.pointerType === "touch" ? R.tapToPlay : R.pressEnter;

    // Player OSD.
    if (s.vod) {
      q(".p-title").textContent = R.vodTitle;
      q(".p-sub").textContent = R.vodMeta;
      q(".p-elapsed").textContent = R.vodElapsed;
      q(".p-remain").textContent = R.vodRemaining;
    } else {
      q(".p-title").textContent = ch.name;
      q(".p-sub").textContent = `${num3(ch.i)} · ${prog.title}`;
      q(".p-ch").textContent = `${ch.i + 1}  ${ch.name}`;
      q(".p-prog").textContent = `${prog.title} · ${hhmm(prog.start)}–${hhmm(prog.end)}`;
      q(".p-elapsed").textContent = "";
      q(".p-remain").textContent = "";
    }
    q(".p-shift").textContent = s.shift > 0 && !s.vod ? clockShift(s.shift) : "";
    const track = q(".sc-player .track");
    if (s.vod) track.style.setProperty("--fill", "40.5%");
    else if (this.has("rewind")) track.style.setProperty("--fill", `${100 - Math.min(90, s.shift * 0.9)}%`);
    else track.style.setProperty("--fill", this.progress(prog));
    setIcon(q(".pp .i"), s.paused || !s.started ? "PLAY" : "PAUSE", true);
    setIcon(q("[data-nav='p-vol'] .i"), s.muted ? "SPEAKER_SLASH" : "SPEAKER_HIGH");
    const labels = q(".multi-labels");
    if (s.multi) {
      const four = [s.tuned, ...[0, 1, 2, 3, 4].filter((i) => i !== s.tuned)].slice(0, 4);
      labels.replaceChildren(toDOM(four.map((i, k) => h("span", { class: k === 0 ? "ml is-sound" : "ml" },
        k === 0 ? icon("SPEAKER_HIGH", { fill: true }) : null, this.channels[i].name))));
      this.pics.player.set(four.map((i) => this.channels[i]));
    } else {
      labels.replaceChildren();
      this.pics.player.set([ch]);
    }
    this.pics.live.set([ch]);

    // Flash shown by a demo (sleep), kept while the demo lasts.
    if (s.flash) this.flash(s.flash, "MOON", 0);
    else if (this.el.flash.classList.contains("show") && !this.timers.flash) this.el.flash.classList.remove("show");

    this.renderGuideInfo();
    this.loop();
  }

  renderGuideInfo() {
    if (this.state.scene !== "guide") return;
    const cell = this.cells.find((x) => x.key === this.state.focus.guide);
    if (!cell) return;
    const c = this.channels[cell.ch];
    const R = this.R;
    const q = (sel) => this.screen.querySelector(sel);
    q(".g-over").textContent = `${c.i + 1} · ${c.name}`;
    q(".g-title").textContent = cell.p.title;
    const dur = fmt(R.minutes, { n: Math.round((cell.p.end - cell.p.start) / MIN) });
    const when = cell.onNow ? R.onNowLabel : cell.past && c.catchup ? R.catchupHere : cell.past ? "" : fmt(R.startsAt, { time: hhmm(cell.p.start) });
    q(".g-meta").textContent = [`${hhmm(cell.p.start)}–${hhmm(cell.p.end)}`, dur, when].filter(Boolean).join(" · ");
    q(".g-syn").textContent = fmt(R.synopsis, { title: cell.p.title, channel: c.name });
  }

  // -------------------------------------------------------------------------- motion
  /** Animates the picture only while something plays, is on screen and motion is allowed. */
  loop() {
    const s = this.state;
    const run = s.started && !s.paused && !this.demoSaved && !document.hidden && this.visibleOnScreen && !this.opts.reducedMotion();
    if (run && !this.raf) {
      let last = performance.now();
      let acc = 0;
      const tick = (now) => {
        this.raf = requestAnimationFrame(tick);
        const dt = Math.min(0.1, (now - last) / 1000);
        last = now;
        acc += dt;
        if (acc < 1 / 30) return; // 30 fps is plenty for slow light
        const step = acc;
        acc = 0;
        for (const p of [this.pics.live, this.pics.player]) {
          if (p.canvas.offsetParent === null) continue;
          p.t += step;
          p.draw();
        }
      };
      this.raf = requestAnimationFrame(tick);
    } else if (!run && this.raf) {
      cancelAnimationFrame(this.raf);
      this.raf = null;
    }
  }
}
