// Settings › Locations, and the files that kept SQL tabs, saved queries, metrics boards and query history live in
// (locations.rs). Each file is one JSON object keyed by connection name, held in memory here and written back shortly
// after a change.

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { $, el } from "./dom";
import { flushPrefs, loadPrefs, prefsProblem } from "./prefs";

type Doc = "sql_tabs" | "history" | "favorites" | "metrics";
/** Where each doc lived before it had a file: localStorage keys `<prefix><connection>`. */
const LEGACY: Partial<Record<Doc, string>> = { sql_tabs: "sql-tabs:", history: "history:" };

type Held = { data: Record<string, unknown>; writable: boolean; timer: number; problem: string };
const held = new Map<Doc, Held>();

async function load(doc: Doc) {
  const h: Held = { data: {}, writable: false, timer: 0, problem: "" };
  held.set(doc, h);
  try {
    const text = await invoke<string | null>("read_doc", { doc });
    const data = text ? JSON.parse(text) : {};
    if (typeof data !== "object" || data === null || Array.isArray(data)) throw "not a JSON object";
    h.data = data;
    h.writable = true; // only a file that loaded is ever written over
  } catch (e) {
    h.problem = String(e); // kept in memory for this session; the file stays as it is
    return;
  }
  // Move entries from localStorage in once, then forget them there when the file has them.
  const prefix = LEGACY[doc];
  if (!prefix) return;
  const moved: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i)!;
    if (!key.startsWith(prefix)) continue;
    const name = key.slice(prefix.length);
    try {
      if (!(name in h.data)) h.data[name] = JSON.parse(localStorage.getItem(key) ?? "null");
      moved.push(key);
    } catch {} // unreadable: left where it is
  }
  if (moved.length && (await write(doc))) for (const key of moved) localStorage.removeItem(key);
}

async function write(doc: Doc) {
  const h = held.get(doc);
  if (!h?.writable) return false;
  clearTimeout(h.timer);
  h.timer = 0;
  try {
    await invoke("write_doc", { doc, text: JSON.stringify(h.data) });
    return true;
  } catch (e) {
    h.problem = String(e);
    return false;
  }
}

/** Every doc read; connect waits for this before reading tabs, history and saved queries. */
export const docsReady = Promise.all([load("sql_tabs"), load("history"), load("favorites"), load("metrics")]);

export function docEntry<T>(doc: Doc, conn: string): T | undefined {
  return held.get(doc)?.data[conn] as T | undefined;
}

/** Sets (or with `undefined` removes) a connection's entry; the file is written a moment later. */
export function setDocEntry(doc: Doc, conn: string, value: unknown) {
  const h = held.get(doc);
  if (!h) return;
  if (value === undefined) delete h.data[conn];
  else h.data[conn] = value;
  clearTimeout(h.timer);
  h.timer = window.setTimeout(() => write(doc), 500);
}

// ---------- Settings › Locations ----------

type Location = "connections" | "queries" | "history" | "settings";
type Folders = Record<Location, string> & { default: string };
const ROWS: [Location, string, Doc[]][] = [
  ["connections", "Connections", []], ["queries", "SQL tabs, saved queries and metrics boards", ["sql_tabs", "favorites", "metrics"]], ["history", "History", ["history"]],
  ["settings", "Settings, themes and column layouts", []],
];

const settings = $("settings");
const tab = el("button");
tab.setAttribute("role", "tab");
tab.dataset.tab = "locations";
const ns = "http://www.w3.org/2000/svg";
const icon = document.createElementNS(ns, "svg");
icon.setAttribute("viewBox", "0 0 24 24");
icon.setAttribute("aria-hidden", "true");
const path = document.createElementNS(ns, "path");
path.setAttribute("d", "M3 7h7l2 2h9v10H3z");
icon.append(path);
tab.append(icon, "Locations");
settings.querySelector(".prefs-tabs")!.append(tab);
const pane = el("section", { hidden: true });
pane.dataset.pane = "locations";
settings.querySelector(":scope > footer")!.before(pane);
tab.addEventListener("click", render);

const note = el("p", { className: "status" });

async function render() {
  let folders: Folders;
  try {
    folders = await invoke<Folders>("locations");
  } catch (e) {
    return pane.replaceChildren(el("p", { className: "status error", textContent: String(e) }));
  }
  pane.replaceChildren(
    ...ROWS.map(([which, label, docs]) => {
      const choose = el("button", { type: "button", textContent: "Choose…" });
      const reset = el("button", { type: "button", className: "ghost", textContent: "Default", hidden: folders[which] === folders.default });
      choose.onclick = async () => {
        const picked = await openDialog({ directory: true, defaultPath: folders[which] });
        if (typeof picked === "string") move(which, docs, picked);
      };
      reset.onclick = () => move(which, docs, null);
      const problem = [...docs.map((d) => held.get(d)?.problem), which === "settings" ? prefsProblem() : ""].filter(Boolean).join("; ");
      return el("div", { className: "row" },
        el("span", { className: "k", textContent: label }),
        el("span", { className: "loc" },
          el("code", { className: "loc-path", textContent: folders[which], title: folders[which] }),
          el("span", { className: "row-items" }, choose, reset),
          ...(problem ? [el("span", { className: "status error", textContent: problem })] : [])));
    }),
    el("p", { className: "hint note", textContent: "Choose a folder in iCloud Drive or Dropbox to share these between Macs. Files already in the chosen folder are used; otherwise the current ones are copied there. Passwords always stay in the keychain." }),
    note,
  );
}

async function move(which: Location, docs: Doc[], folder: string | null) {
  note.className = "status";
  note.textContent = "";
  try {
    for (const doc of docs) if (held.get(doc)?.timer) await write(doc); // nothing pending is left behind in the old folder
    if (which === "settings") await flushPrefs();
    await invoke("set_location", { which, folder });
    for (const doc of docs) await load(doc);
    if (which === "connections") dispatchEvent(new Event("connections-moved")); // main.ts reloads the launcher's list
    // Settings found in the new folder (another Mac's) apply from the next start, rather than mid-session.
    const found = which === "settings" && (await loadPrefs(false));
    note.textContent = found ? "Moved. The settings found there apply when TableMinus starts again." : "Moved.";
    note.className = "status done";
  } catch (e) {
    note.textContent = String(e);
    note.className = "status error";
  }
  render();
}

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
.loc { display: grid; gap: 6px; min-width: 0; }
.loc-path { font: 11.5px var(--mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
