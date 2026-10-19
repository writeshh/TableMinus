// Workspaces: several connections (or databases of one) open in one window, each with its own session, sidebar and
// panes of tabs. The one on screen lives in `S` (state.ts); switching writes it back into its workspace and brings
// another's out. Also the split panes, and the header's connection button and info popover.

import { listen } from "@tauri-apps/api/event";
import { invoke } from "./ipc";
import { $, el, setStatus } from "./dom";
import type { Editor } from "./editor";
import {
  allTabs, call, connectCommand, dbLabel, keyLabel, newPane, okToDiscard, ownView, S, sessionKey, status, type Info, type Pane,
  type Tab, type Workspace,
} from "./state";
import { forgetRedis, syncRedis } from "./redis";
import { forgetMongo, syncMongo } from "./mongo";
import { forgetDynamo, syncDynamo } from "./dynamo";
import { forgetElastic, syncElastic } from "./elastic";
import { forgetCassandra, syncCassandra } from "./cassandra";
import { avatar, engineId, hostOf, isFile, managesUsers, type Saved } from "./engines";
import { docsReady } from "./locations";
import * as launcher from "./connections";
import { renderHistory } from "./history";
import { render as renderFavorites } from "./favorites";
import { showSafety } from "./safety";
import { openDbPicker } from "./dbpicker";
import { forgetItems } from "./sidebar";
import { settings } from "./settings";
import { colorDot, refresh, showRecent } from "./home";
import { redrawFilters } from "./tabletab";
import { refreshMenu } from "./menu";
import { selection, setSelection } from "./gridview";
import { contextMenu, type MenuItem } from "./dialogs";
import { freeKey, renderDbTabs } from "./workspace-tabs";

/** What switching workspaces needs of main.ts. */
type Host = {
  editor: Editor;
  /** Columns for autocomplete, in the background. */
  loadSchema(): Promise<void>;
  renderTables(): void;
  renderMain(): void;
  select(id: number): void;
  openSqlTab(): void;
  /** Keeps the editor's text and undo history with the SQL tab showing. */
  stashEditor(): void;
  /** The SQL tabs kept for the connection and database on screen. */
  loadSqlTabs(): KeptTab[];
  /** Saves the SQL tabs now if typing left a save waiting. */
  flushSqlTabs(): void;
  showSide(which: "items"): void;
};
let host: Host;

export function show(view: "home" | "workspace") {
  $("return-workspace").hidden = view !== "home" || !S.workspaces.length;
  $("home").hidden = view !== "home";
  $("workspace").hidden = view !== "workspace";
  renderDbTabs();
  refreshMenu();
  if (view === "home") $("search").focus();
}

/** Reopens the active connection, keeping the open tabs; the table list is refreshed. */
export async function reconnect() {
  if (!S.active) return;
  const btn = $<HTMLButtonElement>("reconnect");
  btn.disabled = true;
  status("Reconnecting…", "busy");
  try {
    // Redis and the document engines keep sessions of their own: drop it, and their connect opens a fresh one.
    if (ownView()) await invoke("disconnect", { name: sessionKey() });
    S.info = await invoke<Info>(ownView() ? connectCommand(S.active.kind) : "reconnect", { name: sessionKey(), ...(ownView() ? { database: S.info?.database } : {}) });
    $("lost").hidden = true;
    renderDbTabs();
    S.tables = S.info.tables;
    S.columnsCache.clear();
    host.renderTables();
    host.loadSchema();
    syncViews(true);
    status(`Reconnected · ${S.info.server}`, "done");
  } catch (e) {
    status(String(e), "error");
  } finally {
    btn.disabled = false;
  }
}

