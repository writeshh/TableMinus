import { invoke } from "./ipc";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { createEditor, type Schema } from "./editor/editor";
import { $, el } from "./dom";
import type { Dialect } from "./grid/copy";
import { dropDraft, initStructure, saveStructure, sidebarMenu, tableMenu } from "./database/structure";
import { openBackup, openRestore } from "./database/backup";
import { openProcesses } from "./database/processes";
import { openUsers } from "./database/users";
import "./assistant/mcp"; // adds Settings › MCP before the settings tabs are wired below
import { forgetSchema, initAssistant, renderPane as renderAssistant, toggleAssistant } from "./assistant/ai"; // and Settings › AI
import * as grid from "./grid/grid-edit";
import * as vgrid from "./grid/vgrid";
import { toggleRowDetail } from "./grid/rowdetail";
import { divider, initHiding, togglePane } from "./shell/splitters";
import { openViewer } from "./editor/viewer";
import "./shell/themes"; // adds the theme rows to Settings › Appearance
import "./shell/passcode"; // the lock screen, and its rows in Settings › Security
import { openScript } from "./extend/scripts";
import { initPlugins, pluginMenu } from "./extend/plugins"; // also adds Settings › Plugins
import { initRedis, syncRedis } from "./nosql/redis";
import { initMongo } from "./nosql/mongo";
import { initDynamo } from "./nosql/dynamo";
import { initElastic } from "./nosql/elastic";
import { initCassandra } from "./nosql/cassandra";
import { checkForUpdates, showReleaseNotes } from "./shell/updates";
import { docEntry, flushDocs, setDocEntry } from "./shell/locations"; // also adds Settings › Locations
import { exportDialog, importFile, type Ctx as TransferCtx, type ExportSource } from "./database/importexport";
import { addCommands, commands, isMac, onBindingsChanged, runCommand } from "./shell/commands";
import { openAnything, type Item as AnythingItem } from "./shell/openanything";
import { initMenu, refreshMenu } from "./shell/menu";
import {
  columnOrderMenu, forgetItems, initSearch, initSidebar, loadedItems, newItemsMenu, renderTree, shownSchema, sortColumns, treeMenu,
} from "./workspace/sidebar";
import type { Transform } from "./editor/transforms";
import { initDiagram, openDiagram } from "./database/diagram";
import * as launcher from "./connections/connections";
import { tagLabel } from "./connections/connections";
import "./shell/tip"; // instant tooltips on the icon buttons
import "./shell/console"; // the console log panel and its command
import { addFavorite, initFavorites, showSaved } from "./workspace/favorites"; // also adds the History pane's Saved queries
import "./grid/results"; // its stylesheet before workspace.css, as always
import type { QueryResult } from "./grid/results";
import "./workspace/workspace.css";
import "./metrics/charts"; // its stylesheet, before metrics.ts would bring it
import { openBoard, type BoardCtx } from "./metrics/metrics";
import { flushPrefs } from "./shell/prefs";
// Last, so the modules above (the Settings tabs among them) load in the order they always have.
import {
  allTabs, call, connectCommand, current, dbLabel, isRedis, keyLabel, loadColumns, note, okToDiscard, ownView, S, sessionKey, status,
  type Info, type Tab, type TableTab, type Workspace,
} from "./workspace/state";
import { clamp, initSettings, openSettings, setSetting, settings, showTab } from "./shell/settings";
import {
  closePane, closeWorkspace, connect, cyclePane, cycleWorkspace, focusPane, initWorkspaces, newWorkspace, reconnect,
  reconnectAll, show, showWorkspace, showWorkspaces, splitRight, type KeptTab,
} from "./workspace/workspaces";
import { execute, initRun, reloadResult, resultTarget, runAll, runCurrent, showResults, type SqlTab } from "./workspace/run";
import { confirmDialog, contextMenu, type MenuItem } from "./shell/dialogs";
import {
  applyLayout, browseRequest, initTableTabs, loadTable, openFound, openTable, redrawFilters, reloadTab,
  renderTableTab, showFilters, showPending, toggleFilters,
} from "./workspace/tabletab";
import {
  copySelection, copyText, drawn, fillCell, initGridView, paintSelection, selectedBlock, selectedRows, selection,
  setSelection, syncDetail,
} from "./grid/gridview";
import { findLocal, homeNote, initHome, refresh, saved, showRecent, toolsMenu } from "./connections/home";
import { dumps, hasProcesses, managesUsers } from "./connections/engines";
import { sqlKind } from "./editor/statements";
import { initHistory } from "./workspace/history";
import { initDbPicker, openDbPicker } from "./connections/dbpicker";
import { importFromUrl, initConnForm } from "./connections/connform";
import { DEFAULT_SETTINGS } from "./shell/settings-defaults";
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
// ---------- Workspace ----------

