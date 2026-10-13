import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { EditorState } from "@codemirror/state";
import { createEditor } from "./editor";

type Saved = { name: string; kind: string; url: string; tag: string; confirm_writes: boolean };
type QueryResult = { columns: string[]; rows: (string | null)[][]; affected: number };
type Engine = {
  kind: string;
  label: string;
  mono: string;
  color: string;
  // Every field is optional: a blank input uses the default, shown as its placeholder.
  defaults?: { host: string; port: string; user: string; database: string; ssl: string };
  ssl?: [string, string][];
};

const PG_SSL: [string, string][] = [["prefer", "Prefer"], ["require", "Require"], ["disable", "Disable"]];
const MY_SSL: [string, string][] = [["off", "Off"], ["required", "Required"]];
const ENGINES: Engine[] = [
  { kind: "postgres", label: "PostgreSQL", mono: "Pg", color: "#2f6fd0", ssl: PG_SSL,
    defaults: { host: "127.0.0.1", port: "5432", user: "postgres", database: "", ssl: "prefer" } },
  { kind: "mysql", label: "MySQL", mono: "Ms", color: "#e8891c", ssl: MY_SSL,
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "", ssl: "off" } },
  { kind: "mariadb", label: "MariaDB", mono: "Ma", color: "#1f9d8f", ssl: MY_SSL,
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "", ssl: "off" } },
  { kind: "sqlite", label: "SQLite", mono: "Sl", color: "#7b4fd6" },
];
const engine = (kind: string) => ENGINES.find((e) => e.kind === kind) ?? ENGINES[0];
const TAGS: Record<string, string> = { local: "Local", development: "Development", staging: "Staging", production: "Production" };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const el = <K extends keyof HTMLElementTagNameMap>(tag: K, props: Partial<HTMLElementTagNameMap[K]> = {}, ...kids: (Node | string)[]) => {
  const n = Object.assign(document.createElement(tag), props);
  n.append(...kids);
  return n;
};
const form = $<HTMLFormElement>("form");
const field = (name: string) => form.elements.namedItem(name) as HTMLInputElement & HTMLSelectElement;
const editor = createEditor($("sql"), {
  change: (doc) => {
    const tab = current();
    if (tab?.kind === "sql") tab.sql = doc;
  },
  cursor: () => updateCursor(),
  run: () => runEditor(),
});

let saved: Saved[] = [];
let active: Saved | null = null;
let tables: string[] = [];

// All DOM is built with textContent, never innerHTML: names and cell values come from users and databases.

function setStatus(id: string, text: string, kind: "" | "busy" | "error" | "done" = "") {
  const s = $(id);
  s.textContent = text;
  s.className = `status ${kind}`;
}
const status = (text: string, kind: "" | "busy" | "error" | "done" = "") => setStatus("status", text, kind);

function avatar(kind: string) {
  const e = engine(kind);
  const a = el("span", { className: "avatar", textContent: e.mono, title: e.label });
  a.style.background = e.color;
  return a;
}
function tagPill(tag: string) {
  return tag ? el("span", { className: `tag ${tag}`, textContent: TAGS[tag] ?? tag }) : "";
}

function hostOf(c: Saved) {
  if (c.kind === "sqlite") return c.url;
  try {
    const u = new URL(c.url);
    return u.hostname + (u.pathname.length > 1 ? decodeURIComponent(u.pathname) : "");
  } catch {
    return c.url;
  }
}

// ---------- Launcher ----------

async function refresh() {
  saved = await invoke<Saved[]>("list_connections");
  renderConns();
}

function renderConns() {
  const q = $<HTMLInputElement>("search").value.toLowerCase();
  const shown = saved.filter((c) => [c.name, c.kind, c.tag, hostOf(c)].join(" ").toLowerCase().includes(q));
  $("no-conns").hidden = saved.length > 0;
  $("conns").replaceChildren(
    ...shown.map((c) => {
      const edit = el("button", { className: "icon", textContent: "Edit", title: `Edit ${c.name}` });
      const del = el("button", { className: "icon", textContent: "Delete", title: `Delete ${c.name}` });
      edit.onclick = (e) => (e.stopPropagation(), openEditor(c.kind, c));
      del.onclick = async (e) => {
        e.stopPropagation();
        if (!(await confirmDialog(`Delete "${c.name}"? Its keychain password is removed too.`, "Delete"))) return;
        await invoke("delete_connection", { name: c.name });
        refresh();
      };
      const li = el(
        "li",
        { tabIndex: 0 },
        avatar(c.kind),
        el("div", { className: "who" }, el("div", { className: "name" }, el("span", { textContent: c.name }), tagPill(c.tag)), el("div", { className: "host", textContent: hostOf(c) })),
        el("div", { className: "actions" }, edit, del),
      );
      li.onclick = () => connect(c);
      li.onkeydown = (e) => e.key === "Enter" && connect(c);
      return li;
    }),
  );
}