async function showConnectionInfo() {
  if (!S.active) return;
  const pop = $("conn-info");
  if (!pop.hidden) return void (pop.hidden = true);
  const row = (k: string, v: string) => [el("dt", { textContent: k }), el("dd", { textContent: v })];
  const list = $("conn-info-list");
  list.replaceChildren(...row("Connection", S.active.name), ...row("Server", "Checking…"));
  const r = $("pill").getBoundingClientRect();
  Object.assign(pop.style, { top: `${r.bottom + 6}px`, left: `${Math.max(8, r.right - 340)}px` });
  pop.hidden = false;
  try {
    const i = await call<{ server: string; driver: string; cipher: string | null; ssh: string | null }>(ownView() ? connectCommand(S.active.kind).replace("_connect", "_connection_info") : "connection_info", { name: sessionKey() });
    list.replaceChildren(
      ...row("Connection", S.active.name),
      ...row("Server", i.server),
      ...row("Driver", i.driver),
      ...row("Encryption", i.cipher ? i.cipher : isFile(S.active.kind) ? "Local file" : "Not encrypted"),
      ...(i.ssh ? row("Via SSH", i.ssh) : []),
    );
  } catch (e) {
    list.replaceChildren(...row("Connection", S.active.name), ...row("Error", String(e)));
  }
}

/** Always a new workspace tab, also when the connection is open already (a second database, or a second look). */
export async function connect(c: Saved) {
  setStatus("home-status", `Connecting to ${c.name}…`, "busy");
  try {
    await openWorkspace(c);
    setStatus("home-status", "");
  } catch (e) {
    setStatus("home-status", String(e), "error");
  }
}

const PICKER_DELAY_MS = 1000;

/** A new workspace on its own session of `c`, on `database` when given, in the tab after the current one. */
export async function openWorkspace(c: Saved, database?: string) {
  await docsReady; // kept tabs and history, read at startup
  const key = freeKey(c.name);
  const i = await launcher.withAskedPassword(c.name, () => invoke<Info>(connectCommand(c.kind), { name: key, database }));
  if (!i) return; // the password prompt was cancelled
  launcher.addRecent(c.name);
  if (c.needs_credentials) refresh(); // the backend took the badge off
  const at = S.ws && S.workspaces.includes(S.ws) ? S.workspaces.indexOf(S.ws) + 1 : S.workspaces.length;
  stashWorkspace();
  S.pane = newPane();
  S.ws = { key, conn: c, info: i, tables: i.tables, panes: [S.pane], pane: S.pane, columns: new Map(), expanded: new Set(), lost: false, filter: "" };
  S.workspaces.splice(at, 0, S.ws);
  rememberOpen();
  S.active = c;
  S.columnsCache = S.ws.columns;
  S.expanded = S.ws.expanded;
  $("lost").hidden = true;
  showHeader();
  show("workspace");
  applyInfo(i);
  host.showSide("items");
  renderHistory();
  renderFavorites();
  showSafety(c);
  status(""); // the header already says what is connected
  // Connected without a database: offer the picker once the workspace has been seen, unless something else got there first.
  const opened = S.ws;
  if (i.can_switch && !i.database) setTimeout(() => S.ws === opened && !$<HTMLDialogElement>("dbpicker").open && !opened.info?.database && openDbPicker(), PICKER_DELAY_MS);
  else host.editor.focus();
}

/** The views that stand in for tables and SQL tabs: each shows itself when the workspace is of its engine. */
function syncViews(fresh = false) {
  syncRedis(fresh);
  syncMongo(fresh);
  syncDynamo(fresh);
  syncElastic(fresh);
  syncCassandra(fresh);
}

/** A new connection or database: fresh tree, and a single empty SQL tab. */
export function applyInfo(i: Info) {
  S.info = i;
  S.tables = i.tables;
  S.columnsCache.clear();
  S.expanded.clear();
  $<HTMLInputElement>("filter").value = "";
  forgetItems(sessionKey());
  showHeader();
  host.editor.setDialect(S.active!.kind, new Map(S.tables.map((t) => [t, []]))); // before the new tab's state is created
  host.loadSchema();
  // One pane again: a split's other tabs belonged to the previous database.
  S.pane = newPane();
  if (S.ws) (S.ws.panes = [S.pane]), (S.ws.pane = S.pane);
  layoutPanes();
  S.tabs = [];
  S.currentId = 0;
  S.shownId = 0;
  const kept = settings.keepTabs ? host.loadSqlTabs() : [];
  if (!kept.length) host.openSqlTab();
  for (const k of kept) S.tabs.push({ id: S.nextId++, kind: "sql", title: k.title, sql: k.sql, path: k.path });
  if (kept.length) host.select(S.tabs[0].id);
  host.renderTables();
  syncViews(true);
}

