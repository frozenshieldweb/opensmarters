// SPDX-License-Identifier: GPL-3.0-or-later
/// <reference types="vite/client" />

/** Phosphor icon path data (vite.config.ts, plugin `opensmarters:phosphor`). */
declare module "~icons/*" {
  const paths: string[];
  export default paths;
}
