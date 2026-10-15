import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import type { EditorState } from "@codemirror/state";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { createEditor, type CompleteKey, type FormatStyle, type Schema } from "./editor";
import { fillParams, findParams, sourceTable, type ParamFormat } from "./statements";
import { $, el } from "./dom";
import { OPS, activeFilters, blankFilter, renderFilters, setFilterDefaults, type DefaultColumn, type FilterRow, type Op } from "./filters";
import { csv, inserts, markdown, plain, type Dialect } from "./copy";
import { MIN_WIDTH, arrange, layoutCss, loadLayout, moveColumn, saveLayout, type Layout } from "./columns";
import {
  dropDatabase, dropDraft, initStructure, newDatabase, renameDatabase, saveStructure, showSql, showStructure, sidebarMenu,
  structurePending, tableMenu,
} from "./structure";
import { openBackup, openRestore } from "./backup";
import { openProcesses } from "./processes";
import { openUsers } from "./users";
import "./mcp"; // adds Settings › MCP before the settings tabs are wired below
import { Staged } from "./staging";
import * as grid from "./grid-edit";
import * as vgrid from "./vgrid";
import { syncRowDetail, toggleRowDetail } from "./rowdetail";
import { applyTheme } from "./themes"; // also adds the theme rows to Settings › Appearance
import "./passcode"; // the lock screen, and its rows in Settings › Security
import { docEntry, docsReady, setDocEntry } from "./locations"; // also adds Settings › Locations
import { CANCELLED, NOT_APPROVED, approval, fillLevels, guarded, isSafetyStop, showSafety, type Safety } from "./safety";
import { exportDialog, importFile, type Ctx as TransferCtx, type ExportSource } from "./importexport";
import { addCommands, bindings, commands, isMac, onBindingsChanged, setOverrides } from "./commands";
import { renderShortcuts } from "./shortcuts";
import { openAnything, type Item as AnythingItem } from "./openanything";
import { initMenu, setRecent } from "./menu";
import {
  columnOrderMenu, forgetItems, initSearch, initSidebar, loadedItems, newItemsMenu, renderTree, shownSchema, sortColumns, treeMenu,
} from "./sidebar";
import type { Transform } from "./transforms";
import { initDiagram, openDiagram } from "./diagram";
import { label } from "./keys";
import * as launcher from "./connections";
import { COLORS, GROUP_ICONS, tagColor, tagLabel } from "./connections";
import "./console"; // the console log panel and its command
import { addFavorite, initFavorites, render as renderFavorites } from "./favorites"; // also adds the sidebar's Queries tab
import { ALL, MESSAGES, renderMessages, renderStrip, rowsLabel, searchRows, type QueryResult, type ResultTab, type Results } from "./results";
import "./workspace.css";
import { chartPanel } from "./charts";
import { guessSpec } from "./chartdata";
import { openBoard, type BoardCtx } from "./metrics";
import { readPref, writePref } from "./prefs";

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
type Saved = {
  name: string; kind: string; url: string; tag: string; safety: Safety; ssh?: Ssh; tls?: Tls; options?: Options;
  password_mode?: "keychain" | "ask" | "no_password"; color?: string | null; group?: string;
};
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
  { kind: "postgres", label: "PostgreSQL", mono: "Pg", color: "var(--blue)",
    defaults: { host: "127.0.0.1", port: "5432", user: "postgres", database: "" } },
  { kind: "mysql", label: "MySQL", mono: "Ms", color: "var(--orange)",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { kind: "mariadb", label: "MariaDB", mono: "Ma", color: "var(--pink)",
    defaults: { host: "127.0.0.1", port: "3306", user: "root", database: "" } },
  { kind: "sqlite", label: "SQLite", mono: "Sl", color: "var(--yellow)" },
];
const engine = (kind: string) => ENGINES.find((e) => e.kind === kind) ?? ENGINES[0];

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
  if (!tag) return "";
  const pill = el("span", { className: `tag ${tag}`, textContent: tagLabel(tag) });
  const color = tagColor(tag);
  if (color) pill.classList.add("colored"), pill.style.setProperty("--tag", color);
  return pill;
}
/** The connection's status color as a small dot; "" when it has none. */
function colorDot(c: Saved) {
  const color = c.color && COLORS[c.color];
  if (!color) return "";
  const dot = el("span", { className: "color-dot", title: `${c.color} connection` });
  dot.style.background = color;
  return dot;
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
    const all = await invoke<launcher.Launcher<Saved>>("launcher");
    saved = all.connections;
    launcher.setLauncher(all);
    setStatus("home-status", "");
  } catch (e) {
    setStatus("home-status", String(e), "error"); // e.g. an unreadable connections file, which is left untouched
  }
  renderConns();
}

/** Reloads the list, then says what happened (refresh clears the status line). */
const homeNote = async (text: string, error = false) => (await refresh(), setStatus("home-status", text, error ? "error" : "done"));
const collapsed = new Set<string>(); // group names folded in the launcher, for this session

/** The launcher list: Recent first (when not searching), then each group, then the ungrouped connections. */
function renderConns() {
  const q = $<HTMLInputElement>("search").value.toLowerCase();
  const shown = saved.filter((c) => [c.name, c.kind, c.tag, c.group ?? "", hostOf(c)].join(" ").toLowerCase().includes(q));
  const by = settings.launcherSort;
  const key = (c: Saved) => (by === "kind" ? engine(c.kind).label : by === "tag" ? c.tag || "\uffff" : "");
  shown.sort((a, b) => key(a).localeCompare(key(b)) || a.name.localeCompare(b.name));
  $("no-conns").hidden = saved.length > 0;
  const sections: HTMLElement[] = [];
  const recent = q ? [] : launcher.recent().flatMap((n) => saved.filter((c) => c.name === n)).slice(0, 5);
  if (recent.length && saved.length > 5) sections.push(section({ name: "", icon: "" }, "Recent", recent, false));
  for (const g of launcher.groups) sections.push(section(g, g.name, shown.filter((c) => c.group === g.name), true));
  const loose = shown.filter((c) => !c.group || !launcher.groups.some((g) => g.name === c.group));
  if (loose.length || launcher.groups.length) sections.push(section({ name: "", icon: "" }, launcher.groups.length ? "Ungrouped" : "", loose, true));
  $("conns").replaceChildren(...sections);
}

/** One heading (a group's is a drop target and has its own menu) and its connections. */
function section(g: launcher.Group, title: string, conns: Saved[], dropTarget: boolean) {
  const items = collapsed.has(title) ? [] : conns.map(connRow);
  const out = el("li", { className: "conn-section" });
  if (title) {
    const head = el("div", { className: "conn-group", tabIndex: 0 },
      el("span", { className: "fold", textContent: collapsed.has(title) ? "▸" : "▾" }),
      g.icon ? el("span", { textContent: GROUP_ICONS[g.icon] ?? "" }) : "",
      el("span", { className: "group-name", textContent: title }),
      el("span", { className: "count", textContent: String(conns.length) }));
    const fold = () => (collapsed.has(title) ? collapsed.delete(title) : collapsed.add(title), renderConns());
    head.onclick = fold;
    head.onkeydown = (e) => e.key === "Enter" && fold();
    if (g.name) head.oncontextmenu = (e) => contextMenu(e, groupMenu(g));
    if (dropTarget) head.dataset.group = g.name;
    out.append(head);
  }
  out.append(el("ul", { className: "conns" }, ...items));
  return out;
}

function groupMenu(g: launcher.Group): MenuItem[] {
  return [
    ["Edit Group…", async () => (await launcher.editGroup(g)) && refresh()],
    ["Export Group…", () => launcher.exportConnections({ kind: "group", name: g.name }, `the group “${g.name}”`, homeNote)],
    "-",
    ["Delete Group", async () => {
      if (!(await confirmDialog(`Delete the group "${g.name}"? Its connections stay, ungrouped.`, "Delete"))) return;
      await invoke("delete_group", { name: g.name }).catch((e) => homeNote(String(e), true));
      refresh();
    }],
  ];
}

function connRow(c: Saved) {
  const edit = el("button", { className: "icon", textContent: "Edit", title: `Edit ${c.name}` });
  const del = el("button", { className: "icon", textContent: "Delete", title: `Delete ${c.name}` });
  edit.onclick = (e) => (e.stopPropagation(), openEditor(c.kind, c));
  del.onclick = (e) => (e.stopPropagation(), deleteConn(c));
  const li = el(
    "li",
    { tabIndex: 0 },
    avatar(c.kind),
    el("div", { className: "who" }, el("div", { className: "name" }, el("span", { textContent: c.name }), tagPill(c.tag)), el("div", { className: "host", textContent: hostOf(c) })),
    el("div", { className: "actions" }, edit, del),
  );
  if (c.color) li.style.setProperty("--conn-color", COLORS[c.color] ?? "transparent"), li.classList.add("colored");
  li.onclick = () => !dragged && connect(c);
  li.oncontextmenu = (e) => contextMenu(e, connMenu(c));
  li.onkeydown = (e) => e.key === "Enter" && connect(c);
  li.onpointerdown = (e) => e.button === 0 && dragToGroup(e, c);
  return li;
}

async function deleteConn(c: Saved) {
  if (!(await confirmDialog(`Delete "${c.name}"? Its keychain password is removed too.`, "Delete"))) return;
  await invoke("delete_connection", { name: c.name });
  refresh();
}

const moveTo = (c: Saved, group: string) => invoke("move_to_group", { names: [c.name], group }).then(refresh, (e) => homeNote(String(e), true));

/** A launcher row's right-click: connection actions, Move to group, then the tools. */
function connMenu(c: Saved): MenuItem[] {
  const groupItems: MenuItem[] = launcher.groups.filter((g) => g.name !== c.group).map((g) => [`Move to “${g.name}”`, () => moveTo(c, g.name)]);
  if (c.group) groupItems.push(["Remove from Group", () => moveTo(c, "")]);
  return [
    ["Connect", () => connect(c)],
    ["Edit…", () => openEditor(c.kind, c)],
    ["Duplicate", () => invoke<string>("duplicate_connection", { name: c.name }).then((n) => homeNote(`Duplicated as “${n}”.`), (e) => homeNote(String(e), true))],
    ["Copy as URL", () => navigator.clipboard.writeText(launcher.connectionUrl(c)).then(() => homeNote("Copied the URL (without the password).")).catch((e) => homeNote(String(e), true))],
    ["Export…", () => launcher.exportConnections({ kind: "connection", name: c.name }, `“${c.name}”`, homeNote)],
    ...(groupItems.length ? ["-" as const, ...groupItems] : []),
    "-",
    ...toolsMenu(c),
    "-",
    ["Delete…", () => deleteConn(c)],
  ];
}

let dragged = false; // set while a drag ends, so the row's click doesn't connect
/** Drags a row onto a group heading (or Ungrouped) to move it there. Pointer events rather than HTML drag and drop,
 *  which the app window's own file drop handling gets in the way of. */