// The macOS window has no visible title bar (tauri.conf.json); the CSS makes room for the traffic lights, except in
// full screen, where they are hidden.
if (isMac && "__TAURI_INTERNALS__" in window) {
  document.body.classList.add("overlay-title");
  const win = getCurrentWindow();
  const syncFullScreen = () => void win.isFullscreen().then((f) => document.body.classList.toggle("full-screen", f), () => {});
  win.onResized(syncFullScreen).catch(() => {}); // a preview without Tauri has no window events
  syncFullScreen();
}
// ---------- SQL tabs kept per connection and database ----------

let saveTimer = 0; // a save waiting for typing to pause (saveSqlTabsSoon)

/** The workspace on screen's entry in sql_tabs: connection and database, or the connection alone without one. */
const tabsKey = () => (!S.active ? "" : S.info?.database ? `${S.active.name}\u001f${S.info.database}` : S.active.name);

const keptTabs = (key: string) => {
  const kept = docEntry<KeptTab[]>("sql_tabs", key);
  return Array.isArray(kept) ? kept.filter((k) => typeof k?.title === "string" && typeof k?.sql === "string") : [];
};

function loadSqlTabs(): KeptTab[] {
  if (!S.active) return [];
  const key = tabsKey();
  const kept = keptTabs(key);
  if (kept.length || key === S.active.name) return kept;
  // Kept before databases had their own (under the connection's name): the first database opened takes them over.
  const before = keptTabs(S.active.name);
  if (before.length) setDocEntry("sql_tabs", key, before), setDocEntry("sql_tabs", S.active.name, undefined);
  return before;
}

function saveSqlTabs() {
  clearTimeout(saveTimer);
  saveTimer = 0;
  if (!S.active || !settings.keepTabs) return;
  const kept: KeptTab[] = allTabs().flatMap((t) => (t.kind === "sql" && t.sql.trim() ? [{ title: t.title, sql: t.sql, path: t.path }] : []));
  setDocEntry("sql_tabs", tabsKey(), kept.length ? kept : undefined);
}

/** Saves now if typing left a save waiting: before the screen's workspace or database changes, and at quit. */
function flushSqlTabs() {
  if (saveTimer) saveSqlTabs();
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
    const tab: SqlTab = { id: S.nextId++, kind: "sql", title: fileName(path), sql, path };
    addTab(tab);
    saveSqlTabs();
  } catch (e) {
    status(String(e), "error");
  }
}