function confirmDialog(text: string, okLabel: string): Promise<boolean> {
  const d = $<HTMLDialogElement>("confirm");
  $("confirm-text").textContent = text;
  $("confirm-ok").textContent = okLabel;
  d.showModal();
  $("confirm-cancel").focus(); // the safe choice is the default
  return new Promise((resolve) => {
    $("confirm-ok").onclick = () => (d.close(), resolve(true));
    $("confirm-cancel").onclick = () => d.close();
    d.onclose = () => resolve(false); // Esc, or after ok (already resolved)
  });
}

// ---------- Step 1: engine picker ----------

let picked = ENGINES[0].kind;

function openPicker() {
  $<HTMLInputElement>("engine-search").value = "";
  renderEngines();
  $<HTMLDialogElement>("picker").showModal();
  $("engine-search").focus();
}

function renderEngines() {
  const q = $<HTMLInputElement>("engine-search").value.toLowerCase();
  const shown = ENGINES.filter((e) => e.label.toLowerCase().includes(q) || e.kind.includes(q));
  if (!shown.some((e) => e.kind === picked) && shown.length) picked = shown[0].kind;
  $("engines").replaceChildren(
    ...shown.map((e) => {
      const b = el("button", { className: "engine", type: "button" }, avatar(e.kind), el("span", { textContent: e.label }));
      b.setAttribute("role", "option");
      b.setAttribute("aria-selected", String(e.kind === picked));
      b.onclick = () => ((picked = e.kind), renderEngines());
      b.ondblclick = () => nextFromPicker();
      return b;
    }),
  );
}

function nextFromPicker() {
  $<HTMLDialogElement>("picker").close();
  openEditor(picked);
}

// ---------- Step 2: connection form ----------

let editing: Saved | null = null;
let editingKind = "postgres";

function openEditor(kind: string, existing?: Saved) {
  const e = engine(kind);
  editing = existing ?? null;
  editingKind = kind;
  form.reset();
  $("editor-title").textContent = `${e.label} connection`;
  form.querySelector<HTMLElement>(".net")!.hidden = kind === "sqlite";
  form.querySelector<HTMLElement>(".file")!.hidden = kind !== "sqlite";
  setStatus("form-status", "");

  const d = e.defaults;
  if (d) {
    for (const k of ["host", "port", "user"] as const) field(k).placeholder = d[k];
    field("database").placeholder = "Optional, choose one after connecting";
    field("ssl").replaceChildren(...e.ssl!.map(([v, label]) => el("option", { value: v, textContent: label })));
    field("ssl").value = d.ssl;
  }
  field("name").placeholder = d ? `${e.label} · ${d.host}` : e.label;
  field("password").placeholder = existing ? "Unchanged (in keychain)" : "None";
  field("confirm").value = settings.safeDefault ? "1" : "0"; // overwritten below when editing

  if (existing) {
    field("name").value = existing.name;
    field("tag").value = existing.tag;
    field("confirm").value = existing.confirm_writes ? "1" : "0";
    if (kind === "sqlite") field("path").value = existing.url;
    else {
      // Only fill fields that differ from the default, so placeholders keep meaning "default".
      const u = new URL(existing.url);
      const set = (k: string, v: string) => (field(k).value = v && v !== (d as Record<string, string>)[k] ? v : "");
      set("host", u.hostname);
      set("port", u.port);
      set("user", decodeURIComponent(u.username));
      set("database", decodeURIComponent(u.pathname.slice(1)));
      field("ssl").value = kind === "postgres" ? u.searchParams.get("sslmode") ?? d!.ssl : u.searchParams.get("require_ssl") === "true" ? "required" : "off";
    }
  }
  $<HTMLDialogElement>("editor").showModal();
  field("name").focus();
}

const val = (k: string) => field(k).value.trim() || field(k).placeholder;

function formToSaved(): Saved {
  const e = engine(editingKind);
  let url: string;
  if (editingKind === "sqlite") {
    url = field("path").value.trim();
    if (!url) throw "Choose a database file path.";
  } else {
    const d = e.defaults!;
    const host = field("host").value.trim() || d.host;
    const port = field("port").value.trim() || d.port;
    const user = field("user").value.trim() || d.user;
    const db = field("database").value.trim() || d.database;
    const scheme = editingKind === "postgres" ? "postgres" : "mysql";
    const params = editingKind === "postgres" ? `?sslmode=${field("ssl").value}` : field("ssl").value === "required" ? "?require_ssl=true" : "";
    url = `${scheme}://${encodeURIComponent(user)}@${host}:${port}${db ? "/" + encodeURIComponent(db) : ""}${params}`;
  }
  const name = val("name");
  if (name !== editing?.name && saved.some((c) => c.name === name)) throw `A connection named "${name}" already exists.`;
  return { name, kind: editingKind, url, tag: field("tag").value, confirm_writes: field("confirm").value === "1" };
}

