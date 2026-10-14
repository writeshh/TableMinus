// Saved queries (favorites): the sidebar's Queries tab. Each connection has its own, in folders, kept in
// favorites.json next to the SQL tabs (Settings › Locations, so a shared folder shares them). A favorite with a
// keyword is also an autocomplete snippet in the editor.

import { $, el } from "./dom";
import { docEntry, setDocEntry } from "./locations";

export type Favorite = { id: string; name: string; sql: string; folder: string; keyword: string };
type Saved = { folders: string[]; items: Favorite[] };
type MenuItem = [string, () => void] | "-";

export type Ctx = {
  conn: () => { name: string; kind: string } | null;
  /** Opens SQL in a new SQL tab. */
  open: (title: string, sql: string) => void;
  menu: (e: MouseEvent, items: MenuItem[]) => void;
  confirm: (text: string, okLabel: string) => Promise<boolean>;
  /** The keyword favorites changed: the editor's snippets follow. */
  snippets: (list: Favorite[]) => void;
};

let ctx: Ctx;

function load(): Saved {
  const c = ctx.conn();
  const s = c && docEntry<Saved>("favorites", c.name);
  return {
    folders: Array.isArray(s?.folders) ? s.folders.filter((f) => typeof f === "string") : [],
    items: Array.isArray(s?.items) ? s.items.filter((f) => f && typeof f.sql === "string") : [],
  };
}

function save(s: Saved) {
  const c = ctx.conn();
  if (!c) return;
  s.folders = [...new Set(s.folders.filter(Boolean))].sort((a, b) => a.localeCompare(b));
  setDocEntry("favorites", c.name, s);
  render();
}

// ---------- The Queries tab ----------

const search = el("input", { type: "search", placeholder: "Search saved queries" });
const newFolder = el("button", { type: "button", className: "ghost", textContent: "New folder", title: "Add a folder" });
const list = el("div", { className: "favorites" });
const none = el("p", { className: "hint", textContent: "Save a query with Save as Favorite (right-click a history entry, or the command) and it appears here." });
const pane = el("div", { id: "side-queries", className: "side-pane", hidden: true },
  el("div", { className: "favorites-head" }, search, newFolder), list, none);
$("side-history").after(pane);
const tabButton = el("button", { textContent: "Queries", title: "Saved queries" });
tabButton.dataset.side = "queries";
$("side-history").parentElement!.querySelector(".side-tabs")!.querySelector('[data-side="history"]')!.after(tabButton);

export function initFavorites(c: Ctx) {
  ctx = c;
  search.oninput = render;
  newFolder.onclick = async () => {
    const name = await askName("New folder", "");
    if (name) save({ ...load(), folders: [...load().folders, name] });
  };
}

/** Redraws the tab for the active connection, and hands its snippets to the editor. */
export function render() {
  if (!ctx) return;
  const s = load();
  ctx.snippets(s.items.filter((f) => f.keyword.trim()));
  const q = search.value.toLowerCase();
  const shown = s.items.filter((f) => !q || `${f.name} ${f.keyword} ${f.sql}`.toLowerCase().includes(q));
  const folders = [...new Set([...s.folders, ...s.items.map((f) => f.folder).filter(Boolean)])].sort((a, b) => a.localeCompare(b));
  const row = (f: Favorite) => {
    const b = el("button", { className: "history-item", title: "Open in a new SQL tab" },
      el("span", { className: "when", textContent: f.keyword ? `${f.name} · ${f.keyword}` : f.name }),
      el("code", { textContent: f.sql }));
    b.onclick = () => ctx.open(f.name, f.sql);
    b.oncontextmenu = (e) => ctx.menu(e, [
      ["Open in a new tab", () => ctx.open(f.name, f.sql)],
      ["Edit…", () => edit(f)],
      ["Copy SQL", () => navigator.clipboard.writeText(f.sql).catch(() => {})],
      "-",
      ["Delete", async () => (await ctx.confirm(`Delete the saved query "${f.name}"?`, "Delete")) && save({ ...load(), items: load().items.filter((x) => x.id !== f.id) })],
    ]);
    return b;
  };
  list.replaceChildren(
    ...folders.map((folder) => {
      const inside = shown.filter((f) => f.folder === folder);
      const summary = el("summary", { textContent: `${folder} (${inside.length})` });
      summary.oncontextmenu = (e) => ctx.menu(e, [
        ["Rename…", () => renameFolder(folder)],
        ["Delete folder", () => deleteFolder(folder)],
      ]);
      return el("details", { open: !!q || inside.length > 0 }, summary, ...inside.map(row));
    }),
    ...shown.filter((f) => !f.folder).map(row),
  );
  none.hidden = s.items.length > 0 || folders.length > 0;
}

