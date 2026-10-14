import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import type { EditorState } from "@codemirror/state";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { createEditor, type FormatStyle, type Schema } from "./editor";
import { fillParams, findParams, type ParamFormat } from "./statements";
import { $, el } from "./dom";
import { activeFilters, blankFilter, renderFilters, type FilterRow } from "./filters";
import { csv, inserts, markdown, plain, type Dialect } from "./copy";
import { MIN_WIDTH, layoutCss, loadLayout, saveLayout, type Layout } from "./columns";
import { dropDraft, initStructure, saveStructure, showStructure, sidebarMenu, structurePending, tableMenu } from "./structure";
import { openBackup, openRestore } from "./backup";
import { openProcesses } from "./processes";
import "./mcp"; // adds Settings › MCP before the settings tabs are wired below
import { Staged } from "./staging";
import * as grid from "./grid-edit";
import { syncRowDetail, toggleRowDetail } from "./rowdetail";
import { applyTheme } from "./themes"; // also adds the theme rows to Settings › Appearance
import "./passcode"; // the lock screen, and its rows in Settings › Security
import { docEntry, docsReady, setDocEntry } from "./locations"; // also adds Settings › Locations
import { CANCELLED, NOT_APPROVED, approval, fillLevels, guarded, isSafetyStop, showSafety, type Safety } from "./safety";
import { exportDialog, importFile, type Ctx as TransferCtx, type ExportSource } from "./importexport";
import { addCommands, bindings, commands, isMac, onBindingsChanged, setOverrides } from "./commands";
import { renderShortcuts } from "./shortcuts";
import { openAnything, type Item as AnythingItem } from "./openanything";
import { initMenu } from "./menu";
import { forgetItems, initSearch, initSidebar, loadedItems, renderTree, treeMenu } from "./sidebar";
import { label } from "./keys";
import "./workspace.css";

type Ssh = { host: string; port: number | null; user: string; key: string };
type Tls = { mode: string; ca: string; cert: string; key: string };
type Options = {
  startup_sql: string; startup_script: string;
  direct_tls: boolean; no_channel_binding: boolean; system_schemas: boolean;
  socket: string; sni: string; cleartext_auth: boolean;
};
const OPTIONS_DEFAULT: Options = {
  startup_sql: "", startup_script: "", direct_tls: false, no_channel_binding: false, system_schemas: false,
  socket: "", sni: "", cleartext_auth: false,
};
type Saved = { name: string; kind: string; url: string; tag: string; safety: Safety; ssh?: Ssh; tls?: Tls; options?: Options };
type QueryResult = { columns: string[]; rows: (string | null)[][]; affected: number; truncated?: boolean };
type Engine = {
  kind: string;
  label: string;
  mono: string;
  color: string;
  // Every field is optional: a blank input uses the default, shown as its placeholder.
  defaults?: { host: string; port: string; user: string; database: string };
};

// libpq's sslmode values; MySQL's ssl-mode means the same. Prefer and Require encrypt without checking the
// certificate, as libpq does.
const TLS_MODES: [string, string][] = [
  ["disable", "Disable"], ["prefer", "Prefer"], ["require", "Require"],
  ["verify-ca", "Verify CA"], ["verify-full", "Verify CA and host name"],
];
const TLS_DEFAULT: Tls = { mode: "prefer", ca: "", cert: "", key: "" };
const ENGINES: Engine[] = [
  { kind: "postgres", label: "PostgreSQL", mono: "Pg", color: "#2f6fd0",
    defaults: { host: "127.0.0.1", port: "5432", user: "postgres", database: "" } },
  { kind: "mysql", label: "MySQL", mono: "Ms", color: "#e8891c",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { kind: "mariadb", label: "MariaDB", mono: "Ma", color: "#1f9d8f",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { kind: "sqlite", label: "SQLite", mono: "Sl", color: "#7b4fd6" },
];
const engine = (kind: string) => ENGINES.find((e) => e.kind === kind) ?? ENGINES[0];
const TAGS: Record<string, string> = { local: "Local", development: "Development", staging: "Staging", production: "Production" };

const form = $<HTMLFormElement>("form");
const field = (name: string) => form.elements.namedItem(name) as HTMLInputElement & HTMLSelectElement;
const editor = createEditor($("sql"), {
  change: (doc) => {
    const tab = current();
    if (tab?.kind === "sql") tab.sql = doc;
    saveSqlTabsSoon();
  },
  cursor: () => updateCursor(),
});

function formatSql() {
  try {
    editor.format(settings.formatStyle);
  } catch (e) {
    status(`Couldn't format: ${e instanceof Error ? e.message.split("\n")[0] : e}`, "error");
  }
}

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
  const via = c.ssh ? ` via ${c.ssh.host}` : "";
  try {
    const u = new URL(c.url);
    return u.hostname + (u.pathname.length > 1 ? decodeURIComponent(u.pathname) : "") + via;
  } catch {
    return c.url + via;
  }
}

// ---------- Launcher ----------

async function refresh() {
  try {
    saved = await invoke<Saved[]>("list_connections");
    setStatus("home-status", "");
  } catch (e) {
    setStatus("home-status", String(e), "error"); // e.g. an unreadable connections file, which is left untouched
  }
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
      li.oncontextmenu = (e) => contextMenu(e, toolsMenu(c));
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
  field("via").value = existing?.ssh ? "ssh" : "";
  showSsh();
  const tls = existing?.tls ?? TLS_DEFAULT;
  field("ssl").value = tls.mode;
  field("ssl_ca").value = tls.ca;
  field("ssl_cert").value = tls.cert;
  field("ssl_key").value = tls.key;
  showTlsFiles();
  const options = { ...OPTIONS_DEFAULT, ...existing?.options };
  for (const [k, v] of Object.entries(options)) {
    const input = field(`opt_${k}`);
    if (typeof v === "boolean") input.checked = v;
    else input.value = v;
  }
  for (const node of form.querySelectorAll<HTMLElement>("[data-engines]")) node.hidden = !node.dataset.engines!.split(" ").includes(kind);
  // Open the section when something in it is set, so saved options are never out of sight.
  form.querySelector<HTMLDetailsElement>("details.more")!.open = !!existing?.options;

  const d = e.defaults;
  if (d) {
    for (const k of ["host", "port", "user"] as const) field(k).placeholder = d[k];
    field("database").placeholder = "Optional, choose one after connecting";
  }
  field("name").placeholder = d ? `${e.label} · ${d.host}` : e.label;
  field("password").placeholder = existing ? "Unchanged (in keychain)" : "None";
  field("passphrase").placeholder = existing ? "Unchanged (in keychain), if any" : "Only for SQLCipher-encrypted files";
  field("ssh_password").placeholder = existing?.ssh ? "Unchanged (in keychain)" : "None";
  field("safety").value = settings.safeLevel; // overwritten below when editing

  if (existing) {
    field("name").value = existing.name;
    field("tag").value = existing.tag;
    field("safety").value = existing.safety;
    if (existing.ssh) {
      field("ssh_host").value = existing.ssh.host;
      field("ssh_port").value = existing.ssh.port?.toString() ?? "";
      field("ssh_user").value = existing.ssh.user;
      field("ssh_key").value = existing.ssh.key;
    }
    if (kind === "sqlite") field("path").value = existing.url;
    else {
      // Only fill fields that differ from the default, so placeholders keep meaning "default".
      const u = new URL(existing.url);
      const set = (k: string, v: string) => (field(k).value = v && v !== (d as Record<string, string>)[k] ? v : "");
      set("host", u.hostname);
      set("port", u.port);
      set("user", decodeURIComponent(u.username));
      set("database", decodeURIComponent(u.pathname.slice(1)));

    }
  }
  $<HTMLDialogElement>("editor").showModal();
  field("name").focus();
}

const val = (k: string) => field(k).value.trim() || field(k).placeholder;
/** The keychain-stored secret's input: the login password, or for SQLite the SQLCipher passphrase. */
const secretField = () => field(editingKind === "sqlite" ? "passphrase" : "password");

field("ssl").replaceChildren(...TLS_MODES.map(([v, label]) => el("option", { value: v, textContent: label })));

function showTlsFiles() {
  form.querySelector<HTMLElement>(".tls-files")!.hidden = field("ssl").value === "disable";
}
field("ssl").addEventListener("change", showTlsFiles);

/** The form's options, or undefined when all are defaults. Options for other engines are dropped. */
function formOptions(): Options | undefined {
  const o: Options = { ...OPTIONS_DEFAULT };
  for (const k of Object.keys(OPTIONS_DEFAULT) as (keyof Options)[]) {
    const input = field(`opt_${k}`);
    if (input.closest<HTMLElement>("[data-engines]")?.hidden) continue;
    (o as Record<string, string | boolean>)[k] = typeof OPTIONS_DEFAULT[k] === "boolean" ? input.checked : input.value.trim();
  }
  const isDefault = (Object.keys(o) as (keyof Options)[]).every((k) => o[k] === OPTIONS_DEFAULT[k]);
  return isDefault ? undefined : o;
}

function formTls(): Tls | undefined {
  if (editingKind === "sqlite") return undefined;
  const tls = { mode: field("ssl").value, ca: field("ssl_ca").value.trim(), cert: field("ssl_cert").value.trim(), key: field("ssl_key").value.trim() };
  if (tls.mode === "disable") Object.assign(tls, { ca: "", cert: "", key: "" });
  if (!tls.cert !== !tls.key) throw "A client certificate needs both the certificate file and its key file.";
  const isDefault = (Object.keys(TLS_DEFAULT) as (keyof Tls)[]).every((k) => tls[k] === TLS_DEFAULT[k]);
  return isDefault ? undefined : tls;
}

// "Choose…" buttons fill the input named by their data-pick attribute from the native file dialog.
for (const b of form.querySelectorAll<HTMLButtonElement>("button[data-pick]")) {
  b.onclick = async () => {
    const input = field(b.dataset.pick!);
    const path = await openFileDialog({ multiple: false, directory: false, defaultPath: input.value || undefined });
    if (typeof path === "string") input.value = path;
  };
}

const overSsh = () => editingKind !== "sqlite" && field("via").value === "ssh";
function showSsh() {
  form.querySelector<HTMLElement>(".ssh")!.hidden = !overSsh();
}
field("via").addEventListener("change", () => (showSsh(), overSsh() && field("ssh_host").focus()));

function formSsh(): Ssh | undefined {
  if (!overSsh()) return undefined;
  const host = field("ssh_host").value.trim();
  if (!host) throw "Enter the SSH host, or choose Connect: Directly.";
  const portText = field("ssh_port").value.trim();
  const port = portText ? Number(portText) : null;
  if (port !== null && !(Number.isInteger(port) && port > 0 && port < 65536)) throw "The SSH port must be a number from 1 to 65535.";
  return { host, port, user: field("ssh_user").value.trim(), key: field("ssh_key").value.trim() };
}

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
    url = `${scheme}://${encodeURIComponent(user)}@${host}:${port}${db ? "/" + encodeURIComponent(db) : ""}`;
  }
  const name = val("name");
  if (name !== editing?.name && saved.some((c) => c.name === name)) throw `A connection named "${name}" already exists.`;
  const conn: Saved = { name, kind: editingKind, url, tag: field("tag").value, safety: field("safety").value as Safety };
  const ssh = formSsh();
  if (ssh) conn.ssh = ssh; // omitted, not null: the backend refuses unknown or malformed fields
  const tls = formTls();
  if (tls) conn.tls = tls;
  const options = formOptions();
  if (options) conn.options = options;
  return conn;
}

