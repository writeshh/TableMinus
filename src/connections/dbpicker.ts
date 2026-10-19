// Open a Database (⌘K): the connection's databases, to open in place or in a new workspace tab, and to create,
// rename or drop.

import { $, el, setStatus } from "../dom";
import type { Editor } from "../editor/editor";
import { allTabs, call, connectCommand, isMongo, isRedis, okToDiscard, ownView, S, sessionKey, status, type Info } from "../workspace/state";
import { applyInfo, openWorkspace, showWorkspace } from "../workspace/workspaces";
import { dropDatabase, newDatabase, renameDatabase } from "../database/structure";

/** What opening a database needs of main.ts. */
type Host = {
  editor: Editor;
  /** Keeps the editor's text and undo history with the SQL tab showing. */
  stashEditor(): void;
  /** Keeps the SQL tabs under the database on screen. */
  saveSqlTabs(): void;
};
let host: Host;

let databases: string[] = [];
let dbPick = 0;

export async function openDbPicker() {
  if (!S.active || !S.info?.can_switch) return;
  const d = $<HTMLDialogElement>("dbpicker");
  $<HTMLInputElement>("db-search").value = "";
  databases = [];
  renderDbs();
  if (!d.open) d.showModal();
  $("db-search").focus();
  await loadDbs(S.info?.database ?? "");
}

/** Fills the list, with `pick` selected when it's there. */
async function loadDbs(pick: string) {
  setStatus("db-status", "Loading…", "busy");
  try {
    databases = isRedis()
      ? (await call<{ index: number; keys: number }[]>("redis_databases", { name: sessionKey() })).map((d) => String(d.index))
      : await call<string[]>(isMongo() ? "mongo_databases" : S.active?.kind === "cassandra" ? "cassandra_keyspaces" : "list_databases", { name: sessionKey() });
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
      if (name === S.info?.database) li.append(el("span", { className: "current", textContent: "current" }));
      li.onclick = () => ((dbPick = i), renderDbs());
      li.ondblclick = (e) => (e.metaKey || e.ctrlKey ? openDbInNewTab(name) : openDb(name));
      return li;
    }),
  );
  $("dbs").querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
}

async function openDb(name?: string) {
  name ??= shownDbs()[dbPick];
  if (!name || !S.active) return;
  for (const t of allTabs()) if (!(await okToDiscard(t))) return;
  setStatus("db-status", `Opening ${name}…`, "busy");
  try {
    const next = await call<Info>(ownView() ? connectCommand(S.active.kind) : "use_database", { name: sessionKey(), database: name });
    host.stashEditor();
    host.saveSqlTabs(); // under the database being left
    applyInfo(next);
    $<HTMLDialogElement>("dbpicker").close();
    status(`${name} · ${S.tables.length} tables`, "done");
    host.editor.focus();
  } catch (e) {
    setStatus("db-status", String(e), "error");
  }
}

/** Opens the database in a new tab: its own session of the connection, sidebar and item tabs. Its tab comes forward
 *  when it's open already; the database on screen, or a connection still without one, opens in place. */
async function openDbInNewTab(name = shownDbs()[dbPick]) {
  if (!name || !S.active) return;
  if (name === S.info?.database) return $<HTMLDialogElement>("dbpicker").close();
  if (!S.info?.database) return openDb(name);
  const conn = S.active.name;
  const open = S.workspaces.find((w) => w.conn.name === conn && (w === S.ws ? S.info : w.info)?.database === name);
  if (open) return $<HTMLDialogElement>("dbpicker").close(), showWorkspace(open);
  setStatus("db-status", `Opening ${name}…`, "busy");
  try {
    await openWorkspace(S.active, name);
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

export function initDbPicker(h: Host) {
  host = h;
  $("db-new").onclick = () => databaseOp(newDatabase, (name) => loadDbs(String(name)));
  $("db-rename").onclick = () => {
    const db = shownDbs()[dbPick];
    if (db) databaseOp(() => renameDatabase(db), async (to) => {
      // MySQL moved the tables out from under this tab; follow them. (Postgres can't rename the database in use.)
      if (db === S.info?.database) await openDb(String(to));
      else await loadDbs(String(to));
    });
  };
  $("db-drop").onclick = () => {
    const db = shownDbs()[dbPick];
    if (db) databaseOp(() => dropDatabase(db), async () => {
      if (db === S.info?.database) {
        host.stashEditor();
        host.saveSqlTabs(); // kept under the dropped database, should it come back
        applyInfo(await call<Info>("connect", { name: sessionKey() })); // MySQL: now on none
      }
      await loadDbs(S.info?.database ?? "");
    });
  };
  $("db-open-new").onclick = () => openDbInNewTab();
  $("db-search").addEventListener("input", () => ((dbPick = 0), renderDbs()));
  $("db-search").addEventListener("keydown", (e) => {
    const n = shownDbs().length;
    if (e.key === "ArrowDown") dbPick = Math.min(n - 1, dbPick + 1);
    else if (e.key === "ArrowUp") dbPick = Math.max(0, dbPick - 1);
    else if (e.key === "Escape") return e.preventDefault(), $<HTMLDialogElement>("dbpicker").close(); // a search field would swallow it
    else if (e.key === "Enter") return e.preventDefault(), e.metaKey || e.ctrlKey ? openDbInNewTab() : openDb();
    else return;
    e.preventDefault();
    renderDbs();
  });
  $("db-open").onclick = () => openDb();
  $("db-cancel").onclick = () => $<HTMLDialogElement>("dbpicker").close();
  $("db-button").onclick = openDbPicker;
}
