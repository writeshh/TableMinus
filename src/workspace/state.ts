// The workspace on screen, shared by the modules that draw and change it: its connection, tables, sidebar state and the
// focused pane's tabs. Another module can't assign an imported `let`, so the state lives on one object, `S`;
// workspaces.ts swaps it in and out per workspace. Also the helpers they all use on it.

import { codeOf, invoke } from "../ipc";
import { $, setStatus } from "../dom";
import { label } from "../shell/keys";
import { bindings, isMac } from "../shell/commands";
import type { Saved } from "../connections/engines";
import type { QueryResult } from "../grid/results";
import type { Sort } from "../grid/gridview";
import type { FilterRow } from "./filters";
import type { Layout } from "./columns";
import type { Staged } from "../grid/staging";
import type { SqlTab } from "./run";
import { dropDraft, structurePending } from "../database/structure";
import { confirmDialog } from "../shell/dialogs";
import { renderDbTabs } from "./workspace-tabs";

export type StatusKind = "" | "busy" | "error" | "done";

export type Info = { server: string; database: string | null; can_switch: boolean; tables: string[] };

type Column = { name: string; data_type: string; nullable: boolean; default: string | null; primary: boolean };

export type TableTab = {
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

/** ⌘T's tab when Settings › General says Empty tab: the next table or SQL query opened takes its place. */
export type EmptyTab = { id: number; kind: "empty"; title: string };

export type Tab = SqlTab | TableTab | EmptyTab;

/** The tabs of one pane (the window splits into two side by side). While a pane has focus its tabs and current
 *  tab live in `S.tabs` and `S.currentId`; the rest is its history for Back/Forward and Reopen Closed Tab. */
export type Pane = {
  tabs: Tab[]; currentId: number; back: number[]; forward: number[]; closed: { tab: Tab; index: number }[];
  /** What an unfocused pane shows: a still copy of its last frame. */
  shot?: HTMLElement;
};

export const newPane = (): Pane => ({ tabs: [], currentId: 0, back: [], forward: [], closed: [] });

export type Workspace = {
  /** The backend's handle on this workspace's own session (`name` in every command); see `freeKey`. */
  key: string;
  conn: Saved; info: Info | null; tables: string[];
  /** Side by side; `pane` is the one with focus. */
  panes: Pane[]; pane: Pane;
  columns: Map<string, Column[]>; expanded: Set<string>; lost: boolean; filter: string;
};

export const S = {
  active: null as Saved | null,
  info: null as Info | null,
  tables: [] as string[],
  tabs: [] as Tab[],
  currentId: 0,
  nextId: 1,
  /** The SQL tab whose state is in the editor right now. */
  shownId: 0,
  pane: newPane(),
  workspaces: [] as Workspace[],
  /** The one on screen (or last on screen, while the launcher shows). */
  ws: null as Workspace | null,
  /** Swapped per workspace, so a late answer lands in its own. */
  columnsCache: new Map<string, Column[]>(),
  expanded: new Set<string>(),
};

export const current = () => S.tabs.find((t) => t.id === S.currentId);

export const sessionKey = () => S.ws?.key ?? "";

export const isRedis = () => S.active?.kind === "redis";
export const isMongo = () => S.active?.kind === "mongodb";

/** The command that opens a session: Redis and the document engines aren't SQL, and have their own (nosql/redis.rs,
 *  nosql/mongo.rs, ...). */
const OWN_CONNECT: Record<string, string> = {
  redis: "redis_connect", mongodb: "mongo_connect", dynamodb: "dynamo_connect", elasticsearch: "elastic_connect",
  cassandra: "cassandra_connect",
};
export const connectCommand = (kind: string) => OWN_CONNECT[kind] ?? "connect";
/** A workspace with a view of its own in place of tables and SQL tabs (redis.ts, docview.ts). */
export const ownView = () => !!S.active && S.active.kind in OWN_CONNECT;

/** A database as the header shows it: Redis numbers its databases. */
export const dbLabel = (database: string) => (isRedis() ? `db${database}` : database);

/** Every tab of the workspace on screen, in all its panes. */
export const allTabs = () => (S.ws?.panes ?? [S.pane]).flatMap((p) => (p === S.pane ? S.tabs : p.tabs));

export const status = (text: string, kind: StatusKind = "") => setStatus("status", text, kind);

/** The first shortcut of a command, as it reads on screen; "" when it has none. */
export const keyLabel = (id: string) => label(bindings(id)[0] ?? "", isMac);

/** invoke for workspace commands: a dropped connection shows the reconnect banner before the error moves on. */
export async function call<T>(cmd: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    if (codeOf(e) === "disconnected") ($("lost").hidden = false), renderDbTabs();
    throw e;
  }
}

export async function loadColumns(table: string) {
  const cache = S.columnsCache; // the workspace may change while this waits
  if (!cache.has(table)) cache.set(table, await call<Column[]>("columns", { name: sessionKey(), table }));
  return cache.get(table)!;
}

/** A short note where the current tab shows messages: the run bar for SQL tabs, the footer for table tabs. */
export function note(text: string, kind: StatusKind) {
  if (current()?.kind !== "table") return status(text, kind);
  // Written directly: a full re-render would also redraw the grid and drop the selection.
  const m = $("foot-msg");
  m.textContent = text;
  m.className = `foot-msg ${kind}`;
}

// ---------- Unsaved edits ----------

export const pendingCount = (tab: Tab) =>
  tab.kind === "table" ? tab.staged.count + structurePending(tab.id)
  : tab.kind === "sql" ? (tab.out?.tabs ?? []).reduce((n, r) => n + (r.edit?.staged.count ?? 0), 0)
  : 0;

export async function okToDiscard(tab: Tab) {
  const n = pendingCount(tab);
  if (!n || (await confirmDialog(`Discard ${n} unsaved change${n === 1 ? "" : "s"} in ${tab.title}?`, "Discard"))) {
    if (tab.kind === "table") (tab.staged.clear(), dropDraft(tab.id));
    else if (tab.kind === "sql") for (const r of tab.out?.tabs ?? []) r.edit?.staged.clear();
    return true;
  }
  return false;
}