async function saveForm() {
  const conn = formToSaved();
  await invoke("save_connection", {
    conn,
    password: secretField().value || null,
    sshPassword: field("ssh_password").value || null,
    previous: editing?.name ?? null,
  });
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
      conn, password: secretField().value, sshPassword: field("ssh_password").value, savedName: editing?.name ?? null,
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
  $("return-workspace").hidden = view !== "home" || !workspaces.length;
  $("home").hidden = view !== "home";
  $("workspace").hidden = view !== "workspace";
  if (view === "home") $("search").focus();
}

type Info = { server: string; database: string | null; can_switch: boolean; tables: string[] };
let info: Info | null = null;
/** The first shortcut of a command, as it reads on screen; "" when it has none. */
const keyLabel = (id: string) => label(bindings(id)[0] ?? "", isMac);

/** invoke for workspace commands: a dropped connection shows the reconnect banner before the error moves on. */
async function call<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    if (e === "DISCONNECTED") $("lost").hidden = false;
    throw e;
  }
}

/** Reopens the active connection, keeping the open tabs; the table list is refreshed. */
async function reconnect() {
  if (!active) return;
  const btn = $<HTMLButtonElement>("reconnect");
  btn.disabled = true;
  status("Reconnecting…", "busy");
  try {
    info = await invoke<Info>("reconnect", { name: active.name });
    $("lost").hidden = true;
    tables = info.tables;
    columnsCache.clear();
    renderTables();
    loadSchema();
    status(`Reconnected · ${info.server}`, "done");
  } catch (e) {
    status(String(e), "error");
  } finally {
    btn.disabled = false;
  }
}
$("reconnect").onclick = reconnect;

// Keep-alive found the active connection dead: show the banner now rather than on the next query.
listen<string>("connection-lost", (e) => {
  if (e.payload === active?.name) $("lost").hidden = false;
  const w = workspaces.find((w) => w.conn.name === e.payload && w !== ws);
  if (w) (w.lost = true), renderWorkspaceButton();
}).catch(() => {}); // no event bridge outside the app

// ---------- Connection info (click the status pill) ----------

async function showConnectionInfo() {
  if (!active) return;
  const pop = $("conn-info");
  if (!pop.hidden) return void (pop.hidden = true);
  const row = (k: string, v: string) => [el("dt", { textContent: k }), el("dd", { textContent: v })];
  const list = $("conn-info-list");
  list.replaceChildren(...row("Connection", active.name), ...row("Server", "Checking…"));
  const r = $("pill").getBoundingClientRect();
  Object.assign(pop.style, { top: `${r.bottom + 6}px`, left: `${Math.max(8, r.right - 340)}px` });
  pop.hidden = false;
  try {
    const i = await call<{ server: string; driver: string; cipher: string | null; ssh: string | null }>("connection_info", { name: active.name });
    list.replaceChildren(
      ...row("Connection", active.name),
      ...row("Server", i.server),
      ...row("Driver", i.driver),
      ...row("Encryption", i.cipher ? i.cipher : active.kind === "sqlite" ? "Local file" : "Not encrypted"),
      ...(i.ssh ? row("Via SSH", i.ssh) : []),
    );
  } catch (e) {
    list.replaceChildren(...row("Connection", active.name), ...row("Error", String(e)));
  }
}
$("pill").onclick = showConnectionInfo;
$("pill").onkeydown = (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), showConnectionInfo());
$("info-reconnect").onclick = () => (($("conn-info").hidden = true), reconnect());
$("info-disconnect").onclick = () => (($("conn-info").hidden = true), closeWorkspace());
document.addEventListener("pointerdown", (e) => {
  const pop = $("conn-info");
  if (!pop.hidden && !pop.contains(e.target as Node) && e.target !== $("pill")) pop.hidden = true;
});
document.addEventListener("keydown", (e) => e.key === "Escape" && ($("conn-info").hidden = true));

/** Opens a connection as a new workspace; one that's already open is brought forward instead. */
async function connect(c: Saved) {
  const open = workspaces.find((w) => w.conn.name === c.name);
  if (open) return showWorkspace(open);
  setStatus("home-status", `Connecting to ${c.name}…`, "busy");
  try {
    await docsReady; // kept tabs and history, read at startup
    const i = await invoke<Info>("connect", { name: c.name });
    stashWorkspace();
    pane = newPane();
    ws = { conn: c, info: i, tables: i.tables, panes: [pane], pane, columns: new Map(), expanded: new Set(), lost: false, filter: "" };
    workspaces.push(ws);
    active = c;
    columnsCache = ws.columns;
    expanded = ws.expanded;
    $("lost").hidden = true;
    setStatus("home-status", "");
    showHeader();
    show("workspace");
    applyInfo(i);
    showSide("items");
    renderHistory();
    showSafety(c);
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
  $<HTMLInputElement>("filter").value = "";
  forgetItems(active!.name);
  showHeader();
  editor.setDialect(active!.kind, new Map(tables.map((t) => [t, []]))); // before the new tab's state is created
  loadSchema();
  // One pane again: a split's other tabs belonged to the previous database.
  pane = newPane();
  if (ws) (ws.panes = [pane]), (ws.pane = pane);
  layoutPanes();
  tabs = [];
  currentId = 0;
  shownId = 0;
  const kept = settings.keepTabs ? loadSqlTabs() : [];
  if (!kept.length) openSqlTab();
  for (const k of kept) tabs.push({ id: nextId++, kind: "sql", title: k.title, sql: k.sql, path: k.path });
  if (kept.length) select(tabs[0].id);
  renderTables();
}

/** The workspace header, the database button and the sidebar filter for the active connection. */
function showHeader() {
  const c = active!;
  $("where-conn").replaceChildren(avatar(c.kind), el("span", { textContent: c.name }), el("span", { className: "host", textContent: hostOf(c) }));
  document.body.dataset.env = c.tag; // production gets a red edge
  const btn = $("db-button");
  btn.hidden = !info?.can_switch;
  btn.textContent = info?.database ? `${info.database} ▾` : "Choose database ▾";
  btn.title = `Open a database (${keyLabel("open-database")})`;
  $<HTMLInputElement>("filter").hidden = !tables.length;
  renderWorkspaceButton();
}

// ---------- Workspaces: several connections open in one window, each with its own panes and tabs ----------

type Workspace = {
  conn: Saved; info: Info | null; tables: string[];
  /** Side by side; `pane` is the one with focus. */
  panes: Pane[]; pane: Pane;
  columns: Map<string, Column[]>; expanded: Set<string>; lost: boolean; filter: string;
};
let workspaces: Workspace[] = [];
let ws: Workspace | null = null; // the one on screen (or last on screen, while the launcher shows)

/** Every tab of the workspace on screen, in all its panes. */
const allTabs = () => (ws?.panes ?? [pane]).flatMap((p) => (p === pane ? tabs : p.tabs));

/** Writes the on-screen state back into its workspace before another takes the screen. Off screen (the launcher
 *  is showing) it was written when the launcher came up, and may have changed since (Reconnect All). */
function stashWorkspace() {
  if (!ws || $("workspace").hidden) return;
  stashEditor();
  Object.assign(pane, { tabs, currentId });
  Object.assign(ws, {
    info, tables, pane, columns: columnsCache, expanded, lost: !$("lost").hidden, filter: $<HTMLInputElement>("filter").value,
  });
}

function showWorkspace(w: Workspace) {
  stashWorkspace();
  ws = w;
  ({ conn: active, info, tables, pane, columns: columnsCache, expanded } = w);
  ({ tabs, currentId } = pane);
  shownId = 0; // the editor holds another workspace's tab
  filtersDrawnFor = 0;
  $("lost").hidden = !w.lost;
  $<HTMLInputElement>("filter").value = w.filter;
  showHeader();
  show("workspace");
  editor.setDialect(w.conn.kind, new Map(tables.map((t) => [t, []])));
  loadSchema();
  layoutPanes();
  renderTables();
  renderHistory();
  renderMain();
}

function cycleWorkspace(step: number) {
  const i = workspaces.indexOf(ws!);
  if (workspaces.length > 1) showWorkspace(workspaces[(i + step + workspaces.length) % workspaces.length]);
}

/** The launcher, with the open workspaces kept; connecting from it adds one. */
function newWorkspace() {
  stashWorkspace();
  show("home");
}

/** Reopens every workspace's connection, keeping their tabs. */
async function reconnectAll() {
  stashWorkspace();
  for (const w of workspaces) {
    if (w === ws && !$("workspace").hidden) {
      await reconnect();
      continue;
    }
    try {
      w.info = await invoke<Info>("reconnect", { name: w.conn.name });
      Object.assign(w, { tables: w.info.tables, columns: new Map(), lost: false });
      forgetItems(w.conn.name);
    } catch (e) {
      status(`${w.conn.name}: ${e}`, "error");
    }
  }
  renderWorkspaceButton();
}

/** The connection name in the header opens the switcher ("Show All Workspaces"); a count shows when there are several. */
function renderWorkspaceButton() {
  const head = $("where-conn");
  head.querySelector(".ws-count")?.remove();
  const others = workspaces.filter((w) => w !== ws);
  if (others.length) {
    const lost = others.some((w) => w.lost);
    head.append(el("span", { className: `ws-count${lost ? " lost" : ""}`, textContent: `+${others.length}`,
      title: lost ? "Another workspace lost its connection" : `${others.length} more workspace${others.length === 1 ? "" : "s"}` }));
  }
}

function showWorkspaces() {
  const r = $("where-conn").getBoundingClientRect();
  const items: MenuItem[] = workspaces.map((w) => [
    `${w === ws ? "✓ " : "    "}${w.conn.name}${w.info?.database ? ` · ${w.info.database}` : ""}${w.lost ? " (disconnected)" : ""}`,
    () => showWorkspace(w),
  ]);
  contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), [
    ...items, "-",
    [`New Workspace…  ${keyLabel("new-workspace")}`, newWorkspace],
    ["Reconnect All", reconnectAll],
    ["Close Workspace", closeWorkspace],
  ]);
}
$("where-conn").onclick = showWorkspaces;
$("where-conn").onkeydown = (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), showWorkspaces());
$("return-workspace").onclick = () => ws && showWorkspace(ws);

// ---------- Split panes: the focused pane is live, the others show a still picture of their tab ----------

// ponytail: an unfocused pane is a copy of its last frame (a query finishing there shows once it has focus);
// render each pane live once the grid and editor can draw into more than one place.
const live = () => $("live");

let nextShot = 1;

/** A still copy of the live pane: same look, no ids, nothing focusable. Clicking it focuses that pane. */
function snapshot(p: Pane): HTMLElement {
  const shot = live().cloneNode(true) as HTMLElement;
  for (const n of [shot, ...shot.querySelectorAll<HTMLElement>("[id]")]) (n.dataset.id = n.id), n.removeAttribute("id");
  for (const n of shot.querySelectorAll<HTMLElement>("button, input, select, textarea, [tabindex], [contenteditable]")) {
    n.tabIndex = -1;
    n.removeAttribute("contenteditable");
  }
  shot.classList.add("shot");
  // The column layout sheet targets #out; scoped to this copy, it neither leaks into the live grid nor goes stale.
  const scope = `shot-${nextShot++}`;
  shot.classList.add(scope);
  for (const st of shot.querySelectorAll("style")) st.textContent = (st.textContent ?? "").replaceAll("#out", `.${scope} [data-id="out"]`);
  shot.onpointerdown = (e) => {
    e.preventDefault();
    const tab = (e.target as HTMLElement).closest<HTMLElement>("[data-tab]")?.dataset.tab;
    focusPane(p);
    if (tab) select(Number(tab));
  };
  return shot;
}

