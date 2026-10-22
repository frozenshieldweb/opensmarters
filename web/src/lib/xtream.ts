// SPDX-License-Identifier: GPL-3.0-or-later
// Recognising an Xtream Codes link pasted where a playlist address goes.

/** An Xtream Codes link pasted as a playlist (get.php?username=…&password=…). */
export function detectXtream(url: string): { server: string; username: string; password: string } | null {
  try {
    const u = new URL(url.trim());
    const username = u.searchParams.get("username");
    const password = u.searchParams.get("password");
    if (!/\/get\.php$/i.test(u.pathname) || !username || !password) return null;
    return { server: u.origin, username, password };
  } catch {
    return null;
  }
}
