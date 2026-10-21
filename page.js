// SPDX-License-Identifier: GPL-3.0-or-later
// The page's markup as functions of CONTENT and RELEASE. scripts/build.mjs renders it to
// index.html (so the page works without JavaScript and paints without waiting for it);
// main.js re-renders single sections only for the dev-only ?preview= switch.
import { h } from "./h.js";
import { icon } from "./icons.js";
import {
  fmt, isTodo, shippedFeatures, primaryDownload, downloadsList, sizeText, link, repoLinks, hasTodos,
} from "./util.js";

// The spec's policy (docs/LAUNCH_PROMPT.md §A5). Browsers ignore frame-ancestors in a <meta>
// tag (and log an error), so the meta carries the rest and the full policy, with
// frame-ancestors, goes in an HTTP header where the host allows it (README.md, _headers).
export const CSP_HEADER =
  "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; font-src 'self'; " +
  "connect-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";
export const CSP = CSP_HEADER.replace(" frame-ancestors 'none';", "");

// The "Open corners" symbol (assets/brand/symbol-gradient.svg), cropped to its 5:4 frame.
const SYMBOL_PATHS = [
  "M 707.825 184.32 A 16.319 16.319 0 0 1 724.143 200.639 L 724.143 282.232 A 16.319 16.319 0 0 1 707.825 298.551 L 314.543 298.551 A 97.912 97.912 0 0 0 216.631 396.463 L 216.631 602.732 A 16.319 16.319 0 0 1 200.312 619.051 L 118.719 619.051 A 16.319 16.319 0 0 1 102.4 602.732 L 102.4 396.463 A 212.143 212.143 0 0 1 314.543 184.32 Z",
  "M 316.175 839.68 A 16.319 16.319 0 0 1 299.857 823.361 L 299.857 741.768 A 16.319 16.319 0 0 1 316.175 725.449 L 709.457 725.449 A 97.912 97.912 0 0 0 807.369 627.537 L 807.369 421.268 A 16.319 16.319 0 0 1 823.688 404.949 L 905.281 404.949 A 16.319 16.319 0 0 1 921.6 421.268 L 921.6 627.537 A 212.143 212.143 0 0 1 709.457 839.68 Z",
];

const gradientStops = () => [
  h("stop", { offset: "0", "stop-color": "#E6BE91" }),
  h("stop", { offset: "0.36", "stop-color": "#E6BE91" }),
  h("stop", { offset: "1", "stop-color": "#C9868A" }),
];

/** One hidden SVG with the symbol and the champagne → rose gradient, used via <use>. */
export function svgDefs() {
  return h("svg", { class: "defs", width: "0", height: "0", "aria-hidden": "true", focusable: "false" },
    h("defs", {},
      h("linearGradient", { id: "g-light", x1: "102.4", y1: "184.3", x2: "921.6", y2: "839.7", gradientUnits: "userSpaceOnUse" }, gradientStops()),
      h("linearGradient", { id: "g-frame", x1: "0", y1: "0", x2: "1000", y2: "578", gradientUnits: "userSpaceOnUse" }, gradientStops()),
    ),
    h("symbol", { id: "sym", viewBox: "102.4 184.32 819.2 655.36" },
      h("g", { fill: "url(#g-light)" }, SYMBOL_PATHS.map((d) => h("path", { d }))),
    ),
  );
}

export const symbol = (cls, label) =>
  h("svg", label ? { class: cls, viewBox: "0 0 5 4", role: "img", "aria-label": label } : { class: cls, viewBox: "0 0 5 4", "aria-hidden": "true", focusable: "false" },
    h("use", { href: "#sym", width: "5", height: "4" }));