function layoutPanes() {
  const panes = ws?.panes ?? [pane];
  $("panes").replaceChildren(...panes.map((p) => (p === pane ? live() : p.shot ?? snapshot(p))));
  $("panes").classList.toggle("split", panes.length > 1);
}

function focusPane(p: Pane) {
  if (p === pane || !ws) return;
  stashEditor();
  Object.assign(pane, { tabs, currentId });
  pane.shot = snapshot(pane);
  ws.pane = pane = p;
  ({ tabs, currentId } = p);
  shownId = 0;
  filtersDrawnFor = 0;
  layoutPanes();
  renderMain();
  renderTables();
}

function cyclePane(step: number) {
  const panes = ws?.panes ?? [];
  if (panes.length > 1) focusPane(panes[(panes.indexOf(pane) + step + panes.length) % panes.length]);
}

/** A new pane to the right of the focused one, with a new SQL tab. */
function splitRight() {
  if (!ws) return;
  stashEditor();
  Object.assign(pane, { tabs, currentId });
  pane.shot = snapshot(pane);
  const p = newPane();
  ws.panes.splice(ws.panes.indexOf(pane) + 1, 0, p);
  ws.pane = pane = p;
  tabs = [];
  currentId = 0;
  layoutPanes();
  openSqlTab();
  editor.focus();
}

/** Its last tab closed: the pane goes and its neighbour takes focus. */
function closePane() {
  if (!ws || ws.panes.length < 2) return;
  const i = ws.panes.indexOf(pane);
  ws.panes.splice(i, 1);
  const next = ws.panes[Math.max(0, i - 1)];
  ws.pane = pane = next;
  ({ tabs, currentId } = next);
  delete next.shot;
  shownId = 0;
  filtersDrawnFor = 0;
  layoutPanes();
  renderMain();
  renderTables();
}

// ---------- SQL tabs kept per connection ----------

type KeptTab = { title: string; sql: string; path?: string };

function loadSqlTabs(): KeptTab[] {
  const kept = active && docEntry<KeptTab[]>("sql_tabs", active.name);
  return Array.isArray(kept) ? kept.filter((k) => typeof k?.title === "string" && typeof k?.sql === "string") : [];
}

function saveSqlTabs() {
  if (!active || !settings.keepTabs) return;
  const kept: KeptTab[] = allTabs().flatMap((t) => (t.kind === "sql" && t.sql.trim() ? [{ title: t.title, sql: t.sql, path: t.path }] : []));
  setDocEntry("sql_tabs", active.name, kept.length ? kept : undefined);
}

// ---------- .sql files (⌘O, ⌘S, ⌘⇧S on SQL tabs) ----------

const SQL_FILES = [{ name: "SQL", extensions: ["sql"] }, { name: "All files", extensions: ["*"] }];
const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

async function openSqlFile() {
  if ($("workspace").hidden) return;
  const path = await openFileDialog({ multiple: false, directory: false, filters: SQL_FILES });
  if (typeof path !== "string") return;
  try {
    const sql = await invoke<string>("read_text_file", { path });
    const tab: SqlTab = { id: nextId++, kind: "sql", title: fileName(path), sql, path };
    tabs.push(tab);
    select(tab.id);
    saveSqlTabs();
  } catch (e) {
    status(String(e), "error");
  }
}

/** Saves the current SQL tab to its file; with `pick`, or when it has none yet, asks where first. */
async function saveSqlFile(pick: boolean) {
  const tab = current();
  if (tab?.kind !== "sql") return;
  const sql = shownId === tab.id ? editor.view.state.doc.toString() : tab.sql;
  let path = tab.path;
  if (pick || !path) {
    const chosen = await saveFileDialog({ defaultPath: path ?? `${tab.title}.sql`, filters: SQL_FILES });
    if (!chosen) return;
    path = chosen;
  }
  try {
    await invoke("write_text_file", { path, text: sql });
    tab.path = path;
    tab.title = fileName(path);
    renderMain();
    saveSqlTabs();
    status(`Saved ${tab.title}`, "done");
  } catch (e) {
    status(String(e), "error");
  }
}

