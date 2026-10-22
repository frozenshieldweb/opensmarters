// SPDX-License-Identifier: GPL-3.0-or-later
// opensmarters web: the browser UI of the self-hosted web edition (docs/WEB_PROMPT.md §6).

import { render } from "preact";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/shell.css";
import "./styles/screens.css";
import "./styles/player.css";
import { App } from "./app";
import { installFocus } from "./focus/dom";
import { back } from "./state/router";

installFocus(() => {
  back();
});

const root = document.getElementById("app");
if (root) render(<App />, root);

// Install as an app: a service worker that caches the app shell only (never /api).
if (import.meta.env.PROD && "serviceWorker" in navigator && window.isSecureContext) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js", { scope: "/" }).catch(() => {});
  });
}
