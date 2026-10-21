// SPDX-License-Identifier: GPL-3.0-or-later
// Progressive enhancement of the pre-rendered page (index.html works without it):
// download toasts and checksums, tooltips, the ambient light, the replica and the links
// between the feature beats and the replica. No network requests, no storage, no cookies.
import { CONTENT as C } from "./content.js";
import { RELEASE } from "./release.js";
import { h, toDOM, replace } from "./h.js";
import { icon } from "./icons.js";
import { downloadCluster, beats, notices } from "./page.js";
import { Replica } from "./replica.js";
import { fmt, shippedFeatures, previewRelease } from "./util.js";

const $ = (s, el = document) => el.querySelector(s);
const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");

// ---- dev-only preview switch: ?preview=0.2.0 renders the page as if that release (with an
// installer) existed. Never linked, never the default; a notice says it isn't released.
const asked = new URLSearchParams(window.location.search).get("preview");
const R = asked && /^\d+\.\d+\.\d+$/.test(asked) ? previewRelease(RELEASE, asked) : RELEASE;
if (R !== RELEASE) {
  replace("cluster", downloadCluster(C, R));
  replace("beats", beats(C, R));
  replace("notices", notices(C, R));
}

// ---------------------------------------------------------------------------- toasts
const toasts = $("#toasts");
/** DESIGN §6.12: bottom-centre, max 2, newest at the bottom, never takes focus. */
function toast({ text, glyph = "CHECK_CIRCLE", tone = "", img = null, action = null, ms = 4000 }) {
  const el = toDOM(h("div", { class: `toast${tone ? ` toast-${tone}` : ""}` },
    img ? h("img", { class: "toast-img", src: img, width: "48", height: "48", alt: "" }) : icon(glyph, { cls: "toast-icon" }),
    h("span", { class: "toast-text" }, text),
    action ? h("button", { type: "button", class: "btn btn-secondary btn-sm toast-action" }, icon("COPY"), h("span", {}, action.label)) : null,
    h("button", { type: "button", class: "icon-btn toast-close", "aria-label": C.toastClose }, icon("X"))));
  if (action) $(".toast-action", el).addEventListener("click", () => action.run($(".toast-action span", el)));
  let timer = null;
  const close = () => {
    el.classList.remove("show");
    setTimeout(() => el.remove(), reduced.matches ? 100 : 240);
  };
  const arm = () => {
    clearTimeout(timer);
    timer = setTimeout(close, ms);
  };
  // Pause while the pointer or the keyboard is on it (WCAG 2.2.1).
  el.addEventListener("pointerenter", () => clearTimeout(timer));
  el.addEventListener("pointerleave", arm);
  el.addEventListener("focusin", () => clearTimeout(timer));
  el.addEventListener("focusout", arm);
  $(".toast-close", el).addEventListener("click", close);
  toasts.append(el);
  while (toasts.children.length > 2) toasts.firstElementChild.remove();
  requestAnimationFrame(() => el.classList.add("show"));
  arm();
}

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Swaps a copy button's label to "Copied" for a moment. */
function copied(labelEl) {
  if (!labelEl) return;
  const before = labelEl.textContent;
  labelEl.textContent = C.sha.copied;
  setTimeout(() => {
    labelEl.textContent = before;
  }, 2000);
}

// ---------------------------------------------------------------------------- clicks
document.addEventListener("click", async (e) => {
  const t = e.target;
  // A link whose URL is still a TODO in release.js: say so instead of going nowhere.
  const todo = t.closest("a[data-todo]");
  if (todo) {
    e.preventDefault();
    toast({ text: C.download.todoLink, glyph: "WARNING", tone: "warning" });
    return;
  }
  // A download: let the browser fetch it, and point at the checksum.
  const dl = t.closest("a[data-file]");
  if (dl) {
    const sha = dl.dataset.sha;
    toast({
      text: fmt(C.download.toastDownloading, { version: dl.dataset.version, short: `${sha.slice(0, 8)}…` }),
      img: "img/ready-128.png",
      ms: 8000,
      action: { label: C.download.toastCopyHash, run: async (label) => (await copy(sha)) && copied(label) },
    });
    return;
  }
  if (t.closest("#get-on-pc")) {
    const url = `${window.location.origin}${window.location.pathname}`;
    if (await copy(url)) toast({ text: C.download.linkCopied, glyph: "LINK" });
    else toast({ text: fmt(C.download.linkCopyFailed, { url }), glyph: "LINK", ms: 8000 });
    return;
  }
  const cp = t.closest("button.copy");
  if (cp && (await copy(cp.dataset.copy))) copied($(".copy-label", cp));
});

// ---------------------------------------------------------------------------- checksums
// The popover opens without JavaScript (popover attribute, centred by the browser); here
// it is placed under its button.
function placePopover() {
  const pop = $("#sha-pop");
  const btn = $("#sha-button");
  if (!pop || !btn || !pop.matches(":popover-open")) return;
  const b = btn.getBoundingClientRect();
  const p = pop.getBoundingClientRect();
  const gap = 12;
  const vw = document.documentElement.clientWidth;
  const vh = window.innerHeight;
  let top = b.bottom + gap;
  if (top + p.height > vh - 16 && b.top - gap - p.height > 16) top = b.top - gap - p.height;
  const left = Math.min(Math.max(16, b.left), vw - p.width - 16);
  pop.style.setProperty("--top", `${Math.max(16, top)}px`);
  pop.style.setProperty("--left", `${Math.max(16, left)}px`);
}
document.addEventListener("toggle", (e) => {
  if (e.target.id === "sha-pop" && e.newState === "open") placePopover();
}, true);
window.addEventListener("resize", placePopover);
window.addEventListener("scroll", placePopover, { passive: true });

