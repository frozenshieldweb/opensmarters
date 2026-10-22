// SPDX-License-Identifier: GPL-3.0-or-later
// The clock and toasts (DESIGN.md §6.12).

import { signal } from "@preact/signals";

/** Unix seconds, updated every 10 s (clock, programme progress). */
export const now = signal(Math.floor(Date.now() / 1000));
setInterval(() => {
  now.value = Math.floor(Date.now() / 1000);
}, 10_000);

export type ToastKind = "info" | "success" | "warning" | "error";
export interface Toast {
  id: number;
  kind: ToastKind;
  text: string;
}

export const toasts = signal<Toast[]>([]);
let toastId = 1;

/** Shows a toast for 4 s (errors 6 s); at most two at once, newest at the bottom. */
export function toast(text: string, kind: ToastKind = "info"): void {
  const t: Toast = { id: toastId++, kind, text };
  toasts.value = [...toasts.value.filter((x) => x.text !== text), t].slice(-2);
  setTimeout(
    () => {
      toasts.value = toasts.value.filter((x) => x.id !== t.id);
    },
    kind === "error" ? 6000 : 4000,
  );
}
