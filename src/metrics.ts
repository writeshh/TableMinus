// Metrics boards (Ref: `MetricsBoardWindow`, `Metrics*View`): dashboards of widgets on a grid. Tables, scoreboards
// and charts show a query's rows; input fields and dropdowns set named query parameters (statements.ts) that the
// queries use; buttons run a query and refresh the board; exporters and importers move data (importexport.ts); a
// connection widget picks the open connection the board runs on. Boards are kept per connection in metrics.json
// (locations.ts) and travel as files with a version.
//
// Queries go through run_query, so safe mode applies. A widget whose query may change data runs only from its own
// Run button, after asking; auto-refresh and parameter changes skip it, and skip anything safe mode wants approved.

import { invoke } from "./ipc";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { connName, el } from "./dom";
import { docEntry, setDocEntry } from "./locations";
import { CANCELLED, NOT_APPROVED, guarded, isSafetyStop, type Approval } from "./safety";
import { fillParams, findParams, type Dialect, type ParamFormat } from "./statements";
import { chartPickers, drawChart } from "./charts";
import { SLOTS, formatNumber, toNumber, type ChartKind, type ChartSpec } from "./chartdata";
import { exportDialog, importFile, type Ctx as TransferCtx } from "./importexport";
import type { QueryResult } from "./results";
import "./metrics.css";

export type Kind = "table" | "score" | ChartKind | "input" | "dropdown" | "button" | "exporter" | "importer" | "connection";
type Format = "csv" | "json" | "sql";

export type Widget = {
  id: string; kind: Kind; title: string;
  /** Place on the board's grid, in cells. */
  x: number; y: number; w: number; h: number;
  /** Rows for tables, scoreboards, charts and a dropdown's options; what a button runs; what an exporter exports. */
  sql: string;
  /** Charts: the x column and the series. */
  chart?: { x: string; y: string[] };
  /** Input fields and dropdowns: the parameter they set, and its value. A connection widget: the connection. */
  param?: string; value?: string;
  /** Dropdowns: options besides the query's, one per line. */
  options?: string;
  /** Seconds between refreshes, 0 for never; missing: the board's rate. */
  refresh?: number;
  /** Importers: into which table (empty: ask), from which format. */
  table?: string; format?: Format;
};
export type Board = { id: string; name: string; locked: boolean; refresh: number; widgets: Widget[] };

export type Session = { key: string; label: string; dialect: Dialect };
export type BoardCtx = {
  /** The saved connection's name: boards are kept per connection. */
  conn: string;
  /** The workspace that opened the board; queries run there unless a connection widget picks another. */
  session: Session;
  /** Every open workspace, for the connection widget. */
  sessions: () => Session[];
  format: ParamFormat;
  confirm: (text: string, okLabel: string) => Promise<boolean>;
  transfer: () => TransferCtx;
};

/** The file format's version; a newer file is refused rather than half read. */
const VERSION = 1;
const COLS = 12;
const ROW = 64;
const GAP = 10;
const PAD = 12;
const MAX_WIDGETS = 200;
const MAX_ROWS = 1000;

const KINDS: Record<Kind, { label: string; w: number; h: number }> = {
  table: { label: "Data table", w: 6, h: 4 },
  score: { label: "Scoreboard", w: 3, h: 2 },
  line: { label: "Line chart", w: 6, h: 4 },
  bar: { label: "Bar chart", w: 6, h: 4 },
  pie: { label: "Pie chart", w: 4, h: 4 },
  input: { label: "Input field", w: 3, h: 1 },
  dropdown: { label: "Dropdown", w: 3, h: 1 },
  button: { label: "Button", w: 2, h: 1 },
  exporter: { label: "Exporter", w: 2, h: 1 },
  importer: { label: "Importer", w: 2, h: 1 },
  connection: { label: "Connection", w: 3, h: 1 },
};
const RATES: [number, string][] = [[0, "Off"], [5, "5 s"], [10, "10 s"], [30, "30 s"], [60, "1 min"], [300, "5 min"], [900, "15 min"]];
const CHARTS: Kind[] = ["line", "bar", "pie"];
/** Widgets that show their query's rows, and refresh. */
const shows = (k: Kind) => k === "table" || k === "score" || k === "dropdown" || CHARTS.includes(k);
const hasQuery = (k: Kind) => shows(k) || k === "button" || k === "exporter";
const sets = (k: Kind) => k === "input" || k === "dropdown";
const newId = () => crypto.randomUUID();
const label = (w: Widget) => w.title || KINDS[w.kind].label;
/** The result set a widget shows: the last with rows, else the last (what a statement that changes data returns). */
const shown = (results: QueryResult[]) => [...results].reverse().find((r) => r.columns.length) ?? results.at(-1);
const clip = (s: string, n: number) => (s.length > n ? s.slice(0, n) + "…" : s);

