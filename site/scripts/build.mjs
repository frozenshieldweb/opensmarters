// SPDX-License-Identifier: GPL-3.0-or-later
// Builds the static page in site/ (no dependencies; Node 18+):
//   1. copies the assets it serves from the launch-asset pipeline (assets/launch/web/) and
//      the no-JavaScript screenshot (docs/screenshots/live-tv.jpg) into site/;
//   2. renders site/index.html, site/manifest.webmanifest and site/_headers from page.js,
//      content.js and release.js, so every string lives in content.js and the page works
//      without JavaScript.
//
//   node site/scripts/build.mjs           build
//   node site/scripts/build.mjs --check   fail if anything in site/ is out of date (CI)
import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const SITE = join(dirname(fileURLToPath(import.meta.url)), "..");
const ROOT = join(SITE, "..");
const check = process.argv.includes("--check");

const load = (f) => import(pathToFileURL(join(SITE, f)).href);
const { CONTENT } = await load("content.js");
const { RELEASE } = await load("release.js");
const { documentHTML, manifest, headersFile } = await load("page.js");
const { toHTML } = await load("h.js");

// [source in the repository, destination in site/]
const WEB = "assets/launch/web";
const COPIES = [
  ...["fraunces-latin.woff2", "afterglow-sans-latin.woff2", "phosphor-regular-subset.woff2", "phosphor-fill-subset.woff2",
    "OFL-Fraunces.txt", "OFL-SourceSans3.txt", "LICENSE-Phosphor.txt"].map((f) => [`${WEB}/fonts/${f}`, `fonts/${f}`]),
  ...["1920", "1280"].flatMap((w) => ["avif", "webp", "jpg"].map((x) => [`${WEB}/hero-${w}.${x}`, `img/hero-${w}.${x}`])),
  [`${WEB}/hero-768.webp`, "img/hero-768.webp"],
  [`${WEB}/hero-768.jpg`, "img/hero-768.jpg"],
  [`${WEB}/ready-128.png`, "img/ready-128.png"],
  [`${WEB}/og-image.jpg`, "og-image.jpg"],
  [`${WEB}/favicon.svg`, "favicon.svg"],
  [`${WEB}/favicon-32.png`, "favicon-32.png"],
  [`${WEB}/apple-touch-icon.png`, "apple-touch-icon.png"],
  [`${WEB}/icon-512-maskable.png`, "icon-512-maskable.png"],
  ["docs/screenshots/live-tv.jpg", "img/replica-fallback.jpg"],
];

const stale = [];
function put(dest, data) {
  const path = join(SITE, dest);
  const same = existsSync(path) && Buffer.compare(readFileSync(path), Buffer.from(data)) === 0;
  if (same) return;
  if (check) {
    stale.push(dest);
    return;
  }
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, data);
  console.log(`  wrote ${relative(ROOT, path).replaceAll("\\", "/")}`);
}

for (const [src, dest] of COPIES) put(dest, readFileSync(join(ROOT, src)));
put("index.html", documentHTML(CONTENT, RELEASE, toHTML));
put("manifest.webmanifest", manifest(CONTENT));
put("_headers", headersFile());

if (check && stale.length) {
  console.error(`site/ is out of date (run node site/scripts/build.mjs):\n  ${stale.join("\n  ")}`);
  process.exit(1);
}
console.log(check ? "site/ is up to date" : `site/ built for ${RELEASE.product} ${RELEASE.version}`);