async function saveForm() {
  const conn = formToSaved();
  await invoke("save_connection", { conn, password: field("password").value || null, previous: editing?.name ?? null });
  return conn;
}

async function guard(fn: () => Promise<unknown>) {
  try {
    await fn();
  } catch (e) {
    setStatus("form-status", String(e), "error");
  }
}

$("test").onclick = () =>
  guard(async () => {
    const conn = formToSaved();
    setStatus("form-status", "Testing…", "busy");
    const msg = await invoke<string>("test_connection", {
      kind: conn.kind, url: conn.url, password: field("password").value, savedName: editing?.name ?? null,
    });
    setStatus("form-status", msg, "done");
  });

$("save").onclick = () =>
  guard(async () => {
    await saveForm();
    $<HTMLDialogElement>("editor").close();
    refresh();
  });

form.addEventListener("submit", (e) => {
  e.preventDefault();
  guard(async () => {
    const conn = await saveForm();
    $<HTMLDialogElement>("editor").close();
    await refresh();
    connect(conn);
  });
});

// ---------- Workspace ----------

function show(view: "home" | "workspace") {
  $("home").hidden = view !== "home";
  $("workspace").hidden = view !== "workspace";
  if (view === "home") $("search").focus();
}

type Info = { server: string; database: string | null; can_switch: boolean; tables: string[] };
let info: Info | null = null;
const shortcut = navigator.userAgent.includes("Mac") ? "⌘K" : "Ctrl+K";

async function connect(c: Saved) {
  setStatus("home-status", `Connecting to ${c.name}…`, "busy");
  try {
    const i = await invoke<Info>("connect", { name: c.name });
    active = c;
    setStatus("home-status", "");
    $("where-conn").replaceChildren(avatar(c.kind), el("span", { textContent: c.name }), el("span", { className: "host", textContent: hostOf(c) }));
    document.body.dataset.env = c.tag; // production gets a red edge
    show("workspace");
    applyInfo(i);
    showSide("items");
    renderHistory();
    status(`Connected · ${i.server}`, "done");
    // Connected without a database: go straight to choosing one.
    if (i.can_switch && !i.database) openDbPicker();
    else editor.focus();
  } catch (e) {
    setStatus("home-status", String(e), "error");
  }
}

/** A new connection or database: fresh tree, and a single empty SQL tab. */
function applyInfo(i: Info) {
  info = i;
  tables = i.tables;
  columnsCache.clear();
  expanded.clear();
  const btn = $("db-button");
  btn.hidden = !i.can_switch;
  btn.textContent = i.database ? `${i.database} ▾` : "Choose database ▾";
  btn.title = `Open a database (${shortcut})`;
  $<HTMLInputElement>("filter").value = "";
  $<HTMLInputElement>("filter").hidden = !tables.length;
  editor.setDialect(active!.kind, tables); // before the new tab's state is created
  tabs = [];
  shownId = 0;
  openSqlTab();
  renderTables();
}

// ---------- Open database (⌘K) ----------

let databases: string[] = [];
let dbPick = 0;

async function openDbPicker() {
  if (!active || !info?.can_switch) return;
  const d = $<HTMLDialogElement>("dbpicker");
  $<HTMLInputElement>("db-search").value = "";
  setStatus("db-status", "Loading…", "busy");
  databases = [];
  renderDbs();
  if (!d.open) d.showModal();
  $("db-search").focus();
  try {
    databases = await invoke<string[]>("list_databases", { name: active.name });
    dbPick = Math.max(0, databases.indexOf(info?.database ?? ""));
    setStatus("db-status", "");
    renderDbs();
  } catch (e) {
    setStatus("db-status", String(e), "error");
  }
}

function shownDbs() {
  const q = $<HTMLInputElement>("db-search").value.toLowerCase();
  return databases.filter((d) => d.toLowerCase().includes(q));
}

function renderDbs() {
  const shown = shownDbs();
  dbPick = Math.min(dbPick, Math.max(0, shown.length - 1));
  $("dbs").replaceChildren(
    ...shown.map((name, i) => {
      const li = el("li", { textContent: name });
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(i === dbPick));
      if (name === info?.database) li.append(el("span", { className: "current", textContent: "current" }));
      li.onclick = () => ((dbPick = i), renderDbs());
      li.ondblclick = () => openDb(name);
      return li;
    }),
  );
  $("dbs").querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
}