// ---------- Reading boards: from metrics.json and from imported files, neither trusted ----------

const str = (v: unknown, max = 100_000) => (typeof v === "string" ? v.slice(0, max) : "");
const int = (v: unknown, lo: number, hi: number, fallback: number) =>
  typeof v === "number" && Number.isInteger(v) ? Math.min(hi, Math.max(lo, v)) : fallback;

// `any`: untrusted JSON, every field is checked here.
function readWidget(v: any): Widget | null {
  if (!v || typeof v !== "object" || !Object.hasOwn(KINDS, v.kind)) return null;
  const kind = v.kind as Kind;
  const x = int(v.x, 0, COLS - 1, 0);
  const w: Widget = {
    id: str(v.id, 64) || newId(), kind, title: str(v.title, 200), sql: str(v.sql),
    x, y: int(v.y, 0, 10_000, 0), w: Math.min(COLS - x, int(v.w, 1, COLS, KINDS[kind].w)), h: int(v.h, 1, 50, KINDS[kind].h),
  };
  if (v.chart && typeof v.chart === "object") {
    w.chart = { x: str(v.chart.x, 500), y: Array.isArray(v.chart.y) ? v.chart.y.slice(0, SLOTS).map((c: unknown) => str(c, 500)) : [] };
  }
  for (const f of ["param", "value", "options", "table"] as const) if (typeof v[f] === "string") w[f] = str(v[f]);
  if (v.refresh !== undefined) w.refresh = int(v.refresh, 0, 86_400, 0);
  if (v.format === "csv" || v.format === "json" || v.format === "sql") w.format = v.format;
  return w;
}

// `any`: untrusted JSON, every field is checked here.
export function readBoard(v: any): Board | null {
  if (!v || typeof v !== "object" || !Array.isArray(v.widgets)) return null;
  const ids = new Set<string>();
  const widgets = (v.widgets as unknown[]).slice(0, MAX_WIDGETS).map(readWidget).filter((w): w is Widget => !!w);
  for (const w of widgets) if (ids.has(w.id) || !ids.add(w.id)) w.id = newId(); // ids key each widget's state
  return { id: str(v.id, 64) || newId(), name: str(v.name, 200) || "Board", locked: v.locked === true, refresh: int(v.refresh, 0, 86_400, 0), widgets };
}

/** The connection's boards; one empty board when it has none. */
function boardsOf(conn: string): Board[] {
  const kept = docEntry<unknown>("metrics", conn);
  const boards = Array.isArray(kept) ? kept.map(readBoard).filter((b): b is Board => !!b) : [];
  return boards.length ? boards : [{ id: newId(), name: "Board 1", locked: false, refresh: 0, widgets: [] }];
}

/** The board each connection showed last, this session. */
const lastShown = new Map<string, string>();

// ---------- The board window ----------

type Ran = { results: QueryResult[]; read_only: boolean };
/** What a widget shows besides its settings; not kept. */
type State = { result?: QueryResult; error?: string; note?: string; busy?: boolean; due: number; node?: HTMLElement };

const button = (text: string, title = "", className = "ghost") => el("button", { type: "button", textContent: text, title, className });
const option = (value: string, text: string) => el("option", { value, textContent: text });
const row = (text: string, ...kids: Node[]) => el("label", { className: "board-row" }, el("span", { textContent: text }), ...kids);