/** The workspace header, the database button and the sidebar filter for the active connection. */
function showHeader() {
  const c = S.active!;
  $("where-conn").replaceChildren(avatar(engineId(c)), colorDot(c), el("span", { textContent: c.name }), el("span", { className: "host", textContent: hostOf(c) }));
  document.body.dataset.env = c.tag; // production gets a red edge
  const btn = $("db-button");
  btn.hidden = !S.info?.can_switch;
  btn.textContent = S.info?.database ? `${dbLabel(S.info.database)} ▾` : "Choose database ▾";
  btn.title = `Open a database (${keyLabel("open-database")})`;
  $<HTMLInputElement>("filter").hidden = !S.tables.length;
  $("users-button").hidden = !managesUsers(c);
  renderWorkspaceButton();
}

/** Writes the on-screen state back into its workspace before another takes the screen. Off screen (the launcher
 *  is showing) it was written when the launcher came up, and may have changed since (Reconnect All). */
function stashWorkspace() {
  if (!S.ws || $("workspace").hidden) return;
  host.stashEditor();
  host.flushSqlTabs(); // with this workspace's tabs and database, before another's take the screen
  Object.assign(S.pane, { tabs: S.tabs, currentId: S.currentId });
  Object.assign(S.ws, {
    info: S.info, tables: S.tables, pane: S.pane, columns: S.columnsCache, expanded: S.expanded, lost: !$("lost").hidden, filter: $<HTMLInputElement>("filter").value,
  });
}

export function showWorkspace(w: Workspace) {
  stashWorkspace();
  S.ws = w;
  ({ conn: S.active, info: S.info, tables: S.tables, pane: S.pane, columns: S.columnsCache, expanded: S.expanded } = w);
  ({ tabs: S.tabs, currentId: S.currentId } = S.pane);
  S.shownId = 0; // the editor holds another workspace's tab
  redrawFilters();
  $("lost").hidden = !w.lost;
  $<HTMLInputElement>("filter").value = w.filter;
  showHeader();
  show("workspace");
  host.editor.setDialect(w.conn.kind, new Map(S.tables.map((t) => [t, []])));
  host.loadSchema();
  layoutPanes();
  host.renderTables();
  renderHistory();
  renderFavorites();
  host.renderMain();
  syncViews();
}

export function cycleWorkspace(step: number) {
  const i = S.workspaces.indexOf(S.ws!);
  if (S.workspaces.length > 1) showWorkspace(S.workspaces[(i + step + S.workspaces.length) % S.workspaces.length]);
}

/** The launcher, with the open workspaces kept; connecting from it adds one. */
export function newWorkspace() {
  stashWorkspace();
  show("home");
}

/** Reopens every workspace's connection, keeping their tabs. */
export async function reconnectAll() {
  stashWorkspace();
  for (const w of S.workspaces) {
    if (w === S.ws && !$("workspace").hidden) {
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
  const others = S.workspaces.filter((w) => w !== S.ws);
  if (others.length) {
    const lost = others.some((w) => w.lost);
    head.append(el("span", { className: `ws-count${lost ? " lost" : ""}`, textContent: `+${others.length}`,
      title: lost ? "Another workspace lost its connection" : `${others.length} more workspace${others.length === 1 ? "" : "s"}` }));
  }
  renderDbTabs();
}

export function showWorkspaces() {
  const r = $("where-conn").getBoundingClientRect();
  const items: MenuItem[] = S.workspaces.map((w) => [
    `${w === S.ws ? "✓ " : "    "}${w.conn.name}${w.info?.database ? ` · ${w.info.database}` : ""}${w.lost ? " (disconnected)" : ""}`,
    () => showWorkspace(w),
  ]);
  contextMenu(new MouseEvent("click", { clientX: r.left, clientY: r.bottom + 4 }), [
    ...items, "-",
    [`New Workspace…  ${keyLabel("new-workspace")}`, newWorkspace],
    ["Reconnect All", reconnectAll],
    ["Close Workspace", () => closeWorkspace()],
  ]);
}

// ponytail: an unfocused pane is a copy of a frame, drawn again when its tab changes (redrawPane), not a live view:
// scrolling or clicking in it focuses it first. Render each pane live once the grid and editor can draw into more
// than one place.
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
    if (tab) host.select(Number(tab));
  };
  return shot;
}