let saveTimer = 0;
function saveSqlTabsSoon() {
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(saveSqlTabs, 400);
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
    databases = await call<string[]>("list_databases", { name: active.name });
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
  for (const t of allTabs()) if (!(await okToDiscard(t))) return;
  setStatus("db-status", `Opening ${name}…`, "busy");
  try {
    applyInfo(await call<Info>("use_database", { name: active.name, database: name }));
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

// ---------- Sidebar: tables (expandable to columns) and history ----------

type Column = { name: string; data_type: string; nullable: boolean; default: string | null; primary: boolean };
let columnsCache = new Map<string, Column[]>(); // swapped per workspace, so a late answer lands in its own
let expanded = new Set<string>();

async function loadColumns(table: string) {
  const cache = columnsCache; // the workspace may change while this waits
  if (!cache.has(table)) cache.set(table, await call<Column[]>("columns", { name: active!.name, table }));
  return cache.get(table)!;
}

/** The sidebar tree (sidebar.ts), drawing table rows with `tableRow`. */
function renderTables() {
  const shown = active ? renderTree($("tables"), $<HTMLInputElement>("filter").value) : 0;
  const hint = $("no-tables");
  hint.hidden = shown > 0;
  hint.textContent =
    info?.can_switch && !info.database ? `No database selected. Press ${keyLabel("open-database")} to choose one.`
    : tables.length ? "No tables match."
    : "This database has no tables.";
}

/** A table's row in the sidebar: click opens it, the chevron shows its columns. `extra` joins its menu. */
function tableRow(t: string, label: string, extra: MenuItem[]) {
  const open = current()?.kind === "table" ? (current() as TableTab).table : null;
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
  const row = el("div", { className: "tree-row", title: t }, chevron, el("span", { className: "tbl-icon" }), el("span", { className: "label", textContent: label }));
  row.classList.toggle("active", t === open);
  row.onclick = () => openTable(t);
  row.oncontextmenu = (e) => (e.stopPropagation(), contextMenu(e, [...tableMenu(t), "-", ...transferMenu(t), ...extra]));
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
}

type Side = "items" | "history" | "search";
function showSide(which: Side) {
  for (const b of document.querySelectorAll<HTMLElement>(".side-tabs button")) b.setAttribute("aria-pressed", String(b.dataset.side === which));
  $("side-items").hidden = which !== "items";
  $("side-history").hidden = which !== "history";
  $("side-search").hidden = which !== "search";
  if (which === "search") $("db-search-text").focus();
}

// History lives per connection in history.json (Settings › Locations), newest first, 500 per connection.
type HistoryEntry = { sql: string; at: number; db: string | null };

function loadHistory(): HistoryEntry[] {
  const list = active && docEntry<HistoryEntry[]>("history", active.name);
  return Array.isArray(list) ? list : [];
}

function addHistory(sql: string) {
  if (!active) return;
  const list = [{ sql: sql.trim(), at: Date.now(), db: info?.database ?? null }, ...loadHistory()].slice(0, 500);
  setDocEntry("history", active.name, list);
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

type SqlTab = {
  id: number; kind: "sql"; title: string; sql: string; state?: EditorState; result?: QueryResult; status?: [string, StatusKind];
  /** The .sql file the tab was opened from or saved to. */
  path?: string;
  /** The statement behind `result`, for exporting it. */
  ran?: string;
};
type Sort = { column: string; desc: boolean };
type TableTab = {
  id: number; kind: "table"; title: string; table: string; view: "data" | "structure"; page: number;
  total?: number; result?: QueryResult; error?: string; loading?: boolean;
  sort: Sort[]; filters: FilterRow[]; combine: "and" | "or"; filtersShown: boolean; layout: Layout;
  /** Staged cell edits, added and deleted rows (grid-edit.ts). */
  staged: Staged;
  /** Row identities (ctid / rowid) of the page's rows, for saving again when a key matches several rows. */
  ids?: string[];
  message?: [string, StatusKind];
  /** A table being created in the structure view; it doesn't exist yet. */
  create?: boolean;
};
type Tab = SqlTab | TableTab;
type StatusKind = "" | "busy" | "error" | "done";
const PAGE = 300;
let tabs: Tab[] = [];
let currentId = 0;
let nextId = 1;
let shownId = 0; // the SQL tab whose state is in the editor right now
const current = () => tabs.find((t) => t.id === currentId);

/** The tabs of one pane (the window splits into two side by side). While a pane has focus its tabs and current
 *  tab live in `tabs` and `currentId`; the rest is its history for Back/Forward and Reopen Closed Tab. */
type Pane = {
  tabs: Tab[]; currentId: number; back: number[]; forward: number[]; closed: { tab: Tab; index: number }[];
  /** What an unfocused pane shows: a still copy of its last frame. */
  shot?: HTMLElement;
};
const newPane = (): Pane => ({ tabs: [], currentId: 0, back: [], forward: [], closed: [] });
let pane: Pane = newPane();
let navigating = false; // Back/Forward selects without recording the move

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

function openTable(table: string, view: TableTab["view"] = "data", create = false) {
  const existing = tabs.find((t) => t.kind === "table" && t.table === table) as TableTab | undefined;
  if (existing) {
    if (existing.view !== view && !pendingCount(existing)) existing.view = view;
    return select(existing.id);
  }
  const tab: TableTab = {
    id: nextId++, kind: "table", title: table, table, view, page: 0, staged: new Staged(), create,
    sort: [], filters: [blankFilter()], combine: "and", filtersShown: false, layout: loadLayout(active!.name, table),
  };
  tabs.push(tab);
  select(tab.id);
  if (view === "data") loadTable(tab);
}

/** Keeps the editor's text and undo history with the SQL tab that's showing. */
function stashEditor() {
  const t = current();
  if (t?.kind === "sql" && shownId === t.id) t.state = editor.view.state;
}

function select(id: number) {
  stashEditor();
  if (!navigating && currentId && id !== currentId && tabs.some((t) => t.id === currentId)) {
    pane.back = [...pane.back, currentId].slice(-50);
    pane.forward = [];
  }
  currentId = id;
  renderMain();
  renderTables();
}

/** Closes a tab; `remember` keeps it for Reopen Closed Tab (not for a table that was just dropped). */
async function closeTab(id: number, remember = true) {
  const i = tabs.findIndex((t) => t.id === id);
  if (i < 0 || !(await okToDiscard(tabs[i]))) return;
  if (id === currentId) stashEditor();
  if (remember) pane.closed = [...pane.closed, { tab: tabs[i], index: i }].slice(-20);
  tabs.splice(i, 1);
  saveSqlTabs();
  if (!tabs.length && (ws?.panes.length ?? 1) > 1) return closePane();
  if (!tabs.length) return void closeWorkspace(); // last tab gone: leave the connection, like TablePlus
  if (id === currentId) select(tabs[Math.max(0, i - 1)].id);
  else renderMain();
}

/** Every other tab closed, each asking first if it has unsaved changes. */
async function closeOtherTabs(id: number) {
  for (const t of tabs.filter((t) => t.id !== id)) if (!(await okToDiscard(t))) return;
  pane.closed = [...pane.closed, ...tabs.filter((t) => t.id !== id).map((tab) => ({ tab, index: 0 }))].slice(-20);
  tabs = tabs.filter((t) => t.id === id);
  saveSqlTabs();
  select(id);
}

function reopenClosedTab() {
  const last = pane.closed.at(-1);
  if (!last) return;
  pane.closed = pane.closed.slice(0, -1);
  tabs.splice(Math.min(last.index, tabs.length), 0, last.tab);
  saveSqlTabs();
  select(last.tab.id);
  if (last.tab.kind === "table" && last.tab.view === "data") loadTable(last.tab);
}

/** Tab n (1-based); 9 is the last tab, as in browsers. */
function selectTabAt(n: number) {
  const t = n === 9 ? tabs.at(-1) : tabs[n - 1];
  if (t) select(t.id);
}

function cycleTab(step: number) {
  const i = tabs.findIndex((t) => t.id === currentId);
  if (tabs.length > 1) select(tabs[(i + step + tabs.length) % tabs.length].id);
}

/** Back and Forward through the tabs opened in this pane; closed ones are skipped. */
function goBack(forward: boolean) {
  const [from, to] = forward ? [pane.forward, pane.back] : [pane.back, pane.forward];
  while (from.length) {
    const id = from.pop()!;
    if (!tabs.some((t) => t.id === id) || id === currentId) continue;
    to.push(currentId);
    navigating = true;
    select(id);
    navigating = false;
    return;
  }
}

/** Swaps the tab's label for a text field; Enter keeps the new name, Esc or an empty name keeps the old one. */
function renameTab(id: number) {
  const t = tabs.find((t) => t.id === id);
  const label = $("tabs").querySelector(`[data-tab="${id}"] .label`);
  if (!t || !label) return;
  const input = el("input", { className: "tab-rename", value: t.title, ariaLabel: "Tab name" });
  label.replaceWith(input);
  input.select();
  let done = false;
  const finish = (keep: boolean) => {
    if (done) return;
    done = true;
    if (keep && input.value.trim()) t.title = input.value.trim();
    saveSqlTabs();
    renderMain();
  };
  input.onkeydown = (e) => {
    e.stopPropagation(); // Space and Enter belong to the field, not the tab button
    if (e.key === "Enter") finish(true);
    else if (e.key === "Escape") finish(false);
  };
  input.onclick = (e) => e.stopPropagation();
  input.onblur = () => finish(true);
}

function tabMenu(id: number): MenuItem[] {
  return [
    ["Rename…", () => renameTab(id)],
    "-",
    ["Close Tab", () => closeTab(id)],
    ["Close Other Tabs", () => closeOtherTabs(id)],
    ...(pane.closed.length ? [["Reopen Closed Tab", reopenClosedTab] as MenuItem] : []),
  ];
}

/** Reload Current Tab: a table's rows (and count) or structure again; on a SQL tab, the sidebar's items. */
async function reloadTab() {
  const tab = current();
  if (tab?.kind !== "table") return reloadItems();
  if (!(await okToDiscard(tab))) return;
  columnsCache.delete(tab.table);
  tab.total = undefined;
  if (tab.view === "data") loadTable(tab);
  else (dropDraft(tab.id), renderMain());
}

function renderMain() {
  const tab = current();
  if (!tab) return;
  $("tabs").replaceChildren(
    ...tabs.map((t) => {
      const close = el("span", { className: "close", textContent: "×", title: "Close tab" });
      close.onclick = (e) => (e.stopPropagation(), closeTab(t.id));
      const b = el("button", { className: `tab ${t.kind}`, title: t.kind === "sql" && t.path ? t.path : t.title },
        el("span", { className: t.kind === "sql" ? "tab-sql" : "tbl-icon", textContent: t.kind === "sql" ? "SQL" : "" }),
        el("span", { className: "label", textContent: t.title }),
        close,
      );
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(t.id === currentId));
      b.dataset.tab = String(t.id);
      b.onclick = () => select(t.id);
      b.onauxclick = (e) => e.button === 1 && closeTab(t.id); // middle-click closes
      b.ondblclick = () => renameTab(t.id);
      b.oncontextmenu = (e) => contextMenu(e, tabMenu(t.id));
      return b;
    }),
  );
  $("tabs").querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });

  const env = active?.tag ? `${TAGS[active.tag] ?? active.tag} | ` : "";
  $("pill").textContent = `${env}${info?.server ?? ""}${info?.database ? ` : ${info.database}` : ""} : ${tab.title}`;
  $("pill").className = `pill ${active?.tag ?? ""}`;

  $("sql-pane").hidden = tab.kind !== "sql";
  $("table-foot").hidden = tab.kind !== "table";
  showFilters(tab);
  applyLayout(tab);
  if (tab.kind === "sql") {
    if (shownId !== tab.id) {
      editor.setState((tab.state ??= editor.newState(tab.sql)));
      shownId = tab.id;
    }
    updateCursor();
    status(...(tab.status ?? ["", ""]));
    if (tab.result) render(tab.result, 0);
    else $("out").replaceChildren(el("p", { className: "hint empty", textContent: "Run a query to see results here." }));
    syncDetail();
    return;
  }
  for (const b of $("view-seg").querySelectorAll<HTMLElement>("button")) b.setAttribute("aria-pressed", String(b.dataset.view === tab.view));
  const from = tab.page * PAGE;
  const shown = tab.result?.rows.length ?? 0;
  $("page-info").textContent =
    tab.view === "structure" ? (tab.create ? "New table" : "")
    : tab.loading ? "Loading…"
    : `${shown ? from + 1 : 0}–${from + shown} of ${tab.total ?? "?"} rows`;
  $<HTMLButtonElement>("page-prev").disabled = tab.view !== "data" || tab.page === 0;
  $<HTMLButtonElement>("page-next").disabled = tab.view !== "data" || tab.total === undefined || from + shown >= tab.total;
  showPending(tab);
  const [msg, kind] = tab.message ?? ["", ""];
  const m = $("foot-msg");
  m.textContent = tab.view === "data" && tab.result && !grid.keyOf(tab) ? "Read-only: no primary key" : msg;
  $("add-row").hidden = tab.view !== "data" || !tab.result || !grid.keyOf(tab);
  m.className = `foot-msg ${kind}`;
  if (tab.view === "structure") showStructure($("out"), tab.id, tab.table, !!tab.create);
  else if (tab.error) $("out").replaceChildren(el("p", { className: "hint empty error-text", textContent: tab.error }));
  else if (tab.result) {
    const quickFilter = (ri: number, ci: number): MenuItem[] => {
      if (ri >= tab.result!.rows.length) return grid.cellMenu(tab, ri, ci, selectedRows()); // an added row
      const column = tab.result!.columns[ci];
      const value = tab.result!.rows[ri][ci];
      const shown = value === null ? "NULL" : value.length > 24 ? value.slice(0, 24) + "…" : value;
      return ["-", [`Filter rows where ${column} = ${shown}`, () => filterByCell(tab, column, value)], ...grid.cellMenu(tab, ri, ci, selectedRows())];
    };
    render(tab.result, from, {
      sort: tab.sort, onSort: (c, add) => sortBy(tab, c, add), table: tab.table, menu: quickFilter,
      resize: (column, width, done) => resizeColumn(tab, column, width, done),
    });
    grid.decorate(tab);
    paintSelection();
  }
  else $("out").replaceChildren();
  syncDetail();
}

/** The row detail follows the selection's row on a table tab's data. */
function syncDetail() {
  const tab = current();
  const table = tab?.kind === "table" && tab.view === "data" && !tab.error ? tab : null;
  const sel = selection;
  syncRowDetail(table, sel && sel.r === table?.result ? sel.focus[0] : null);
}

/** The rows the selection covers, top to bottom. */
function selectedRows() {
  if (!selection) return [];
  const { r0, r1 } = selectedBlock(selection);
  return Array.from({ length: r1 - r0 + 1 }, (_, i) => r0 + i);
}

function showPending(tab: Tab) {
  const pending = pendingCount(tab);
  $("edit-info").textContent = pending ? `${pending} unsaved change${pending === 1 ? "" : "s"}` : "";
  $("discard-edits").hidden = $("save-edits").hidden = !pending;
  $("preview-edits").hidden = !pending || tab.kind !== "table" || tab.view !== "data";
  $("save-edits").textContent = tab.kind === "table" && tab.view === "structure" ? "Preview SQL…" : "Save";
}

// ---------- Inline cell editing (table tabs) ----------

const pendingCount = (tab: Tab) =>
  tab.kind === "table" ? tab.staged.count + structurePending(tab.id) : 0;

async function okToDiscard(tab: Tab) {
  const n = pendingCount(tab);
  if (!n || (await confirmDialog(`Discard ${n} unsaved change${n === 1 ? "" : "s"} in ${tab.title}?`, "Discard"))) {
    if (tab.kind === "table") (tab.staged.clear(), dropDraft(tab.id));
    return true;
  }
  return false;
}

function fillCell(td: HTMLTableCellElement, v: string | null) {
  td.textContent = v ?? "NULL";
  td.className = v === null ? "null" : v === "" ? "empty-val" : numeric.test(v) ? "num" : "";
  td.title = v && v.length > 40 ? v : ""; // full value on hover when the cell is truncated
}

async function saveEdits() {
  const tab = current();
  if (tab?.kind === "table" && tab.view === "structure") return saveStructure(tab.id, $("out"));
  if (tab?.kind === "table") return grid.save(tab);
}

async function loadTable(tab: TableTab) {
  const name = active!.name; // the tab's workspace, even if another takes the screen meanwhile
  tab.loading = true;
  tab.error = undefined;
  if (tab.id === currentId) renderMain();
  try {
    await loadColumns(tab.table); // structure view, the filter bar's columns, and the primary key for editing
    if (tab.view === "data") {
      // ponytail: exact count(*) can be slow on huge tables; estimate from catalog stats when that hurts.
      const request = {
        table: tab.table, filters: activeFilters(tab), combine: tab.combine, sort: tab.sort,
        limit: PAGE, offset: tab.page * PAGE, count: tab.total === undefined, identity: true,
      };
      const page = await call<{ columns: string[]; rows: (string | null)[][]; total: number | null; ids?: string[] }>("browse", { name, request });
      tab.result = { columns: page.columns, rows: page.rows, affected: 0 };
      tab.ids = page.ids;
      if (page.total !== null) tab.total = page.total;
    }
  } catch (e) {
    tab.error = String(e);
  }
  tab.loading = false;
  if (tab.id === currentId) renderMain(); // the user may have moved to another tab meanwhile
}

// ---------- Filters and sorting (table tabs) ----------

let filtersDrawnFor = 0; // the tab whose filter bar is in the DOM; redrawing on every render would steal focus

function showFilters(tab: Tab) {
  const bar = $("filters");
  const shown = tab.kind === "table" && tab.view === "data" && tab.filtersShown;
  bar.hidden = !shown;
  const toggle = $("toggle-filters");
  if (tab.kind === "table") {
    const n = activeFilters(tab).length;
    toggle.textContent = n ? `Filters (${n})` : "Filters";
    toggle.setAttribute("aria-pressed", String(tab.filtersShown));
  }
  if (!shown) return void (filtersDrawnFor = 0);
  const columns = columnsCache.get(tab.table)?.map((c) => c.name);
  if (filtersDrawnFor === tab.id || !columns) return;
  filtersDrawnFor = tab.id;
  renderFilters(bar, tab, columns, () => applyFilters(tab));
}