async function openDb(name?: string) {
  name ??= shownDbs()[dbPick];
  if (!name || !active) return;
  setStatus("db-status", `Opening ${name}…`, "busy");
  try {
    applyInfo(await invoke<Info>("use_database", { name: active.name, database: name }));
    $<HTMLDialogElement>("dbpicker").close();
    status(`${name} · ${tables.length} tables`, "done");
    editor.focus();
  } catch (e) {
    setStatus("db-status", String(e), "error");
  }
}

$("db-search").addEventListener("input", () => ((dbPick = 0), renderDbs()));
$("db-search").addEventListener("keydown", (e) => {
  const n = shownDbs().length;
  if (e.key === "ArrowDown") dbPick = Math.min(n - 1, dbPick + 1);
  else if (e.key === "ArrowUp") dbPick = Math.max(0, dbPick - 1);
  else if (e.key === "Enter") return e.preventDefault(), openDb();
  else return;
  e.preventDefault();
  renderDbs();
});
$("db-open").onclick = () => openDb();
$("db-cancel").onclick = () => $<HTMLDialogElement>("dbpicker").close();
$("db-button").onclick = openDbPicker;
document.addEventListener("keydown", (e) => {
  if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey) && !$("workspace").hidden) {
    e.preventDefault();
    openDbPicker();
  }
});

// ---------- Sidebar: tables (expandable to columns) and history ----------

type Column = { name: string; data_type: string; nullable: boolean; default: string | null; primary: boolean };
const columnsCache = new Map<string, Column[]>();
const expanded = new Set<string>();

async function loadColumns(table: string) {
  if (!columnsCache.has(table)) columnsCache.set(table, await invoke<Column[]>("columns", { name: active!.name, table }));
  return columnsCache.get(table)!;
}

function renderTables() {
  const q = $<HTMLInputElement>("filter").value.toLowerCase();
  const shown = tables.filter((t) => t.toLowerCase().includes(q));
  const open = current()?.kind === "table" ? (current() as TableTab).table : null;
  $("tables").replaceChildren(
    ...shown.map((t) => {
      const isOpen = expanded.has(t);
      const chevron = el("button", { className: "chevron", textContent: "›", title: isOpen ? "Hide columns" : "Show columns" });
      chevron.setAttribute("aria-expanded", String(isOpen));
      chevron.onclick = async (e) => {
        e.stopPropagation();
        if (isOpen) expanded.delete(t);
        else {
          expanded.add(t);
          try {
            await loadColumns(t);
          } catch (err) {
            expanded.delete(t);
            status(String(err), "error");
          }
        }
        renderTables();
      };
      const row = el("div", { className: "tree-row", title: t }, chevron, el("span", { className: "tbl-icon" }), el("span", { className: "label", textContent: t }));
      row.classList.toggle("active", t === open);
      row.onclick = () => openTable(t);
      const li = el("li", {}, row);
      if (isOpen)
        li.append(
          el("ul", { className: "cols" }, ...(columnsCache.get(t) ?? []).map((c) =>
            el("li", { title: `${c.name} ${c.data_type}${c.nullable ? "" : " not null"}` },
              el("span", { className: c.primary ? "key" : "nokey", textContent: c.primary ? "⚿" : "", title: c.primary ? "Primary key" : "" }),
              el("span", { className: "label", textContent: c.name }),
              el("span", { className: "type", textContent: c.data_type }),
            ),
          )),
        );
      return li;
    }),
  );
  const hint = $("no-tables");
  hint.hidden = shown.length > 0;
  hint.textContent =
    info?.can_switch && !info.database ? `No database selected. Press ${shortcut} to choose one.`
    : tables.length ? "No tables match."
    : "This database has no tables.";
}

function showSide(which: "items" | "history") {
  for (const b of document.querySelectorAll<HTMLElement>(".side-tabs button")) b.setAttribute("aria-pressed", String(b.dataset.side === which));
  $("side-items").hidden = which !== "items";
  $("side-history").hidden = which !== "history";
}

// History lives per connection on this machine, newest first.
// ponytail: localStorage capped at 500 entries; move to a file when it needs to be bigger or synced.
type HistoryEntry = { sql: string; at: number; db: string | null };
const historyKey = () => `history:${active?.name}`;

function loadHistory(): HistoryEntry[] {
  try {
    return JSON.parse(localStorage.getItem(historyKey()) ?? "[]");
  } catch {
    return [];
  }
}

