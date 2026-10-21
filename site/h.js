// SPDX-License-Identifier: GPL-3.0-or-later
// A tiny element tree shared by the build step (toHTML, in Node) and the page (toDOM, in
// the browser). Text is always text: toDOM never parses markup (no innerHTML), toHTML
// escapes every string.

const VOID = new Set(["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "wbr"]);
const SVG_NS = "http://www.w3.org/2000/svg";

/** h("a", { href, class }, "text", h("span", …)) → { tag, attrs, children } */
export function h(tag, attrs, ...children) {
  return { tag, attrs: attrs || {}, children: flat(children) };
}

function flat(list) {
  const out = [];
  for (const c of list) {
    if (c === null || c === undefined || c === false || c === "") continue;
    if (Array.isArray(c)) out.push(...flat(c));
    else out.push(typeof c === "number" ? String(c) : c);
  }
  return out;
}

const ESC = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
export const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ESC[c]);

// Private-use code points (Phosphor icons) are written as entities: readable source.
const escText = (s) =>
  esc(s).replace(/[-]/g, (c) => `&#x${c.codePointAt(0).toString(16)};`);

/** Serialises a tree to HTML (build step). */
export function toHTML(node) {
  if (typeof node === "string") return escText(node);
  if (Array.isArray(node)) return node.map(toHTML).join("");
  const { tag, attrs, children } = node;
  let s = `<${tag}`;
  for (const [k, v] of Object.entries(attrs)) {
    if (v === false || v === null || v === undefined) continue;
    s += v === true ? ` ${k}` : ` ${k}="${esc(v)}"`;
  }
  s += ">";
  if (VOID.has(tag)) return s;
  // One line per <head> child and per top-level section: readable generated files.
  // (Only where whitespace can't change the rendering.)
  const sep = tag === "head" || tag === "html" || tag === "body" ? "\n" : "";
  return `${s}${sep}${children.map(toHTML).join(sep)}${sep}</${tag}>`;
}

/** Builds real DOM nodes (runtime). */
export function toDOM(node, ns) {
  if (typeof node === "string") return document.createTextNode(node);
  if (Array.isArray(node)) {
    const f = document.createDocumentFragment();
    for (const c of node) f.append(toDOM(c, ns));
    return f;
  }
  const space = node.tag === "svg" ? SVG_NS : ns;
  const el = space ? document.createElementNS(space, node.tag) : document.createElement(node.tag);
  for (const [k, v] of Object.entries(node.attrs)) {
    if (v === false || v === null || v === undefined) continue;
    if (k === "style") throw new Error("inline styles are not allowed (CSP)");
    el.setAttribute(k, v === true ? "" : String(v));
  }
  for (const c of node.children) el.append(toDOM(c, space));
  return el;
}

/** Replaces the element with id `id` by a freshly built tree (runtime). */
export function replace(id, node) {
  const old = document.getElementById(id);
  if (old) old.replaceWith(toDOM(node));
}