/** Saves the current SQL tab to its file; with `pick`, or when it has none yet, asks where first. */
async function saveSqlFile(pick: boolean) {
  const tab = current();
  if (tab?.kind !== "sql") return;
  const sql = S.shownId === tab.id ? editor.view.state.doc.toString() : tab.sql;
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

function saveSqlTabsSoon() {
  clearTimeout(saveTimer);
  saveTimer = window.setTimeout(saveSqlTabs, 400);
}
// ---------- Sidebar: tables (expandable to columns) and history ----------

/** The sidebar tree (sidebar.ts), drawing table rows with `tableRow`. */
function renderTables() {
  if (ownView()) return; // the Redis view lists keys instead (redis.ts), the MongoDB one collections (mongo.ts)
  const shown = S.active ? renderTree($("tables"), $<HTMLInputElement>("filter").value) : 0;
  const hint = $("no-tables");
  hint.hidden = shown > 0;
  hint.textContent =
    S.info?.can_switch && !S.info.database ? `No database selected. Press ${keyLabel("open-database")} to choose one.`
    : S.tables.length ? "No tables match."
    : "This database has no tables.";
}

/** A table's row in the sidebar: click opens it, the chevron shows its columns. `first` and `last` go around its
 *  menu. */
function tableRow(t: string, label: string, first: MenuItem[], last: MenuItem[]) {
  const open = current()?.kind === "table" ? (current() as TableTab).table : null;
  const isOpen = S.expanded.has(t);
  const chevron = el("button", { className: "chevron", textContent: "›", title: isOpen ? "Hide columns" : "Show columns" });
  chevron.setAttribute("aria-expanded", String(isOpen));
  chevron.onclick = async (e) => {
    e.stopPropagation();
    if (isOpen) S.expanded.delete(t);
    else {
      S.expanded.add(t);
      try {
        await loadColumns(t);
      } catch (err) {
        S.expanded.delete(t);
        status(String(err), "error");
      }
    }
    renderTables();
  };
  const row = el("div", { className: "tree-row", title: t }, chevron, el("span", { className: "tbl-icon" }), el("span", { className: "label", textContent: label }));
  row.classList.toggle("active", t === open);
  row.onclick = () => openTable(t);
  row.oncontextmenu = (e) => (e.stopPropagation(),
    contextMenu(e, [...first, "-", ...tableMenu(t), "-", ...transferMenu(t), "-", ["Show diagram", () => showDiagram(t)],
      ...pluginMenu("table", () => ({ table: t, engine: S.active?.kind ?? null })), ...last]));
  const li = el("li", {}, row);
  if (isOpen)
    li.append(
      el("ul", { className: "cols", oncontextmenu: (e: MouseEvent) => (e.stopPropagation(), contextMenu(e, columnOrderMenu())) },
        ...sortColumns(S.columnsCache.get(t) ?? []).map((c) =>
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
  for (const p of document.querySelectorAll<HTMLElement>("aside > .side-pane")) p.hidden = p.id !== `side-${which}`;
  if (which === "search") $("db-search-text").focus();
}
// ---------- Tabs: SQL editors and table browsers ----------

let navigating = false; // Back/Forward selects without recording the move

function openSqlTab(): SqlTab {
  // Lowest free number, so closing tabs doesn't make the names climb forever.
  const used = new Set(S.tabs.map((t) => t.title));
  const name = (n: number) => (n === 1 ? "SQL Query" : `SQL Query ${n}`);
  let n = 1;
  while (used.has(name(n))) n++;
  const tab: SqlTab = { id: S.nextId++, kind: "sql", title: name(n), sql: "" };
  addTab(tab);
  return tab;
}

/** ⌘T: a SQL tab, or an empty one (Settings › General). */
function newTab() {
  if (settings.newTabKind === "sql") return (openSqlTab(), editor.focus());
  if (current()?.kind === "empty") return;
  addTab({ id: S.nextId++, kind: "empty", title: "New Tab" });
}

/** Adds and selects a tab; an empty tab showing is replaced by it, in its place. */
function addTab(tab: Tab) {
  const i = S.tabs.findIndex((t) => t.id === S.currentId && t.kind === "empty");
  if (i < 0) S.tabs.push(tab);
  else S.tabs.splice(i, 1, tab);
  select(tab.id);
}

/** A new SQL tab holding `sql`, named `title` (or the next free "SQL Query n"). */
function openSqlIn(title: string | null, sql: string) {
  const tab = openSqlTab();
  if (title) tab.title = title;
  tab.sql = sql;
  tab.state = editor.newState(sql);
  S.shownId = 0; // force renderMain to load the new state
  renderMain();
  editor.focus();
}

/** Keeps the editor's text and undo history with the SQL tab that's showing. */
function stashEditor() {
  const t = current();
  if (t?.kind === "sql" && S.shownId === t.id) t.state = editor.view.state;
}

function select(id: number) {
  stashEditor();
  if (!navigating && S.currentId && id !== S.currentId && S.tabs.some((t) => t.id === S.currentId)) {
    S.pane.back = [...S.pane.back, S.currentId].slice(-50);
    S.pane.forward = [];
  }
  S.currentId = id;
  renderMain();
  renderTables();
}

/** Closes a tab; `remember` keeps it for Reopen Closed Tab (not for a table that was just dropped). */
async function closeTab(id: number, remember = true) {
  const i = S.tabs.findIndex((t) => t.id === id);
  if (i < 0 || !(await okToDiscard(S.tabs[i]))) return;
  if (id === S.currentId) stashEditor();
  if (remember) S.pane.closed = [...S.pane.closed, { tab: S.tabs[i], index: i }].slice(-20);
  S.tabs.splice(i, 1);
  saveSqlTabs();
  if (!S.tabs.length && (S.ws?.panes.length ?? 1) > 1) return closePane();
  // Last tab gone: unlike TablePlus, stay connected with no tab open. ⌘W again leaves (closeCurrent).
  if (!S.tabs.length) return void ((S.currentId = 0), renderMain(), renderTables());
  if (id === S.currentId) select(S.tabs[Math.max(0, i - 1)].id);
  else renderMain();
}

/** ⌘W: the current tab; with none open, the empty pane, then the workspace and its connection. */
function closeCurrent() {
  if (S.tabs.length) return closeTab(S.currentId);
  if ((S.ws?.panes.length ?? 1) > 1) return closePane();
  return closeWorkspace();
}

/** Every other tab closed, each asking first if it has unsaved changes. */
async function closeOtherTabs(id: number) {
  for (const t of S.tabs.filter((t) => t.id !== id)) if (!(await okToDiscard(t))) return;
  S.pane.closed = [...S.pane.closed, ...S.tabs.filter((t) => t.id !== id).map((tab) => ({ tab, index: 0 }))].slice(-20);
  S.tabs = S.tabs.filter((t) => t.id === id);
  saveSqlTabs();
  select(id);
}

function reopenClosedTab() {
  const last = S.pane.closed.at(-1);
  if (!last) return;
  S.pane.closed = S.pane.closed.slice(0, -1);
  S.tabs.splice(Math.min(last.index, S.tabs.length), 0, last.tab);
  saveSqlTabs();
  select(last.tab.id);
  if (last.tab.kind === "table" && last.tab.view === "data") loadTable(last.tab);
}

/** Tab n (1-based); 9 is the last tab, as in browsers. */
function selectTabAt(n: number) {
  const t = n === 9 ? S.tabs.at(-1) : S.tabs[n - 1];
  if (t) select(t.id);
}

function cycleTab(step: number) {
  const i = S.tabs.findIndex((t) => t.id === S.currentId);
  if (S.tabs.length > 1) select(S.tabs[(i + step + S.tabs.length) % S.tabs.length].id);
}

/** Back and Forward through the tabs opened in this pane; closed ones are skipped. */
function goBack(forward: boolean) {
  const [from, to] = forward ? [S.pane.forward, S.pane.back] : [S.pane.back, S.pane.forward];
  while (from.length) {
    const id = from.pop()!;
    if (!S.tabs.some((t) => t.id === id) || id === S.currentId) continue;
    to.push(S.currentId);
    navigating = true;
    select(id);
    navigating = false;
    return;
  }
}

/** Swaps the tab's label for a text field; Enter keeps the new name, Esc or an empty name keeps the old one. */
function renameTab(id: number) {
  const t = S.tabs.find((t) => t.id === id);
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
    ...(S.pane.closed.length ? [["Reopen Closed Tab", reopenClosedTab] as MenuItem] : []),
  ];
}

function renderMain() {
  refreshMenu(); // the tab decides which items apply (Save needs a SQL tab)
  const tab = current();
  $("tabs").replaceChildren(
    ...S.tabs.map((t) => {
      const close = el("span", { className: "close", textContent: "×", title: "Close tab" });
      close.onclick = (e) => (e.stopPropagation(), closeTab(t.id));
      const b = el("button", { className: `tab ${t.kind}`, title: t.kind === "sql" && t.path ? t.path : t.title },
        ...(t.kind === "empty" ? [] : [el("span", { className: t.kind === "sql" ? "tab-sql" : "tbl-icon", textContent: t.kind === "sql" ? "SQL" : "" })]),
        el("span", { className: "label", textContent: t.title }),
        close,
      );
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(t.id === S.currentId));
      b.dataset.tab = String(t.id);
      b.onclick = () => select(t.id);
      b.onauxclick = (e) => e.button === 1 && closeTab(t.id); // middle-click closes
      b.ondblclick = () => renameTab(t.id);
      b.oncontextmenu = (e) => contextMenu(e, tabMenu(t.id));
      return b;
    }),
  );
  $("tabs").querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });

  const env = S.active?.tag ? `${tagLabel(S.active.tag)} | ` : "";
  $("pill").textContent = `${env}${S.info?.server ?? ""}${S.info?.database ? ` : ${dbLabel(S.info.database)}` : ""}${tab && !ownView() ? ` : ${tab.title}` : ""}`;
  $("pill").className = `pill ${S.active?.tag ?? ""}`;
  if (tab?.kind === "empty") return showNoTab(true);
  if (!tab) return showNoTab();

  $("sql-pane").hidden = tab.kind !== "sql";
  $("table-foot").hidden = tab.kind !== "table";
  showFilters(tab);
  applyLayout(tab);
  if (tab.kind === "sql") {
    if (S.shownId !== tab.id) {
      editor.setState((tab.state ??= editor.newState(tab.sql)));
      S.shownId = tab.id;
    }
    updateCursor();
    status(...(tab.status ?? ["", ""]));
    showResults(tab);
    syncDetail();
    return;
  }
  renderTableTab(tab);
  syncDetail();
}

