// SPDX-License-Identifier: GPL-3.0-or-later
// Who is signed in, their provider accounts, the current account and their settings.

import { computed, signal } from "@preact/signals";
import { api, ApiError, onUnauthorized } from "../api/client";
import type { Account, Settings, Status, User } from "../api/types";
import { load, save } from "../lib/storage";

export const status = signal<Status | null>(null);
/** Set when /api/status itself failed (server down): the boot screen shows it. */
export const bootError = signal<string | null>(null);
export const user = computed<User | null>(() => status.value?.user ?? null);

export const accounts = signal<Account[] | null>(null);
export const accountsError = signal<string | null>(null);
const accountIdSig = signal<string | null>(null);

export const DEFAULT_SETTINGS: Settings = { time_format: "24h", live_format: "ts", autoplay_next: true };
export const settings = signal<Settings>(DEFAULT_SETTINGS);

/** The account being browsed (the first one unless the user picked another). */
export const account = computed<Account | null>(() => {
  const list = accounts.value;
  if (!list || list.length === 0) return null;
  return list.find((a) => a.id === accountIdSig.value) ?? list[0] ?? null;
});
export const accountId = computed(() => account.value?.id ?? null);

const accountKey = () => `account:${user.value?.id ?? ""}`;

/** Switches the current account (remembered per user in this browser). */
export function selectAccount(id: string): void {
  accountIdSig.value = id;
  save(accountKey(), id);
}

/** Fetches /api/status. */
export async function refreshStatus(): Promise<void> {
  try {
    status.value = await api.status();
    bootError.value = null;
  } catch (e) {
    bootError.value = e instanceof ApiError ? e.message : "Can't reach the server.";
  }
}

/** Loads the signed-in user's accounts and settings. */
export async function loadUserData(): Promise<void> {
  accountIdSig.value = load(accountKey());
  accountsError.value = null;
  const [acc, set] = await Promise.allSettled([api.accounts(), api.settings()]);
  if (acc.status === "fulfilled") accounts.value = acc.value;
  else {
    accounts.value = accounts.value ?? null;
    accountsError.value = acc.reason instanceof ApiError ? acc.reason.message : "Can't load your providers.";
  }
  if (set.status === "fulfilled") settings.value = { ...DEFAULT_SETTINGS, ...set.value };
}

export async function reloadAccounts(): Promise<void> {
  accounts.value = await api.accounts();
}

/** Replaces one account in the list (after an edit or refresh). */
export function putAccount(a: Account): void {
  const list = accounts.value ?? [];
  accounts.value = list.some((x) => x.id === a.id) ? list.map((x) => (x.id === a.id ? a : x)) : [...list, a];
}

export function dropAccount(id: string): void {
  accounts.value = (accounts.value ?? []).filter((a) => a.id !== id);
}

/** Saves the settings (optimistic; reverts on failure). */
export async function updateSettings(patch: Partial<Settings>): Promise<void> {
  const before = settings.value;
  const next = { ...before, ...patch };
  settings.value = next;
  try {
    const saved = await api.saveSettings(patch);
    settings.value = { ...DEFAULT_SETTINGS, ...saved };
  } catch (e) {
    settings.value = before;
    throw e;
  }
}

/** Signed in (from the login, setup or invite response). */
export async function signedIn(u: User): Promise<void> {
  status.value = { version: status.value?.version ?? "", setup_required: false, user: u };
  accounts.value = null;
  await loadUserData();
}

/** Signs out (always forgets the local session state, even if the request fails). */
export async function signOut(): Promise<void> {
  try {
    await api.logout();
  } finally {
    signedOut();
  }
}

export function signedOut(): void {
  status.value = status.value ? { ...status.value, user: null } : null;
  accounts.value = null;
  settings.value = DEFAULT_SETTINGS;
}

onUnauthorized(() => {
  if (user.value) signedOut();
});