function addHistory(sql: string) {
  const list = [{ sql: sql.trim(), at: Date.now(), db: info?.database ?? null }, ...loadHistory()].slice(0, 500);
  try {
    localStorage.setItem(historyKey(), JSON.stringify(list));
  } catch {}
  renderHistory();
}

const dayFormat = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "long", year: "numeric" });
const timeFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });

function renderHistory() {
  const q = $<HTMLInputElement>("history-search").value.toLowerCase();
  const list = loadHistory().filter((h) => h.sql.toLowerCase().includes(q));
  const groups = new Map<string, HistoryEntry[]>();
  for (const h of list) {
    const day = dayFormat.format(h.at);
    groups.set(day, [...(groups.get(day) ?? []), h]);
  }
  $("history").replaceChildren(
    ...[...groups].map(([day, entries], i) => {
      const d = el("details", { open: i === 0 }, el("summary", { textContent: day }));
      for (const h of entries) {
        const item = el("button", { className: "history-item", title: "Open in the SQL editor" },
          el("span", { className: "when", textContent: timeFormat.format(h.at) + (h.db ? ` · ${h.db}` : "") }),
          el("code", { textContent: h.sql }),
        );
        item.onclick = () => {
          const tab = current()?.kind === "sql" ? (current() as SqlTab) : openSqlTab();
          tab.sql = h.sql;
          tab.state = editor.newState(h.sql);
          shownId = 0; // force renderMain to load the new state
          renderMain();
          editor.focus();
        };
        d.append(item);
      }
      return d;
    }),
  );
  $("no-history").hidden = list.length > 0;
}

// ---------- Tabs: SQL editors and table browsers ----------

type SqlTab = { id: number; kind: "sql"; title: string; sql: string; state?: EditorState; result?: QueryResult; status?: [string, StatusKind] };
type TableTab = { id: number; kind: "table"; title: string; table: string; view: "data" | "structure"; page: number; total?: number; result?: QueryResult; error?: string; loading?: boolean };
type Tab = SqlTab | TableTab;
type StatusKind = "" | "busy" | "error" | "done";
const PAGE = 300;
let tabs: Tab[] = [];
let currentId = 0;
let nextId = 1;
let shownId = 0; // the SQL tab whose state is in the editor right now
const current = () => tabs.find((t) => t.id === currentId);

function openSqlTab(): SqlTab {
  // Lowest free number, so closing tabs doesn't make the names climb forever.
  const used = new Set(tabs.map((t) => t.title));
  const name = (n: number) => (n === 1 ? "SQL Query" : `SQL Query ${n}`);
  let n = 1;
  while (used.has(name(n))) n++;
  const tab: SqlTab = { id: nextId++, kind: "sql", title: name(n), sql: "" };
  tabs.push(tab);
  select(tab.id);
  return tab;
}

function openTable(table: string) {
  const existing = tabs.find((t) => t.kind === "table" && t.table === table);
  if (existing) return select(existing.id);
  const tab: TableTab = { id: nextId++, kind: "table", title: table, table, view: "data", page: 0 };
  tabs.push(tab);
  select(tab.id);
  loadTable(tab);
}

function select(id: number) {
  const prev = current();
  if (prev?.kind === "sql" && shownId === prev.id) prev.state = editor.view.state; // text + undo stay with the tab
  currentId = id;
  renderMain();
  renderTables();
}

function closeTab(id: number) {
  const i = tabs.findIndex((t) => t.id === id);
  if (i < 0) return;
  tabs.splice(i, 1);
  if (!tabs.length) return void closeWorkspace(); // last tab gone: leave the connection, like TablePlus
  if (id === currentId) select(tabs[Math.max(0, i - 1)].id);
  else renderMain();
}