/** The workspace with every tab closed (or an empty tab showing): still connected, saying how to go on or leave. */
function showNoTab(emptyTab = false) {
  for (const id of ["sql-pane", "table-foot", "result-strip", "filters"]) $(id).hidden = true;
  redrawFilters();
  setSelection(null);
  syncDetail();
  $("out").replaceChildren(el("p", {
    className: "hint empty",
    textContent: emptyTab ? "Open a table from the sidebar, or a SQL query, and it opens in this tab."
      : `No tabs open. Open a table from the sidebar, or press ${keyLabel("new-tab")} for a new tab. ${keyLabel("close-tab")} closes the connection.`,
  }));
}
// ---------- Inline cell editing (table tabs) ----------

async function saveEdits() {
  const tab = current();
  if (tab?.kind === "table" && tab.view === "structure") return saveStructure(tab.id, $("out"));
  if (tab?.kind === "table") return grid.save(tab);
  const target = resultTarget();
  if (target) return grid.save(target);
}
// ---------- Filters and sorting (table tabs) ----------

$("tools-button").onclick = () => {
  const r = $("tools-button").getBoundingClientRect(); // under the button, also when opened from the keyboard
  if (!S.active) return;
  const items: MenuItem[] = toolsMenu(S.active, S.info?.database);
  if (managesUsers(S.active)) items.push(["Users…", showUsers]); // needs the workspace's connection, so not on launcher rows
  items.push("-", ["Metrics Board", () => openBoard(boardCtx())], ["Run Custom Script…", showScript]);
  contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), items);
};