function dragToGroup(start: PointerEvent, c: Saved) {
  let ghost: HTMLElement | null = null;
  let over: HTMLElement | null = null;
  const move = (e: PointerEvent) => {
    if (!ghost && Math.hypot(e.clientX - start.clientX, e.clientY - start.clientY) < 6) return;
    ghost ??= document.body.appendChild(el("div", { className: "drag-ghost", textContent: c.name }));
    Object.assign(ghost.style, { left: `${e.clientX + 8}px`, top: `${e.clientY + 8}px` });
    over?.classList.remove("drop");
    over = (document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null)?.closest<HTMLElement>("[data-group]") ?? null;
    over?.classList.add("drop");
  };
  const up = () => {
    removeEventListener("pointermove", move);
    removeEventListener("pointerup", up);
    if (!ghost) return;
    ghost.remove();
    over?.classList.remove("drop");
    dragged = true;
    setTimeout(() => (dragged = false));
    if (over && over.dataset.group !== (c.group ?? "")) moveTo(c, over.dataset.group!);
  };
  addEventListener("pointermove", move);
  addEventListener("pointerup", up);
}

/** The launcher's ⋯ menu. */
function launcherMenu(): MenuItem[] {
  return [
    ["New Connection…", openPicker],
    ["New Group…", async () => (await launcher.editGroup()) && refresh()],
    ["Manage Tags…", () => launcher.manageTags(refresh)],
    "-",
    ["Import from URL…", importFromUrl],
    ["Import Connections…", () => launcher.importConnections(homeNote)],
    ["Export All Connections…", () => launcher.exportConnections({ kind: "all" }, "all connections", homeNote)],
  ];
}
$("launcher-menu").onclick = () => {
  const r = $("launcher-menu").getBoundingClientRect();
  contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), launcherMenu());
};

/** Opens a new connection's form filled from a URL or SQLite path; saving it is up to the user. */
async function importFromUrl(text?: string) {
  let error = "";
  for (;;) {
    const t = text ?? (await launcher.prompt({
      title: "Import from URL", text: "Paste a connection URL or a SQLite file path.", ok: "Import", error,
      placeholder: "postgresql://user@localhost:5432/database",
    }));
    if (t === null) return;
    try {
      return openFromUrl(launcher.parseConnectionUrl(t));
    } catch (e) {
      if (text !== undefined) return homeNote(String(e), true); // from Finder or a link: nothing to retype
      error = String(e);
    }
  }
}

function openFromUrl(u: ReturnType<typeof launcher.parseConnectionUrl>) {
  const c: Saved = { name: u.name, kind: u.kind, url: u.url, tag: "", safety: settings.safeLevel };
  if (u.tls) c.tls = { ...TLS_DEFAULT, mode: u.tls };
  openEditor(u.kind, c, true, u.password);
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

/** The form for a new connection, or for editing `existing`; `fresh` fills it from `existing` as a new one. */
function openEditor(kind: string, existing?: Saved, fresh = false, password = "") {
  const e = engine(kind);
  editing = fresh ? null : existing ?? null;
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
  field("password").placeholder = editing ? "Unchanged (in keychain)" : "None";
  field("passphrase").placeholder = editing ? "Unchanged (in keychain), if any" : "Only for SQLCipher-encrypted files";
  field("tag").replaceChildren(el("option", { value: "", textContent: "None" }), ...launcher.tags.map((t) => el("option", { value: t.name, textContent: tagLabel(t.name) })));
  field("group").replaceChildren(el("option", { value: "", textContent: "None" }), ...launcher.groups.map((g) => el("option", { value: g.name, textContent: g.name })));
  field("password_mode").value = existing?.password_mode ?? "keychain";
  field("color").value = existing?.color ?? "";
  if (existing?.tag && !launcher.tags.some((t) => t.name === existing.tag)) field("tag").append(el("option", { value: existing.tag, textContent: existing.tag }));
  showPasswordMode();
  field("ssh_password").placeholder = existing?.ssh ? "Unchanged (in keychain)" : "None";
  field("safety").value = settings.safeLevel; // overwritten below when editing

  if (existing) {
    field("name").value = existing.name;
    field("tag").value = existing.tag;
    field("group").value = existing.group ?? "";
    field(kind === "sqlite" ? "passphrase" : "password").value = password;
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

$("swatches").replaceChildren(...[["", "None"], ...Object.keys(COLORS).map((k) => [k, k])].map(([value, title]) => {
  const input = el("input", { type: "radio", name: "color", value, title, ariaLabel: title });
  if (value) input.style.setProperty("--swatch", COLORS[value]);
  return input;
}));

/** The password field says where a typed password goes. */
function showPasswordMode() {
  const mode = field("password_mode").value;
  field("password").placeholder = mode === "ask" ? "Asked when connecting (used here for Test)" : mode === "no_password" ? "None" : editing ? "Unchanged (in keychain)" : "None";
}
field("password_mode").addEventListener("change", showPasswordMode);
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
  // Defaults are left out, as the backend writes them.
  const mode = editingKind === "sqlite" ? "keychain" : (field("password_mode").value as Saved["password_mode"]);
  if (mode !== "keychain") conn.password_mode = mode;
  if (field("color").value) conn.color = field("color").value;
  if (field("group").value) conn.group = field("group").value;
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
    password: conn.password_mode ? null : secretField().value || null,
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
  renderDbTabs();
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
    if (e === "DISCONNECTED") ($("lost").hidden = false), renderDbTabs();
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
    info = await invoke<Info>("reconnect", { name: sessionKey() });
    $("lost").hidden = true;
    renderDbTabs();
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

// Keep-alive found a connection dead: show the banner (or the mark on its tab) now rather than on the next query.
listen<string>("connection-lost", (e) => {
  const w = workspaces.find((w) => w.key === e.payload);
  if (!w) return; // the MCP server's own session, or one closed meanwhile
  w.lost = true;
  if (w === ws) $("lost").hidden = false;
  renderWorkspaceButton();
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
    const i = await call<{ server: string; driver: string; cipher: string | null; ssh: string | null }>("connection_info", { name: sessionKey() });
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
    await openWorkspace(c);
    setStatus("home-status", "");
  } catch (e) {
    setStatus("home-status", String(e), "error");
  }
}

/** A session key no open workspace uses: the connection's name, else name + "\u001f" + n (see `connName`). */
function freeKey(name: string) {
  const taken = new Set(workspaces.map((w) => w.key));
  let key = name;
  for (let n = 2; taken.has(key); n++) key = `${name}\u001f${n}`;
  return key;
}

/** A new workspace on its own session of `c`, on `database` when given, in the tab after the current one. */
async function openWorkspace(c: Saved, database?: string) {
  await docsReady; // kept tabs and history, read at startup
  const key = freeKey(c.name);
  const i = await launcher.withAskedPassword(c.name, () => invoke<Info>("connect", { name: key, database }));
  if (!i) return; // the password prompt was cancelled
  launcher.addRecent(c.name);
  const at = ws && workspaces.includes(ws) ? workspaces.indexOf(ws) + 1 : workspaces.length;
  stashWorkspace();
  pane = newPane();
  ws = { key, conn: c, info: i, tables: i.tables, panes: [pane], pane, columns: new Map(), expanded: new Set(), lost: false, filter: "" };
  workspaces.splice(at, 0, ws);
  rememberOpen();
  active = c;
  columnsCache = ws.columns;
  expanded = ws.expanded;
  $("lost").hidden = true;
  showHeader();
  show("workspace");
  applyInfo(i);
  showSide("items");
  renderHistory();
  renderFavorites();
  showSafety(c);
  status(`Connected · ${i.server}`, "done");
  // Connected without a database: go straight to choosing one.
  if (i.can_switch && !i.database) openDbPicker();
  else editor.focus();
}

/** A new connection or database: fresh tree, and a single empty SQL tab. */
function applyInfo(i: Info) {
  info = i;
  tables = i.tables;
  columnsCache.clear();
  expanded.clear();
  $<HTMLInputElement>("filter").value = "";
  forgetItems(sessionKey());
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
  $("where-conn").replaceChildren(avatar(c.kind), colorDot(c), el("span", { textContent: c.name }), el("span", { className: "host", textContent: hostOf(c) }));
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
  /** The backend's handle on this workspace's own session (`name` in every command); see `freeKey`. */
  key: string;
  conn: Saved; info: Info | null; tables: string[];
  /** Side by side; `pane` is the one with focus. */
  panes: Pane[]; pane: Pane;
  columns: Map<string, Column[]>; expanded: Set<string>; lost: boolean; filter: string;
};
let workspaces: Workspace[] = [];
let ws: Workspace | null = null; // the one on screen (or last on screen, while the launcher shows)
const sessionKey = () => ws?.key ?? "";

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
  renderFavorites();
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
      w.info = await invoke<Info>("reconnect", { name: w.key });
      Object.assign(w, { tables: w.info.tables, columns: new Map(), lost: false });
      forgetItems(w.key);
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
  renderDbTabs();
}

// ---------- Workspace tabs: one per open workspace (a connection, or another database of one), across the top ----------

let draggedTab: Workspace | null = null;

/** Each tab shows its connection's tag colour, its database (or the connection, without one), a mark when its
 *  connection was lost and × to close; drag one to reorder. The order is also ⌃⌘[ / ⌃⌘]'s. While the launcher shows
 *  (⌘N, or +) it has a tab of its own, like a browser's new tab, so the open workspaces stay one click away. */
function renderDbTabs() {
  const strip = $("db-tabs");
  strip.hidden = !workspaces.length;
  if (strip.hidden) return;
  const home = !$("home").hidden;
  const launcherTab = () => {
    const close = el("span", { className: "close", textContent: "×", title: "Back to the workspace" });
    close.onclick = (e) => (e.stopPropagation(), ws && showWorkspace(ws));
    const b = el("button", { className: "db-tab new-ws", title: "Connect to open another workspace" },
      el("span", { className: "dot" }), el("span", { className: "label", textContent: "New workspace" }), close);
    b.setAttribute("role", "tab");
    b.setAttribute("aria-selected", "true");
    return b;
  };
  const add = el("button", { className: "add", textContent: "+", title: `New workspace (${keyLabel("new-workspace")})` });
  add.onclick = newWorkspace;
  strip.replaceChildren(
    ...workspaces.map((w) => {
      const database = (w === ws ? info : w.info)?.database;
      const lost = w === ws ? !$("lost").hidden : w.lost;
      const close = el("span", { className: "close", textContent: "×", title: "Close tab" });
      close.onclick = (e) => (e.stopPropagation(), closeDbTab(w));
      const b = el("button", {
        className: `db-tab ${w.conn.tag}`, draggable: true,
        title: `${w.conn.name}${database ? ` · ${database}` : ""}${lost ? " (connection lost)" : ""}`,
      },
        el("span", { className: "dot" }),
        el("span", { className: "label", textContent: database ?? w.conn.name }),
        ...(database ? [el("span", { className: "conn", textContent: w.conn.name })] : []),
        ...(lost ? [el("span", { className: "lost-mark", textContent: "!", title: "Connection lost" })] : []),
        close,
      );
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(!home && w === ws));
      b.onclick = () => (home || w !== ws) && showWorkspace(w);
      b.onauxclick = (e) => e.button === 1 && closeDbTab(w); // middle-click closes
      b.ondragstart = (e) => ((draggedTab = w), e.dataTransfer?.setData("text/plain", w.conn.name));
      b.ondragend = () => (draggedTab = null);
      b.ondragover = (e) => draggedTab && draggedTab !== w && e.preventDefault();
      b.ondrop = (e) => {
        e.preventDefault();
        if (!draggedTab || draggedTab === w) return;
        const r = b.getBoundingClientRect();
        workspaces.splice(workspaces.indexOf(draggedTab), 1);
        workspaces.splice(workspaces.indexOf(w) + (e.clientX > r.left + r.width / 2 ? 1 : 0), 0, draggedTab);
        draggedTab = null;
        renderDbTabs();
      };
      return b;
    }),
    ...(home ? [launcherTab()] : []),
    add,
  );
  strip.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Closes a workspace tab; closing one in the background comes back to the tab on screen (or the launcher). */
async function closeDbTab(w: Workspace) {
  const home = !$("home").hidden;
  if (w === ws && !home) return closeWorkspace();
  const back = home ? null : ws;
  showWorkspace(w); // its tabs may have unsaved edits to ask about
  await closeWorkspace(back);
  if (home && workspaces.length) newWorkspace();
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
    ["Close Workspace", () => closeWorkspace()],
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
  // The grid draws only the rows in view (vgrid.ts): the copy must sit at the same scroll to show them.
  const out = shot.querySelector<HTMLElement>('[data-id="out"]');
  if (out) out.dataset.scroll = String($("out").scrollTop);
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
  for (const out of $("panes").querySelectorAll<HTMLElement>('.shot [data-id="out"]')) out.scrollTop = Number(out.dataset.scroll ?? 0);
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

// ponytail: only a connection's first workspace keeps its SQL tabs; key them by database if the others should.
const keepsTabs = () => !!active && sessionKey() === active.name;

function loadSqlTabs(): KeptTab[] {
  const kept = keepsTabs() && docEntry<KeptTab[]>("sql_tabs", active!.name);
  return Array.isArray(kept) ? kept.filter((k) => typeof k?.title === "string" && typeof k?.sql === "string") : [];
}

function saveSqlTabs() {
  if (!active || !keepsTabs() || !settings.keepTabs) return;
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
  databases = [];
  renderDbs();
  if (!d.open) d.showModal();
  $("db-search").focus();
  await loadDbs(info?.database ?? "");
}

/** Fills the list, with `pick` selected when it's there. */
async function loadDbs(pick: string) {
  setStatus("db-status", "Loading…", "busy");
  try {
    databases = await call<string[]>("list_databases", { name: sessionKey() });
    $<HTMLInputElement>("db-search").value = "";
    dbPick = Math.max(0, databases.indexOf(pick));
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
      li.ondblclick = (e) => (e.altKey ? openDb(name) : openDbInNewTab(name));
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
    applyInfo(await call<Info>("use_database", { name: sessionKey(), database: name }));
    $<HTMLDialogElement>("dbpicker").close();
    status(`${name} · ${tables.length} tables`, "done");
    editor.focus();
  } catch (e) {
    setStatus("db-status", String(e), "error");
  }
}

/** Opens the database in a new tab: its own session of the connection, sidebar and item tabs. Its tab comes forward
 *  when it's open already; the database on screen, or a connection still without one, opens in place. */
async function openDbInNewTab(name = shownDbs()[dbPick]) {
  if (!name || !active) return;
  if (name === info?.database) return $<HTMLDialogElement>("dbpicker").close();
  if (!info?.database) return openDb(name);
  const conn = active.name;
  const open = workspaces.find((w) => w.conn.name === conn && (w === ws ? info : w.info)?.database === name);
  if (open) return $<HTMLDialogElement>("dbpicker").close(), showWorkspace(open);
  setStatus("db-status", `Opening ${name}…`, "busy");
  try {
    await openWorkspace(active, name);
    $<HTMLDialogElement>("dbpicker").close();
  } catch (e) {
    setStatus("db-status", String(e), "error");
  }
}

/** Runs a create, rename or drop from the window, then lists the databases again. */
async function databaseOp(run: () => Promise<string | boolean | null>, after: (done: string | boolean) => Promise<void> | void) {
  setStatus("db-status", "");
  try {
    const done = await run();
    if (done) await after(done);
  } catch (e) {
    setStatus("db-status", String(e), "error");
  }
}

$("db-new").onclick = () => databaseOp(newDatabase, (name) => loadDbs(String(name)));
$("db-rename").onclick = () => {
  const db = shownDbs()[dbPick];
  if (db) databaseOp(() => renameDatabase(db), async (to) => {
    // MySQL moved the tables out from under this tab; follow them. (Postgres can't rename the database in use.)
    if (db === info?.database) await openDb(String(to));
    else await loadDbs(String(to));
  });
};
$("db-drop").onclick = () => {
  const db = shownDbs()[dbPick];
  if (db) databaseOp(() => dropDatabase(db), async () => {
    if (db === info?.database) applyInfo(await call<Info>("connect", { name: sessionKey() })); // MySQL: now on none
    await loadDbs(info?.database ?? "");
  });
};
$("db-open-here").onclick = () => openDb();
$("db-search").addEventListener("input", () => ((dbPick = 0), renderDbs()));
$("db-search").addEventListener("keydown", (e) => {
  const n = shownDbs().length;
  if (e.key === "ArrowDown") dbPick = Math.min(n - 1, dbPick + 1);
  else if (e.key === "ArrowUp") dbPick = Math.max(0, dbPick - 1);
  else if (e.key === "Enter") return e.preventDefault(), e.altKey ? openDb() : openDbInNewTab();
  else return;
  e.preventDefault();
  renderDbs();
});
$("db-open").onclick = () => openDbInNewTab();
$("db-cancel").onclick = () => $<HTMLDialogElement>("dbpicker").close();
$("db-button").onclick = openDbPicker;

// ---------- Sidebar: tables (expandable to columns) and history ----------

type Column = { name: string; data_type: string; nullable: boolean; default: string | null; primary: boolean };
let columnsCache = new Map<string, Column[]>(); // swapped per workspace, so a late answer lands in its own
let expanded = new Set<string>();

async function loadColumns(table: string) {
  const cache = columnsCache; // the workspace may change while this waits
  if (!cache.has(table)) cache.set(table, await call<Column[]>("columns", { name: sessionKey(), table }));
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

/** A table's row in the sidebar: click opens it, the chevron shows its columns. `first` and `last` go around its
 *  menu. */
function tableRow(t: string, label: string, first: MenuItem[], last: MenuItem[]) {
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
  row.oncontextmenu = (e) => (e.stopPropagation(),
    contextMenu(e, [...first, "-", ...tableMenu(t), "-", ...transferMenu(t), "-", ["Show diagram", () => showDiagram(t)], ...last]));
  const li = el("li", {}, row);
  if (isOpen)
    li.append(
      el("ul", { className: "cols", oncontextmenu: (e: MouseEvent) => (e.stopPropagation(), contextMenu(e, columnOrderMenu())) },
        ...sortColumns(columnsCache.get(t) ?? []).map((c) =>
        el("li", { title: `${c.name} ${c.data_type}${c.nullable ? "" : " not null"}` },
          el("span", { className: c.primary ? "key" : "nokey", textContent: c.primary ? "⚿" : "", title: c.primary ? "Primary key" : "" }),
          el("span", { className: "label", textContent: c.name }),
          el("span", { className: "type", textContent: c.data_type }),
        ),
      )),
    );
  return li;
}

type Side = "items" | "history" | "queries" | "search";
function showSide(which: Side) {
  for (const b of document.querySelectorAll<HTMLElement>(".side-tabs button")) b.setAttribute("aria-pressed", String(b.dataset.side === which));
  for (const p of document.querySelectorAll<HTMLElement>("aside > .side-pane")) p.hidden = p.id !== `side-${which}`;
  if (which === "search") $("db-search-text").focus();
}

// History lives per connection in history.json (Settings › Locations), newest first, for as long as Settings ›
// General keeps it, and at most 5,000 per connection.
type HistoryEntry = { sql: string; at: number; db: string | null };
const HISTORY_CAP = 5000;

function loadHistory(): HistoryEntry[] {
  const list = active && docEntry<HistoryEntry[]>("history", active.name);
  const years = settings.historyYears;
  const since = years ? Date.now() - years * 365.25 * 864e5 : 0;
  return Array.isArray(list) ? list.filter((h) => h.at >= since) : [];
}

function addHistory(sql: string) {
  if (!active) return;
  const list = [{ sql: sql.trim(), at: Date.now(), db: info?.database ?? null }, ...loadHistory()].slice(0, HISTORY_CAP);
  setDocEntry("history", active.name, list);
  renderHistory();
}

async function clearHistory() {
  if (!active || !loadHistory().length) return;
  if (!(await confirmDialog(`Clear the query history of "${active.name}"?`, "Clear"))) return;
  setDocEntry("history", active.name, undefined);
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
        item.oncontextmenu = (e) => contextMenu(e, [
          ["Open in a new tab", () => openSqlIn(null, h.sql)],
          ["Save as favorite…", () => addFavorite(h.sql)],
          ["Copy SQL", () => copyText(h.sql, "SQL")],
        ]);
        d.append(item);
      }
      return d;
    }),
  );
  $("no-history").hidden = list.length > 0;
}