/** Opens the connection's boards. `add` (a chart from a result) goes on the board shown last. */
export function openBoard(ctx: BoardCtx, add?: Omit<Widget, "id" | "x" | "y" | "w" | "h">) {
  const boards = boardsOf(ctx.conn);
  let board = boards.find((b) => b.id === lastShown.get(ctx.conn)) ?? boards[0];
  let session = ctx.session;
  const states = new Map<string, State>();
  const st = (w: Widget) => states.get(w.id) ?? states.set(w.id, { due: 0 }).get(w.id)!;
  const save = () => setDocEntry("metrics", ctx.conn, boards);

  const pick = el("select", { title: "Boards of this connection" });
  const addKind = el("select", { title: "Add a widget", className: "edit-only" },
    option("", "Add widget…"), ...Object.entries(KINDS).map(([k, v]) => option(k, v.label)));
  const rate = el("select", { title: "How often the board's widgets refresh, unless a widget has its own rate" },
    ...RATES.map(([s, l]) => option(String(s), s ? `Refresh every ${l}` : "Auto-refresh off")));
  const lock = button("Lock", "Lock the board: no adding, moving or editing widgets");
  const refreshAll = button("Refresh", "Run every widget's query again");
  const close = button("Close", "", "primary");
  const tools: [string, string, () => unknown, boolean][] = [
    ["New", "A new, empty board", newBoard, false],
    ["Rename…", "Rename this board", rename, true],
    ["Duplicate", "Copy this board", duplicate, false],
    ["Delete…", "Delete this board", remove, true],
    ["Import…", "Add a board from a file", importBoard, false],
    ["Export…", "Save this board to a file", exportBoard, false],
  ];
  const grid = el("div", { className: "board-grid" });
  const status = el("p", { className: "status" });
  const d = el("dialog", { className: "metrics-board" },
    el("header", { className: "board-head" },
      pick,
      ...tools.map(([text, title, run, edit]) => {
        const b = button(text, title, edit ? "ghost edit-only" : "ghost");
        b.onclick = () => run();
        return b;
      }),
      el("span", { className: "spacer" }), addKind, rate, refreshAll, lock, close),
    grid,
    el("footer", { className: "board-foot" }, status));
  document.body.append(d);
  const say = (text: string, kind: "" | "error" | "done" = "") => ((status.textContent = text), (status.className = `status ${kind}`));
  const fail = (e: unknown) => say(String(e), "error");

  pick.onchange = () => show(boards.find((b) => b.id === pick.value) ?? board);
  addKind.onchange = () => {
    if (addKind.value) addWidget({ kind: addKind.value as Kind, title: "", sql: "" });
    addKind.value = "";
  };
  rate.onchange = () => {
    board.refresh = Number(rate.value);
    for (const w of board.widgets) st(w).due = 0;
    save();
  };
  lock.onclick = () => ((board.locked = !board.locked), save(), render());
  refreshAll.onclick = () => loadAll();
  close.onclick = () => d.close();

  // ---------- Boards ----------

  function show(b: Board) {
    board = b;
    lastShown.set(ctx.conn, b.id);
    states.clear();
    const picked = b.widgets.find((w) => w.kind === "connection")?.value;
    session = (picked && ctx.sessions().find((s) => connName(s.key) === picked)) || ctx.session;
    render();
    loadAll();
  }

  function render() {
    pick.replaceChildren(...boards.map((b) => option(b.id, b.name)));
    pick.value = board.id;
    rate.value = String(RATES.some(([s]) => s === board.refresh) ? board.refresh : 0);
    d.classList.toggle("locked", board.locked);
    lock.textContent = board.locked ? "Unlock" : "Lock";
    lock.setAttribute("aria-pressed", String(board.locked));
    grid.replaceChildren(...board.widgets.map(widgetNode));
    if (!board.widgets.length) {
      grid.append(el("p", { className: "hint board-empty", textContent: board.locked ? "This board has no widgets."
        : "Add a widget: a table, a number or a chart from a query, or an input field or dropdown that feeds the queries' parameters." }));
    }
  }

  async function newBoard() {
    const name = await askName("New board", `Board ${boards.length + 1}`);
    if (!name) return;
    const b: Board = { id: newId(), name, locked: false, refresh: 0, widgets: [] };
    boards.push(b);
    save();
    show(b);
  }

  async function rename() {
    const name = await askName("Rename board", board.name);
    if (name) (board.name = name), save(), render();
  }

  function duplicate() {
    const b = readBoard(JSON.parse(JSON.stringify({ ...board, id: "", name: `${board.name} copy` })))!;
    for (const w of b.widgets) w.id = newId();
    boards.push(b);
    save();
    show(b);
  }

  async function remove() {
    if (!(await ctx.confirm(`Delete the board “${board.name}” and its ${board.widgets.length} widgets?`, "Delete"))) return;
    boards.splice(boards.indexOf(board), 1);
    if (!boards.length) boards.push({ id: newId(), name: "Board 1", locked: false, refresh: 0, widgets: [] });
    save();
    show(boards[0]);
  }

  async function exportBoard() {
    const path = await saveFileDialog({ defaultPath: `${board.name.replace(/[\\/:]/g, "-")}.json`, filters: [{ name: "Metrics board", extensions: ["json"] }] });
    if (typeof path !== "string") return;
    try {
      await invoke("write_text_file", { path, text: JSON.stringify({ version: VERSION, board }, null, 2) });
      say(`Exported “${board.name}”.`, "done");
    } catch (e) {
      fail(e);
    }
  }

  async function importBoard() {
    const path = await openFileDialog({ title: "Import a metrics board", filters: [{ name: "Metrics board", extensions: ["json"] }] });
    if (typeof path !== "string") return;
    try {
      const file = JSON.parse(await invoke<string>("read_text_file", { path }));
      if (file?.version !== VERSION) throw new Error(typeof file?.version === "number" && file.version > VERSION
        ? "the board was saved by a newer version of the app" : "not a metrics board file");
      const b = readBoard(file.board);
      if (!b) throw new Error("not a metrics board file");
      b.id = newId(); // a second import of the same file is a second board
      boards.push(b);
      save();
      show(b);
      say(`Imported “${b.name}”.`, "done");
    } catch (e) {
      fail(e instanceof Error ? e.message : e);
    }
  }

  // ---------- Widgets ----------

  function addWidget(spec: Omit<Widget, "id" | "x" | "y" | "w" | "h">) {
    const k = KINDS[spec.kind];
    const y = board.widgets.reduce((m, w) => Math.max(m, w.y + w.h), 0);
    const w: Widget = { ...spec, id: newId(), x: 0, y, w: k.w, h: k.h };
    if (sets(w.kind)) w.param ??= `p${board.widgets.filter((x) => sets(x.kind)).length + 1}`;
    board.widgets.push(w);
    save();
    render();
    st(w).node?.scrollIntoView({ block: "nearest" });
    if (w.sql) load(w);
    else if (w.kind !== "connection") editWidget(w);
  }

  const place = (n: HTMLElement, w: Widget) => {
    n.style.gridColumn = `${w.x + 1} / span ${w.w}`;
    n.style.gridRow = `${w.y + 1} / span ${w.h}`;
  };

  function widgetNode(w: Widget): HTMLElement {
    const s = st(w);
    const tools = el("span", { className: "widget-tools" });
    if (shows(w.kind) && w.sql.trim()) {
      const run = button("↻", "Run the query now");
      run.onclick = () => load(w, true);
      tools.append(run);
    }
    const edit = button("Edit", "Change the widget", "ghost edit-only");
    edit.onclick = () => editWidget(w);
    const del = button("×", "Remove the widget", "ghost edit-only");
    del.onclick = async () => {
      if (!(await ctx.confirm(`Remove “${label(w)}” from the board?`, "Remove"))) return;
      board.widgets.splice(board.widgets.indexOf(w), 1);
      states.delete(w.id);
      save();
      render();
    };
    tools.append(edit, del);
    const head = el("header", {},
      el("span", { className: "widget-title", textContent: label(w), title: label(w) }),
      ...(s.busy ? [el("span", { className: "status busy" })] : []), tools);
    const body = el("div", { className: "widget-body" });
    const resize = el("span", { className: "widget-resize edit-only", title: "Drag to resize" });
    const node = el("section", { className: `widget ${w.kind}` }, head, body, resize);
    place(node, w);
    drag(head, node, w, false);
    drag(resize, node, w, true);
    s.node = node;
    fillBody(w, body);
    return node;
  }

  const paint = (w: Widget) => {
    const s = st(w);
    if (s.node?.isConnected) s.node.replaceWith(widgetNode(w));
  };

  /** Moving a widget by its header, or sizing it by its corner, a cell at a time. */
  function drag(handle: HTMLElement, node: HTMLElement, w: Widget, resize: boolean) {
    handle.addEventListener("pointerdown", (e) => {
      if (board.locked || e.button !== 0 || (e.target as Element).closest("button, input, select")) return;
      e.preventDefault();
      handle.setPointerCapture(e.pointerId);
      const cell = (grid.clientWidth - 2 * PAD + GAP) / COLS;
      const start = { x: e.clientX, y: e.clientY, at: { ...w } };
      node.classList.add("moving");
      const move = (ev: PointerEvent) => {
        const dx = Math.round((ev.clientX - start.x) / cell);
        const dy = Math.round((ev.clientY - start.y) / (ROW + GAP));
        if (resize) [w.w, w.h] = [Math.min(COLS - w.x, Math.max(1, start.at.w + dx)), Math.min(50, Math.max(1, start.at.h + dy))];
        else [w.x, w.y] = [Math.min(COLS - w.w, Math.max(0, start.at.x + dx)), Math.max(0, start.at.y + dy)];
        place(node, w);
      };
      const end = () => {
        handle.removeEventListener("pointermove", move);
        node.classList.remove("moving");
        if (w.x !== start.at.x || w.y !== start.at.y || w.w !== start.at.w || w.h !== start.at.h) save();
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("lostpointercapture", end, { once: true });
    });
  }

  function fillBody(w: Widget, body: HTMLElement) {
    const s = st(w);
    const note = (text: string, kind = "") => el("p", { className: `widget-note ${kind}`, textContent: text });
    switch (w.kind) {
      case "input": {
        const input = el("input", { value: w.value ?? "", placeholder: `${example(ctx.format, w.param ?? "")} — press Return`, title: `Sets the parameter ${w.param}` });
        input.onchange = () => setParam(w, input.value);
        return body.append(input);
      }
      case "button": {
        const b = button(w.title || "Run", w.sql.trim() ? "Runs its query, then refreshes the board" : "Refreshes the board", "");
        b.onclick = () => press(w);
        return body.append(b);
      }
      case "exporter": {
        const b = button("Export…", "Export its query's rows to a file", "");
        b.onclick = () => exportRows(w);
        return body.append(b);
      }
      case "importer": {
        const b = button(`Import ${(w.format ?? "csv").toUpperCase()}…`, w.table ? `Into ${w.table}` : "Into a table you choose", "");
        b.onclick = () => importRows(w);
        return body.append(b);
      }
      case "connection": {
        const select = el("select", { title: "The open connection this board's queries run on" },
          ...ctx.sessions().map((x) => option(x.key, x.label)));
        select.value = session.key;
        select.onchange = () => {
          session = ctx.sessions().find((x) => x.key === select.value) ?? ctx.session;
          w.value = connName(session.key);
          save();
          loadAll();
        };
        return body.append(select);
      }
    }
    if (!w.sql.trim() && w.kind !== "dropdown") return body.append(note(board.locked ? "No query." : "No query yet: Edit to add one."));
    if (s.error) body.append(note(s.error, "error"));
    else if (s.note) body.append(note(s.note));
    if (w.kind === "dropdown") return body.append(dropdown(w, s.result));
    const r = s.result;
    if (!r) {
      if (!s.error && !s.note) body.append(note("Loading…"));
      return;
    }
    if (w.kind === "table") return body.append(table(r));
    if (w.kind === "score") return body.append(score(r));
    const host = el("div", { className: "widget-chart" });
    body.append(host);
    drawChart(host, r, { kind: w.kind as ChartKind, x: w.chart?.x ?? "", y: w.chart?.y ?? [] });
  }

  function dropdown(w: Widget, r?: QueryResult) {
    const select = el("select", { title: `Sets the parameter ${w.param}` }, option("", "—"));
    const seen = new Set<string>();
    const add = (value: string, text = value) => !seen.has(value) && (seen.add(value), select.append(option(value, text)));
    for (const line of (w.options ?? "").split("\n")) if (line.trim()) add(line.trim());
    for (const row of r?.rows ?? []) if (row[0] !== null) add(row[0], row[1] ?? row[0]);
    if (w.value && !seen.has(w.value)) add(w.value); // kept even when the options no longer have it
    select.value = w.value ?? "";
    select.onchange = () => setParam(w, select.value);
    return select;
  }

  // ---------- Parameters and running ----------

  const params = () =>
    Object.fromEntries(board.widgets.filter((w) => sets(w.kind) && w.param).map((w) => [w.param!, w.value ?? ""]));

  /** `sql` with the board's parameter values in it, or the names that have no value yet. */
  function fill(sql: string): string | { missing: string[] } {
    const found = findParams(sql, session.dialect, ctx.format);
    const values = params();
    const missing = [...new Set(found.map((p) => p.name).filter((n) => !values[n]))];
    return missing.length ? { missing } : fillParams(sql, found, values);
  }

  function setParam(w: Widget, value: string) {
    w.value = value;
    save();
    for (const x of board.widgets) {
      if (x !== w && shows(x.kind) && findParams(x.sql, session.dialect, ctx.format).some((p) => p.name === w.param)) load(x);
    }
  }

  /** Runs `sql` for `w` on the board's connection. One that may change data runs only when `manual`, after asking,
   *  and so does one safe mode wants approved. Null when it didn't run; the widget's note says why. */
  async function run(w: Widget, sql: string, manual: boolean): Promise<QueryResult[] | null> {
    const s = st(w);
    const name = session.key;
    const conn = connName(name);
    const reads = await invoke<boolean>("is_read_only", { name, sql });
    if (!reads && !manual) return (s.note = "May change data, so it runs only from ↻, after asking."), null;
    if (!reads && !(await ctx.confirm(`“${label(w)}” may change data on “${conn}”. Run it? ${clip(sql.trim(), 300)}`, "Run"))) {
      return (s.note = "Cancelled."), null;
    }
    const send = (a: Approval) => invoke<Ran>("run_query", { name, sql, ...a, maxRows: MAX_ROWS });
    try {
      const ran = manual
        ? await guarded(conn, send, async () => !reads || ctx.confirm(`Run “${label(w)}” on “${conn}”?`, "Run")) // asked already when it writes
        : await send(NOT_APPROVED);
      s.note = "";
      return ran.results;
    } catch (e) {
      if (e === CANCELLED) return (s.note = "Cancelled."), null;
      if (!manual && isSafetyStop(e)) return (s.note = "Safe mode asks first, so it runs only from ↻."), null;
      throw e;
    }
  }

  const rateOf = (w: Widget) => w.refresh ?? board.refresh;

  /** Runs a widget's query and shows its rows. Only `manual` runs ask (see `run`). */
  async function load(w: Widget, manual = false) {
    const s = st(w);
    s.due = rateOf(w) ? Date.now() + rateOf(w) * 1000 : Infinity;
    if (!shows(w.kind) || !w.sql.trim() || s.busy) return;
    const sql = fill(w.sql);
    if (typeof sql !== "string") {
      s.note = `Waiting for a value for ${sql.missing.join(", ")}.`;
      return paint(w);
    }
    s.busy = true;
    paint(w);
    try {
      const results = await run(w, sql, manual);
      if (results) {
        s.result = shown(results);
        s.error = "";
      }
    } catch (e) {
      s.error = String(e);
    }
    s.busy = false;
    paint(w);
  }

  function loadAll() {
    for (const w of board.widgets) load(w);
    say(`Refreshed ${new Date().toLocaleTimeString()} · ${session.label}`);
  }

  /** A button: its query, if it has one (asking first if it may change data), then every widget again. */
  async function press(w: Widget) {
    const sql = fill(w.sql);
    if (typeof sql !== "string") return say(`“${label(w)}” needs a value for ${sql.missing.join(", ")}.`, "error");
    try {
      if (sql.trim()) {
        const results = await run(w, sql, true);
        if (!results) return say(st(w).note ?? "");
        const r = results.at(-1);
        say(`“${label(w)}”: ${r?.columns.length ? `${formatNumber(r.rows.length)} rows` : `${formatNumber(r?.affected ?? 0)} rows affected`}.`, "done");
      }
      loadAll();
    } catch (e) {
      fail(e);
    }
  }

  const transfer = () => {
    const t = ctx.transfer();
    return { ...t, conn: session.key, imported: (table: string) => (t.imported(table), loadAll()) };
  };

  async function exportRows(w: Widget) {
    const sql = fill(w.sql);
    if (typeof sql !== "string") return say(`“${label(w)}” needs a value for ${sql.missing.join(", ")}.`, "error");
    if (!sql.trim()) return say(`“${label(w)}” has no query: Edit to add one.`, "error");
    try {
      const reads = await invoke<boolean>("is_read_only", { name: session.key, sql });
      if (!reads && !(await ctx.confirm(`“${label(w)}” may change data on “${connName(session.key)}”. Export it anyway?`, "Export"))) return;
      exportDialog(transfer(), { kind: "query", queries: [{ sql, name: label(w).toLowerCase().replace(/\s+/g, "-") }] });
    } catch (e) {
      fail(e);
    }
  }

  const importRows = (w: Widget) => importFile(transfer(), w.format ?? "csv", w.table || null);

  // ---------- A widget's settings ----------

  function editWidget(w: Widget) {
    const title = el("input", { value: w.title, placeholder: KINDS[w.kind].label });
    const fields: Node[] = [row("Title", title)];
    const sql = el("textarea", { className: "board-sql", value: w.sql, rows: 7, spellcheck: false });
    const queryHint: Partial<Record<Kind, string>> = {
      score: "Shows the first row's first column, and its second column under it.",
      dropdown: "Optional: options from a query; its first column is the value, the second the label.",
      button: "Optional: runs when pressed, then the board refreshes.",
      exporter: "Its rows go to a file; the format is chosen when exporting.",
    };
    if (hasQuery(w.kind)) {
      sql.placeholder = w.kind === "score" ? "select count(*) from orders" : "select …";
      fields.push(row("Query", sql));
      if (queryHint[w.kind]) fields.push(el("p", { className: "hint", textContent: queryHint[w.kind] }));
    }
    let spec: ChartSpec = { kind: (CHARTS.includes(w.kind) ? w.kind : "bar") as ChartKind, x: w.chart?.x ?? "", y: w.chart?.y ?? [] };
    const pickers = el("div");
    const showPickers = () => {
      const r = st(w).result;
      pickers.replaceChildren(r?.columns.length
        ? chartPickers(r, spec, (next) => (spec = next))
        : el("p", { className: "hint", textContent: "Run the query (Try) to pick its columns; until then the chart guesses." }));
    };
    if (CHARTS.includes(w.kind)) (fields.push(row("Columns", pickers)), showPickers());
    const param = el("input", { value: w.param ?? "", placeholder: "name", pattern: "[A-Za-z_][\\w.]*", required: true });
    const options = el("textarea", { value: w.options ?? "", rows: 4, placeholder: "One option per line" });
    if (sets(w.kind)) {
      fields.push(row("Parameter", param),
        el("p", { className: "hint", textContent: `Queries use it as ${example(ctx.format, "name")}. Its value goes in as typed: put quotes around text, e.g. 'Ada'.` }));
    }
    if (w.kind === "dropdown") fields.push(row("Options", options));
    const refresh = el("select", {}, option("", "The board's rate"), ...RATES.map(([s, l]) => option(String(s), s ? `Every ${l}` : "Never")));
    refresh.value = w.refresh === undefined ? "" : String(w.refresh);
    if (shows(w.kind)) fields.push(row("Auto-refresh", refresh));
    const tables = ctx.transfer().tables;
    const table = el("select", {}, option("", "Choose when importing"), ...tables.map((t) => option(t, t)));
    table.value = w.table ?? "";
    const format = el("select", {}, option("csv", "CSV"), option("json", "JSON"), option("sql", "SQL file"));
    format.value = w.format ?? "csv";
    if (w.kind === "importer") fields.push(row("Into table", table), row("Format", format));

    const status = el("p", { className: "status" });
    const cancel = button("Cancel", "", "");
    const ok = el("button", { type: "submit", className: "primary", textContent: "Save" });
    const footer = el("footer", {}, cancel, el("span", { className: "spacer" }));
    if (CHARTS.includes(w.kind) || w.kind === "dropdown" || w.kind === "table" || w.kind === "score") {
      const tryIt = button("Try", "Run the query to see its columns", "");
      tryIt.onclick = async () => {
        const filled = fill(sql.value);
        if (typeof filled !== "string") return (status.textContent = `Needs a value for ${filled.missing.join(", ")}.`);
        status.textContent = "Running…";
        try {
          const results = await run(w, filled, true);
          if (results) st(w).result = shown(results);
          status.textContent = results ? `${formatNumber(st(w).result?.rows.length ?? 0)} rows.` : st(w).note ?? "";
          showPickers();
        } catch (e) {
          status.textContent = String(e);
        }
      };
      footer.append(tryIt);
    }
    footer.append(ok);
    const form = el("form", { method: "dialog", className: "board-form" }, ...fields, status, footer);
    const dialog = el("dialog", { className: "board-edit" }, el("h3", { textContent: `${KINDS[w.kind].label} settings` }), form);
    document.body.append(dialog);
    dialog.addEventListener("close", () => dialog.remove());
    cancel.onclick = () => dialog.close();
    form.onsubmit = (e) => {
      e.preventDefault();
      const oldParam = w.param;
      w.title = title.value.trim();
      if (hasQuery(w.kind) && w.sql !== sql.value) (w.sql = sql.value), (st(w).result = undefined); // no stale rows under a new query
      if (CHARTS.includes(w.kind)) (w.kind = spec.kind), (w.chart = { x: spec.x, y: spec.y });
      if (sets(w.kind)) w.param = param.value.trim();
      if (w.kind === "dropdown") w.options = options.value;
      if (shows(w.kind)) {
        if (refresh.value) w.refresh = Number(refresh.value);
        else delete w.refresh;
      }
      if (w.kind === "importer") (w.table = table.value), (w.format = format.value as Format);
      save();
      dialog.close();
      paint(w);
      if (shows(w.kind)) load(w);
      if (sets(w.kind) && (oldParam !== w.param || w.value)) setParam(w, w.value ?? "");
    };
    dialog.showModal();
    (hasQuery(w.kind) ? sql : title).focus();
  }

  /** A short form for a name; null when cancelled. */
  function askName(heading: string, value: string): Promise<string | null> {
    const input = el("input", { value, required: true });
    const cancel = button("Cancel", "", "");
    const form = el("form", { method: "dialog", className: "board-form" }, input,
      el("footer", {}, el("span", { className: "spacer" }), cancel, el("button", { type: "submit", className: "primary", textContent: "OK" })));
    const dialog = el("dialog", { className: "small" }, el("h3", { textContent: heading }), form);
    document.body.append(dialog);
    dialog.showModal();
    input.select();
    return new Promise((resolve) => {
      let name: string | null = null;
      cancel.onclick = () => dialog.close();
      form.onsubmit = (e) => (e.preventDefault(), (name = input.value.trim() || null), dialog.close());
      dialog.addEventListener("close", () => (dialog.remove(), resolve(name)));
    });
  }

  // Auto-refresh: due widgets, while nothing is asked or edited on top of the board.
  const timer = setInterval(() => {
    if (document.querySelector("dialog[open]:not(.metrics-board)")) return;
    const now = Date.now();
    for (const w of board.widgets) if (shows(w.kind) && rateOf(w) > 0 && now >= st(w).due) load(w);
  }, 1000);
  d.addEventListener("close", () => (clearInterval(timer), d.remove()));
  d.showModal();
  show(board);
  if (add) addWidget(add);
}

/** How `name` is written as a parameter in `format`. */
function example(format: ParamFormat, name: string) {
  const forms: Record<ParamFormat, string> = { colon: `:${name}`, dollar: `$${name}`, dollarBraces: `\${${name}}`, doubleBraces: `{{${name}}}`, percent: `%${name}%`, question: "?" };
  return forms[format];
}

function table(r: QueryResult) {
  if (!r.columns.length) return el("p", { className: "widget-note", textContent: `${formatNumber(r.affected)} rows affected.` });
  return el("table", { className: "widget-table" },
    el("thead", {}, el("tr", {}, ...r.columns.map((c) => el("th", { textContent: c })))),
    el("tbody", {}, ...r.rows.map((row) => el("tr", {}, ...row.map((v) => el("td", { textContent: v ?? "NULL", className: v === null ? "null" : toNumber(v) !== null ? "num" : "" }))))));
}

function score(r: QueryResult) {
  const v = r.rows[0]?.[0] ?? null;
  const n = toNumber(v);
  return el("div", { className: "score" },
    el("strong", { textContent: !r.columns.length ? formatNumber(r.affected) : n !== null ? formatNumber(n) : (v ?? "NULL"), title: v ?? "" }),
    el("span", { textContent: r.rows[0]?.[1] ?? (r.rows.length ? "" : "No rows") }));
}