async function applyFilters(tab: TableTab) {
  if (!(await okToDiscard(tab))) return;
  tab.page = 0;
  tab.total = undefined; // a different filter matches a different number of rows
  loadTable(tab);
}

function toggleFilters(open?: boolean) {
  const tab = current();
  if (tab?.kind !== "table" || tab.view !== "data") return;
  tab.filtersShown = open ?? !tab.filtersShown;
  filtersDrawnFor = 0;
  renderMain();
  if (tab.filtersShown) $("filters").querySelector<HTMLInputElement>("input[type=text]:not([hidden])")?.focus();
}

/** Header click: ascending, then descending, then off. With Shift, the column joins the existing sort. */
async function sortBy(tab: TableTab, column: string, add: boolean) {
  if (!(await okToDiscard(tab))) return;
  const i = tab.sort.findIndex((s) => s.column === column);
  const cur = tab.sort[i];
  const next = !cur ? { column, desc: false } : !cur.desc ? { column, desc: true } : null;
  if (!add) tab.sort = next ? [next] : [];
  else if (i < 0) tab.sort.push(next!);
  else if (next) tab.sort[i] = next;
  else tab.sort.splice(i, 1);
  tab.page = 0;
  loadTable(tab);
}

/** Right-click on a cell: filter the table to rows with this value. */
function filterByCell(tab: TableTab, column: string, value: string | null) {
  const row: FilterRow = value === null ? { ...blankFilter(), column, op: "is_null" } : { ...blankFilter(), column, op: "eq", value };
  const blank = tab.filters.length === 1 && !activeFilters(tab).length;
  tab.filters = blank ? [row] : [...tab.filters, row];
  tab.filtersShown = true;
  filtersDrawnFor = 0;
  applyFilters(tab);
}

let menuClose: (() => void) | null = null;
type MenuItem = [string, () => void] | "-";

/** Backup, restore and the process list, on a launcher row's right-click and the workspace's Tools button. */
function toolsMenu(c: Saved, database?: string | null): MenuItem[] {
  const items: MenuItem[] = [["Backup…", () => openBackup(c, database)], ["Restore…", () => openRestore(c, database, confirmDialog)]];
  if (c.kind !== "sqlite") items.push("-", ["Processes", () => openProcesses(c, confirmDialog)]);
  return items;
}
$("tools-button").onclick = () => {
  const r = $("tools-button").getBoundingClientRect(); // under the button, also when opened from the keyboard
  if (active) contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), toolsMenu(active, info?.database));
};

/** A small context menu at the pointer; any click or Esc closes it. */
function contextMenu(e: MouseEvent, items: MenuItem[]) {
  e.preventDefault();
  menuClose?.();
  const menu = el("div", { className: "menu" });
  menu.setAttribute("role", "menu");
  for (const item of items) {
    if (item === "-") menu.append(el("hr"));
    else {
      const b = el("button", { type: "button", textContent: item[0] });
      b.setAttribute("role", "menuitem");
      b.onclick = () => (close(), item[1]());
      menu.append(b);
    }
  }
  document.body.append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(e.clientX, innerWidth - r.width - 8)}px`;
  menu.style.top = `${Math.min(e.clientY, innerHeight - r.height - 8)}px`;
  menu.querySelector("button")?.focus();
  const onDown = (ev: Event) => !menu.contains(ev.target as Node) && close();
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && close();
  function close() {
    menu.remove();
    removeEventListener("pointerdown", onDown, true);
    removeEventListener("keydown", onKey, true);
    menuClose = null;
  }
  addEventListener("pointerdown", onDown, true);
  addEventListener("keydown", onKey, true);
  menuClose = close;
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

/** The sidebar's items again (new or dropped tables), with columns and autocomplete reloaded. */
async function reloadItems() {
  const i = await call<Info>("connect", { name: active!.name }); // reuses the open connection
  forgetItems(active!.name);
  info = i;
  tables = i.tables;
  columnsCache.clear();
  renderTables();
  loadSchema();
}

/** Columns for every table, for autocomplete. Runs in the background; the editor works without it meanwhile. */
async function loadSchema() {
  if (!active) return;
  const conn = active;
  try {
    const pairs = await invoke<[string, string][]>("schema", { name: conn.name });
    if (active !== conn) return; // switched connections meanwhile
    const schema: Schema = new Map(tables.map((t) => [t, []]));
    for (const [table, column] of pairs) (schema.get(table) ?? schema.set(table, []).get(table)!).push(column);
    editor.setDialect(conn.kind, schema);
  } catch {} // autocomplete just keeps table names
}

const fmt = (n: number) => n.toLocaleString();
const rowLimit = () => settings.sqlLimit || null;

/** Runs SQL from the current SQL tab; the result and status stay with that tab. */
async function run(sql: string) {
  const tab = current();
  if (!active || tab?.kind !== "sql" || !sql.trim()) return;
  const say = (text: string, kind: StatusKind) => {
    tab.status = [text, kind];
    if (tab.id === currentId) status(text, kind);
  };
  say("Running…", "busy");
  const t0 = performance.now();
  try {
    const conn = active;
    const r = await guarded(conn.name, (a) => call<QueryResult>("run_query", { name: conn.name, sql, ...a, maxRows: rowLimit() }),
      () => (say("", ""), ask(conn.safety === "alert1" ? `Run this on "${conn.name}"?` : `This may change data on "${conn.name}".`, sql.trim(), "Run anyway")));
    tab.result = r;
    tab.ran = sql;
    if (tab.id === currentId) render(r, 0);
    const ms = Math.round(performance.now() - t0);
    const limited = r.truncated ? ` (first ${fmt(r.rows.length)}; raise the row limit to see more)` : "";
    say(r.columns.length ? `${fmt(r.rows.length)} rows${limited} · ${ms} ms` : `${fmt(r.affected)} rows affected · ${ms} ms`, "done");
    addHistory(sql);
  } catch (e) {
    say(e === CANCELLED ? "Cancelled" : String(e), e === CANCELLED ? "" : "error");
  }
}

// ---------- Query parameters ----------

const paramValues = new Map<string, string>(); // last value per name, for this session

/** Asks for the values of `names`; resolves to null when cancelled. */
function askParams(names: string[]): Promise<Record<string, string> | null> {
  const d = $<HTMLDialogElement>("params");
  const inputs = names.map((name) => {
    const input = el("input", { name, value: paramValues.get(name) ?? "", placeholder: "value, e.g. 42 or 'text'" });
    const label = el("label", { textContent: /^\d+$/.test(name) ? `Parameter ${name}` : name });
    label.append(input);
    return { name, input, label };
  });
  $("params-fields").replaceChildren(...inputs.map((i) => i.label));
  d.showModal();
  inputs[0]?.input.select();
  return new Promise((resolve) => {
    const done = (values: Record<string, string> | null) => {
      d.close();
      resolve(values);
    };
    $("params-cancel").onclick = () => done(null);
    d.oncancel = () => done(null);
    $<HTMLFormElement>("params-form").onsubmit = (e) => {
      e.preventDefault();
      const values = Object.fromEntries(inputs.map((i) => [i.name, i.input.value]));
      for (const [k, v] of Object.entries(values)) paramValues.set(k, v);
      done(values);
    };
  });
}

/** `statements` with their parameters filled in (asking once for every name), or null when cancelled. */
async function withParams(statements: string[]): Promise<string[] | null> {
  if (settings.paramFormat === "off" || !active) return statements;
  const format = settings.paramFormat;
  const found = statements.map((s) => findParams(s, active!.kind as Parameters<typeof findParams>[1], format));
  const names = [...new Set(found.flat().map((p) => p.name))];
  if (!names.length) return statements;
  const values = await askParams(names);
  return values && statements.map((s, i) => fillParams(s, found[i], values));
}

const runCurrent = async () => {
  const filled = await withParams([editor.current()]);
  if (filled) run(filled[0]);
};

/** Runs each statement in turn, stopping at the first error; the last result is shown. Safe mode asks once. */
async function runAll() {
  const tab = current();
  const texts = editor.all().map((s) => s.text);
  if (!active || tab?.kind !== "sql" || !texts.length) return;
  const filled = await withParams(texts);
  if (!filled) return;
  const statements = filled.map((text) => ({ text }));
  if (statements.length === 1) return run(statements[0].text);
  const say = (text: string, kind: StatusKind) => {
    tab.status = [text, kind];
    if (tab.id === currentId) status(text, kind);
  };
  const t0 = performance.now();
  let approved = NOT_APPROVED; // asked once, then kept for the rest
  let affected = 0;
  for (let i = 0; i < statements.length; i++) {
    const sql = statements[i].text;
    say(`Running ${i + 1} of ${statements.length}…`, "busy");
    try {
      const r = await call<QueryResult>("run_query", { name: active.name, sql, ...approved, maxRows: rowLimit() });
      affected += r.affected;
      if (r.columns.length || i === statements.length - 1) {
        tab.result = r;
        tab.ran = sql;
        if (tab.id === currentId) render(r, 0);
      }
      addHistory(sql);
    } catch (e) {
      if (!isSafetyStop(e) || (e === "CONFIRM" && approved.confirmed)) return say(`Statement ${i + 1} of ${statements.length} failed: ${e}`, "error");
      say("", "");
      const rest = statements.slice(i).map((x) => x.text).join(";\n");
      const next = await approval(e, active.name, () => ask(`Run these statements on "${active!.name}"?`, rest, "Run anyway"));
      if (!next) return say(`Cancelled before statement ${i + 1}`, "");
      approved = next;
      i--; // run this statement again, approved
    }
  }
  const ms = Math.round(performance.now() - t0);
  const cut = tab.result?.truncated ? ` · last result cut at ${fmt(tab.result.rows.length)} rows` : "";
  say(`${statements.length} statements · ${fmt(affected)} rows affected${cut} · ${ms} ms`, "done");
}

const numeric = /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i;

type RenderOptions = {
  sort: Sort[];
  onSort: (column: string, add: boolean) => void;
  /** The table the rows come from, for "Copy as INSERT". */
  table: string;
  /** Extra context-menu items for a cell. */
  menu?: (row: number, col: number) => MenuItem[];
  /** Header drag: a new width while dragging, the final one when `done`; null resets to automatic. */
  resize?: (column: string, width: number | null, done: boolean) => void;
};

/** Result grid with row numbers starting after `offset`. With options, headers sort the rows when clicked. */
function render(r: QueryResult, offset: number, sorting?: RenderOptions) {
  if (!r.columns.length) return $("out").replaceChildren(el("p", { className: "hint empty", textContent: `${r.affected} rows affected.` }));
  const table = el("table");
  const head = table.createTHead().insertRow();
  head.append(el("th", { className: "rownum" }));
  for (const c of r.columns) {
    const th = el("th", { textContent: c });
    if (sorting) {
      th.className = "sortable";
      th.title = "Sort by this column; Shift-click to add it to the sort";
      const i = sorting.sort.findIndex((s) => s.column === c);
      if (i >= 0) {
        const s = sorting.sort[i];
        th.append(el("span", { className: "sort", textContent: `${s.desc ? "▼" : "▲"}${sorting.sort.length > 1 ? i + 1 : ""}` }));
        th.setAttribute("aria-sort", s.desc ? "descending" : "ascending");
      }
      th.onclick = (e) => sorting.onSort(c, e.shiftKey);
      if (sorting.resize) th.append(resizeGrip(th, c, sorting.resize));
    }
    head.append(th);
  }
  const body = table.createTBody();
  // ponytail: plain table; switch to a virtualized grid when pages get much larger than 300 rows.
  r.rows.forEach((row, i) => {
    const tr = body.insertRow();
    tr.append(el("td", { className: "rownum", textContent: String(offset + i + 1) }));
    for (const v of row) fillCell(tr.insertCell(), v);
  });
  if (!r.rows.length) {
    const tr = body.insertRow();
    tr.append(el("td", { className: "hint", colSpan: r.columns.length + 1, textContent: "No rows." }));
  }
  $("out").replaceChildren(table);
  if (selection?.r !== r) selection = null; // a redraw of the same rows (after an edit) keeps it
  if (r.rows.length) wireSelection(table, r, sorting);
}

// ---------- Selection and copying ----------

/** A block of cells, from the anchor (first click) to the focus (Shift-click or drag end), inclusive. */
type Selection = { r: QueryResult; anchor: [number, number]; focus: [number, number]; table?: string };
let selection: Selection | null = null;

function selectedBlock(sel: Selection) {
  const [r0, r1] = [Math.min(sel.anchor[0], sel.focus[0]), Math.max(sel.anchor[0], sel.focus[0])];
  const [c0, c1] = [Math.min(sel.anchor[1], sel.focus[1]), Math.max(sel.anchor[1], sel.focus[1])];
  return { r0, r1, c0, c1 };
}

function paintSelection() {
  const block = selection && selectedBlock(selection);
  $("out").querySelectorAll("tbody tr").forEach((tr, ri) => {
    [...tr.children].forEach((td, i) => {
      const ci = i - 1; // the row number comes first
      td.classList.toggle("sel", !!block && ci >= 0 && ri >= block.r0 && ri <= block.r1 && ci >= block.c0 && ci <= block.c1);
    });
  });
  syncDetail();
}

/** The selected cells (or, with `wholeRows`, every column of the selected rows) with their column names. Hidden
 *  columns are left out. */
function selectedCells(wholeRows = false) {
  if (!selection) return null;
  const { r0, r1, c0, c1 } = selectedBlock(selection);
  const [from, to] = wholeRows ? [0, selection.r.columns.length - 1] : [c0, c1];
  const tab = current();
  const hidden = new Set(tab?.kind === "table" && tab.table === selection.table ? tab.layout.hidden : []);
  const keep = selection.r.columns.map((_, i) => i).filter((i) => i >= from && i <= to && !hidden.has(selection!.r.columns[i]));
  return {
    columns: keep.map((i) => selection!.r.columns[i]),
    rows: selection.r.rows.slice(r0, r1 + 1).map((row) => keep.map((i) => row[i])),
  };
}

// ---------- Column layout (table tabs) ----------

function applyLayout(tab: Tab) {
  const columns = tab.kind === "table" && tab.view === "data" ? tab.result?.columns : undefined;
  $("column-layout").textContent = columns ? layoutCss(columns, (tab as TableTab).layout) : "";
  if (tab.kind === "table") {
    const n = tab.layout.hidden.length;
    $("columns-button").textContent = n ? `Columns (${n} hidden)` : "Columns";
  }
}

function resizeColumn(tab: TableTab, column: string, width: number | null, done: boolean) {
  if (width === null) delete tab.layout.widths[column];
  else tab.layout.widths[column] = Math.round(width);
  applyLayout(tab);
  if (done) saveLayout(active!.name, tab.table, tab.layout);
}

/** A drag handle on a header's right edge; double-click returns the column to its automatic width. */
function resizeGrip(th: HTMLElement, column: string, resize: NonNullable<RenderOptions["resize"]>) {
  const grip = el("span", { className: "grip", title: "Drag to resize; double-click to fit" });
  grip.onclick = (e) => e.stopPropagation(); // not a sort click
  grip.ondblclick = (e) => (e.stopPropagation(), resize(column, null, true));
  grip.onpointerdown = (e) => {
    e.preventDefault();
    e.stopPropagation();
    grip.setPointerCapture(e.pointerId);
    grip.classList.add("active");
    const startX = e.clientX;
    const startWidth = th.getBoundingClientRect().width;
    let width = startWidth;
    grip.onpointermove = (m) => resize(column, (width = Math.max(MIN_WIDTH, startWidth + m.clientX - startX)), false);
    grip.onpointerup = () => {
      grip.onpointermove = grip.onpointerup = null;
      grip.classList.remove("active");
      resize(column, width, true);
    };
  };
  return grip;
}

/** The Columns button: a checklist of the table's columns. Unchecking hides a column. */
function showColumnsMenu() {
  const tab = current();
  if (tab?.kind !== "table" || !tab.result) return;
  menuClose?.();
  const t = tab;
  const menu = el("div", { className: "popover columns-menu" });
  const save = () => (saveLayout(active!.name, t.table, t.layout), applyLayout(t));
  const boxes = t.result!.columns.map((c) => {
    const box = el("input", { type: "checkbox", checked: !t.layout.hidden.includes(c) });
    box.onchange = () => {
      t.layout.hidden = box.checked ? t.layout.hidden.filter((h) => h !== c) : [...t.layout.hidden, c];
      save();
    };
    const row = el("label", { className: "check" });
    row.append(box, c);
    return { row, box };
  });
  const showAll = el("button", { type: "button", textContent: "Show all" });
  showAll.onclick = () => ((t.layout.hidden = []), boxes.forEach((b) => (b.box.checked = true)), save());
  const reset = el("button", { type: "button", textContent: "Reset widths" });
  reset.onclick = () => ((t.layout.widths = {}), save());
  const foot = el("footer");
  foot.append(showAll, el("span", { className: "spacer" }), reset);
  menu.append(...boxes.map((b) => b.row), foot);
  document.body.append(menu);
  const r = $("columns-button").getBoundingClientRect();
  const h = menu.getBoundingClientRect().height;
  Object.assign(menu.style, { left: `${Math.max(8, r.left)}px`, top: `${Math.max(8, r.top - h - 6)}px` });
  const onDown = (ev: Event) => !menu.contains(ev.target as Node) && ev.target !== $("columns-button") && close();
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && close();
  function close() {
    menu.remove();
    removeEventListener("pointerdown", onDown, true);
    removeEventListener("keydown", onKey, true);
    menuClose = null;
  }
  addEventListener("pointerdown", onDown, true);
  addEventListener("keydown", onKey, true);
  menuClose = close;
}

/** A short note where the current tab shows messages: the run bar for SQL tabs, the footer for table tabs. */
function note(text: string, kind: StatusKind) {
  if (current()?.kind !== "table") return status(text, kind);
  // Written directly: a full re-render would also redraw the grid and drop the selection.
  const m = $("foot-msg");
  m.textContent = text;
  m.className = `foot-msg ${kind}`;
}

async function copyText(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    note(`Copied ${what}`, "done");
  } catch (e) {
    note(`Couldn't copy: ${e}`, "error");
  }
}