/** A one-field dialog (the webview has no prompt()); resolves to the trimmed name, or null when cancelled. */
function askName(title: string, value: string): Promise<string | null> {
  const input = el("input", { value, required: true });
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const form = el("form", { className: "favorite-form" }, input,
    el("footer", {}, cancel, el("span", { className: "spacer" }), el("button", { type: "submit", className: "primary", textContent: "OK" })));
  const d = el("dialog", { className: "transfer" }, el("h3", { textContent: title }), form);
  document.body.append(d);
  return new Promise((resolve) => {
    let result: string | null = null;
    cancel.onclick = () => d.close();
    form.onsubmit = (e) => (e.preventDefault(), (result = input.value.trim() || null), d.close());
    d.onclose = () => (d.remove(), resolve(result));
    d.showModal();
    input.select();
  });
}

async function renameFolder(from: string) {
  const to = await askName("Rename folder", from);
  if (!to || to === from) return;
  const s = load();
  save({ folders: s.folders.map((f) => (f === from ? to : f)), items: s.items.map((f) => (f.folder === from ? { ...f, folder: to } : f)) });
}

/** Deletes a folder; its queries move to the top level rather than disappear. */
async function deleteFolder(folder: string) {
  const s = load();
  const n = s.items.filter((f) => f.folder === folder).length;
  if (n && !(await ctx.confirm(`Delete the folder "${folder}"? Its ${n} saved quer${n === 1 ? "y moves" : "ies move"} to the top level.`, "Delete"))) return;
  save({ folders: s.folders.filter((f) => f !== folder), items: s.items.map((f) => (f.folder === folder ? { ...f, folder: "" } : f)) });
}

// ---------- The editor (Ref: FavoriteEditorWindow) ----------

/** Saves SQL as a new favorite, after the user names it. */
export const addFavorite = (sql: string) => edit({ id: "", name: firstLine(sql), sql: sql.trim(), folder: "", keyword: "" });

const firstLine = (sql: string) => {
  const line = sql.trim().split("\n")[0].replace(/\s+/g, " ");
  return line.length > 40 ? line.slice(0, 40) + "…" : line;
};

function edit(f: Favorite) {
  if (!ctx.conn()) return;
  const s = load();
  const name = el("input", { value: f.name, required: true });
  const folder = el("input", { value: f.folder, placeholder: "None" });
  folder.setAttribute("list", "favorite-folders");
  const folders = el("datalist", { id: "favorite-folders" }, ...s.folders.map((x) => el("option", { value: x })));
  const keyword = el("input", { value: f.keyword, placeholder: "e.g. top10", spellcheck: false, pattern: "\\w*", title: "Letters, digits and _" });
  const sql = el("textarea", { value: f.sql, rows: 8, spellcheck: false });
  const error = el("p", { className: "status error" });
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const ok = el("button", { type: "submit", className: "primary", textContent: f.id ? "Save" : "Add" });
  const label = (text: string, control: HTMLElement) => el("label", {}, el("span", { textContent: text }), control);
  const form = el("form", { className: "favorite-form" },
    label("Name", name), label("Folder", folder), folders,
    label("Keyword", keyword), el("p", { className: "hint indent", textContent: "Typing the keyword in the editor offers this query as a completion." }),
    label("SQL", sql), error,
    el("footer", {}, cancel, el("span", { className: "spacer" }), ok));
  const d = el("dialog", { className: "transfer" }, el("h3", { textContent: f.id ? "Edit saved query" : "Save query" }), form);
  d.onclose = () => d.remove();
  document.body.append(d);
  cancel.onclick = () => d.close();
  form.onsubmit = (e) => {
    e.preventDefault();
    const k = keyword.value.trim();
    const all = load();
    if (!sql.value.trim()) return void (error.textContent = "The SQL is empty.");
    if (k && all.items.some((x) => x.id !== f.id && x.keyword === k)) return void (error.textContent = `Another saved query has the keyword "${k}".`);
    const next: Favorite = { id: f.id || crypto.randomUUID(), name: name.value.trim() || firstLine(sql.value), sql: sql.value, folder: folder.value.trim(), keyword: k };
    const items = f.id ? all.items.map((x) => (x.id === f.id ? next : x)) : [...all.items, next];
    save({ folders: next.folder ? [...all.folders, next.folder] : all.folders, items });
    d.close();
  };
  d.showModal();
  name.select();
}