function renderMain() {
  const tab = current();
  if (!tab) return;
  $("tabs").replaceChildren(
    ...tabs.map((t) => {
      const close = el("span", { className: "close", textContent: "×", title: "Close tab" });
      close.onclick = (e) => (e.stopPropagation(), closeTab(t.id));
      const b = el("button", { className: `tab ${t.kind}`, title: t.title },
        el("span", { className: t.kind === "sql" ? "tab-sql" : "tbl-icon", textContent: t.kind === "sql" ? "SQL" : "" }),
        el("span", { className: "label", textContent: t.title }),
        close,
      );
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(t.id === currentId));
      b.onclick = () => select(t.id);
      b.onauxclick = (e) => e.button === 1 && closeTab(t.id); // middle-click closes
      return b;
    }),
  );
  $("tabs").querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });

  const env = active?.tag ? `${TAGS[active.tag] ?? active.tag} | ` : "";
  $("pill").textContent = `${env}${info?.server ?? ""}${info?.database ? ` : ${info.database}` : ""} : ${tab.title}`;
  $("pill").className = `pill ${active?.tag ?? ""}`;

  $("sql-pane").hidden = tab.kind !== "sql";
  $("table-foot").hidden = tab.kind !== "table";
  if (tab.kind === "sql") {
    if (shownId !== tab.id) {
      editor.setState((tab.state ??= editor.newState(tab.sql)));
      shownId = tab.id;
    }
    updateCursor();
    status(...(tab.status ?? ["", ""]));
    if (tab.result) render(tab.result, 0);
    else $("out").replaceChildren(el("p", { className: "hint empty", textContent: "Run a query to see results here." }));
    return;
  }
  for (const b of $("view-seg").querySelectorAll<HTMLElement>("button")) b.setAttribute("aria-pressed", String(b.dataset.view === tab.view));
  const from = tab.page * PAGE;
  const shown = tab.result?.rows.length ?? 0;
  $("page-info").textContent =
    tab.view === "structure" ? `${columnsCache.get(tab.table)?.length ?? 0} columns`
    : tab.loading ? "Loading…"
    : `${shown ? from + 1 : 0}–${from + shown} of ${tab.total ?? "?"} rows`;
  $<HTMLButtonElement>("page-prev").disabled = tab.view !== "data" || tab.page === 0;
  $<HTMLButtonElement>("page-next").disabled = tab.view !== "data" || tab.total === undefined || from + shown >= tab.total;
  if (tab.error) $("out").replaceChildren(el("p", { className: "hint empty error-text", textContent: tab.error }));
  else if (tab.view === "structure") renderStructure(columnsCache.get(tab.table) ?? []);
  else if (tab.result) render(tab.result, from);
  else $("out").replaceChildren();
}

async function loadTable(tab: TableTab) {
  tab.loading = true;
  tab.error = undefined;
  if (tab.id === currentId) renderMain();
  try {
    if (tab.view === "structure") await loadColumns(tab.table);
    else {
      const q = quote(tab.table);
      tab.result = await invoke<QueryResult>("run_query", { name: active!.name, sql: `select * from ${q} limit ${PAGE} offset ${tab.page * PAGE}`, confirmed: false });
      // ponytail: exact count(*) can be slow on huge tables; estimate from catalog stats when that hurts.
      if (tab.total === undefined) {
        const c = await invoke<QueryResult>("run_query", { name: active!.name, sql: `select count(*) from ${q}`, confirmed: false });
        tab.total = Number(c.rows[0]?.[0] ?? 0);
      }
    }
  } catch (e) {
    tab.error = String(e);
  }
  tab.loading = false;
  if (tab.id === currentId) renderMain(); // the user may have moved to another tab meanwhile
}

function quote(table: string) {
  const q = active!.kind === "postgres" || active!.kind === "sqlite" ? '"' : "`";
  const parts = active!.kind === "postgres" ? table.split(".") : [table]; // postgres names are schema.table
  return parts.map((p) => q + p.split(q).join(q + q) + q).join(".");
}