/** The custom script window on the workspace's session (scripts.ts). */
const showScript = () => void openScript({
  conn: hostConn, database: () => S.info?.database ?? null, call,
  confirm: (sql) => confirmDialog(`Run this from the script? ${sql.length > 200 ? sql.slice(0, 200) + "…" : sql}`, "Run"),
});

/** The user manager on the workspace's session (users.ts). */
const showUsers = () => S.active && openUsers({ conn: { name: sessionKey(), kind: S.active.kind }, call, confirm: confirmDialog });

/** The sidebar's items again (new or dropped tables), with columns and autocomplete reloaded. */
async function reloadItems() {
  if (isRedis()) return syncRedis(true);
  const i = await call<Info>(connectCommand(S.active!.kind), { name: sessionKey() }); // reuses the open connection
  forgetItems(sessionKey());
  S.info = i;
  S.tables = i.tables;
  S.columnsCache.clear();
  renderTables();
  loadSchema();
}

/** Columns for every table, for autocomplete. Runs in the background; the editor works without it meanwhile. */
async function loadSchema() {
  forgetSchema(); // the assistant's copy, with types, is fetched again when next needed
  renderAssistant();
  if (!S.active || ownView()) return;
  const [conn, key] = [S.active, sessionKey()];
  try {
    const pairs = await invoke<[string, string][]>("schema", { name: key });
    if (sessionKey() !== key) return; // switched workspaces meanwhile
    const schema: Schema = new Map(S.tables.map((t) => [t, []]));
    for (const [table, column] of pairs) (schema.get(table) ?? schema.set(table, []).get(table)!).push(column);
    editor.setDialect(conn.kind, schema);
  } catch {} // autocomplete just keeps table names
}
// ---------- Column layout (table tabs) ----------

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
    setSelection({ r, anchor: [0, 0], focus: [r.rows.length - 1, r.columns.length - 1], table });
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
  // The structure view's fields sit inside the results area too: a space typed there is text.
  if (a instanceof HTMLElement && (a.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(a.tagName))) return;
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
$("save-edits").onclick = () => saveEdits();

/** The on-screen connection for the modules below: `name` is the session key they send (shown via `connName`). */
const hostConn = () => S.active && { ...S.active, name: sessionKey() };
$("result-save").onclick = () => saveEdits();

/** What a metrics board needs from the workspace on screen; see metrics.ts. */
function boardCtx(): BoardCtx {
  const session = (w: Workspace) => ({
    key: w.key, label: w.info?.database ? `${w.conn.name} · ${w.info.database}` : w.conn.name, dialect: sqlKind(w.conn.kind) as Dialect,
  });
  return {
    conn: S.active!.name, session: session(S.ws!), sessions: () => S.workspaces.map(session),
    format: settings.paramFormat === "off" ? "colon" : settings.paramFormat,
    confirm: confirmDialog, transfer: transferCtx,
  };
}
grid.initGridEdit({
  call,
  conn: hostConn,
  columns: (table) => S.columnsCache.get(table),
  render: renderMain,
  reload: (tab) => ("kind" in tab ? loadTable(tab as TableTab) : reloadResult(tab)),
  confirm: confirmDialog,
  fill: fillCell,
  logOriginals: () => settings.logOriginals,
});
$("side-items").oncontextmenu = (e) =>
  S.active && contextMenu(e, [...sidebarMenu(), "-", ...newItemsMenu(), "-", ["Show diagram", () => showDiagram()], "-", ...treeMenu()]);