function copySelection(format: "plain" | "csv" | "markdown" | "insert") {
  const cells = selectedCells(format === "insert");
  if (!cells || !active) return;
  const n = cells.rows.length;
  const rows = `${n} row${n === 1 ? "" : "s"}`;
  if (format === "plain") return copyText(plain(cells), n === 1 && cells.columns.length === 1 ? "cell" : rows);
  if (format === "csv") return copyText(csv(cells), `${rows} as CSV`);
  if (format === "markdown") return copyText(markdown(cells), `${rows} as Markdown`);
  copyText(inserts(cells, selection!.table ?? "table_name", active.kind as Dialect), `${rows} as INSERT statements`);
}

function wireSelection(table: HTMLTableElement, r: QueryResult, options?: RenderOptions) {
  const cellOf = (target: EventTarget | null): [number, number] | null => {
    const td = (target as HTMLElement | null)?.closest?.("td");
    const tr = td?.parentElement as HTMLTableRowElement | null;
    if (!td || !tr || td.querySelector("input")) return null; // an open cell editor keeps its own clicks
    const ri = tr.sectionRowIndex;
    const ci = td.cellIndex - 1;
    return ci < 0 ? [ri, -1] : [ri, ci]; // -1: the row number
  };
  const last = r.columns.length - 1;
  let dragging = false;
  table.tBodies[0].onpointerdown = (e) => {
    const at = cellOf(e.target);
    if (!at || e.button !== 0) return;
    const [ri, ci] = at;
    const wholeRow = ci < 0;
    if (e.shiftKey && selection) selection.focus = [ri, wholeRow ? last : ci];
    else selection = { r, anchor: [ri, wholeRow ? 0 : ci], focus: [ri, wholeRow ? last : ci], table: options?.table };
    if (wholeRow) (selection.anchor[1] = 0), (selection.focus[1] = last);
    dragging = !e.shiftKey;
    paintSelection();
  };
  table.tBodies[0].onpointerover = (e) => {
    if (!dragging || !selection || !(e.buttons & 1)) return;
    const at = cellOf(e.target);
    if (!at) return;
    selection.focus = [at[0], at[1] < 0 ? last : at[1]];
    paintSelection();
  };
  table.onpointerup = () => (dragging = false);
  table.tBodies[0].oncontextmenu = (e) => {
    const at = cellOf(e.target);
    if (!at) return;
    const [ri, ci] = [at[0], Math.max(0, at[1])];
    const block = selection && selectedBlock(selection);
    const inside = block && ri >= block.r0 && ri <= block.r1 && ci >= block.c0 && ci <= block.c1;
    if (!inside) (selection = { r, anchor: [ri, ci], focus: [ri, ci], table: options?.table }), paintSelection();
    const column = r.columns[ci];
    const values = () => copyText(r.rows.map((row) => row[ci] ?? "NULL").join("\n"), `${r.rows.length} values of ${column}`);
    contextMenu(e, [
      [isMac ? "Copy  ⌘C" : "Copy  Ctrl+C", () => copySelection("plain")],
      ["Copy rows as CSV with header", () => copySelection("csv")],
      ["Copy rows as Markdown table", () => copySelection("markdown")],
      ...(options ? [["Copy rows as INSERT statements", () => copySelection("insert")] as MenuItem] : []),
      "-",
      [`Copy column name "${column}"`, () => copyText(column, "column name")],
      [`Copy all values of ${column} on this page`, values],
      ...(options?.menu?.(ri, ci) ?? []),
    ]);
  };
}

/** Whether keyboard focus is somewhere that has its own copy and select-all (inputs, the SQL editor). */
const typingSomewhere = () => {
  const a = document.activeElement as HTMLElement | null;
  return !!a && (a.isContentEditable || a.matches("input, textarea, select") || !!a.closest(".cm-editor"));
};

document.addEventListener("keydown", (e) => {
  if (!(e.metaKey || e.ctrlKey) || $("workspace").hidden || typingSomewhere() || document.querySelector("dialog[open]")) return;
  const key = e.key.toLowerCase();
  const tab = current();
  const rows = tab?.kind === "table" && tab.view === "data" ? tab : null;
  if (key === "z" && rows) (e.preventDefault(), e.shiftKey ? grid.redo(rows) : grid.undo(rows));
  else if (key === "backspace" && rows && selection?.r === rows.result) (e.preventDefault(), grid.deleteRows(rows, selectedRows()));
  else if (key === "c" && selection) (e.preventDefault(), copySelection("plain"));
  else if (key === "a" && $("out").querySelector("tbody td:not(.hint)")) {
    const r = selection?.r ?? (current() as { result?: QueryResult } | undefined)?.result;
    if (!r?.rows.length) return;
    e.preventDefault();
    const table = current()?.kind === "table" ? (current() as TableTab).table : undefined;
    selection = { r, anchor: [0, 0], focus: [r.rows.length - 1, r.columns.length - 1], table };
    paintSelection();
  }
});

