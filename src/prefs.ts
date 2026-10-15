// Settings, themes, column layouts and other remembered choices, kept in settings.json in Settings › Locations'
// Settings folder, so they can sync between Macs through iCloud Drive or Dropbox. localStorage keeps a copy so every
// module can read synchronously at startup. The file wins: when it differs from the copy (changed on another Mac),
// the copy is refreshed and the page starts again once with it.

import { invoke } from "@tauri-apps/api/core";

/** localStorage keys that aren't preferences: kept SQL tabs and history, which moved to files of their own. */
const NOT_PREFS = ["sql-tabs:", "history:"];
export const isPref = (key: string) => !NOT_PREFS.some((p) => key.startsWith(p));

/** Whether two sets of preferences hold the same keys and values. */
export function same(a: Record<string, unknown>, b: Record<string, unknown>) {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => k in b && JSON.stringify(a[k]) === JSON.stringify(b[k]));
}

const mem: Record<string, unknown> = {};
try {
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i)!;
    if (!isPref(key)) continue;
    try {
      mem[key] = JSON.parse(localStorage.getItem(key)!);
    } catch {} // not JSON: not one of ours
  }
} catch {} // unavailable storage: the file alone, once it loads

/** Only a file that loaded (or didn't exist yet) is ever written over. */
let writable = false;
let timer = 0;
let problem = "";

export const prefsProblem = () => problem;

export function readPref<T>(key: string): T | undefined {
  return mem[key] as T | undefined;
}

/** Sets (or with `undefined` removes) a preference; the file is written a moment later. */
export function writePref(key: string, value: unknown) {
  if (value === undefined) delete mem[key];
  else mem[key] = value;
  try {
    if (value === undefined) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
  } catch {} // the file still has it
  clearTimeout(timer);
  timer = window.setTimeout(flushPrefs, 400);
}

export async function flushPrefs() {
  clearTimeout(timer);
  timer = 0;
  if (!writable) return;
  try {
    await invoke("write_doc", { doc: "settings", text: JSON.stringify(mem) });
    problem = "";
  } catch (e) {
    problem = String(e);
  }
}

/** Reads settings.json. True when it held something other than the copy, which is then refreshed; at startup the
 *  page starts again with it (once). */
export async function loadPrefs(startup: boolean): Promise<boolean> {
  writable = false;
  let text: string | null;
  try {
    text = await invoke<string | null>("read_doc", { doc: "settings" });
  } catch (e) {
    problem = String(e); // e.g. an unreadable file, which is left as it is; the copy carries on
    return false;
  }
  if (text === null) {
    writable = true;
    await flushPrefs(); // the first file starts with what this Mac had
    return false;
  }
  let file: unknown;
  try {
    file = JSON.parse(text);
  } catch {
    file = null;
  }
  if (typeof file !== "object" || file === null || Array.isArray(file)) {
    problem = "settings.json is not a JSON object; fix or move it";
    return false;
  }
  writable = true;
  const data = file as Record<string, unknown>;
  if (same(data, mem)) return false;
  for (const key of Object.keys(mem)) delete mem[key];
  Object.assign(mem, data);
  if (!startup) {
    // This session's modules hold the old values; saving them now would write over what was found.
    writable = false;
    problem = "settings found in this folder apply when TableMinus starts again; changes until then aren't saved";
  }
  try {
    for (let i = localStorage.length - 1; i >= 0; i--) {
      const key = localStorage.key(i)!;
      if (isPref(key) && !(key in data)) localStorage.removeItem(key);
    }
    for (const [key, value] of Object.entries(data)) localStorage.setItem(key, JSON.stringify(value));
  } catch {
    return true; // no copy to start again from
  }
  if (startup && !sessionStorage.getItem("prefs-restarted")) {
    sessionStorage.setItem("prefs-restarted", "1"); // never a loop, whatever the storage does
    location.reload();
  }
  return true;
}

export const prefsReady = loadPrefs(true);
