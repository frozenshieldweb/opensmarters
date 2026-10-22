/// <reference types="vitest/config" />
// SPDX-License-Identifier: GPL-3.0-or-later
// Vite config for the opensmarters web UI.
//
//   npm run dev       the UI with the mock API (mock/server.ts) on http://localhost:5173
//   npm run build     the production build in web/dist (embedded by crates/web)
//   npm run preview   serves web/dist with the production CSP and the mock API
//
// Set OPENSMARTERS_API=http://127.0.0.1:8080 to use a real server (`opensmarters-web --demo`)
// instead of the mock: /api is then proxied to it.

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { defineConfig, type Plugin, type PreviewServer, type ViteDevServer } from "vite";

const here = dirname(fileURLToPath(import.meta.url));
const realApi = process.env["OPENSMARTERS_API"];

/** The production CSP (docs/WEB_PROMPT.md §5), also applied by `vite preview` for checks. */
export const CSP =
  "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; " +
  "style-src 'self'; script-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/**
 * Phosphor icons (MIT) as data, not markup: `import house from "~icons/regular/house"` gives
 * the SVG's path data, rendered by <Icon> as real SVG elements (no innerHTML, no icon font).
 */
function phosphorIcons(): Plugin {
  const PREFIX = "~icons/";
  const base = join(here, "node_modules", "@phosphor-icons", "core", "assets");
  return {
    name: "opensmarters:phosphor",
    enforce: "pre",
    resolveId(id) {
      return id.startsWith(PREFIX) ? "\0" + id : null;
    },
    load(id) {
      if (!id.startsWith("\0" + PREFIX)) return null;
      const [weight, name] = id.slice(PREFIX.length + 1).split("/");
      if (!weight || !name || !/^(regular|fill)$/.test(weight) || !/^[a-z0-9-]+$/.test(name)) {
        throw new Error(`bad icon import ${id}`);
      }
      const file = join(base, weight, weight === "regular" ? `${name}.svg` : `${name}-${weight}.svg`);
      const svg = readFileSync(file, "utf8");
      const paths = [...svg.matchAll(/<path d="([^"]+)"/g)].map((m) => m[1]);
      if (paths.length === 0 || /<(?!svg|path|\/svg)[a-z]/i.test(svg)) {
        throw new Error(`unexpected markup in ${file}`);
      }
      return `export default ${JSON.stringify(paths)};`;
    },
  };
}

/** The mock API in the dev and preview servers (unless a real server is configured). */
function mockApi(): Plugin {
  const attach = async (server: ViteDevServer | PreviewServer) => {
    const { MockApi } = await import("./mock/server.ts");
    const mock = new MockApi(join(here, ".media"));
    server.middlewares.use((req, res, next) => {
      void mock.handle(req, res, next);
    });
  };
  return {
    name: "opensmarters:mock-api",
    apply: () => !realApi,
    configureServer: attach,
    configurePreviewServer: attach,
  };
}

/** The production security headers on `vite preview` (and the safe subset in dev). */
function securityHeaders(): Plugin {
  const common = (res: { setHeader(k: string, v: string): void }) => {
    res.setHeader("X-Content-Type-Options", "nosniff");
    res.setHeader("Referrer-Policy", "no-referrer");
  };
  return {
    name: "opensmarters:security-headers",
    configureServer(server) {
      // No CSP in dev: Vite injects its styles with <style> elements there.
      server.middlewares.use((_req, res, next) => {
        common(res);
        next();
      });
    },
    configurePreviewServer(server) {
      server.middlewares.use((req, res, next) => {
        common(res);
        if (!req.url?.startsWith("/api/img/")) res.setHeader("Content-Security-Policy", CSP);
        next();
      });
    },
  };
}

/**
 * The service worker (sw.template.js) with the list of built files to cache as the app
 * shell. It never caches /api (data, images, video).
 */
function serviceWorker(): Plugin {
  return {
    name: "opensmarters:service-worker",
    apply: "build",
    generateBundle(_opts, bundle) {
      const files = Object.keys(bundle)
        .filter((f) => !f.endsWith(".map"))
        .map((f) => "/" + f);
      const shell = [
        "/",
        ...files,
        "/fonts/afterglow-sans-latin.woff2",
        "/fonts/fraunces-latin.woff2",
        "/img/hero-1920.avif",
        "/img/player-idle.jpg",
        "/img/home-ambient.jpg",
      ];
      const version = createHash("sha256").update(shell.join("\n")).digest("hex").slice(0, 12);
      const template = readFileSync(join(here, "sw.template.js"), "utf8");
      this.emitFile({
        type: "asset",
        fileName: "sw.js",
        source: template.replace('"__VERSION__"', JSON.stringify(version)).replace("[] /* __SHELL__ */", JSON.stringify(shell)),
      });
    },
  };
}

export default defineConfig({
  plugins: [phosphorIcons(), mockApi(), securityHeaders(), serviceWorker()],
  oxc: {
    jsx: { runtime: "automatic", importSource: "preact" },
  },
  resolve: {
    alias: {
      react: "preact/compat",
      "react-dom": "preact/compat",
    },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    // Never inline assets as data: URIs (fonts would break `font-src 'self'`).
    assetsInlineLimit: 0,
    sourcemap: false,
    reportCompressedSize: true,
    // hls.js (≈ 180 kB gzipped) is its own chunk, loaded on the first playback only.
    chunkSizeWarningLimit: 700,
  },
  server: {
    port: 5173,
    strictPort: false,
    proxy: realApi ? { "/api": { target: realApi, changeOrigin: false } } : {},
  },
  preview: {
    port: 4173,
    proxy: realApi ? { "/api": { target: realApi, changeOrigin: false } } : {},
  },
  test: {
    include: ["src/**/*.test.ts", "mock/**/*.test.ts"],
    environment: "node",
  },
});