/** Inline confirmation under the run bar. Cancel is focused, so Enter or Esc never confirms by accident. */
function ask(text: string, detail: string, okLabel: string): Promise<boolean> {
  const bar = $("ask");
  $("ask-text").textContent = text;
  $("ask-detail").textContent = detail.length > 120 ? detail.slice(0, 120) + "…" : detail;
  $("ask-ok").textContent = okLabel;
  bar.hidden = false;
  $("ask-cancel").focus();
  return new Promise((resolve) => {
    const done = (ok: boolean) => {
      bar.hidden = true;
      document.removeEventListener("keydown", onKey);
      resolve(ok);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && done(false);
    document.addEventListener("keydown", onKey);
    $("ask-ok").onclick = () => done(true);
    $("ask-cancel").onclick = () => done(false);
  });
}

/** Runs SQL from the current SQL tab; the result and status stay with that tab. */
async function run(sql: string, confirmed = false) {
  const tab = current();
  if (!active || tab?.kind !== "sql" || !sql.trim()) return;
  const say = (text: string, kind: StatusKind) => {
    tab.status = [text, kind];
    if (tab.id === currentId) status(text, kind);
  };
  say("Running…", "busy");
  const t0 = performance.now();
  try {
    const r = await invoke<QueryResult>("run_query", { name: active.name, sql, confirmed });
    tab.result = r;
    if (tab.id === currentId) render(r, 0);
    const ms = Math.round(performance.now() - t0);
    say(r.columns.length ? `${r.rows.length} rows · ${ms} ms` : `${r.affected} rows affected · ${ms} ms`, "done");
    addHistory(sql);
  } catch (e) {
    if (e !== "CONFIRM") return say(String(e), "error");
    say("", "");
    if (await ask(`This may change data on "${active.name}".`, sql.trim(), "Run anyway")) return run(sql, true);
    say("Cancelled", "");
  }
}

const numeric = /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i;

/** Result grid with row numbers starting after `offset`. */
function render(r: QueryResult, offset: number) {
  if (!r.columns.length) return $("out").replaceChildren(el("p", { className: "hint empty", textContent: `${r.affected} rows affected.` }));
  const table = el("table");
  const head = table.createTHead().insertRow();
  head.append(el("th", { className: "rownum" }));
  for (const c of r.columns) head.append(el("th", { textContent: c }));
  const body = table.createTBody();
  // ponytail: plain table; switch to a virtualized grid when pages get much larger than 300 rows.
  r.rows.forEach((row, i) => {
    const tr = body.insertRow();
    tr.append(el("td", { className: "rownum", textContent: String(offset + i + 1) }));
    for (const v of row) {
      const td = tr.insertCell();
      td.textContent = v ?? "NULL";
      if (v === null) td.className = "null";
      else if (v === "") td.className = "empty-val";
      else if (numeric.test(v)) td.className = "num";
      if (v && v.length > 40) td.title = v; // full value on hover when the cell is truncated
    }
  });
  if (!r.rows.length) {
    const tr = body.insertRow();
    tr.append(el("td", { className: "hint", colSpan: r.columns.length + 1, textContent: "No rows." }));
  }
  $("out").replaceChildren(table);
}

function renderStructure(cols: Column[]) {
  render(
    {
      columns: ["column", "type", "nullable", "default", "key"],
      rows: cols.map((c) => [c.name, c.data_type, c.nullable ? "YES" : "NO", c.default, c.primary ? "PRIMARY" : ""]),
      affected: 0,
    },
    0,
  );
}

function updateCursor() {
  const { line, column } = editor.cursor();
  $("cursor").textContent = `line ${line}, column ${column}`;
}

$("new-sql").onclick = () => (openSqlTab(), editor.focus());
for (const b of document.querySelectorAll<HTMLElement>(".side-tabs button")) b.onclick = () => showSide(b.dataset.side as "items" | "history");
$("history-search").addEventListener("input", renderHistory);
for (const b of $("view-seg").querySelectorAll<HTMLElement>("button"))
  b.onclick = () => {
    const tab = current();
    if (tab?.kind !== "table" || tab.view === b.dataset.view) return;
    tab.view = b.dataset.view as TableTab["view"];
    if (tab.view === "structure" && columnsCache.has(tab.table)) renderMain();
    else loadTable(tab);
  };
const turnPage = (delta: number) => {
  const tab = current();
  if (tab?.kind !== "table") return;
  tab.page = Math.max(0, tab.page + delta);
  loadTable(tab);
};
$("page-prev").onclick = () => turnPage(-1);
$("page-next").onclick = () => turnPage(1);
/** Leaves the workspace and closes the server connection; the launcher comes back. */
function closeWorkspace() {
  if (active) invoke("disconnect", { name: active.name }).catch(() => {});
  active = null;
  info = null;
  tabs = [];
  shownId = 0;
  delete document.body.dataset.env;
  show("home");
}

// Cmd/Ctrl+W: close the tab; on the last tab close the workspace; on the launcher close the window.
document.addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey) || document.querySelector("dialog[open]")) return;
  const key = e.key.toLowerCase();
  if (key === "w") {
    e.preventDefault();
    if ($("workspace").hidden) getCurrentWindow().close();
    else closeTab(currentId);
  } else if (key === "t" && !$("workspace").hidden) (e.preventDefault(), openSqlTab(), editor.focus());
});

// ---------- Settings (⌘,) ----------

type Settings = { theme: "auto" | "light" | "dark"; zebra: boolean; fontSize: number; lineHeight: number; indent: "tab" | "2" | "4"; safeDefault: boolean };
const DEFAULT_SETTINGS: Settings = { theme: "auto", zebra: true, fontSize: 13, lineHeight: 1.5, indent: "2", safeDefault: true };
let settings: Settings = loadSettings();

