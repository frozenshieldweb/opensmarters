// SPDX-License-Identifier: GPL-3.0-or-later
// Release helpers shared by the build step and the page.

/** "v{version} · {size} MB" + { version: "0.1.0", size: 59 } → "v0.1.0 · 59 MB" */
export function fmt(s, vars = {}) {
  return s.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
}

/** A config value that hasn't been filled in yet ("TODO…", empty or missing). */
export const isTodo = (v) => v === undefined || v === null || v === "" || /^TODO\b/.test(String(v));

/** Compares "0.10.0" > "0.2.0"; a pre-release sorts before its release. */
export function cmpVersion(a, b) {
  const parse = (v) => {
    const [core, pre] = String(v).split("-", 2);
    return { n: core.split(".").map((x) => parseInt(x, 10) || 0), pre: pre || "" };
  };
  const x = parse(a);
  const y = parse(b);
  for (let i = 0; i < 3; i++) if (x.n[i] !== y.n[i]) return (x.n[i] || 0) - (y.n[i] || 0);
  if (x.pre === y.pre) return 0;
  if (!x.pre) return 1;
  if (!y.pre) return -1;
  return x.pre < y.pre ? -1 : 1;
}

/** The feature ids this release ships (since <= version), in release order. */
export function shippedFeatures(release) {
  if (isTodo(release.version)) return [];
  return release.features.filter((f) => cmpVersion(f.since, release.version) <= 0).map((f) => f.id);
}

/** The installer when there is one, else the portable zip. */
export function primaryDownload(release) {
  const d = release.downloads || {};
  const ok = (e) => e && !isTodo(e.file);
  if (ok(d.installer)) return { kind: "installer", ...d.installer };
  if (ok(d.portable)) return { kind: "portable", ...d.portable };
  return { kind: "portable", file: "TODO", url: "TODO", sizeMB: 0, sha256: "TODO" };
}

/** Every file with a checksum, installer first. */
export function downloadsList(release) {
  const d = release.downloads || {};
  return [
    d.installer && !isTodo(d.installer.file) ? { kind: "installer", ...d.installer } : null,
    d.portable && !isTodo(d.portable.file) ? { kind: "portable", ...d.portable } : null,
  ].filter(Boolean);
}

/** Whole megabytes for captions ("59"); one decimal below 10 MB. */
export const sizeText = (mb) => (mb >= 10 ? String(Math.round(mb)) : mb > 0 ? mb.toFixed(1) : "NN");

/** A link that works, or "#todo" plus a flag the page turns into a visible notice. */
export function link(url) {
  return isTodo(url) ? { href: "#todo", todo: true } : { href: url, todo: false };
}

/** The repository's pages (README build section, licence, security policy). */
export function repoLinks(release) {
  const base = isTodo(release.sourceUrl) ? null : release.sourceUrl.replace(/\/+$/, "");
  return {
    source: base,
    build: base && `${base}#build-from-source`,
    licence: base && `${base}/blob/main/LICENSE`,
    security: base && `${base}/blob/main/docs/SECURITY.md`,
    signing: base && `${base}/blob/main/docs/CODE_SIGNING.md`,
  };
}

/** True when anything the visitor would click is still a TODO. */
export function hasTodos(release) {
  const files = downloadsList(release);
  return isTodo(release.sourceUrl) || files.length === 0 || files.some((f) => isTodo(f.url)) || isTodo(release.downloads.sumsUrl);
}

/** The synthetic release for the dev-only ?preview=x.y.z switch: never the default. */
export function previewRelease(release, version) {
  const zip = release.downloads.portable || { sizeMB: 0 };
  return {
    ...release,
    version,
    preview: true,
    downloads: {
      installer: {
        file: `opensmarters-${version}-windows-x64-setup.exe`,
        url: "TODO",
        sizeMB: zip.sizeMB,
        sha256: "0".repeat(64),
      },
      portable: { ...zip, file: `opensmarters-${version}-windows-x64.zip`, url: "TODO", sha256: "0".repeat(64) },
      sumsFile: "SHA256SUMS.txt",
      sumsUrl: "TODO",
    },
  };
}