// Space toggles the row detail; it's the grid's key only when nothing else has focus (a button would click).
document.addEventListener("keydown", (e) => {
  if (e.key !== " " || e.metaKey || e.ctrlKey || e.altKey || $("workspace").hidden || document.querySelector("dialog[open]")) return;
  const a = document.activeElement;
  if (current()?.kind !== "table" || (a && a !== document.body && !$("out").contains(a))) return;
  e.preventDefault();
  toggleRowDetail();
});

// ⌘V on the grid pastes tab-separated text into the selected cells.
document.addEventListener("paste", (e) => {
  const tab = current();
  if (typingSomewhere() || document.querySelector("dialog[open]") || tab?.kind !== "table" || tab.view !== "data") return;
  if (!selection || selection.r !== tab.result) return;
  e.preventDefault();
  grid.paste(tab, selectedBlock(selection), e.clipboardData?.getData("text/plain") ?? "");
});

function updateCursor() {
  const { line, column } = editor.cursor();
  $("cursor").textContent = `line ${line}, column ${column}`;
}

$("new-sql").onclick = () => (openSqlTab(), editor.focus());
for (const b of document.querySelectorAll<HTMLElement>(".side-tabs button")) b.onclick = () => showSide(b.dataset.side as Side);
$("history-search").addEventListener("input", renderHistory);
for (const b of $("view-seg").querySelectorAll<HTMLElement>("button"))
  b.onclick = async () => {
    const tab = current();
    if (tab?.kind !== "table" || tab.view === b.dataset.view || !(await okToDiscard(tab))) return;
    tab.view = b.dataset.view as TableTab["view"];
    if (tab.view === "structure") renderMain(); // the structure view loads its own
    else loadTable(tab);
  };
const turnPage = async (delta: number) => {
  const tab = current();
  if (tab?.kind !== "table" || !(await okToDiscard(tab))) return;
  tab.page = Math.max(0, tab.page + delta);
  loadTable(tab);
};
$("save-edits").onclick = () => saveEdits();
$("discard-edits").onclick = () => {
  const tab = current();
  if (tab?.kind === "table") (tab.staged.clear(), dropDraft(tab.id), renderMain());
};
$("toggle-filters").onclick = () => toggleFilters();
$("add-row").onclick = () => current()?.kind === "table" && grid.addRow(current() as TableTab);
$("preview-edits").onclick = () => current()?.kind === "table" && grid.preview(current() as TableTab);
grid.initGridEdit({
  call,
  conn: () => active,
  columns: (table) => columnsCache.get(table),
  render: renderMain,
  reload: (tab) => loadTable(tab as TableTab),
  confirm: confirmDialog,
  fill: fillCell,
});
$("side-items").oncontextmenu = (e) => active && contextMenu(e, [...sidebarMenu(), "-", ...treeMenu()]);
initSidebar({
  conn: () => active,
  tables: () => tables,
  call,
  tableRow,
  openTable: (t) => openTable(t),
  openFound(table, mode, text) {
    openTable(table);
    const tab = current();
    if (tab?.kind !== "table" || tab.table !== table) return;
    tab.filters = [{ ...blankFilter(), op: mode, value: text }];
    tab.filtersShown = true;
    filtersDrawnFor = 0;
    applyFilters(tab);
  },
  menu: contextMenu,
  redraw: () => renderTables(),
});
initSearch();
initStructure({
  call,
  conn: () => active,
  tables: () => tables,
  refreshTables: reloadItems,
  openStructure: (table, create) => openTable(table, "structure", create),
  forgetTable(table) {
    for (const t of tabs.filter((t): t is TableTab => t.kind === "table" && t.table === table)) {
      t.staged.clear();
      dropDraft(t.id);
      if (tabs.length === 1) openSqlTab(); // closing the last tab would leave the workspace
      closeTab(t.id, false);
    }
  },
  retarget(id, table) {
    const t = tabs.find((t): t is TableTab => t.id === id && t.kind === "table");
    if (!t) return;
    columnsCache.delete(t.table);
    Object.assign(t, { table, title: table, create: false, result: undefined, total: undefined });
    if (id === currentId) renderMain();
    renderTables();
  },
  openSql(sql) {
    const t = openSqlTab();
    editor.setState((t.state = editor.newState(sql)));
  },
  note,
  pendingChanged: () => current() && showPending(current()!),
});

// ---------- Import and export (importexport.ts) ----------

function transferCtx(): TransferCtx {
  return {
    conn: active!.name,
    database: info?.database ?? "",
    tables,
    columns: async (table) => (await loadColumns(table)).map((c) => c.name),
    confirm: confirmDialog,
    imported: async (table) => {
      // A new table appears in the sidebar; an open tab of the table shows the new rows.
      try {
        tables = (await call<Info>("connect", { name: active!.name })).tables;
        if (info) info.tables = tables;
        columnsCache.delete(table);
        renderTables();
        for (const t of tabs) if (t.kind === "table" && t.table === table) (t.total = undefined), loadTable(t);
      } catch {} // the import itself is done; a stale sidebar is all that's left
    },
  };
}

function transferMenu(table: string): MenuItem[] {
  return [
    ["Export…", () => exportDialog(transferCtx(), { kind: "tables", tables: [table] })],
    "-",
    ["Import from CSV…", () => importFile(transferCtx(), "csv", table)],
    ["Import from JSON…", () => importFile(transferCtx(), "json", table)],
    ["Import from SQL…", () => importFile(transferCtx(), "sql", table)],
  ];
}

$("export-table").onclick = () => {
  const tab = current();
  if (tab?.kind !== "table" || !active) return;
  const request = { table: tab.table, filters: activeFilters(tab), combine: tab.combine, sort: tab.sort, limit: PAGE, offset: tab.page * PAGE, count: false };
  const source: ExportSource = { kind: "rows", request, pageRows: tab.result?.rows.length ?? 0 };
  exportDialog(transferCtx(), source);
};
$("export-result").onclick = () => {
  const tab = current();
  if (tab?.kind !== "sql" || !active) return;
  if (!tab.ran || !tab.result?.columns.length) return status("Run a query first; its result is exported by running it again.", "error");
  exportDialog(transferCtx(), { kind: "query", sql: tab.ran });
};
$("columns-button").onclick = () => (menuClose ? menuClose() : showColumnsMenu());
$("page-prev").onclick = () => turnPage(-1);
$("page-next").onclick = () => turnPage(1);
/** Leaves the workspace and closes the server connection; the launcher comes back. */
async function closeWorkspace() {
  for (const t of allTabs()) if (!(await okToDiscard(t))) return;
  if (active) invoke("disconnect", { name: active.name }).catch(() => {});
  workspaces = workspaces.filter((w) => w !== ws);
  ws = null;
  active = null;
  info = null;
  tabs = [];
  shownId = 0;
  delete document.body.dataset.env;
  const next = workspaces.at(-1);
  if (next) showWorkspace(next);
  else show("home");
}

// ---------- Settings (⌘,) ----------

type Settings = {
  theme: "auto" | "light" | "dark"; zebra: boolean; fontSize: number; lineHeight: number; indent: "tab" | "2" | "4";
  /** Safe mode level for new connections. */
  safeLevel: Safety; queryTimeout: number; keepAlive: boolean;
  /** Most rows an editor query shows; 0 = no limit. */
  sqlLimit: number;
  formatStyle: FormatStyle;
  paramFormat: ParamFormat | "off";
  wrapLines: boolean;
  showInvisibles: boolean;
  /** Save each connection's SQL tabs and reopen them on connect. */
  keepTabs: boolean;
  /** Rebound shortcuts, command id → keys (see commands.ts); a missing id keeps its defaults. */
  shortcuts: Record<string, string[]>;
};
const DEFAULT_SETTINGS: Settings = {
  theme: "auto", zebra: true, fontSize: 13, lineHeight: 1.5, indent: "2", safeLevel: "alert2", queryTimeout: 300, keepAlive: true,
  sqlLimit: 1000, formatStyle: "standard", paramFormat: "colon", wrapLines: false, showInvisibles: false, keepTabs: true,
  shortcuts: {},
};
let settings: Settings = loadSettings();