// ponytail: settings live in the webview's localStorage (per machine); move to a file if they need syncing.
function loadSettings(): Settings {
  try {
    return { ...DEFAULT_SETTINGS, ...JSON.parse(localStorage.getItem("settings") ?? "{}") };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

function setSetting<K extends keyof Settings>(key: K, value: Settings[K]) {
  settings[key] = value;
  try {
    localStorage.setItem("settings", JSON.stringify(settings));
  } catch {} // unavailable storage: settings still apply for this session
  applySettings();
}

function applySettings() {
  const root = document.documentElement;
  root.style.colorScheme = settings.theme === "auto" ? "" : settings.theme;
  root.style.setProperty("--editor-size", `${settings.fontSize}px`);
  root.style.setProperty("--editor-line", String(settings.lineHeight));
  document.body.classList.toggle("zebra", settings.zebra);
  // Reflect into the settings controls (they may be open while a shortcut changes a value).
  for (const b of $("theme").querySelectorAll("button")) b.setAttribute("aria-pressed", String(b.dataset.value === settings.theme));
  $<HTMLInputElement>("zebra").checked = settings.zebra;
  $<HTMLInputElement>("font-size").value = String(settings.fontSize);
  $<HTMLInputElement>("line-height").value = String(settings.lineHeight);
  $<HTMLSelectElement>("indent").value = settings.indent;
  editor.setIndent(settings.indent === "tab" ? "\t" : " ".repeat(Number(settings.indent)));
  $<HTMLSelectElement>("safe-default").value = settings.safeDefault ? "1" : "0";
}

function showTab(tab: string) {
  for (const t of $("settings").querySelectorAll<HTMLElement>("[role=tab]")) t.setAttribute("aria-selected", String(t.dataset.tab === tab));
  for (const p of $("settings").querySelectorAll<HTMLElement>("[data-pane]")) p.hidden = p.dataset.pane !== tab;
}

const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));
const openSettings = () => {
  const d = $<HTMLDialogElement>("settings");
  if (!d.open) d.showModal();
};

for (const b of document.querySelectorAll<HTMLElement>(".open-settings")) b.onclick = openSettings;
for (const t of $("settings").querySelectorAll<HTMLElement>("[role=tab]")) t.onclick = () => showTab(t.dataset.tab!);
for (const b of $("theme").querySelectorAll<HTMLButtonElement>("button")) b.onclick = () => setSetting("theme", b.dataset.value as Settings["theme"]);
$<HTMLInputElement>("zebra").onchange = (e) => setSetting("zebra", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("font-size").onchange = (e) => setSetting("fontSize", clamp(Number((e.target as HTMLInputElement).value) || 13, 9, 28));
$<HTMLInputElement>("line-height").onchange = (e) => setSetting("lineHeight", clamp(Number((e.target as HTMLInputElement).value) || 1.5, 1.1, 2.2));
$<HTMLSelectElement>("indent").onchange = (e) => setSetting("indent", (e.target as HTMLSelectElement).value as Settings["indent"]);
$<HTMLSelectElement>("safe-default").onchange = (e) => setSetting("safeDefault", (e.target as HTMLSelectElement).value === "1");
$("settings-reset").onclick = () => {
  settings = { ...DEFAULT_SETTINGS };
  setSetting("theme", settings.theme); // persists and re-applies everything
};
$("settings-done").onclick = () => $<HTMLDialogElement>("settings").close();
$("font-keys").textContent = navigator.userAgent.includes("Mac") ? "⌘= / ⌘- / ⌘0" : "Ctrl+= / Ctrl+- / Ctrl+0";

document.addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey)) return;
  if (e.key === ",") return e.preventDefault(), openSettings();
  if ($("workspace").hidden) return;
  // Editor font size; also stops the webview from zooming the whole page.
  const size = e.key === "=" || e.key === "+" ? settings.fontSize + 1 : e.key === "-" ? settings.fontSize - 1 : e.key === "0" ? DEFAULT_SETTINGS.fontSize : 0;
  if (size) (e.preventDefault(), setSetting("fontSize", clamp(size, 9, 28)));
});

showTab("appearance");
applySettings();

// ---------- Wiring ----------

$("create").onclick = openPicker;
$("add").onclick = openPicker;
$("search").addEventListener("input", renderConns);
$("search").addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  const first = $("conns").querySelector("li");
  if (first) (first as HTMLElement).click();
});
$("engine-search").addEventListener("input", renderEngines);
$("engine-search").addEventListener("keydown", (e) => e.key === "Enter" && (e.preventDefault(), nextFromPicker()));
$("picker-next").onclick = nextFromPicker;
$("picker-cancel").onclick = () => $<HTMLDialogElement>("picker").close();
$("editor-cancel").onclick = () => $<HTMLDialogElement>("editor").close();
$("back").onclick = closeWorkspace;
$("filter").addEventListener("input", renderTables);

$("keys").textContent = navigator.userAgent.includes("Mac") ? "⌘↵" : "Ctrl+↵";
const runEditor = () => run(editor.selectionOrAll());
$("run").onclick = runEditor;

show("home");
refresh();
