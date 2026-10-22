// SPDX-License-Identifier: GPL-3.0-or-later
// Synthetic artwork for the mock: posters, backdrops, episode stills and channel logos as small
// SVG images (gradients and soft circles, like crates/fake-panel's generated art). Served as
// `image/svg+xml` through <img>, where SVG can't run scripts.

import { hashStr, rng } from "./catalogue.ts";

const PALETTES: [string, string][] = [
  ["#f4a259", "#5b8e7d"], ["#7d5ba6", "#e07a5f"], ["#3d5a80", "#98c1d9"], ["#c44569", "#574b90"],
  ["#2a9d8f", "#264653"], ["#e9c46a", "#e76f51"], ["#6d597a", "#b56576"], ["#355070", "#eaac8b"],
  ["#588157", "#a3b18a"], ["#9c6644", "#ddb892"], ["#1d3557", "#e63946"], ["#2b2d42", "#8d99ae"],
];

const esc = (s: string) => s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function letters(name: string): string {
  const words = name.split(/\s+/).map((w) => [...w].filter((c) => /[\p{L}\p{N}]/u.test(c))).filter((w) => w.length);
  if (!words.length) return "?";
  return words.length === 1 ? words[0]!.slice(0, 2).join("").toUpperCase() : (words[0]![0]! + words[1]![0]!).toUpperCase();
}

function art(seed: string, w: number, h: number, label: string | null): string {
  const r = rng(hashStr(seed));
  const [a, b] = PALETTES[Math.floor(r() * PALETTES.length)]!;
  const circles: string[] = [];
  const big = Math.min(w, h) * (0.28 + r() * 0.14);
  const bx = w * (0.3 + r() * 0.4);
  const by = h * (0.25 + r() * 0.25);
  circles.push(`<circle cx="${bx.toFixed(1)}" cy="${by.toFixed(1)}" r="${(big * 1.12).toFixed(1)}" fill="none" stroke="#fff" stroke-opacity="0.35" stroke-width="${(w / 160).toFixed(1)}"/>`);
  circles.push(`<circle cx="${bx.toFixed(1)}" cy="${by.toFixed(1)}" r="${big.toFixed(1)}" fill="${r() < 0.5 ? a : b}" fill-opacity="0.85"/>`);
  for (let i = 0; i < 6; i++) {
    circles.push(
      `<circle cx="${(r() * w).toFixed(1)}" cy="${(r() * h).toFixed(1)}" r="${(Math.min(w, h) * (0.04 + r() * 0.12)).toFixed(1)}" fill="#fff" fill-opacity="${(0.05 + r() * 0.1).toFixed(2)}"/>`,
    );
  }
  const text = label
    ? `<rect x="0" y="${(h * 0.74).toFixed(1)}" width="${w}" height="${(h * 0.26).toFixed(1)}" fill="#000" fill-opacity="0.18"/>` +
      `<text x="${w / 2}" y="${(h * 0.92).toFixed(1)}" text-anchor="middle" font-family="Georgia, serif" font-size="${(h * 0.14).toFixed(1)}" fill="#fff" fill-opacity="0.9" letter-spacing="4">${esc(letters(label))}</text>`
    : "";
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="${r() < 0.5 ? 1 : 0.3}" y2="1">` +
    `<stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs>` +
    `<rect width="${w}" height="${h}" fill="url(#g)"/>${circles.join("")}${text}</svg>`
  );
}

function logo(seed: string, name: string): string {
  const r = rng(hashStr(seed));
  const [a, b] = PALETTES[Math.floor(r() * PALETTES.length)]!;
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="160" height="160" viewBox="0 0 160 160">` +
    `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs>` +
    `<circle cx="80" cy="80" r="70" fill="url(#g)"/><circle cx="80" cy="80" r="56" fill="none" stroke="#fff" stroke-width="5" stroke-opacity="0.8"/>` +
    `<text x="80" y="97" text-anchor="middle" font-family="Verdana, sans-serif" font-size="46" font-weight="bold" fill="#fff">${esc(letters(name))}</text></svg>`
  );
}

/** The SVG for an image token (`p-…` poster, `q-…` series poster, `b-…`/`c-…` backdrop, `s-…` still, `l-…` logo). */
export function imageFor(token: string, nameOf: (kind: string, id: string) => string | null): string | null {
  const m = /^([pqbcsl])-(.+)$/.exec(token);
  if (!m) return null;
  const kind = m[1]!;
  const id = decodeURIComponent(m[2]!);
  const name = nameOf(kind, id);
  if (name === null) return null;
  switch (kind) {
    case "p":
    case "q":
      return art(`${kind}${id}`, 400, 600, name);
    case "b":
    case "c":
      return art(`${kind}${id}`, 1280, 720, null);
    case "s":
      return art(`s${id}`, 640, 360, null);
    case "l":
      return logo(`l${id}`, name);
    default:
      return null;
  }
}