// ---------------------------------------------------------------------------- tooltips
// Chips show their tooltip on hover and focus (CSS); Esc hides it (WCAG 1.4.13).
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  for (const w of document.querySelectorAll(".chip-wrap:hover, .chip-wrap:focus-within")) w.classList.add("tip-off");
});
document.addEventListener("focusout", (e) => e.target.closest?.(".chip-wrap")?.classList.remove("tip-off"));
// Keep a shown tooltip inside the viewport (chips near the right edge on narrow screens).
function fitTip(wrap) {
  const tip = wrap && $(".tip", wrap);
  if (!tip) return;
  tip.style.setProperty("--tip-dx", "0px");
  requestAnimationFrame(() => {
    const r = tip.getBoundingClientRect();
    const over = r.right - (document.documentElement.clientWidth - 16);
    if (over > 0) tip.style.setProperty("--tip-dx", `${-Math.min(over, r.left - 16)}px`);
  });
}
document.addEventListener("pointerover", (e) => fitTip(e.target.closest?.(".chip-wrap")));
document.addEventListener("focusin", (e) => fitTip(e.target.closest?.(".chip-wrap")));
document.addEventListener("pointerout", (e) => {
  const w = e.target.closest?.(".chip-wrap");
  if (w && !w.contains(e.relatedTarget)) w.classList.remove("tip-off");
});

// ---------------------------------------------------------------------------- light
// The page's only ambient effect: a very soft champagne light (≤ 8 %) that follows the
// focused element, eased over 400 ms (instant with reduced motion).
const glow = $("#glow");
let lit = null;
function light(el) {
  lit = el;
  if (!el) {
    glow.classList.remove("on");
    return;
  }
  const r = el.getBoundingClientRect();
  glow.style.setProperty("--x", `${Math.round(r.left + r.width / 2)}px`);
  glow.style.setProperty("--y", `${Math.round(r.top + r.height / 2)}px`);
  glow.classList.add("on");
}
document.addEventListener("focusin", (e) => {
  const t = e.target;
  if (t.id === "replica") return; // the replica reports its own focused item
  light(t.id === "top" ? null : t);
});
document.addEventListener("focusout", (e) => {
  if (!e.relatedTarget) light(null);
});
const relight = () => lit && light(lit);
window.addEventListener("scroll", relight, { passive: true });
window.addEventListener("resize", relight);

// ---------------------------------------------------------------------------- replica
const root = $("#replica");
const tabs = [...document.querySelectorAll(".rp-tab")];
const replica = new Replica(root, C.replica, new Set(shippedFeatures(R)), {
  reducedMotion: () => reduced.matches,
  sceneName: (s) => C.replica.tabs[s],
  onFocusMove: (el) => light(el),
  onScene: (scene) => {
    for (const t of tabs) {
      const on = t.dataset.scene === scene;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
    }
  },
  // Esc returns to the page: focus goes to the headline, so the next Tab reaches the
  // download button.
  onLeave: () => {
    root.blur();
    $("#top").focus();
  },
});
reduced.addEventListener("change", () => replica.loop());

// Scene tabs (ARIA tabs, automatic activation).
for (const t of tabs) {
  t.addEventListener("click", () => replica.setScene(t.dataset.scene));
  t.addEventListener("keydown", (e) => {
    const i = tabs.indexOf(t);
    const j = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: tabs.length - 1 }[e.key];
    if (j === undefined) return;
    e.preventDefault();
    const next = tabs[(j + tabs.length) % tabs.length];
    next.focus();
    replica.setScene(next.dataset.scene);
  });
}

// Linked storytelling: hovering or focusing a feature beat shows it in the replica;
// moving away restores the visitor's state. A click keeps it and moves into the replica.
const engaged = new Set();
function engage(beat, on, why) {
  const key = `${beat.dataset.feature}:${why}`;
  if (on) engaged.add(key);
  else engaged.delete(key);
  const active = [...engaged].map((k) => k.split(":")[0]);
  if (active.length) replica.demo(active[active.length - 1]);
  else replica.restore();
}
document.addEventListener("pointerover", (e) => {
  const b = e.target.closest?.(".beat");
  if (b && !b.contains(e.relatedTarget)) engage(b, true, "hover");
});
document.addEventListener("pointerout", (e) => {
  const b = e.target.closest?.(".beat");
  if (b && !b.contains(e.relatedTarget)) engage(b, false, "hover");
});
document.addEventListener("focusin", (e) => {
  const b = e.target.closest?.(".beat");
  if (b) engage(b, true, "focus");
});
document.addEventListener("focusout", (e) => {
  const b = e.target.closest?.(".beat");
  if (b) engage(b, false, "focus");
});
document.addEventListener("click", (e) => {
  const b = e.target.closest(".beat");
  if (!b) return;
  replica.demo(b.dataset.feature);
  replica.keep();
  engaged.clear();
  root.focus();
});

// The "Keyboard, remote and mouse" chip points at the replica's key hints.
const hint = $("#rp-hint");
const keysChip = (e) => e.target.closest?.('[data-trust="keyboard"]');
for (const [type, on] of [["pointerover", true], ["pointerout", false], ["focusin", true], ["focusout", false]]) {
  document.addEventListener(type, (e) => {
    if (keysChip(e)) hint.classList.toggle("is-hint", on);
  });
}