// ponytail: settings live in the webview's localStorage (per machine); move to a file if they need syncing.
function loadSettings(): Settings {
  try {
    const stored = JSON.parse(localStorage.getItem("settings") ?? "{}");
    if (typeof stored.safeDefault === "boolean" && !stored.safeLevel) stored.safeLevel = stored.safeDefault ? "alert2" : "silent"; // before levels
    delete stored.safeDefault;
    return { ...DEFAULT_SETTINGS, ...stored };
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
  applyTheme(settings.theme);
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
  $<HTMLSelectElement>("safe-default").value = settings.safeLevel;
  $<HTMLInputElement>("query-timeout").value = String(settings.queryTimeout);
  $<HTMLInputElement>("keep-alive").checked = settings.keepAlive;
  $<HTMLSelectElement>("row-limit").value = String(settings.sqlLimit);
  $<HTMLSelectElement>("format-style").value = settings.formatStyle;
  $<HTMLSelectElement>("param-format").value = settings.paramFormat;
  $<HTMLInputElement>("wrap-lines").checked = settings.wrapLines;
  $<HTMLInputElement>("show-invisibles").checked = settings.showInvisibles;
  $<HTMLInputElement>("keep-tabs").checked = settings.keepTabs;
  editor.setDisplay(settings.wrapLines, settings.showInvisibles);
  setOverrides(settings.shortcuts);
  // The backend enforces these; outside the app (a plain browser preview) there's no backend to tell.
  invoke("set_preferences", { keepAlive: settings.keepAlive, queryTimeoutSecs: settings.queryTimeout, maxRows: settings.sqlLimit }).catch(() => {});
}

function showTab(tab: string) {
  if (tab === "shortcuts") renderShortcuts($("shortcuts-pane"), settings.shortcuts, (o) => setSetting("shortcuts", o));
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
$<HTMLInputElement>("query-timeout").onchange = (e) => setSetting("queryTimeout", clamp(Math.round(Number((e.target as HTMLInputElement).value) || 0), 0, 86400));
$<HTMLInputElement>("keep-alive").onchange = (e) => setSetting("keepAlive", (e.target as HTMLInputElement).checked);
fillLevels($("safe-default"));
fillLevels(field("safety"));
$<HTMLSelectElement>("safe-default").onchange = (e) => setSetting("safeLevel", (e.target as HTMLSelectElement).value as Safety);
$("settings-reset").onclick = () => {
  settings = { ...DEFAULT_SETTINGS };
  setSetting("theme", settings.theme); // persists and re-applies everything
  if (!$("shortcuts-pane").hidden) showTab("shortcuts");
};
$("settings-done").onclick = () => $<HTMLDialogElement>("settings").close();

showTab("general");
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

$("run").onclick = runCurrent;
$("run-all").onclick = runAll;
$("explain").onclick = async () => {
  const sql = editor.current();
  if (!sql.trim() || !active) return;
  const prefix = active.kind === "sqlite" ? "explain query plan " : "explain ";
  const filled = await withParams([prefix + sql]);
  if (filled) run(filled[0]);
};
$("format-sql").onclick = () => (formatSql(), editor.focus());
$<HTMLInputElement>("wrap-lines").onchange = (e) => setSetting("wrapLines", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("show-invisibles").onchange = (e) => setSetting("showInvisibles", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("keep-tabs").onchange = (e) => setSetting("keepTabs", (e.target as HTMLInputElement).checked);
$<HTMLSelectElement>("param-format").onchange = (e) => setSetting("paramFormat", (e.target as HTMLSelectElement).value as Settings["paramFormat"]);
$<HTMLSelectElement>("format-style").onchange = (e) => setSetting("formatStyle", (e.target as HTMLSelectElement).value as FormatStyle);
$<HTMLSelectElement>("row-limit").onchange = (e) => setSetting("sqlLimit", Number((e.target as HTMLSelectElement).value));

// ---------- Commands (commands.ts): shortcuts, and later the menu bar and Open Anything ----------

const inWorkspace = () => !$("workspace").hidden;
const onSqlTab = () => inWorkspace() && current()?.kind === "sql";
const fontBy = (size: number) => setSetting("fontSize", clamp(size, 9, 28));

addCommands([
  { id: "new-tab", label: "New Tab", menu: "File", keys: ["Mod+KeyT"], when: inWorkspace, run: () => (openSqlTab(), editor.focus()) },
  { id: "reopen-tab", label: "Reopen Closed Tab", menu: "File", keys: ["Mod+Shift+KeyT"], when: () => inWorkspace() && !!pane.closed.length,
    run: reopenClosedTab },
  { id: "open-file", label: "Open…", menu: "File", keys: ["Mod+KeyO"], when: inWorkspace, run: openSqlFile },
  { id: "save", label: "Save", menu: "File", keys: ["Mod+KeyS"], when: inWorkspace, sep: true,
    run: () => (current()?.kind === "sql" ? saveSqlFile(false) : saveEdits()) },
  { id: "save-as", label: "Save As…", menu: "File", keys: ["Mod+Shift+KeyS"], when: inWorkspace,
    run: () => (current()?.kind === "sql" ? saveSqlFile(true) : saveEdits()) },
  // On the launcher there's no tab: close the window, like other apps.
  { id: "close-tab", label: "Close Tab", menu: "File", keys: ["Mod+KeyW"], sep: true,
    run: () => (inWorkspace() ? closeTab(currentId) : getCurrentWindow().close()) },
  { id: "close-other-tabs", label: "Close Other Tabs", menu: "File", when: () => inWorkspace() && tabs.length > 1,
    run: () => closeOtherTabs(currentId) },
  { id: "rename-tab", label: "Rename Tab…", menu: "File", when: inWorkspace, run: () => renameTab(currentId) },
  { id: "settings", label: "Settings…", menu: "File", keys: ["Mod+Comma"], overDialogs: true, sep: true, run: openSettings },
  { id: "run-current", label: "Run Current Query", menu: "Edit", keys: ["Mod+Enter"], when: onSqlTab, sep: true, run: runCurrent },
  { id: "run-all", label: "Run All Queries", menu: "Edit", keys: ["Mod+Shift+Enter"], when: onSqlTab, run: runAll },
  { id: "format", label: "Format SQL", menu: "Edit", keys: ["Mod+KeyI"], when: onSqlTab, run: formatSql },
  { id: "filter", label: "Filter Rows", menu: "Edit", keys: ["Mod+KeyF"], when: () => inWorkspace() && current()?.kind === "table",
    run: () => toggleFilters(true) },
  { id: "font-bigger", label: "Increase Font Size", menu: "View", keys: ["Mod+Equal", "Mod+Shift+Equal"], when: inWorkspace,
    run: () => fontBy(settings.fontSize + 1) },
  { id: "font-smaller", label: "Decrease Font Size", menu: "View", keys: ["Mod+Minus"], when: inWorkspace, run: () => fontBy(settings.fontSize - 1) },
  { id: "font-reset", label: "Actual Font Size", menu: "View", keys: ["Mod+Digit0"], when: inWorkspace, run: () => fontBy(DEFAULT_SETTINGS.fontSize) },
  { id: "reload-tab", label: "Reload Current Tab", menu: "View", keys: ["Mod+KeyR"], when: inWorkspace, sep: true, run: reloadTab },
  { id: "open-anything", label: "Open Anything", menu: "Navigate", keys: ["Mod+KeyP"], when: inWorkspace,
    run: () => openAnything(anythingItems()) },
  { id: "find-items", label: "Find Items in Sidebar…", menu: "Navigate", keys: ["Mod+Shift+KeyF"], when: inWorkspace,
    run: () => (showSide("items"), $("filter").focus()) },
  { id: "search-database", label: "Search in Database…", menu: "Navigate", when: inWorkspace, run: () => showSide("search") },
  { id: "go-back", label: "Go Back", menu: "Navigate", keys: ["Ctrl+Mod+ArrowLeft"], when: inWorkspace, run: () => goBack(false) },
  { id: "go-forward", label: "Go Forward", menu: "Navigate", keys: ["Ctrl+Mod+ArrowRight"], when: inWorkspace, run: () => goBack(true) },
  { id: "next-tab", label: "Select Next Tab", menu: "Window", keys: ["Mod+Shift+BracketRight", "Ctrl+Tab"], when: inWorkspace,
    run: () => cycleTab(1) },
  { id: "previous-tab", label: "Select Previous Tab", menu: "Window", keys: ["Mod+Shift+BracketLeft", "Ctrl+Shift+Tab"],
    when: inWorkspace, run: () => cycleTab(-1) },
  ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => ({
    id: `tab-${n}`, label: n === 9 ? "Last Tab" : `Tab ${n}`, menu: "Window" as const, keys: [`Mod+Digit${n}`],
    when: inWorkspace, run: () => selectTabAt(n), sep: n === 1, hidden: true,
  })),
  { id: "open-database", label: "Open a Database…", menu: "Connection", keys: ["Mod+KeyK"], when: inWorkspace, run: openDbPicker },
  { id: "reconnect", label: "Reconnect", menu: "Connection", keys: ["Mod+Shift+KeyR"], when: inWorkspace, run: reconnect },
  { id: "reconnect-all", label: "Reconnect All Workspaces", menu: "Connection", when: () => workspaces.length > 0, run: reconnectAll },
  { id: "disconnect", label: "Disconnect", menu: "Connection", keys: ["Alt+Mod+Period"], when: inWorkspace, sep: true, run: closeWorkspace },
  { id: "new-workspace", label: "New Workspace", menu: "File", keys: ["Mod+KeyN"], when: inWorkspace, run: newWorkspace },
  { id: "reload-workspace", label: "Reload Workspace", menu: "View", keys: ["Alt+Mod+KeyR"], when: inWorkspace,
    run: async () => (await reloadItems(), current()?.kind === "table" && reloadTab()) },
  { id: "split-right", label: "Split Pane Right", menu: "Window", keys: ["Mod+Shift+KeyD"], when: inWorkspace, sep: true, run: splitRight },
  { id: "next-pane", label: "Select Next Pane", menu: "Window", keys: ["Alt+Mod+BracketRight"], when: () => (ws?.panes.length ?? 0) > 1,
    run: () => cyclePane(1) },
  { id: "previous-pane", label: "Select Previous Pane", menu: "Window", keys: ["Alt+Mod+BracketLeft"], when: () => (ws?.panes.length ?? 0) > 1,
    run: () => cyclePane(-1) },
  { id: "show-workspaces", label: "Show All Workspaces", menu: "Window", when: inWorkspace, sep: true, run: showWorkspaces },
  { id: "next-workspace", label: "Select Next Workspace", menu: "Window", keys: ["Ctrl+Mod+BracketRight"], when: () => workspaces.length > 1,
    run: () => cycleWorkspace(1) },
  { id: "previous-workspace", label: "Select Previous Workspace", menu: "Window", keys: ["Ctrl+Mod+BracketLeft"],
    when: () => workspaces.length > 1, run: () => cycleWorkspace(-1) },
]);

const ITEM_KINDS = { schema: "schema", view: "view", matview: "materialized view", function: "function", procedure: "procedure", sequence: "sequence", type: "type" };

/** What Open Anything searches: the workspace's items and SQL tabs, the other workspaces, and every command that
 *  applies right now. */
function anythingItems(): AnythingItem[] {
  const sideItems = active ? loadedItems(active.name) : [];
  const kinds = new Set(sideItems.map((i) => i.name));
  const sqlTabs = (ws?.panes ?? [pane]).flatMap((p) =>
    (p === pane ? tabs : p.tabs).flatMap((t) => (t.kind === "sql" ? [{ p, t }] : [])),
  );
  return [
    ...tables.filter((t) => !kinds.has(t)).map((t) => ({ label: t, kind: "table", run: () => openTable(t) })),
    ...sideItems.filter((i) => i.kind !== "schema").map((i) => ({
      label: i.name, kind: ITEM_KINDS[i.kind],
      run: () => (i.kind === "view" || i.kind === "matview" ? openTable(i.name) : navigator.clipboard.writeText(i.name).then(() => note(`Copied ${i.name}`, "done"), () => {})),
    })),
    ...sqlTabs.map(({ p, t }) => ({ label: t.title, kind: "SQL tab", detail: t.path, run: () => (focusPane(p), select(t.id)) })),
    ...workspaces.filter((w) => w !== ws).map((w) => ({ label: w.conn.name, kind: "workspace", run: () => showWorkspace(w) })),
    ...commands
      .filter((c) => c.id !== "open-anything" && !c.hidden && (!c.when || c.when()))
      .map((c) => ({ label: c.label.replace(/…$/, ""), kind: "command", detail: keyLabel(c.id), run: c.run })),
  ];
}

const hasTools = () => inWorkspace() && !!active;
addCommands([
  { id: "backup", label: "Backup…", menu: "Tools", when: hasTools, run: () => openBackup(active!, info?.database) },
  { id: "restore", label: "Restore…", menu: "Tools", when: hasTools, run: () => openRestore(active!, info?.database, confirmDialog) },
  { id: "processes", label: "Process List", menu: "Tools", keys: ["Mod+Period"], when: () => hasTools() && active!.kind !== "sqlite",
    sep: true, run: () => openProcesses(active!, confirmDialog) },
  { id: "shortcuts", label: "Keyboard Shortcuts", menu: "Help", overDialogs: true, run: () => (openSettings(), showTab("shortcuts")) },
]);
initMenu();

/** Shortcut hints next to buttons, kept in step with Settings › Shortcuts. */
function showKeys() {
  $("keys").textContent = keyLabel("run-current");
  $("keys-all").textContent = keyLabel("run-all");
  $("keys-format").textContent = keyLabel("format");
  $("reconnect-key").textContent = keyLabel("reconnect");
  $("font-keys").textContent = ["font-bigger", "font-smaller", "font-reset"].map(keyLabel).filter(Boolean).join(" / ");
  $("toggle-filters").title = `Filter rows (${keyLabel("filter")})`;
  $("save-edits").title = `Save changes (${keyLabel("save")})`;
}
onBindingsChanged(showKeys);
showKeys();

addEventListener("connections-moved", refresh); // Settings › Locations
show("home");
refresh();