function layoutPanes() {
  const panes = S.ws?.panes ?? [S.pane];
  $("panes").replaceChildren(...panes.map((p) => (p === S.pane ? live() : p.shot ?? snapshot(p))));
  for (const out of $("panes").querySelectorAll<HTMLElement>('.shot [data-id="out"]')) out.scrollTop = Number(out.dataset.scroll ?? 0);
  $("panes").classList.toggle("split", panes.length > 1);
}

export function focusPane(p: Pane) {
  if (p === S.pane || !S.ws) return;
  host.stashEditor();
  Object.assign(S.pane, { tabs: S.tabs, currentId: S.currentId });
  S.pane.shot = snapshot(S.pane);
  S.ws.pane = S.pane = p;
  ({ tabs: S.tabs, currentId: S.currentId } = p);
  S.shownId = 0;
  redrawFilters();
  layoutPanes();
  host.renderMain();
  host.renderTables();
}

/** Draws an unfocused pane's tab again (a query finished there, a table loaded): rendered live for a moment, copied
 *  into its still picture, then the focused pane drawn back, all before the browser paints. */
function redrawPane(p: Pane) {
  if (!S.ws || p === S.pane || !S.ws.panes.includes(p)) return;
  host.stashEditor();
  const back = { pane: S.pane, selection, out: $("out").scrollTop, editor: host.editor.view.scrollDOM.scrollTop };
  Object.assign(S.pane, { tabs: S.tabs, currentId: S.currentId });
  S.pane = p;
  ({ tabs: S.tabs, currentId: S.currentId } = p);
  S.shownId = 0;
  redrawFilters();
  host.renderMain();
  p.shot = snapshot(p);
  S.pane = back.pane;
  ({ tabs: S.tabs, currentId: S.currentId } = S.pane);
  S.shownId = 0;
  redrawFilters();
  setSelection(back.selection);
  host.renderMain();
  $("out").scrollTop = back.out;
  host.editor.view.scrollDOM.scrollTop = back.editor;
  layoutPanes();
}

/** Panes waiting to be drawn again, once per frame however many results arrive. */
const redrawing = new Set<Pane>();

/** After `tab` changed while the user may have moved on: drawn again where it shows, live or in another pane's
 *  still picture. `draw` is how the live pane draws it (all of it by default). */
export function redraw(tab: Tab, draw: () => void = host.renderMain) {
  if (tab.id === S.currentId) return draw();
  const p = S.ws?.panes.find((x) => x !== S.pane && x.currentId === tab.id);
  if (!p || redrawing.has(p)) return;
  redrawing.add(p);
  requestAnimationFrame(() => (redrawing.delete(p), redrawPane(p)));
}

export function cyclePane(step: number) {
  const panes = S.ws?.panes ?? [];
  if (panes.length > 1) focusPane(panes[(panes.indexOf(S.pane) + step + panes.length) % panes.length]);
}

/** A new pane to the right of the focused one, with a new SQL tab. */
export function splitRight() {
  if (!S.ws) return;
  host.stashEditor();
  Object.assign(S.pane, { tabs: S.tabs, currentId: S.currentId });
  S.pane.shot = snapshot(S.pane);
  const p = newPane();
  S.ws.panes.splice(S.ws.panes.indexOf(S.pane) + 1, 0, p);
  S.ws.pane = S.pane = p;
  S.tabs = [];
  S.currentId = 0;
  layoutPanes();
  host.openSqlTab();
  host.editor.focus();
}