// ---------- Tabs: SQL editors and table browsers ----------

type SqlTab = {
  id: number; kind: "sql"; title: string; sql: string; state?: EditorState; status?: [string, StatusKind];
  /** The .sql file the tab was opened from or saved to. */
  path?: string;
  /** What the last run returned (results.ts). */
  out?: Results;
  /** The result on screen, for select-all. */
  result?: QueryResult;
};
type Sort = { column: string; desc: boolean };
type TableTab = {
  id: number; kind: "table"; title: string; table: string; view: "data" | "structure";
  /** The first row on the page, from 0. */
  offset: number;
  total?: number;
  /** `total` is the catalog's estimate; Count in the page settings replaces it with the exact number. */
  estimated?: boolean;
  /** The default sort was applied (on the first load, once the key is known). */
  sortSet?: boolean;
  result?: QueryResult; error?: string; loading?: boolean;
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

/** A new SQL tab holding `sql`, named `title` (or the next free "SQL Query n"). */
function openSqlIn(title: string | null, sql: string) {
  const tab = openSqlTab();
  if (title) tab.title = title;
  tab.sql = sql;
  tab.state = editor.newState(sql);
  shownId = 0; // force renderMain to load the new state
  renderMain();
  editor.focus();
}

/** Shows `table`'s tab, opening one if needed: `fresh` opens another even when one is open, and a new tab starts
 *  with `filters` when given. */
function openTable(table: string, view: TableTab["view"] = "data", create = false, opts: { filters?: FilterRow[]; fresh?: boolean } = {}) {
  const { filters, fresh = false } = opts;
  const existing = fresh ? undefined : (tabs.find((t) => t.kind === "table" && t.table === table) as TableTab | undefined);
  if (existing) {
    if (existing.view !== view && !pendingCount(existing)) existing.view = view;
    return select(existing.id);
  }
  const tab: TableTab = {
    id: nextId++, kind: "table", title: table, table, view, offset: 0, staged: new Staged(), create,
    sort: [], filters: filters ?? [], combine: "and", filtersShown: !!filters || settings.filterBar, layout: loadLayout(active!.name, table),
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
  foreignKeys.delete(fkKey(tab.table));
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

  const env = active?.tag ? `${tagLabel(active.tag)} | ` : "";
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
    showResults(tab);
    syncDetail();
    return;
  }
  $("result-strip").hidden = true;
  for (const b of $("view-seg").querySelectorAll<HTMLElement>("button")) b.setAttribute("aria-pressed", String(b.dataset.view === tab.view));
  const from = tab.offset;
  const shown = tab.result?.rows.length ?? 0;
  const total = tab.total === undefined ? "?" : `${tab.estimated ? "~" : ""}${fmt(tab.total)}`;
  $("page-info").textContent =
    tab.view === "structure" ? (tab.create ? "New table" : "")
    : tab.loading ? "Loading…"
    : `${shown ? fmt(from + 1) : 0}–${fmt(from + shown)} of ${total} rows`;
  $<HTMLButtonElement>("page-info").disabled = tab.view !== "data";
  $<HTMLButtonElement>("page-prev").disabled = tab.view !== "data" || tab.offset === 0;
  // An estimate can be off either way: a full page means there may be more.
  const last = shown < settings.pageSize || (!tab.estimated && tab.total !== undefined && from + shown >= tab.total);
  $<HTMLButtonElement>("page-next").disabled = tab.view !== "data" || last;
  showPending(tab);
  const [msg, kind] = tab.message ?? ["", ""];
  const m = $("foot-msg");
  const keyless = tab.view === "data" && tab.result ? grid.keylessNote(tab) : null; // read-only, or a warning
  const readOnly = !!keyless && !grid.findBy(tab);
  m.textContent = readOnly ? keyless : msg || keyless || "";
  $("add-row").hidden = tab.view !== "data" || !tab.result || !grid.findBy(tab);
  m.className = `foot-msg ${readOnly ? "" : msg ? kind : keyless ? "warn" : ""}`;
  if (tab.view === "structure") showStructure($("out"), tab.id, tab.table, !!tab.create);
  else if (tab.error) $("out").replaceChildren(el("p", { className: "hint empty error-text", textContent: tab.error }));
  else if (tab.result) {
    const quickFilter = (ri: number, ci: number): MenuItem[] => {
      if (ri >= tab.result!.rows.length) return grid.cellMenu(tab, ri, ci, selectedRows()); // an added row
      const column = tab.result!.columns[ci];
      const value = tab.result!.rows[ri][ci];
      const shown = value === null ? "NULL" : value.length > 24 ? value.slice(0, 24) + "…" : value;
      const follow = keysOf(tab.table).filter((fk) => fk.columns.includes(column))
        .map((fk): MenuItem => [`Open referenced row in ${fk.ref_table}`, () => followKey(tab, ri, fk)]);
      return ["-", ...follow, [`Filter rows where ${column} = ${shown}`, () => filterByCell(tab, column, value)], ...grid.cellMenu(tab, ri, ci, selectedRows())];
    };
    render(tab.result, from, {
      sort: tab.sort, onSort: (c, add) => sortBy(tab, c, add), table: tab.table, menu: quickFilter,
      resize: (column, width, done) => resizeColumn(tab, column, width, done), edit: tab,
      reorder: (from, to) => reorderColumn(tab, from, to),
      reference: (column) => keysOf(tab.table).find((fk) => fk.columns.includes(column)),
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
  tab.kind === "table" ? tab.staged.count + structurePending(tab.id)
  : (tab.out?.tabs ?? []).reduce((n, r) => n + (r.edit?.staged.count ?? 0), 0);

async function okToDiscard(tab: Tab) {
  const n = pendingCount(tab);
  if (!n || (await confirmDialog(`Discard ${n} unsaved change${n === 1 ? "" : "s"} in ${tab.title}?`, "Discard"))) {
    if (tab.kind === "table") (tab.staged.clear(), dropDraft(tab.id));
    else for (const r of tab.out?.tabs ?? []) r.edit?.staged.clear();
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
  const target = resultTarget();
  if (target) return grid.save(target);
}

async function loadTable(tab: TableTab) {
  const name = sessionKey(); // the tab's workspace, even if another takes the screen meanwhile
  tab.loading = true;
  tab.error = undefined;
  if (tab.id === currentId) renderMain();
  try {
    // Structure view, the filter bar's columns, and the primary key for editing and the default sort.
    const columns = await loadColumns(tab.table);
    loadForeignKeys(tab.table);
    if (tab.view === "data") {
      if (!tab.sortSet) {
        tab.sortSet = true;
        const key = settings.defaultSort === "none" ? [] : columns.filter((c) => c.primary);
        if (!tab.sort.length) tab.sort = key.map((c) => ({ column: c.name, desc: settings.defaultSort === "desc" }));
      }
      const request = {
        ...browseRequest(tab), limit: settings.pageSize, offset: tab.offset, count: tab.total === undefined,
        estimate_over: settings.estimateOver || null, identity: true, wkt: settings.geometryWkt,
      };
      const page = await call<{ columns: string[]; rows: (string | null)[][]; total: number | null; estimated: boolean; ids?: string[] }>("browse", { name, request });
      tab.result = arrange({ columns: page.columns, rows: page.rows, affected: 0 }, tab.layout.order);
      tab.ids = page.ids;
      if (page.total !== null) (tab.total = page.total), (tab.estimated = page.estimated);
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
  renderFilters(bar, tab, columns, () => applyFilters(tab), {
    primary: (columnsCache.get(tab.table) ?? []).filter((c) => c.primary).map((c) => c.name),
    showSql: () => showFilterSql(tab),
    exportRows: () => $("export-table").click(),
  });
}

/** The filter bar's SQL button: the query behind the page, with the filter values shown as literals. */
async function showFilterSql(tab: TableTab) {
  try {
    const request = { ...browseRequest(tab), limit: settings.pageSize, offset: tab.offset };
    await showSql(`SQL for ${tab.table}`, await call<string>("filter_sql", { name: sessionKey(), request }));
  } catch (e) {
    status(String(e), "error");
  }
}

const MAX_PAGE = 100_000; // browse.rs MAX_PAGE

/** The rows a table tab shows, without paging: its table, filters and sort. */
const browseRequest = (tab: TableTab) => ({ table: tab.table, filters: activeFilters(tab), combine: tab.combine, sort: tab.sort });

async function applyFilters(tab: TableTab) {
  if (!(await okToDiscard(tab))) return;
  tab.offset = 0;
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
  tab.offset = 0;
  loadTable(tab);
}

type ForeignKey = { name: string; columns: string[]; ref_table: string; ref_columns: string[] };
/** Each table's foreign keys, by session key and table; asked for once, as the table first loads. */
const foreignKeys = new Map<string, ForeignKey[]>();
const fkKey = (table: string) => `${sessionKey()}\u0000${table}`;
const keysOf = (table: string) => foreignKeys.get(fkKey(table)) ?? [];

function loadForeignKeys(table: string) {
  const key = fkKey(table);
  if (foreignKeys.has(key)) return;
  foreignKeys.set(key, []); // a failure just means no links
  call<{ foreign_keys: ForeignKey[] }>("table_structure", { name: sessionKey(), table }).then((s) => {
    foreignKeys.set(key, s.foreign_keys);
    const tab = current();
    if (s.foreign_keys.length && tab?.kind === "table" && tab.table === table && fkKey(table) === key) renderMain(); // mark the headers
  }, () => {});
}

/** Opens the row a foreign key's value points at: the referenced table, filtered to it. */
async function followKey(tab: TableTab, ri: number, fk: ForeignKey) {
  const values = fk.columns.map((c) => grid.valueAt(tab, ri, tab.result!.columns.indexOf(c)));
  if (values.some((v) => typeof v !== "string")) return note("The key is NULL, so it references no row", "");
  let refColumns = fk.ref_columns;
  // SQLite leaves the columns out of a key that references the primary key.
  if (refColumns.length !== fk.columns.length || refColumns.some((c) => !c)) {
    try {
      refColumns = (await loadColumns(fk.ref_table)).filter((c) => c.primary).map((c) => c.name);
    } catch (e) {
      return note(String(e), "error");
    }
  }
  const filters = refColumns.map((column, i): FilterRow => ({ ...blankFilter(), column, op: "eq", value: values[i] as string }));
  const open = tabs.find((t): t is TableTab => t.kind === "table" && t.table === fk.ref_table);
  if (!open) return openTable(fk.ref_table, "data", false, { filters });
  if (!(await okToDiscard(open))) return;
  Object.assign(open, { view: "data", filters, combine: "and", filtersShown: true });
  filtersDrawnFor = 0;
  select(open.id);
  applyFilters(open);
}

/** Right-click on a cell: filter the table to rows with this value. */
function filterByCell(tab: TableTab, column: string, value: string | null) {
  const row: FilterRow = value === null ? { ...blankFilter(), column, op: "is_null" } : { ...blankFilter(), column, op: "eq", value };
  const blank = tab.filters.length <= 1 && !activeFilters(tab).length;
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
  if (!active) return;
  const items: MenuItem[] = toolsMenu(active, info?.database);
  if (active.kind !== "sqlite") items.push(["Users…", showUsers]); // needs the workspace's connection, so not on launcher rows
  items.push("-", ["Metrics Board", () => openBoard(boardCtx())]);
  contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), items);
};

/** The user manager on the workspace's session (users.ts). */
const showUsers = () => active && openUsers({ conn: { name: sessionKey(), kind: active.kind }, call, confirm: confirmDialog });

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
  // Inside the topmost modal window when there is one, or the window would keep it out of reach.
  ([...document.querySelectorAll("dialog:modal")].pop() ?? document.body).append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(e.clientX, innerWidth - r.width - 8)}px`;
  menu.style.top = `${Math.min(e.clientY, innerHeight - r.height - 8)}px`;
  menu.querySelector("button")?.focus();
  const onDown = (ev: Event) => !menu.contains(ev.target as Node) && close();
  // Esc closes only the menu, not a window it sits in.
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && (ev.preventDefault(), ev.stopPropagation(), close());
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
  const i = await call<Info>("connect", { name: sessionKey() }); // reuses the open connection
  forgetItems(sessionKey());
  info = i;
  tables = i.tables;
  columnsCache.clear();
  renderTables();
  loadSchema();
}

/** Columns for every table, for autocomplete. Runs in the background; the editor works without it meanwhile. */
async function loadSchema() {
  if (!active) return;
  const [conn, key] = [active, sessionKey()];
  try {
    const pairs = await invoke<[string, string][]>("schema", { name: key });
    if (sessionKey() !== key) return; // switched workspaces meanwhile
    const schema: Schema = new Map(tables.map((t) => [t, []]));
    for (const [table, column] of pairs) (schema.get(table) ?? schema.set(table, []).get(table)!).push(column);
    editor.setDialect(conn.kind, schema);
  } catch {} // autocomplete just keeps table names
}

const fmt = (n: number) => n.toLocaleString();
const rowLimit = () => settings.sqlLimit || null;

type Ran = { results: QueryResult[]; read_only: boolean };
let nextResultId = 1;

/** Runs statements from the current SQL tab, one after another; results and status stay with that tab. Each result
 *  set gets a result tab (pinned ones from earlier runs stay), each statement a line in Messages. Safe mode asks
 *  once for the lot. A failed statement stops the rest when Stop on error is on. */
async function execute(statements: string[]) {
  const tab = current();
  statements = statements.filter((s) => s.trim());
  if (!active || tab?.kind !== "sql" || !statements.length || !(await okToDiscard(tab))) return;
  const [conn, key] = [active, sessionKey()];
  const say = (text: string, kind: StatusKind) => {
    tab.status = [text, kind];
    if (tab.id === currentId) status(text, kind);
  };
  const res: Results = { tabs: (tab.out?.tabs ?? []).filter((r) => r.pinned), messages: [], shown: MESSAGES };
  tab.out = res;
  const many = statements.length > 1;
  const t0 = performance.now();
  let approved = NOT_APPROVED; // asked once, then kept for the rest
  let [done, failed, affected, shown] = [0, 0, 0, 0];
  let stopped = "";
  for (let i = 0; i < statements.length; i++) {
    const sql = statements[i];
    say(many ? `Running ${i + 1} of ${statements.length}…` : "Running…", "busy");
    const started = performance.now();
    const took = () => Math.round(performance.now() - started);
    try {
      const ran = await call<Ran>("run_query", { name: key, sql, ...approved, maxRows: rowLimit() });
      ran.results.forEach((result, index) => {
        if (!result.columns.length) return void (affected += result.affected);
        const r: ResultTab = { id: nextResultId++, title: `Result ${res.tabs.length + 1}`, sql, index, result, pinned: false, readOnly: ran.read_only };
        const table = ran.results.length === 1 ? sourceTable(sql, tables) : null;
        if (table) {
          r.edit = { id: r.id, table, result, staged: new Staged() };
          // The key columns decide whether it's editable; draw again once they're known.
          loadColumns(table).then(() => tab.id === currentId && showResults(tab), () => {});
        }
        res.tabs.push(r);
        shown = r.id; // the last new result shows, as before there were tabs
      });
      res.messages.push({ sql, text: ran.results.map(rowsText).join(" · "), error: false, ms: took(), at: Date.now() });
      done++;
      addHistory(sql);
    } catch (e) {
      if (isSafetyStop(e) && !(e === "CONFIRM" && approved.confirmed)) {
        say("", "");
        const rest = statements.slice(i).join(";\n");
        const question =
          many ? `Run these statements on "${conn.name}"?`
          : conn.safety === "alert1" ? `Run this on "${conn.name}"?` : `This may change data on "${conn.name}".`;
        const next = await approval(e, conn.name, () => ask(question, rest.trim(), "Run anyway"));
        if (!next) {
          stopped = i ? `Cancelled before statement ${i + 1}` : "Cancelled";
          break;
        }
        approved = next;
        i--; // run this statement again, approved
        continue;
      }
      failed++;
      res.messages.push({ sql, text: String(e), error: true, ms: took(), at: Date.now() });
      if (!many || settings.stopOnError) {
        stopped = many ? `Statement ${i + 1} of ${statements.length} failed: ${e}` : String(e);
        break;
      }
    } finally {
      if (tab.id === currentId) showResults(tab); // results appear as they come
    }
  }
  res.shown = failed ? MESSAGES : shown || MESSAGES;
  if (tab.id === currentId) showResults(tab);
  const ms = Math.round(performance.now() - t0);
  const only = res.tabs.find((r) => r.id === shown)?.result;
  if (stopped) say(stopped, failed ? "error" : "");
  else if (!many && only) {
    const limited = only.truncated ? ` (first ${fmt(only.rows.length)}; Load more, or raise the row limit)` : "";
    say(`${fmt(only.rows.length)} rows${limited} · ${ms} ms`, "done");
  } else if (!many) say(`${fmt(affected)} rows affected · ${ms} ms`, "done");
  else say(`${done} of ${statements.length} statements${failed ? ` · ${failed} failed` : ""} · ${fmt(affected)} rows affected · ${ms} ms`, failed ? "error" : "done");
}

/** One result's line in Messages. */
const rowsText = (r: QueryResult) =>
  r.columns.length ? `${fmt(r.rows.length)} row${r.rows.length === 1 ? "" : "s"}${r.truncated ? " (more not loaded)" : ""}` : `${fmt(r.affected)} rows affected`;

/** A SQL tab's results under the editor: the strip, then the result, every result (in append mode), or Messages. */
function showResults(tab: SqlTab) {
  const res = tab.out;
  $("result-strip").hidden = !res;
  if (!res) {
    tab.result = undefined;
    return void $("out").replaceChildren(el("p", { className: "hint empty", textContent: "Run a query to see results here." }));
  }
  const append = settings.resultsMode === "append";
  const valid = res.shown === MESSAGES || (append ? res.shown === ALL && res.tabs.length > 0 : res.tabs.some((r) => r.id === res.shown));
  if (!valid) res.shown = !res.tabs.length ? MESSAGES : append ? ALL : res.tabs.at(-1)!.id;
  renderStrip($("result-tabs"), res, append, {
    show: (id) => ((res.shown = id), showResults(tab)),
    menu: (e, r) => contextMenu(e, resultMenu(tab, r)),
    togglePin: (r) => ((r.pinned = !r.pinned), showResults(tab)),
  });
  const shown = res.shown === ALL ? res.tabs : res.tabs.filter((r) => r.id === res.shown);
  const search = $<HTMLInputElement>("result-search");
  search.hidden = append || !shown.length;
  search.value = res.search ?? "";
  const target = shown.length === 1 && !append ? editable(shown[0]) : null;
  const chart = $("result-chart");
  chart.hidden = append || shown.length !== 1 || !shown[0].result.columns.length;
  chart.setAttribute("aria-pressed", String(!chart.hidden && !!shown[0].charted));
  showResultEdits(target);
  $("result-found").textContent = "";
  tab.result = shown[0]?.result;
  if (!shown.length) return renderMessages($("out"), res.messages, active?.kind ?? "");
  if (!append) {
    const r = shown[0];
    const rows = searchRows(r.result, search.value);
    tab.result = rows;
    if (r.charted) {
      $("out").replaceChildren(chartPanel(rows, (r.chart ??= guessSpec(rows)), {
        change: (s) => (r.chart = s),
        add: (s) => openBoard(boardCtx(), { kind: s.kind, title: "", sql: r.sql, chart: { x: s.x, y: s.y } }),
      }));
    } else if (!target) render(rows, 0);
    else {
      render(rows, 0, {
        table: target.table,
        menu: (ri, ci) => grid.cellMenu(target, ri, ci, selectedRows()), edit: target,
      });
      grid.decorate(target);
      paintSelection();
    }
    $("result-found").textContent = rows === r.result ? "" : `${fmt(rows.rows.length)} of ${fmt(r.result.rows.length)}`;
  } else {
    $("out").replaceChildren(...shown.map((r) =>
      el("section", { className: "appended" }, el("h4", { textContent: `${r.title} · ${rowsLabel(r.result)}`, title: r.sql }), gridTable(r.result, 0))));
    vgrid.drawGrids($("out"));
    if (selection && !shown.some((r) => r.result === selection!.r)) selection = null;
  }
  for (const r of shown) {
    if (!r.result.truncated || !r.readOnly) continue;
    const more = el("button", { className: "ghost load-more", textContent: `Load ${fmt(rowLimit() ?? 1000)} more rows` });
    more.onclick = () => loadMore(tab, r);
    (drawn.get(r.result)?.parentElement ?? $("out")).append(more);
  }
}

/** The result's staged edits, when its rows can be edited in place: it reads one table, every column is one of the
 *  table's, the table's key is among them, and no search hides rows. */
function editable(r: ResultTab) {
  const e = r.edit;
  if (r.charted) return null;
  const columns = e && columnsCache.get(e.table);
  if (!e || !columns || (current() as SqlTab).out?.search?.trim()) return null;
  const names = new Set(columns.map((c) => c.name));
  const plain = r.result.columns.every((c) => names.has(c)) && new Set(r.result.columns).size === r.result.columns.length;
  return plain && grid.findBy(e) ? e : null;
}

/** The strip's edit controls for a result edited in place. */
function showResultEdits(target: grid.EditTab | null) {
  const n = target?.staged.count ?? 0;
  const [msg, kind] = target?.message ?? ["", ""];
  const info = $("result-edit-info");
  info.textContent = n ? `${n} unsaved change${n === 1 ? "" : "s"}` : target ? msg || "Editable" : "";
  info.className = `result-edit-info ${n ? "" : kind}`;
  info.title = target ? `Double-click a cell to edit; changes save to ${target.table}` : "";
  for (const id of ["result-discard", "result-preview", "result-save"]) $(id).hidden = !n;
  $("result-add-row").hidden = !target;
}

/** The SQL tab's result whose rows grid edits apply to right now, if any. */
function resultTarget() {
  const tab = current();
  const r = tab?.kind === "sql" ? tab.out?.tabs.find((x) => x.id === tab.out!.shown) : undefined;
  return r && settings.resultsMode === "tabs" ? editable(r) : null;
}

/** Runs a result's statement again after its edits were saved, keeping as many rows as it showed. */
async function reloadResult(edit: grid.EditTab) {
  const tab = current();
  const r = tab?.kind === "sql" ? tab.out?.tabs.find((x) => x.edit === edit) : undefined;
  if (!active || !r || tab?.kind !== "sql") return;
  const maxRows = Math.max(r.result.rows.length, rowLimit() ?? 0) || null;
  try {
    const ran = await call<Ran>("run_query", { name: sessionKey(), sql: r.sql, confirmed: false, unlock: null, maxRows });
    const next = ran.results[r.index];
    if (next?.columns.length) r.result = edit.result = next;
  } catch (e) {
    edit.message = [`Saved, but reloading failed: ${e}`, "error"];
  }
  if (tab.id === currentId) showResults(tab);
}

function resultMenu(tab: SqlTab, r: ResultTab): MenuItem[] {
  const res = tab.out!;
  return [
    [r.pinned ? "Unpin" : "Pin (keep when the tab runs again)", () => ((r.pinned = !r.pinned), showResults(tab))],
    ...(r.result.truncated && r.readOnly ? [["Load more rows", () => loadMore(tab, r)] as MenuItem] : []),
    ["Export…", () => exportResults([r])],
    ["Copy SQL", () => copyText(r.sql, "SQL")],
    "-",
    ["Close", () => ((res.tabs = res.tabs.filter((x) => x !== r)), showResults(tab))],
    ["Close others", () => ((res.tabs = [r]), (res.shown = r.id), showResults(tab))],
  ];
}

/** Runs a result's statement again with room for more rows. Only offered for statements that just read. */
async function loadMore(tab: SqlTab, r: ResultTab) {
  if (!active) return;
  const conn = active;
  const maxRows = r.result.rows.length + (rowLimit() ?? 1000);
  status("Loading more rows…", "busy");
  const t0 = performance.now();
  try {
    const ran = await guarded(conn.name, (a) => call<Ran>("run_query", { name: sessionKey(), sql: r.sql, ...a, maxRows }),
      () => ask(`Run this on "${conn.name}" again?`, r.sql.trim(), "Run"));
    const next = ran.results[r.index];
    if (next?.columns.length && !r.edit?.staged.count) {
      r.result = next;
      if (r.edit) r.edit.result = next;
    }
    tab.status = [`${fmt(r.result.rows.length)} rows${r.result.truncated ? " (more not loaded)" : ""} · ${Math.round(performance.now() - t0)} ms`, "done"];
  } catch (e) {
    tab.status = [e === CANCELLED ? "Cancelled" : String(e), e === CANCELLED ? "" : "error"];
  }
  if (tab.id === currentId) (status(...tab.status), showResults(tab));
}

/** Exports result tabs; each one's statement runs again on the export's own connection. */
function exportResults(list: ResultTab[]) {
  const name = (r: ResultTab) => r.title.toLowerCase().replace(/\s+/g, "-");
  exportDialog(transferCtx(), { kind: "query", queries: list.map((r) => ({ sql: r.sql, name: name(r) })) });
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
  if (filled) execute(filled);
};

/** Runs every statement (in the selection, or the whole tab) in turn. */
async function runAll() {
  const filled = await withParams(editor.all().map((s) => s.text));
  if (filled) execute(filled);
}

const numeric = /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i;

type RenderOptions = {
  /** Without these the headers don't sort (SQL results). */
  sort?: Sort[];
  onSort?: (column: string, add: boolean) => void;
  /** The table the rows come from, for "Copy as INSERT". */
  table: string;
  /** Extra context-menu items for a cell. */
  menu?: (row: number, col: number) => MenuItem[];
  /** Header drag: a new width while dragging, the final one when `done`; null resets to automatic. */
  resize?: (column: string, width: number | null, done: boolean) => void;
  /** The staged edits drawn over the rows, when they can be edited. */
  edit?: grid.EditTab;
  /** Header drag: the column at `from` moves to `to` (positions in the result). */
  reorder?: (from: number, to: number) => void;
  /** The foreign key a column belongs to, marked in its header. */
  reference?: (column: string) => ForeignKey | undefined;
};

/** Shows one result grid; see [`gridTable`]. */
function render(r: QueryResult, offset: number, sorting?: RenderOptions) {
  $("out").replaceChildren(gridTable(r, offset, sorting));
  if (selection?.r !== r) selection = null; // a redraw of the same rows (after an edit) keeps it
  vgrid.drawGrids($("out"));
}

// The grids draw the rows near the viewport (vgrid.ts); scrolling and resizing move that window.
let drawQueued = false;
const queueDraw = () => {
  if (drawQueued) return;
  drawQueued = true;
  requestAnimationFrame(() => ((drawQueued = false), vgrid.drawGrids($("out"))));
};
$("out").addEventListener("scroll", queueDraw, { passive: true });
new ResizeObserver(queueDraw).observe($("out"));

/** The table each result was last drawn in, for painting the selection when several are on screen. */
const drawn = new WeakMap<QueryResult, HTMLTableElement>();

/** Result grid with row numbers starting after `offset`. With options, headers sort the rows when clicked. */
function gridTable(r: QueryResult, offset: number, sorting?: RenderOptions): HTMLElement {
  if (!r.columns.length) return el("p", { className: "hint empty", textContent: `${r.affected} rows affected.` });
  const table = el("table");
  const head = table.createTHead().insertRow();
  head.append(el("th", { className: "rownum" }));
  r.columns.forEach((c, ci) => {
    const th = el("th", { textContent: c });
    if (sorting?.onSort && sorting.sort) {
      const { sort, onSort } = sorting;
      th.className = "sortable";
      th.title = "Sort by this column; Shift-click to add it to the sort";
      const i = sort.findIndex((s) => s.column === c);
      if (i >= 0) {
        const s = sort[i];
        th.append(el("span", { className: "sort", textContent: `${s.desc ? "▼" : "▲"}${sort.length > 1 ? i + 1 : ""}` }));
        th.setAttribute("aria-sort", s.desc ? "descending" : "ascending");
      }
      th.onclick = (e) => !columnDragged && onSort(c, e.shiftKey);
    }
    const fk = sorting?.reference?.(c);
    if (fk) {
      th.append(el("span", { className: "fk-mark", textContent: "→" }));
      th.title = `References ${fk.ref_table}${fk.ref_columns.some(Boolean) ? ` (${fk.ref_columns.join(", ")})` : ""}. Right-click a value to open its row.`;
    }
    if (sorting?.resize) th.append(resizeGrip(th, c, sorting.resize));
    const reorder = sorting?.reorder;
    if (reorder) th.onpointerdown = (e) => e.button === 0 && !(e.target as HTMLElement).closest(".grip") && dragColumn(e, head, ci, reorder);
    head.append(th);
  });
  const body = table.createTBody();
  const edit = sorting?.edit;
  const count = () => r.rows.length + (edit?.staged.added.length ?? 0);
  const row = (i: number) => {
    const tr = document.createElement("tr");
    if (i < r.rows.length) {
      tr.append(el("td", { className: "rownum", textContent: String(offset + i + 1) }));
      for (const v of r.rows[i]) fillCell(tr.insertCell(), v);
    }
    if (edit) grid.paintRow(edit, tr, i); // staged values; an added row is drawn whole there
    paintRow(tr, r, i);
    return tr;
  };
  if (count()) {
    vgrid.virtualize(table, count, row, () => vgrid.widestValues(r.rows, r.columns.length, offset + count()));
    wireSelection(table, r, sorting);
  } else {
    const tr = body.insertRow();
    tr.append(el("td", { className: "hint", colSpan: r.columns.length + 1, textContent: "No rows." }));
  }
  drawn.set(r, table);
  return table;
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

/** Marks the selected cells of drawn row `ri` of `r`. */
function paintRow(tr: HTMLTableRowElement, r: QueryResult, ri: number) {
  const b = selection?.r === r ? selectedBlock(selection) : null;
  const on = !!b && ri >= b.r0 && ri <= b.r1;
  [...tr.cells].forEach((td, i) => td.classList.toggle("sel", on && i - 1 >= b!.c0 && i - 1 <= b!.c1)); // the row number comes first
}

function paintSelection() {
  $("out").querySelectorAll("td.sel").forEach((td) => td.classList.remove("sel"));
  const table = selection && drawn.get(selection.r);
  if (selection && table?.isConnected) {
    for (const tr of table.tBodies[0].rows) {
      const ri = vgrid.indexOf(tr);
      if (ri !== null) paintRow(tr, selection.r, ri);
    }
  }
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

/** Shows `menu` above `anchor`, a footer button; a click outside or Esc closes it. */
function openPopover(anchor: HTMLElement, menu: HTMLElement) {
  menuClose?.();
  document.body.append(menu);
  const r = anchor.getBoundingClientRect();
  const { width, height } = menu.getBoundingClientRect();
  Object.assign(menu.style, { left: `${clamp(r.left, 8, innerWidth - width - 8)}px`, top: `${Math.max(8, r.top - height - 6)}px` });
  const onDown = (ev: Event) => !menu.contains(ev.target as Node) && ev.target !== anchor && close();
  // Esc closes only the menu, not a window it sits in.
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && (ev.preventDefault(), ev.stopPropagation(), close());
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

let columnDragged = false; // set while a header drag ends, so its click doesn't sort

/** Drags header `from` sideways; dropping it between two others moves the column there. */
function dragColumn(start: PointerEvent, head: HTMLTableRowElement, from: number, reorder: (from: number, to: number) => void) {
  const heads = [...head.cells].slice(1); // after the row number
  let to = -1;
  const mark = (at: number) => heads.forEach((h, k) => (h.classList.toggle("drop-before", k === at), h.classList.toggle("drop-after", at === heads.length && k === heads.length - 1)));
  const move = (e: PointerEvent) => {
    if (to < 0 && Math.abs(e.clientX - start.clientX) < 6) return;
    heads[from].classList.add("dragging");
    // Before the first shown header whose middle is right of the pointer; hidden ones have no width.
    to = heads.findIndex((h) => {
      const r = h.getBoundingClientRect();
      return r.width > 0 && e.clientX < r.left + r.width / 2;
    });
    if (to < 0) to = heads.length;
    mark(to);
  };
  const up = () => {
    removeEventListener("pointermove", move);
    removeEventListener("pointerup", up);
    if (to < 0) return;
    heads[from].classList.remove("dragging");
    mark(-1);
    columnDragged = true;
    setTimeout(() => (columnDragged = false));
    const dest = to > from ? to - 1 : to;
    if (dest !== from) reorder(from, dest);
  };
  addEventListener("pointermove", move);
  addEventListener("pointerup", up);
}

/** Moves a table tab's column and remembers the order for the table. Staged edits refer to columns by position,
 *  so the order holds still while there are some. */
function reorderColumn(tab: TableTab, from: number, to: number) {
  if (!tab.result) return;
  if (tab.staged.count) return note("Save or discard the changes before moving columns", "error");
  tab.layout.order = moveColumn(tab.result.columns, from, to);
  saveLayout(active!.name, tab.table, tab.layout);
  tab.result = arrange(tab.result, tab.layout.order);
  renderMain();
}

/** The Columns button: a checklist of the table's columns. Unchecking hides a column. */
function showColumnsMenu() {
  const tab = current();
  if (tab?.kind !== "table" || !tab.result) return;
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
  const resetOrder = el("button", { type: "button", textContent: "Reset order", disabled: !t.layout.order.length });
  resetOrder.onclick = () => {
    const own = columnsCache.get(t.table)?.map((c) => c.name);
    if (!own) return;
    if (t.staged.count) return note("Save or discard the changes before moving columns", "error");
    t.layout.order = [];
    save();
    t.result = arrange(t.result!, own);
    menuClose?.();
    renderMain();
  };
  const foot = el("footer");
  foot.append(showAll, el("span", { className: "spacer" }), resetOrder, reset);
  menu.append(...boxes.map((b) => b.row), foot);
  openPopover($("columns-button"), menu);
}

/** The page settings (Ref: PagingPopover, EstimatedOffsetPopover): rows per page, the row to start at, and the
 *  row count, which Count makes exact when it is an estimate. */
function showPagingMenu() {
  const tab = current();
  if (tab?.kind !== "table" || tab.view !== "data") return;
  const t = tab;
  const size = el("input", { type: "number", min: "1", max: String(MAX_PAGE), value: String(settings.pageSize), className: "num-input" });
  const start = el("input", { type: "number", min: "1", value: String(t.offset + 1), className: "num-input wide-num" });
  const total = el("span");
  const count = el("button", { type: "button", textContent: "Count" });
  const showTotal = () => {
    total.textContent = t.total === undefined ? "Not counted" : `${t.estimated ? "About " : ""}${fmt(t.total)} rows${t.estimated ? " (estimated)" : ""}`;
    count.hidden = t.total !== undefined && !t.estimated;
  };
  showTotal();
  count.onclick = async () => {
    count.disabled = true;
    total.textContent = "Counting…";
    try {
      const page = await call<{ total: number | null }>("browse", { name: sessionKey(), request: { ...browseRequest(t), limit: 0, offset: 0, count: true } });
      if (page.total !== null) (t.total = page.total), (t.estimated = false);
      if (t.id === currentId) renderMain();
    } catch (e) {
      total.textContent = String(e);
    }
    count.disabled = false;
    if (total.textContent === "Counting…") showTotal();
  };
  const apply = el("button", { type: "submit", className: "primary", textContent: "Go" });
  const row = (label: string, ...kids: (Node | string)[]) => el("label", { className: "paging-row" }, el("span", { textContent: label }), ...kids);
  const menu = el("form", { className: "popover paging-menu" },
    row("Rows per page", size), row("Start at row", start), el("div", { className: "paging-row" }, el("span", { textContent: "Total" }), total, count),
    el("footer", {}, el("span", { className: "spacer" }), apply));
  menu.onsubmit = async (e) => {
    e.preventDefault();
    if (!(await okToDiscard(t))) return;
    setSetting("pageSize", clamp(Math.round(Number(size.value)) || settings.pageSize, 1, MAX_PAGE));
    t.offset = Math.max(0, Math.round(Number(start.value) || 1) - 1);
    menuClose?.();
    loadTable(t);
  };
  openPopover($("page-info"), menu);
  start.select();
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
    const ri = vgrid.indexOf(td?.parentElement as HTMLTableRowElement | null);
    if (!td || ri === null || td.querySelector("input")) return null; // an open cell editor keeps its own clicks
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
  const rows = tab?.kind === "table" ? (tab.view === "data" ? tab : null) : resultTarget();
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

// Arrow keys move the selection (Shift extends it), Enter edits the focused cell.
const MOVES: Record<string, [number, number]> = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] };
document.addEventListener("keydown", (e) => {
  const sel = selection;
  const table = sel && drawn.get(sel.r);
  if (!sel || !table?.isConnected || e.metaKey || e.ctrlKey || e.altKey || typingSomewhere() || document.querySelector("dialog[open]")) return;
  const tab = current();
  if (e.key === "Enter") {
    const rows = tab?.kind === "table" ? (tab.view === "data" ? tab : null) : resultTarget();
    if (rows && rows.result === sel.r) (e.preventDefault(), grid.editCell(rows, ...sel.focus));
    return;
  }
  const move = MOVES[e.key];
  if (!move) return;
  e.preventDefault();
  const body = table.tBodies[0];
  const ri = clamp(sel.focus[0] + move[0], 0, vgrid.rowCount(body) - 1);
  const ci = clamp(sel.focus[1] + move[1], 0, sel.r.columns.length - 1);
  sel.focus = [ri, ci];
  if (!e.shiftKey) sel.anchor = [ri, ci];
  vgrid.reveal($("out"), body, ri)?.cells[ci + 1]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  paintSelection();
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
  const rows = tab?.kind === "table" ? (tab.view === "data" ? tab : null) : resultTarget();
  if (typingSomewhere() || document.querySelector("dialog[open]") || !rows) return;
  if (!selection || selection.r !== rows.result) return;
  e.preventDefault();
  grid.paste(rows, selectedBlock(selection), e.clipboardData?.getData("text/plain") ?? "");
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
  tab.offset = Math.max(0, tab.offset + delta * settings.pageSize);
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
/** The on-screen connection for the modules below: `name` is the session key they send (shown via `connName`). */
const hostConn = () => active && { ...active, name: sessionKey() };
$("result-save").onclick = () => saveEdits();
$("result-preview").onclick = () => {
  const t = resultTarget();
  if (t) grid.preview(t);
};
$("result-discard").onclick = () => {
  const t = resultTarget();
  if (t) (t.staged.clear(), renderMain());
};
$("result-add-row").onclick = () => {
  const t = resultTarget();
  if (t) grid.addRow(t);
};
$("result-chart").onclick = () => {
  const tab = current();
  const r = tab?.kind === "sql" ? tab.out?.tabs.find((x) => x.id === tab.out!.shown) : undefined;
  if (tab?.kind !== "sql" || !r) return;
  r.charted = !r.charted;
  showResults(tab);
};

/** What a metrics board needs from the workspace on screen; see metrics.ts. */
function boardCtx(): BoardCtx {
  const session = (w: Workspace) => ({
    key: w.key, label: w.info?.database ? `${w.conn.name} · ${w.info.database}` : w.conn.name, dialect: w.conn.kind as Dialect,
  });
  return {
    conn: active!.name, session: session(ws!), sessions: () => workspaces.map(session),
    format: settings.paramFormat === "off" ? "colon" : settings.paramFormat,
    confirm: confirmDialog, transfer: transferCtx,
  };
}

$("result-search").addEventListener("input", (e) => {
  const tab = current();
  if (tab?.kind !== "sql" || !tab.out) return;
  tab.out.search = (e.target as HTMLInputElement).value;
  showResults(tab);
});
grid.initGridEdit({
  call,
  conn: hostConn,
  columns: (table) => columnsCache.get(table),
  render: renderMain,
  reload: (tab) => ("kind" in tab ? loadTable(tab as TableTab) : reloadResult(tab)),
  confirm: confirmDialog,
  fill: fillCell,
  logOriginals: () => settings.logOriginals,
});
$("side-items").oncontextmenu = (e) =>
  active && contextMenu(e, [...sidebarMenu(), "-", ...newItemsMenu(), "-", ["Show diagram", () => showDiagram()], "-", ...treeMenu()]);
/** The ER diagram of the sidebar's schema, or of `table`'s schema centred on it. */
function showDiagram(table?: string) {
  const pg = active?.kind === "postgres";
  openDiagram(pg ? (table?.includes(".") ? table.split(".")[0] : shownSchema()) : null, table);
}
initDiagram({ call, conn: hostConn, openTable: (t) => openTable(t), menu: contextMenu });
initSidebar({
  conn: hostConn,
  tables: () => tables,
  call,
  tableRow,
  openTable: (t, fresh) => openTable(t, "data", false, { fresh }),
  openSql(sql, run) {
    openSqlIn(null, sql);
    if (run) execute([sql]);
  },
  insertSql(text) {
    if (current()?.kind === "sql") editor.insert(text);
    else openSqlIn(null, text);
  },
  note,
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
  conn: hostConn,
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
  itemKind: (name) => loadedItems(sessionKey()).find((i) => i.name === name)?.kind ?? null,
});
// The database's own functions complete in the editor once the sidebar has loaded them.
editor.setFunctionSource(() =>
  loadedItems(sessionKey()).filter((i) => i.kind === "function" || i.kind === "procedure").map((i) => i.name.replace(/^public\./, "")));

// ---------- Import and export (importexport.ts) ----------

function transferCtx(): TransferCtx {
  return {
    conn: sessionKey(),
    database: info?.database ?? "",
    tables,
    columns: async (table) => (await loadColumns(table)).map((c) => c.name),
    confirm: confirmDialog,
    revealExports: () => settings.revealExport,
    imported: async (table) => {
      // A new table appears in the sidebar; an open tab of the table shows the new rows.
      try {
        tables = (await call<Info>("connect", { name: sessionKey() })).tables;
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
  const request = { ...browseRequest(tab), limit: settings.pageSize, offset: tab.offset, count: false, wkt: settings.geometryWkt };
  const source: ExportSource = { kind: "rows", request, pageRows: tab.result?.rows.length ?? 0 };
  exportDialog(transferCtx(), source);
};
$("export-result").onclick = (e) => {
  const tab = current();
  if (tab?.kind !== "sql" || !active) return;
  const list = tab.out?.tabs ?? [];
  if (!list.length) return status("Run a query first; its result is exported by running it again.", "error");
  const on = list.find((r) => r.id === tab.out!.shown) ?? list.at(-1)!;
  if (list.length === 1) return exportResults(list);
  contextMenu(e, [[`Export ${on.title}…`, () => exportResults([on])], [`Export all ${list.length} results…`, () => exportResults(list)]]);
};
$("columns-button").onclick = () => (menuClose ? menuClose() : showColumnsMenu());
$("page-prev").onclick = () => turnPage(-1);
$("page-info").onclick = showPagingMenu;
$("page-next").onclick = () => turnPage(1);
/** Leaves the workspace and closes its session; `next` (else the neighbouring workspace, else the launcher) comes
 *  up. */
async function closeWorkspace(next?: Workspace | null) {
  for (const t of allTabs()) if (!(await okToDiscard(t))) return;
  if (ws) {
    invoke("disconnect", { name: ws.key }).catch(() => {});
    // An asked-for password serves every tab of its connection, so it goes with the last one.
    const name = ws.conn.name;
    if (!workspaces.some((w) => w !== ws && w.conn.name === name)) launcher.forgetAskedPassword(name);
  }
  const i = ws ? workspaces.indexOf(ws) : -1;
  workspaces = workspaces.filter((w) => w !== ws);
  rememberOpen();
  ws = null;
  active = null;
  info = null;
  tabs = [];
  shownId = 0;
  delete document.body.dataset.env;
  next = next && workspaces.includes(next) ? next : workspaces[Math.min(Math.max(i, 0), workspaces.length - 1)];
  if (next) showWorkspace(next);
  else show("home");
}

/** Notes the open workspaces, for reopening them at the next start, and refreshes Open Recent. */
function rememberOpen() {
  launcher.setOpenAtQuit([...new Set(workspaces.map((w) => w.conn.name))]);
  showRecent();
}

/** File › Open Recent. */
function showRecent() {
  setRecent(launcher.recent().flatMap((n) => saved.filter((c) => c.name === n)).map((c) => ({ label: c.name, run: () => connect(c) })));
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
  /** The SQL editor's keys: CodeMirror's Sublime-like defaults, or Vim. */
  keyBinding: "sublime" | "vim";
  /** Completions write keywords in upper case. */
  upperKeywords: boolean;
  /** The key that accepts a completion. */
  completeKey: CompleteKey;
  /** A new filter row's column and operator, and whether a table opens with the filter bar showing. */
  filterColumn: DefaultColumn; filterOp: Op; filterBar: boolean;
  wrapLines: boolean;
  showInvisibles: boolean;
  /** Save each connection's SQL tabs and reopen them on connect. */
  keepTabs: boolean;
  /** Several result sets: a tab each, or all appended in one. */
  resultsMode: "tabs" | "append";
  /** Running several statements stops at the first that fails. */
  stopOnError: boolean;
  /** How many years of query history to keep; 0 = forever. */
  historyYears: number;
  /** Rebound shortcuts, command id → keys (see commands.ts); a missing id keeps its defaults. */
  shortcuts: Record<string, string[]>;
  launcherSort: "name" | "kind" | "tag";
  /** Reconnect, at startup, the workspaces open at quit. */
  reopenWorkspaces: boolean;
  /** Rows on a table tab's page. */
  pageSize: number;
  /** Tables the catalog estimates at this many rows or more show the estimate instead of a count; 0 = always count. */
  estimateOver: number;
  /** The sort a table tab opens with. */
  defaultSort: "none" | "asc" | "desc";
  /** PostGIS geometry and geography as WKT rather than hex WKB. */
  geometryWkt: boolean;
  /** Overlay scroll bars on the grid, shown while scrolling. */
  hideScrollbars: boolean;
  /** Write the values updated and deleted rows had to the console log when saving. */
  logOriginals: boolean;
  /** Show an export's files in Finder once it finishes. */
  revealExport: boolean;
  closeBrackets: boolean;
};
const DEFAULT_SETTINGS: Settings = {
  theme: "auto", zebra: true, fontSize: 13, lineHeight: 1.5, indent: "2", safeLevel: "alert2", queryTimeout: 300, keepAlive: true,
  sqlLimit: 1000, formatStyle: "standard", paramFormat: "colon", wrapLines: false, showInvisibles: false, keepTabs: true,
  keyBinding: "sublime", upperKeywords: false, completeKey: "enter", filterColumn: "any", filterOp: "contains", filterBar: false,
  shortcuts: {}, launcherSort: "name", reopenWorkspaces: false,
  resultsMode: "tabs", stopOnError: true, historyYears: 0,
  pageSize: 300, estimateOver: 1_000_000, defaultSort: "none", geometryWkt: true, hideScrollbars: false, logOriginals: false,
  revealExport: false, closeBrackets: true,
};
let settings: Settings = loadSettings();

/** Kept with the other preferences in settings.json (prefs.ts). */
function loadSettings(): Settings {
  const stored = { ...readPref<Record<string, unknown>>("settings") };
  if (typeof stored.safeDefault === "boolean" && !stored.safeLevel) stored.safeLevel = stored.safeDefault ? "alert2" : "silent"; // before levels
  delete stored.safeDefault;
  return { ...DEFAULT_SETTINGS, ...stored };
}

function setSetting<K extends keyof Settings>(key: K, value: Settings[K]) {
  settings[key] = value;
  writePref("settings", settings);
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
  $<HTMLInputElement>("reopen-workspaces").checked = settings.reopenWorkspaces;
  $<HTMLSelectElement>("conn-sort").value = settings.launcherSort;
  $<HTMLSelectElement>("results-mode").value = settings.resultsMode;
  $<HTMLInputElement>("stop-on-error").checked = settings.stopOnError;
  $<HTMLSelectElement>("history-years").value = String(settings.historyYears);
  $<HTMLInputElement>("page-size").value = String(settings.pageSize);
  $<HTMLInputElement>("estimate-over").value = String(settings.estimateOver);
  $<HTMLSelectElement>("default-sort").value = settings.defaultSort;
  $<HTMLInputElement>("geometry-wkt").checked = settings.geometryWkt;
  $<HTMLInputElement>("hide-scrollbars").checked = settings.hideScrollbars;
  $<HTMLInputElement>("log-originals").checked = settings.logOriginals;
  $<HTMLInputElement>("reveal-export").checked = settings.revealExport;
  $<HTMLInputElement>("close-brackets").checked = settings.closeBrackets;
  document.body.classList.toggle("hide-scrollbars", settings.hideScrollbars);
  const tab = current();
  if (tab?.kind === "sql" && !$("workspace").hidden) showResults(tab);
  editor.setDisplay(settings.wrapLines, settings.showInvisibles);
  $<HTMLSelectElement>("key-binding").value = settings.keyBinding;
  $<HTMLInputElement>("upper-keywords").checked = settings.upperKeywords;
  $<HTMLSelectElement>("complete-key").value = settings.completeKey;
  $<HTMLSelectElement>("filter-column").value = settings.filterColumn;
  $<HTMLSelectElement>("filter-op").value = settings.filterOp;
  $<HTMLInputElement>("filter-bar").checked = settings.filterBar;
  editor.setCompletion(settings.upperKeywords, settings.completeKey);
  editor.setCloseBrackets(settings.closeBrackets);
  editor.setKeyBinding(settings.keyBinding).catch((e) => status(`Vim keys: ${e}`, "error"));
  setFilterDefaults({ column: settings.filterColumn, op: settings.filterOp });
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
$<HTMLSelectElement>("key-binding").onchange = (e) => setSetting("keyBinding", (e.target as HTMLSelectElement).value as Settings["keyBinding"]);
$<HTMLInputElement>("upper-keywords").onchange = (e) => setSetting("upperKeywords", (e.target as HTMLInputElement).checked);
$<HTMLSelectElement>("complete-key").onchange = (e) => setSetting("completeKey", (e.target as HTMLSelectElement).value as CompleteKey);
$("filter-op").replaceChildren(...OPS.filter(([op]) => op !== "raw").map(([op, label]) => el("option", { value: op, textContent: label })));
$<HTMLSelectElement>("filter-column").onchange = (e) => setSetting("filterColumn", (e.target as HTMLSelectElement).value as DefaultColumn);
$<HTMLSelectElement>("filter-op").onchange = (e) => setSetting("filterOp", (e.target as HTMLSelectElement).value as Op);
$<HTMLInputElement>("filter-bar").onchange = (e) => setSetting("filterBar", (e.target as HTMLInputElement).checked);
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
  const first = $("conns").querySelector("li:not(.conn-section)");
  if (first) (first as HTMLElement).click();
});
$("engine-search").addEventListener("input", renderEngines);
$("engine-search").addEventListener("keydown", (e) => e.key === "Enter" && (e.preventDefault(), nextFromPicker()));
$("picker-next").onclick = nextFromPicker;
$("picker-cancel").onclick = () => $<HTMLDialogElement>("picker").close();
$("editor-cancel").onclick = () => $<HTMLDialogElement>("editor").close();
$("back").onclick = () => closeWorkspace();
$("filter").addEventListener("input", renderTables);

$("run").onclick = runCurrent;
$("run-all").onclick = runAll;
$("explain").onclick = async () => {
  const sql = editor.current();
  if (!sql.trim() || !active) return;
  const prefix = active.kind === "sqlite" ? "explain query plan " : "explain ";
  const filled = await withParams([prefix + sql]);
  if (filled) execute(filled);
};
$("format-sql").onclick = () => (formatSql(), editor.focus());
$<HTMLInputElement>("wrap-lines").onchange = (e) => setSetting("wrapLines", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("show-invisibles").onchange = (e) => setSetting("showInvisibles", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("keep-tabs").onchange = (e) => setSetting("keepTabs", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("reopen-workspaces").onchange = (e) => setSetting("reopenWorkspaces", (e.target as HTMLInputElement).checked);
$<HTMLSelectElement>("conn-sort").onchange = (e) => (setSetting("launcherSort", (e.target as HTMLSelectElement).value as Settings["launcherSort"]), renderConns());
$<HTMLSelectElement>("results-mode").onchange = (e) => setSetting("resultsMode", (e.target as HTMLSelectElement).value as Settings["resultsMode"]);
$<HTMLInputElement>("stop-on-error").onchange = (e) => setSetting("stopOnError", (e.target as HTMLInputElement).checked);
$<HTMLSelectElement>("history-years").onchange = (e) => (setSetting("historyYears", Number((e.target as HTMLSelectElement).value)), renderHistory());
$("clear-history").onclick = clearHistory;
initFavorites({
  conn: () => active && { name: active.name, kind: active.kind },
  open: openSqlIn,
  menu: contextMenu,
  confirm: confirmDialog,
  snippets: (list) => editor.setSnippets(list),
});
$<HTMLSelectElement>("param-format").onchange = (e) => setSetting("paramFormat", (e.target as HTMLSelectElement).value as Settings["paramFormat"]);
$<HTMLSelectElement>("format-style").onchange = (e) => setSetting("formatStyle", (e.target as HTMLSelectElement).value as FormatStyle);
$<HTMLSelectElement>("row-limit").onchange = (e) => setSetting("sqlLimit", Number((e.target as HTMLSelectElement).value));
$<HTMLInputElement>("page-size").onchange = (e) => setSetting("pageSize", clamp(Math.round(Number((e.target as HTMLInputElement).value)) || 300, 1, MAX_PAGE));
$<HTMLInputElement>("estimate-over").onchange = (e) => setSetting("estimateOver", Math.max(0, Math.round(Number((e.target as HTMLInputElement).value)) || 0));
$<HTMLSelectElement>("default-sort").onchange = (e) => setSetting("defaultSort", (e.target as HTMLSelectElement).value as Settings["defaultSort"]);
$<HTMLInputElement>("geometry-wkt").onchange = (e) => setSetting("geometryWkt", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("hide-scrollbars").onchange = (e) => setSetting("hideScrollbars", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("log-originals").onchange = (e) => setSetting("logOriginals", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("reveal-export").onchange = (e) => setSetting("revealExport", (e.target as HTMLInputElement).checked);
$<HTMLInputElement>("close-brackets").onchange = (e) => setSetting("closeBrackets", (e.target as HTMLInputElement).checked);

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
  { id: "import-url", label: "Import Connection from URL…", menu: "File", sep: true, run: () => (newWorkspace(), importFromUrl()) },
  { id: "import-connections", label: "Import Connections…", menu: "File", run: () => (newWorkspace(), launcher.importConnections(homeNote)) },
  { id: "export-connections", label: "Export Connections…", menu: "File",
    run: () => (newWorkspace(), launcher.exportConnections({ kind: "all" }, "all connections", homeNote)) },
  { id: "settings", label: "Settings…", menu: "File", keys: ["Mod+Comma"], overDialogs: true, sep: true, run: openSettings },
  { id: "run-current", label: "Run Current Query", menu: "Edit", keys: ["Mod+Enter"], when: onSqlTab, sep: true, run: runCurrent },
  { id: "run-all", label: "Run All Queries", menu: "Edit", keys: ["Mod+Shift+Enter"], when: onSqlTab, run: runAll },
  { id: "format", label: "Format SQL", menu: "Edit", keys: ["Mod+KeyI"], when: onSqlTab, run: formatSql },
  ...([
    ["upper", "Transform to Upper Case", ["Mod+Alt+KeyU"]], ["lower", "Transform to Lower Case", ["Mod+Alt+KeyL"]],
    ["capitalize", "Transform to Capitalized Case", ["Mod+Alt+KeyC"]], ["trim", "Trim Trailing Whitespace", []],
    ["spaces", "Convert Indentation to Spaces", []], ["tabs", "Convert Indentation to Tabs", []],
  ] as [Transform, string, string[]][]).map(([t, label, keys], i) => ({
    id: `transform-${t}`, label, menu: "Edit" as const, keys, when: onSqlTab, sep: i === 0, run: () => editor.transform(t),
  })),
  { id: "shift-right", label: "Shift Right", menu: "Edit", keys: ["Mod+BracketRight"], when: onSqlTab, run: () => editor.shift(true) },
  { id: "shift-left", label: "Shift Left", menu: "Edit", keys: ["Mod+BracketLeft"], when: onSqlTab, run: () => editor.shift(false) },
  { id: "save-favorite", label: "Save Query as Favorite…", menu: "Edit", when: onSqlTab,
    run: () => addFavorite(editor.current().trim() || (current() as SqlTab).sql) },
  { id: "saved-queries", label: "Saved Queries", menu: "Navigate", when: inWorkspace, run: () => showSide("queries") },
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
  const sideItems = active ? loadedItems(sessionKey()) : [];
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
    ...workspaces.filter((w) => w !== ws).map((w) => ({ label: w.conn.name, kind: "workspace", detail: w.info?.database ?? undefined, run: () => showWorkspace(w) })),
    ...commands
      .filter((c) => c.id !== "open-anything" && !c.hidden && (!c.when || c.when()))
      .map((c) => ({ label: c.label.replace(/…$/, ""), kind: "command", detail: keyLabel(c.id), run: c.run })),
  ];
}

const hasTools = () => inWorkspace() && !!active;
addCommands([
  { id: "backup", label: "Backup…", menu: "Tools", when: hasTools, run: () => openBackup(active!, info?.database) },
  { id: "restore", label: "Restore…", menu: "Tools", when: hasTools, run: () => openRestore(active!, info?.database, confirmDialog) },
  { id: "diagram", label: "Show Diagram", menu: "Tools", when: hasTools, run: () => showDiagram() },
  { id: "processes", label: "Process List", menu: "Tools", keys: ["Mod+Period"], when: () => hasTools() && active!.kind !== "sqlite",
    sep: true, run: () => openProcesses(active!, confirmDialog) },
  { id: "users", label: "Users…", menu: "Tools", when: () => hasTools() && active!.kind !== "sqlite", run: () => showUsers() },
  { id: "metrics-board", label: "Metrics Board", menu: "Tools", sep: true, when: hasTools, run: () => openBoard(boardCtx()) },
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

/** `.sqlite` files and database links opened from Finder or a browser, one form at a time. */
async function openFromOs() {
  const items = await invoke<string[]>("take_opened").catch(() => [] as string[]);
  for (const item of items) {
    await importFromUrl(item);
    const d = $<HTMLDialogElement>("editor");
    if (d.open) await new Promise((r) => d.addEventListener("close", r, { once: true }));
  }
}

refresh().then(async () => {
  showRecent();
  if (settings.reopenWorkspaces) {
    for (const name of launcher.openAtQuit()) {
      const c = saved.find((c) => c.name === name);
      if (c) await connect(c);
    }
  }
  listen("opened", openFromOs).catch(() => {}); // no event bridge outside the app
  openFromOs();
});