/** The ER diagram of the sidebar's schema, or of `table`'s schema centred on it. */
function showDiagram(table?: string) {
  const pg = S.active?.kind === "postgres";
  openDiagram(pg ? (table?.includes(".") ? table.split(".")[0] : shownSchema()) : null, table);
}
initDiagram({ call, conn: hostConn, openTable: (t) => openTable(t), menu: contextMenu });
initRedis({ conn: hostConn, call, confirm: confirmDialog, menu: contextMenu });
const docHost = {
  conn: hostConn, call, confirm: confirmDialog, menu: contextMenu,
  collections: () => S.tables,
  reload: reloadItems,
  limit: () => settings.sqlLimit || 50_000,
};
initMongo(docHost);
initDynamo(docHost);
initElastic(docHost);
initCassandra(docHost);
initPlugins({
  conn: hostConn,
  insert(sql) {
    if (current()?.kind === "sql") editor.insert(sql);
    else openSqlIn(null, sql);
  },
  copy: (text, what) => void copyText(text, what),
  say: (text, kind = "done") => note(text, kind),
});
initAssistant({
  conn: hostConn,
  server: () => S.info?.server ?? "",
  schema: () => (S.active?.kind === "postgres" ? shownSchema() : null),
  call,
  insert(sql) {
    if (current()?.kind === "sql") editor.insert(sql);
    else openSqlIn(null, sql);
  },
  openSettings: () => openSettings(),
});
initSidebar({
  conn: hostConn,
  tables: () => S.tables,
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
  openFound,
  menu: contextMenu,
  redraw: () => renderTables(),
});
initSearch();
initStructure({
  call,
  conn: hostConn,
  tables: () => S.tables,
  refreshTables: reloadItems,
  openStructure: (table, create) => openTable(table, "structure", create),
  forgetTable(table) {
    for (const t of S.tabs.filter((t): t is TableTab => t.kind === "table" && t.table === table)) {
      t.staged.clear();
      dropDraft(t.id);
      closeTab(t.id, false);
    }
  },
  retarget(id, table) {
    const t = S.tabs.find((t): t is TableTab => t.id === id && t.kind === "table");
    if (!t) return;
    S.columnsCache.delete(t.table);
    Object.assign(t, { table, title: table, create: false, result: undefined, total: undefined });
    if (id === S.currentId) renderMain();
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
    database: S.info?.database ?? "",
    tables: S.tables,
    columns: async (table) => (await loadColumns(table)).map((c) => c.name),
    confirm: confirmDialog,
    revealExports: () => settings.revealExport,
    imported: async (table) => {
      // A new table appears in the sidebar; an open tab of the table shows the new rows.
      try {
        S.tables = (await call<Info>("connect", { name: sessionKey() })).tables;
        if (S.info) S.info.tables = S.tables;
        S.columnsCache.delete(table);
        renderTables();
        for (const t of S.tabs) if (t.kind === "table" && t.table === table) (t.total = undefined), loadTable(t);
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
  if (tab?.kind !== "table" || !S.active) return;
  const request = { ...browseRequest(tab), limit: settings.pageSize, offset: tab.offset, count: false, wkt: settings.geometryWkt };
  const source: ExportSource = { kind: "rows", request, pageRows: tab.result?.rows.length ?? 0 };
  exportDialog(transferCtx(), source);
};
// ---------- The modules split out of this file ----------

initGridView();
initRun({ editor, boardCtx, transferCtx, renderMain });
initTableTabs({ renderMain, select, addTab, reloadItems });
initHistory({
  load(sql) {
    const tab = current()?.kind === "sql" ? (current() as SqlTab) : openSqlTab();
    tab.sql = sql;
    tab.state = editor.newState(sql);
    S.shownId = 0; // force renderMain to load the new state
    renderMain();
    editor.focus();
  },
  openNew: (sql) => openSqlIn(null, sql),
});
initWorkspaces({
  editor, loadSchema, renderTables, renderMain, select, openSqlTab, stashEditor, loadSqlTabs, flushSqlTabs, showSide,
});
initDbPicker({ editor, stashEditor, saveSqlTabs });
initHome();
initConnForm();
initSettings({ editor });

// ---------- Wiring ----------

$("back").onclick = newWorkspace; // the launcher; this workspace stays connected in its tab
$("filter").addEventListener("input", renderTables);
$("format-sql").onclick = () => (formatSql(), editor.focus());
initFavorites({
  conn: () => S.active && { name: S.active.name, kind: S.active.kind },
  open: openSqlIn,
  menu: contextMenu,
  confirm: confirmDialog,
  snippets: (list) => editor.setSnippets(list),
});

// ---------- Commands (commands.ts): shortcuts, and later the menu bar and Open Anything ----------

const inWorkspace = () => !$("workspace").hidden;
const onSqlTab = () => inWorkspace() && current()?.kind === "sql";
const fontBy = (size: number) => setSetting("fontSize", clamp(size, 9, 28));

addCommands([
  { id: "new-tab", label: "New Tab", menu: "File", keys: ["Mod+KeyT"], when: inWorkspace, run: newTab },
  { id: "reopen-tab", label: "Reopen Closed Tab", menu: "File", keys: ["Mod+Shift+KeyT"], when: () => inWorkspace() && !!S.pane.closed.length,
    run: reopenClosedTab },
  { id: "open-file", label: "Open…", menu: "File", keys: ["Mod+KeyO"], when: inWorkspace, run: openSqlFile },
  { id: "save", label: "Save", menu: "File", keys: ["Mod+KeyS"], when: inWorkspace, sep: true,
    run: () => (current()?.kind === "sql" ? saveSqlFile(false) : saveEdits()) },
  { id: "save-as", label: "Save As…", menu: "File", keys: ["Mod+Shift+KeyS"], when: inWorkspace,
    run: () => (current()?.kind === "sql" ? saveSqlFile(true) : saveEdits()) },
  // On the launcher there's no tab: close the window, like other apps.
  { id: "close-tab", label: "Close Tab", menu: "File", keys: ["Mod+KeyW"], sep: true,
    run: () => (inWorkspace() ? closeCurrent() : getCurrentWindow().close()) },
  { id: "close-other-tabs", label: "Close Other Tabs", menu: "File", when: () => inWorkspace() && S.tabs.length > 1,
    run: () => closeOtherTabs(S.currentId) },
  { id: "rename-tab", label: "Rename Tab…", menu: "File", when: inWorkspace, run: () => renameTab(S.currentId) },
  { id: "sql-viewer", label: "New SQL Viewer", menu: "File", sep: true, run: () => openViewer(confirmDialog) },
  { id: "find-local", label: "Find Local Databases…", menu: "File", run: () => (newWorkspace(), findLocal()) },
  { id: "import-url", label: "Import Connection from URL…", menu: "File", run: () => (newWorkspace(), importFromUrl()) },
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
  { id: "saved-queries", label: "Saved Queries", menu: "Navigate", when: inWorkspace, run: () => (showSide("history"), showSaved()) },
  { id: "filter", label: "Filter Rows", menu: "Edit", keys: ["Mod+KeyF"], when: () => inWorkspace() && current()?.kind === "table",
    run: () => toggleFilters(true) },
  { id: "toggle-sidebar", label: "Toggle Left Sidebar", menu: "View", keys: ["Alt+Mod+Digit1"], when: inWorkspace, run: () => togglePane("sidebar") },
  { id: "toggle-row-detail", label: "Toggle Row Detail", menu: "View", keys: ["Alt+Mod+Digit2"], when: inWorkspace, run: toggleRowDetail },
  { id: "toggle-results", label: "Toggle Results Pane", menu: "View", keys: ["Alt+Mod+Digit3"], when: inWorkspace, run: () => togglePane("results") },
  { id: "toggle-assistant", label: "Toggle Assistant", menu: "View", keys: ["Alt+Mod+Digit4"], when: inWorkspace, run: toggleAssistant },
  { id: "font-bigger", sep: true, label: "Increase Font Size", menu: "View", keys: ["Mod+Equal", "Mod+Shift+Equal"], when: inWorkspace,
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
  { id: "reconnect-all", label: "Reconnect All Workspaces", menu: "Connection", when: () => S.workspaces.length > 0, run: reconnectAll },
  { id: "disconnect", label: "Disconnect", menu: "Connection", keys: ["Alt+Mod+Period"], when: inWorkspace, sep: true, run: closeWorkspace },
  { id: "new-workspace", label: "New Workspace", menu: "File", keys: ["Mod+KeyN"], when: inWorkspace, run: newWorkspace },
  { id: "reload-workspace", label: "Reload Workspace", menu: "View", keys: ["Alt+Mod+KeyR"], when: inWorkspace,
    run: async () => (await reloadItems(), current()?.kind === "table" && reloadTab()) },
  { id: "split-right", label: "Split Pane Right", menu: "Window", keys: ["Mod+Shift+KeyD"], when: inWorkspace, sep: true, run: splitRight },
  { id: "next-pane", label: "Select Next Pane", menu: "Window", keys: ["Alt+Mod+BracketRight"], when: () => (S.ws?.panes.length ?? 0) > 1,
    run: () => cyclePane(1) },
  { id: "previous-pane", label: "Select Previous Pane", menu: "Window", keys: ["Alt+Mod+BracketLeft"], when: () => (S.ws?.panes.length ?? 0) > 1,
    run: () => cyclePane(-1) },
  { id: "show-workspaces", label: "Show All Workspaces", menu: "Window", when: inWorkspace, sep: true, run: showWorkspaces },
  { id: "next-workspace", label: "Select Next Workspace", menu: "Window", keys: ["Ctrl+Mod+BracketRight"], when: () => S.workspaces.length > 1,
    run: () => cycleWorkspace(1) },
  { id: "previous-workspace", label: "Select Previous Workspace", menu: "Window", keys: ["Ctrl+Mod+BracketLeft"],
    when: () => S.workspaces.length > 1, run: () => cycleWorkspace(-1) },
]);

const ITEM_KINDS = { schema: "schema", view: "view", matview: "materialized view", function: "function", procedure: "procedure", sequence: "sequence", type: "type" };

/** What Open Anything searches: the workspace's items and SQL tabs, the other workspaces, and every command that
 *  applies right now. */
function anythingItems(): AnythingItem[] {
  const sideItems = S.active ? loadedItems(sessionKey()) : [];
  const kinds = new Set(sideItems.map((i) => i.name));
  const sqlTabs = (S.ws?.panes ?? [S.pane]).flatMap((p) =>
    (p === S.pane ? S.tabs : p.tabs).flatMap((t) => (t.kind === "sql" ? [{ p, t }] : [])),
  );
  return [
    ...S.tables.filter((t) => !kinds.has(t)).map((t) => ({ label: t, kind: "table", run: () => openTable(t) })),
    ...sideItems.filter((i) => i.kind !== "schema").map((i) => ({
      label: i.name, kind: ITEM_KINDS[i.kind],
      run: () => (i.kind === "view" || i.kind === "matview" ? openTable(i.name) : navigator.clipboard.writeText(i.name).then(() => note(`Copied ${i.name}`, "done"), () => {})),
    })),
    ...sqlTabs.map(({ p, t }) => ({ label: t.title, kind: "SQL tab", detail: t.path, run: () => (focusPane(p), select(t.id)) })),
    ...S.workspaces.filter((w) => w !== S.ws).map((w) => ({ label: w.conn.name, kind: "workspace", detail: w.info?.database ?? undefined, run: () => showWorkspace(w) })),
    ...commands
      .filter((c) => c.id !== "open-anything" && !c.hidden && (!c.when || c.when()))
      .map((c) => ({ label: c.label.replace(/…$/, ""), kind: "command", detail: keyLabel(c.id), run: c.run })),
  ];
}

const hasTools = () => inWorkspace() && !!S.active;
addCommands([
  { id: "backup", label: "Backup…", menu: "Tools", when: () => hasTools() && dumps(S.active!), run: () => openBackup(S.active!, S.info?.database) },
  { id: "restore", label: "Restore…", menu: "Tools", when: () => hasTools() && dumps(S.active!), run: () => openRestore(S.active!, S.info?.database, confirmDialog) },
  { id: "diagram", label: "Show Diagram", menu: "Tools", when: hasTools, run: () => showDiagram() },
  { id: "processes", label: "Process List", menu: "Tools", keys: ["Mod+Period"], when: () => hasTools() && hasProcesses(S.active!),
    sep: true, run: () => openProcesses(S.active!, confirmDialog) },
  { id: "users", label: "Users…", menu: "Tools", when: () => hasTools() && managesUsers(S.active!), run: () => showUsers() },
  { id: "metrics-board", label: "Metrics Board", menu: "Tools", sep: true, when: hasTools, run: () => openBoard(boardCtx()) },
  { id: "custom-script", label: "Run Custom Script…", menu: "Tools", when: hasTools, run: showScript },
  { id: "shortcuts", label: "Keyboard Shortcuts", menu: "Help", overDialogs: true, run: () => (openSettings(), showTab("shortcuts")) },
  { id: "check-updates", label: "Check for Updates…", menu: "Help", run: () => checkForUpdates({ beta: settings.betaUpdates, quiet: false }) },
  { id: "release-notes", label: "Release Notes", menu: "Help", run: showReleaseNotes },
]);
initMenu();
initHiding();
// The header's icon toolbar runs commands, so a button does what its menu item and shortcut do. Users only exists
// where there are users to manage.
for (const b of document.querySelectorAll<HTMLButtonElement>(".tool-icons [data-command]")) b.onclick = () => void runCommand(b.dataset.command!);
divider({
  host: $("workspace"), className: "side-edge", variable: "--side-w", pref: "layout.sidebar", axis: "x", sign: 1, min: 180,
  max: () => Math.min(560, window.innerWidth / 2), size: () => $("workspace").querySelector("aside")!.getBoundingClientRect().width,
  label: "Sidebar width",
});
divider({
  host: $("workspace"), className: "ai-edge", variable: "--ai-w", pref: "layout.assistant", axis: "x", sign: -1, min: 260,
  max: () => Math.min(720, window.innerWidth * 0.45), size: () => $("assistant").getBoundingClientRect().width,
  label: "Assistant width",
});
divider({
  host: $("row-detail").parentElement!, className: "row-detail-edge", variable: "--rd-w", pref: "layout.rowDetail", axis: "x", sign: -1, min: 220,
  max: () => Math.min(640, $("row-detail").parentElement!.getBoundingClientRect().width * 0.6), size: () => $("row-detail").getBoundingClientRect().width,
  label: "Row detail width",
});
divider({
  host: $("sql-pane"), className: "sql-edge", variable: "--sql-h", pref: "layout.editor", axis: "y", sign: 1, min: 120,
  max: () => $("live").getBoundingClientRect().height - 140, size: () => $("sql-pane").getBoundingClientRect().height,
  label: "Editor height",
});
// Closing the window (Quit closes it too, menu.ts) waits until the last typing is in the files. Tauri destroys the
// window once this handler returns.
if ("__TAURI_INTERNALS__" in window) {
  getCurrentWindow().onCloseRequested(async () => {
    stashEditor();
    flushSqlTabs();
    await Promise.all([flushDocs(), flushPrefs()]);
  }).catch(() => {}); // a preview without Tauri has no window events
}

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

/** `.sqlite` files and database links opened from Finder or a browser, one form at a time; `.sql` files open in the viewer. */
async function openFromOs() {
  const items = await invoke<string[]>("take_opened").catch(() => [] as string[]);
  for (const item of items) {
    if (/\.sql$/i.test(item)) {
      await openViewer(confirmDialog, item);
      continue;
    }
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
  // A moment after starting, so the check never slows the first connect.
  if (settings.autoUpdate) setTimeout(() => void checkForUpdates({ beta: settings.betaUpdates, quiet: true }), 5000);
});