/** Its last tab closed: the pane goes and its neighbour takes focus. */
export function closePane() {
  if (!S.ws || S.ws.panes.length < 2) return;
  const i = S.ws.panes.indexOf(S.pane);
  S.ws.panes.splice(i, 1);
  const next = S.ws.panes[Math.max(0, i - 1)];
  S.ws.pane = S.pane = next;
  ({ tabs: S.tabs, currentId: S.currentId } = next);
  delete next.shot;
  S.shownId = 0;
  redrawFilters();
  layoutPanes();
  host.renderMain();
  host.renderTables();
}

export type KeptTab = { title: string; sql: string; path?: string };

/** Leaves the workspace and closes its session; `next` (else the neighbouring workspace, else the launcher) comes
 *  up. */
export async function closeWorkspace(next?: Workspace | null) {
  for (const t of allTabs()) if (!(await okToDiscard(t))) return;
  host.stashEditor();
  host.flushSqlTabs();
  if (S.ws) {
    invoke("disconnect", { name: S.ws.key }).catch(() => {});
    forgetRedis(S.ws.key);
    forgetMongo(S.ws.key);
    forgetDynamo(S.ws.key);
    forgetElastic(S.ws.key);
    forgetCassandra(S.ws.key);
    // An asked-for password serves every tab of its connection, so it goes with the last one.
    const name = S.ws.conn.name;
    if (!S.workspaces.some((w) => w !== S.ws && w.conn.name === name)) launcher.forgetAskedPassword(name);
  }
  const i = S.ws ? S.workspaces.indexOf(S.ws) : -1;
  S.workspaces = S.workspaces.filter((w) => w !== S.ws);
  rememberOpen();
  S.ws = null;
  S.active = null;
  S.info = null;
  S.tabs = [];
  S.shownId = 0;
  delete document.body.dataset.env;
  next = next && S.workspaces.includes(next) ? next : S.workspaces[Math.min(Math.max(i, 0), S.workspaces.length - 1)];
  if (next) showWorkspace(next);
  else show("home");
}

/** Notes the open workspaces, for reopening them at the next start, and refreshes Open Recent. */
function rememberOpen() {
  launcher.setOpenAtQuit([...new Set(S.workspaces.map((w) => w.conn.name))]);
  showRecent();
}

export function initWorkspaces(h: Host) {
  host = h;
  $("reconnect").onclick = reconnect;

  // Keep-alive found a connection dead: show the banner (or the mark on its tab) now rather than on the next query.
  listen<string>("connection-lost", (e) => {
    const w = S.workspaces.find((w) => w.key === e.payload);
    if (!w) return; // the MCP server's own session, or one closed meanwhile
    w.lost = true;
    if (w === S.ws) $("lost").hidden = false;
    renderWorkspaceButton();
  }).catch(() => {}); // no event bridge outside the app
  $("pill").onclick = showConnectionInfo;
  $("pill").onkeydown = (e) => void ((e.key === "Enter" || e.key === " ") && (e.preventDefault(), showConnectionInfo()));
  $("info-reconnect").onclick = () => (($("conn-info").hidden = true), reconnect());
  $("info-disconnect").onclick = () => (($("conn-info").hidden = true), closeWorkspace());
  document.addEventListener("pointerdown", (e) => {
    const pop = $("conn-info");
    if (!pop.hidden && !pop.contains(e.target as Node) && e.target !== $("pill")) pop.hidden = true;
  });
  document.addEventListener("keydown", (e) => e.key === "Escape" && ($("conn-info").hidden = true));
  $("where-conn").onclick = showWorkspaces;
  $("where-conn").onkeydown = (e) => void ((e.key === "Enter" || e.key === " ") && (e.preventDefault(), showWorkspaces()));
  $("return-workspace").onclick = () => S.ws && showWorkspace(S.ws);
}