// The replica's hairline frame: the symbol's geometry as a 1.5 px line. Openings at the
// top-right and bottom-left, in the symbol's proportions (top edge to 76 %, right edge from
// 34 %, bottom edge from 24 %, left edge to 66 %). The SVG stretches with the screen;
// vector-effect keeps the line a hairline.
function frame() {
  const W = 1000, H = 578, R = 30, i = 1;
  const a = `M ${i} ${(H * 0.66).toFixed(1)} L ${i} ${R} A ${R - i} ${R - i} 0 0 1 ${R} ${i} L ${W * 0.76} ${i}`;
  const b = `M ${W - i} ${(H * 0.34).toFixed(1)} L ${W - i} ${H - R} A ${R - i} ${R - i} 0 0 1 ${W - R} ${H - i} L ${W * 0.24} ${H - i}`;
  return h("svg", { class: "rp-frame", viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none", "aria-hidden": "true", focusable: "false" },
    h("g", { fill: "none", stroke: "url(#g-frame)", "stroke-width": "1.5", "stroke-linecap": "round", "vector-effect": "non-scaling-stroke" },
      h("path", { d: a, "vector-effect": "non-scaling-stroke" }),
      h("path", { d: b, "vector-effect": "non-scaling-stroke" })));
}

function heroPicture() {
  return h("div", { class: "bg", "aria-hidden": "true" },
    h("picture", {},
      h("source", { type: "image/avif", srcset: "img/hero-1280.avif 1280w, img/hero-1920.avif 1920w", sizes: "100vw" }),
      h("source", { type: "image/webp", srcset: "img/hero-768.webp 768w, img/hero-1280.webp 1280w, img/hero-1920.webp 1920w", sizes: "100vw" }),
      h("img", {
        src: "img/hero-1280.jpg", srcset: "img/hero-768.jpg 768w, img/hero-1280.jpg 1280w, img/hero-1920.jpg 1920w",
        sizes: "100vw", width: "1920", height: "1080", alt: "", fetchpriority: "high", decoding: "async",
      })));
}

const sr = (text) => h("span", { class: "sr" }, text);

/** An <a> for a release URL; TODO URLs get data-todo (main.js explains them on click). */
function releaseLink(url, attrs, ...children) {
  const l = link(url);
  return h("a", { ...attrs, href: l.href, "data-todo": l.todo ? "" : false }, ...children);
}

/** The download cluster (§A2). All three platform variants are rendered; boot.js sets
 *  <html data-platform> before first paint and CSS shows one (no layout shift). */
export function downloadCluster(C, R) {
  const D = C.download;
  const prim = primaryDownload(R);
  const files = downloadsList(R);
  const repo = repoLinks(R);
  const isZip = prim.kind === "portable";
  const caption = fmt(D.caption, { version: R.version, size: sizeText(prim.sizeMB) });
  const portable = R.downloads.portable;
  const newTab = { target: "_blank", rel: "noopener" };

  const primaryLabel = [icon("DOWNLOAD_SIMPLE"), h("span", {}, D.primary),
    isZip ? h("span", { class: "tag" }, D.zipTag) : null, isZip ? sr(`, ${D.zipTagLabel}`) : null];

  return h("section", { class: "cluster", id: "cluster", "aria-labelledby": "download-heading" },
    h("h2", { class: "sr", id: "download-heading" }, D.heading),
    h("div", { class: "primary-row" },
      // Windows (and no JavaScript): the file itself.
      releaseLink(prim.url, {
        class: "btn btn-primary btn-lg only-win", id: "download", "data-file": prim.file,
        "data-sha": prim.sha256, "data-version": R.version,
      }, primaryLabel),
      // Phones and tablets: copy the page link for later.
      h("button", { type: "button", class: "btn btn-primary btn-lg only-mobile", id: "get-on-pc" },
        icon("LINK"), h("span", {}, D.mobile)),
      // macOS, Linux…: say so, and offer the source.
      h("p", { class: "platform-note only-other" }, icon("INFO"), h("span", {}, D.other)),
      h("a", { class: "btn btn-primary btn-lg only-other", href: repo.build || "#todo", "data-todo": repo.build ? false : "", ...newTab },
        icon("TERMINAL_WINDOW"), h("span", {}, D.buildFromSource), sr(` ${D.newTab}`)),
    ),
    h("p", { class: "caption", id: "download-caption" }, caption),
    h("div", { class: "secondary-row" },
      // The zip as a secondary link when the installer is primary.
      !isZip && portable && !isTodo(portable.file)
        ? releaseLink(portable.url, { class: "btn btn-secondary", "data-file": portable.file, "data-sha": portable.sha256, "data-version": R.version },
          icon("FILE_ZIP"), h("span", {}, D.portable))
        : null,
      // Elsewhere the Windows download moves down here, so it is never lost.
      releaseLink(prim.url, { class: "btn btn-secondary not-win", "data-file": prim.file, "data-sha": prim.sha256, "data-version": R.version },
        icon("DOWNLOAD_SIMPLE"), h("span", {}, D.primary), isZip ? sr(`, ${D.zipTagLabel}`) : null),
      h("a", { class: "btn btn-secondary", href: repo.source || "#todo", "data-todo": repo.source ? false : "", ...newTab },
        icon("CODE"), h("span", {}, D.viewSource), sr(` ${D.newTab}`)),
      h("button", { type: "button", class: "btn btn-ghost", popovertarget: "sha-pop", id: "sha-button" },
        icon("SHIELD_CHECK"), h("span", {}, D.sha)),
    ),
    shaPopover(C, R, files),
    h("p", { class: "note" }, icon("KEY"), h("span", {}, D.note)),
    R.signed ? null : h("p", { class: "note" }, icon("INFO"), h("span", {}, D.unsigned)),
  );
}

function shaPopover(C, R, files) {
  const S = C.sha;
  const sums = link(R.downloads.sumsUrl);
  return h("div", { class: "popover", id: "sha-pop", popover: "auto", role: "dialog", "aria-labelledby": "sha-title" },
    h("div", { class: "popover-head" },
      h("h2", { class: "popover-title", id: "sha-title" }, S.title),
      h("button", { type: "button", class: "icon-btn", popovertarget: "sha-pop", popovertargetaction: "hide", "aria-label": S.close }, icon("X"))),
    h("p", { class: "popover-intro" }, S.intro),
    h("ul", { class: "sums" }, files.map((f) =>
      h("li", { class: "sum" },
        h("div", { class: "sum-head" },
          h("span", { class: "sum-kind" }, f.kind === "installer" ? S.installer : S.portable),
          h("span", { class: "sum-file" }, f.file)),
        h("div", { class: "sum-row" },
          h("code", { class: "hash" }, f.sha256),
          h("button", { type: "button", class: "btn btn-secondary btn-sm copy", "data-copy": f.sha256, "aria-label": fmt(S.copyLabel, { file: f.file }) },
            icon("COPY"), h("span", { class: "copy-label" }, S.copy)))))),
    h("p", { class: "popover-how" }, S.howTo, " ", h("code", {}, fmt(S.command, { file: files[0] ? files[0].file : "" }))),
    h("p", { class: "popover-foot" },
      h("a", { href: sums.href, "data-todo": sums.todo ? "" : false }, fmt(S.sumsLink, { file: isTodo(R.downloads.sumsFile) ? "SHA256SUMS.txt" : R.downloads.sumsFile })),
      h("span", { class: "popover-req" }, fmt(S.requirements, { requirements: R.requirements }))),
  );
}

export function trustRow(C) {
  return h("ul", { class: "chips", "aria-label": C.trustLabel },
    C.trust.map((t) => h("li", { class: "chip-wrap" },
      h("button", { type: "button", class: "chip", "aria-describedby": `tip-${t.id}`, "data-trust": t.id },
        icon(t.icon), h("span", {}, t.label)),
      h("span", { class: "tip", role: "tooltip", id: `tip-${t.id}` }, t.tip))));
}

export function beats(C, R) {
  const ids = shippedFeatures(R);
  return h("section", { class: "beats", id: "beats", "aria-label": C.featuresLabel },
    h("ul", { class: ids.length > 4 ? "beat-list many" : "beat-list" }, ids.map((id) => {
      const f = C.features[id];
      return h("li", {},
        h("button", { type: "button", class: "beat", "data-feature": id, "aria-controls": "replica" },
          icon(f.icon), h("span", {}, f.label), sr(`: ${C.featureHint}`)));
    })));
}

function footer(C, R) {
  const repo = repoLinks(R);
  const a = (href, text) => h("a", { href: href || "#todo", "data-todo": href ? false : "" }, text);
  return h("footer", { class: "footer" },
    h("p", { class: "fine" }, C.footer.line, " ",
      h("span", { class: "links" },
        a(repo.licence, C.footer.licence), a(repo.security, C.footer.security),
        a(repo.signing, C.footer.signing), a(repo.source, C.footer.source))));
}

function showcase(C) {
  const T = C.replica.tabs;
  return h("section", { class: "showcase", "aria-label": C.replica.label },
    h("div", { class: "rp-tabs", role: "tablist", "aria-label": C.replica.tabsLabel },
      Object.entries(T).map(([id, label], i) => h("button", {
        type: "button", role: "tab", class: "rp-tab", id: `tab-${id}`, "data-scene": id,
        "aria-selected": i === 0 ? "true" : "false", "aria-controls": "replica", tabindex: i === 0 ? "0" : "-1",
      }, label))),
    h("div", { class: "rp-wrap" },
      frame(),
      h("div", {
        class: "rp", id: "replica", role: "application", tabindex: "0",
        "aria-label": C.replica.label, "aria-describedby": "rp-hint",
      }, h("div", { class: "rp-screen", id: "rp-screen" })),
      h("noscript", {},
        h("img", { class: "rp-fallback", src: "img/replica-fallback.jpg", width: "1600", height: "900", alt: C.replica.fallbackAlt }))),
    h("p", { class: "rp-hint", id: "rp-hint" },
      h("span", { class: "hint-keys" }, C.replica.hint),
      h("span", { class: "hint-touch" }, C.replica.hintTouch),
      sr(`. ${C.replica.hintEsc}.`)),
  );
}

export function notices(C, R) {
  return h("div", { class: "notices", id: "notices" },
    hasTodos(R) ? h("p", { class: "notice notice-todo" }, icon("WARNING"), h("span", {}, C.notices.todo)) : null,
    R.preview ? h("p", { class: "notice" }, icon("INFO"), h("span", {}, fmt(C.notices.preview, { version: R.version }))) : null);
}

function body(C, R) {
  return h("body", {},
    h("a", { class: "skip", href: "#download" }, C.skip),
    svgDefs(),
    h("div", { class: "glow", id: "glow", "aria-hidden": "true" }),
    heroPicture(),
    h("div", { class: "stage" },
      h("main", { class: "hero", id: "main" },
        notices(C, R),
        h("div", { class: "intro" },
          h("p", { class: "lockup" }, symbol("lockup-symbol"), h("span", { class: "wordmark" }, C.brand)),
          h("h1", { id: "top", tabindex: "-1" }, C.headline.flatMap((line, i) => (i ? [h("br", {}), line] : [line]))),
          h("p", { class: "subhead" }, C.subhead),
          downloadCluster(C, R)),
        // DOM order is the phone order (download → replica → chips); on wide screens the
        // grid puts the chips under the download cluster.
        showcase(C),
        trustRow(C)),
      h("div", { class: "strip" }, beats(C, R), footer(C, R))),
    h("div", { class: "toasts", id: "toasts", "aria-live": "polite" }),
  );
}

function head(C, R) {
  const M = C.meta;
  const abs = (p) => (isTodo(M.siteUrl) ? p : new URL(p, M.siteUrl).href);
  const meta = (attrs) => h("meta", attrs);
  return h("head", {},
    meta({ charset: "utf-8" }),
    meta({ name: "viewport", content: "width=device-width, initial-scale=1" }),
    meta({ "http-equiv": "Content-Security-Policy", content: CSP }),
    meta({ name: "referrer", content: "no-referrer" }),
    h("title", {}, M.title),
    meta({ name: "description", content: M.description }),
    meta({ name: "theme-color", content: "#141013" }),
    meta({ name: "color-scheme", content: "dark" }),
    h("link", { rel: "icon", href: "favicon.svg", type: "image/svg+xml" }),
    h("link", { rel: "icon", href: "favicon-32.png", sizes: "32x32", type: "image/png" }),
    h("link", { rel: "apple-touch-icon", href: "apple-touch-icon.png" }),
    h("link", { rel: "manifest", href: "manifest.webmanifest" }),
    isTodo(M.siteUrl) ? null : h("link", { rel: "canonical", href: M.siteUrl }),
    meta({ property: "og:type", content: "website" }),
    meta({ property: "og:site_name", content: C.brand }),
    meta({ property: "og:title", content: M.ogTitle }),
    meta({ property: "og:description", content: M.ogDescription }),
    isTodo(M.siteUrl) ? null : meta({ property: "og:url", content: M.siteUrl }),
    meta({ property: "og:image", content: abs("og-image.jpg") }),
    meta({ property: "og:image:width", content: "1200" }),
    meta({ property: "og:image:height", content: "630" }),
    meta({ property: "og:image:alt", content: M.ogImageAlt }),
    meta({ name: "twitter:card", content: "summary_large_image" }),
    meta({ name: "twitter:title", content: M.ogTitle }),
    meta({ name: "twitter:description", content: M.ogDescription }),
    meta({ name: "twitter:image", content: abs("og-image.jpg") }),
    meta({ name: "twitter:image:alt", content: M.ogImageAlt }),
    h("link", { rel: "preload", href: "fonts/afterglow-sans-latin.woff2", as: "font", type: "font/woff2", crossorigin: "" }),
    h("link", { rel: "preload", href: "fonts/fraunces-latin.woff2", as: "font", type: "font/woff2", crossorigin: "" }),
    h("link", { rel: "preload", href: "fonts/phosphor-regular-subset.woff2", as: "font", type: "font/woff2", crossorigin: "" }),
    h("link", { rel: "stylesheet", href: "styles.css" }),
    h("script", { src: "boot.js" }),
    ["main.js", "content.js", "release.js", "h.js", "icons.js", "util.js", "page.js", "replica.js"].map((m) =>
      h("link", { rel: "modulepreload", href: m })),
    h("script", { type: "module", src: "main.js" }),
  );
}

/** The whole document (build step). */
export function documentHTML(C, R, toHTML) {
  return "<!doctype html>\n" +
    "<!-- Generated by site/scripts/build.mjs from page.js, content.js and release.js: don't edit by hand. -->\n" +
    toHTML(h("html", { lang: C.lang }, head(C, R), body(C, R))) + "\n";
}

/** HTTP headers for hosts that read a _headers file (Netlify, Cloudflare Pages). */
export function headersFile() {
  return [
    "# Generated by site/scripts/build.mjs. Hosts that can't set headers (GitHub Pages) get",
    "# the <meta> policy in index.html, which can't carry frame-ancestors.",
    "/*",
    `  Content-Security-Policy: ${CSP_HEADER}`,
    "  X-Content-Type-Options: nosniff",
    "  Referrer-Policy: no-referrer",
    "",
  ].join("\n");
}

/** The web app manifest (build step). */
export function manifest(C) {
  return JSON.stringify({
    name: C.meta.manifestName,
    short_name: C.meta.manifestName,
    description: C.meta.manifestDescription,
    lang: C.lang,
    start_url: "./",
    scope: "./",
    display: "standalone",
    background_color: "#141013",
    theme_color: "#141013",
    icons: [
      { src: "favicon.svg", sizes: "any", type: "image/svg+xml" },
      { src: "apple-touch-icon.png", sizes: "180x180", type: "image/png" },
      { src: "icon-512-maskable.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
    ],
  }, null, 2) + "\n";
}
