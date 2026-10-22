// SPDX-License-Identifier: GPL-3.0-or-later
// opensmarters web: a minimal service worker. It caches only the app shell (the built HTML,
// scripts, styles, fonts and a few backgrounds) so the app opens fast and installs as an app.
// It never touches /api: data, images and video always come from the server.

const VERSION = "__VERSION__";
const CACHE = "opensmarters-shell-" + VERSION;
const SHELL = [] /* __SHELL__ */;

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("opensmarters-shell-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin || url.pathname.startsWith("/api/")) return;
  if (req.mode === "navigate") {
    // Pages: the network first (a fresh shell after an update), the cached shell offline.
    event.respondWith(fetch(req).catch(() => caches.match("/", { cacheName: CACHE }).then((r) => r || Response.error())));
    return;
  }
  if (SHELL.includes(url.pathname)) {
    event.respondWith(caches.match(req, { cacheName: CACHE }).then((r) => r || fetch(req)));
  }
});
