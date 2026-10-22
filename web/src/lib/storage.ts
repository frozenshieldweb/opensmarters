// SPDX-License-Identifier: GPL-3.0-or-later
// Small per-browser preferences (the chosen account, sort orders). Never secrets: the session
// is an HttpOnly cookie and provider credentials never reach the browser.

const PREFIX = "opensmarters:";

export function load(key: string): string | null {
  try {
    return localStorage.getItem(PREFIX + key);
  } catch {
    return null;
  }
}

export function save(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(PREFIX + key);
    else localStorage.setItem(PREFIX + key, value);
  } catch {
    // Private mode or storage disabled: preferences just don't persist.
  }
}
