// Running SQL from a SQL tab: its statements one after another (safe mode asks once for the lot), query parameters,
// and the results under the editor (results.ts draws the strip, gridview.ts the grids).

import type { EditorState } from "@codemirror/state";
import { codeOf, invoke } from "./ipc";
import { $, el } from "./dom";
import type { BoardCtx } from "./metrics";
import type { Editor } from "./editor";
import type { Ctx as TransferCtx } from "./importexport";
import { call, current, loadColumns, okToDiscard, S, sessionKey, status, type StatusKind } from "./state";
import { ask, contextMenu, type MenuItem } from "./dialogs";
import {
  ALL, MESSAGES, renderMessages, renderStrip, rowsLabel, searchRows, type QueryResult, type Results, type ResultTab,
} from "./results";
import { settings } from "./settings";
import { approval, CANCELLED, guarded, isSafetyStop, NOT_APPROVED } from "./safety";
import { fillParams, findParams, sourceTable } from "./statements";
import { Staged } from "./staging";
import { fmt, rowsText } from "./run-text";
import { addHistory } from "./history";
import { chartPanel } from "./charts";
import { guessSpec } from "./chartdata";
import { openBoard } from "./metrics";
import { copyText, drawn, gridTable, paintSelection, render, selectedRows, selection, setSelection } from "./gridview";
import * as grid from "./grid-edit";
import * as vgrid from "./vgrid";
import { exportDialog } from "./importexport";
import { ask as askFields } from "./structure";
import { replaceCells } from "./transforms";

/** What running needs of main.ts. */
type Host = {
  editor: Editor;
  boardCtx(): BoardCtx;
  transferCtx(): TransferCtx;
  renderMain(): void;
};
let host: Host;

export type SqlTab = {
  id: number; kind: "sql"; title: string; sql: string; state?: EditorState; status?: [string, StatusKind];
  /** The .sql file the tab was opened from or saved to. */
  path?: string;
  /** What the last run returned (results.ts). */
  out?: Results;
  /** The result on screen, for select-all. */
  result?: QueryResult;
};
const rowLimit = () => settings.sqlLimit || null;

type Ran = { results: QueryResult[]; read_only: boolean };
let nextResultId = 1;

/** Runs statements from the current SQL tab, one after another; results and status stay with that tab. Each result
 *  set gets a result tab (pinned ones from earlier runs stay), each statement a line in Messages. Safe mode asks
 *  once for the lot. A failed statement stops the rest when Stop on error is on. */
export async function execute(statements: string[]) {
  const tab = current();
  statements = statements.filter((s) => s.trim());
  if (!S.active || tab?.kind !== "sql" || !statements.length || !(await okToDiscard(tab))) return;
  const [conn, key] = [S.active, sessionKey()];
  const say = (text: string, kind: StatusKind) => {
    tab.status = [text, kind];
    if (tab.id === S.currentId) status(text, kind);
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
        const table = ran.results.length === 1 ? sourceTable(sql, S.tables) : null;
        if (table) {
          r.edit = { id: r.id, table, result, staged: new Staged() };
          // The key columns decide whether it's editable; draw again once they're known.
          loadColumns(table).then(() => tab.id === S.currentId && showResults(tab), () => {});
        }
        res.tabs.push(r);
        shown = r.id; // the last new result shows, as before there were tabs
      });
      res.messages.push({ sql, text: ran.results.map(rowsText).join(" · "), error: false, ms: took(), at: Date.now() });
      done++;
      addHistory(sql);
    } catch (e) {
      if (isSafetyStop(e) && !(codeOf(e) === "confirm" && approved.confirmed)) {
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
        // Touch ID for several statements: one prompt for the batch, kept as a grant the rest of it sends.
        if (next.unlock === "touch_id" && statements.length - i > 1) {
          try {
            approved = { confirmed: true, unlock: { grant: await invoke<string>("batch_grant") } };
          } catch (err) {
            stopped = i ? `Stopped before statement ${i + 1}: ${err}` : String(err);
            break;
          }
        }
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
      if (tab.id === S.currentId) showResults(tab); // results appear as they come
    }
  }
  res.shown = failed ? MESSAGES : shown || MESSAGES;
  if (tab.id === S.currentId) showResults(tab);
  const ms = Math.round(performance.now() - t0);
  const only = res.tabs.find((r) => r.id === shown)?.result;
  if (stopped) say(stopped, failed ? "error" : "");
  else if (!many && only) {
    const limited = only.truncated ? ` (first ${fmt(only.rows.length)}; Load more, or raise the row limit)` : "";
    say(`${fmt(only.rows.length)} rows${limited} · ${ms} ms`, "done");
  } else if (!many) say(`${fmt(affected)} rows affected · ${ms} ms`, "done");
  else say(`${done} of ${statements.length} statements${failed ? ` · ${failed} failed` : ""} · ${fmt(affected)} rows affected · ${ms} ms`, failed ? "error" : "done");
}

/** A SQL tab's results under the editor: the strip, then the result, every result (in append mode), or Messages. */
export function showResults(tab: SqlTab) {
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
  $("result-replace").hidden = search.hidden || !(shown.length === 1 && editable(shown[0], true));
  const chart = $("result-chart");
  chart.hidden = append || shown.length !== 1 || !shown[0].result.columns.length;
  chart.setAttribute("aria-pressed", String(!chart.hidden && !!shown[0].charted));
  showResultEdits(target);
  $("result-found").textContent = "";
  tab.result = shown[0]?.result;
  if (!shown.length) return renderMessages($("out"), res.messages, S.active?.kind ?? "");
  if (!append) {
    const r = shown[0];
    const rows = searchRows(r.result, search.value);
    tab.result = rows;
    if (r.charted) {
      $("out").replaceChildren(chartPanel(rows, (r.chart ??= guessSpec(rows)), {
        change: (s) => (r.chart = s),
        add: (s) => openBoard(host.boardCtx(), { kind: s.kind, title: "", sql: r.sql, chart: { x: s.x, y: s.y } }),
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
    if (selection && !shown.some((r) => r.result === selection!.r)) setSelection(null);
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
/** The result as an editable table, when it is one; while a search filters it, only with `searching` (Replace). */
function editable(r: ResultTab, searching = false) {
  const e = r.edit;
  if (r.charted) return null;
  const columns = e && S.columnsCache.get(e.table);
  if (!e || !columns || (!searching && (current() as SqlTab).out?.search?.trim())) return null;
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
export function resultTarget() {
  const tab = current();
  const r = tab?.kind === "sql" ? tab.out?.tabs.find((x) => x.id === tab.out!.shown) : undefined;
  return r && settings.resultsMode === "tabs" ? editable(r) : null;
}

/** Runs a result's statement again after its edits were saved, keeping as many rows as it showed. */
export async function reloadResult(edit: grid.EditTab) {
  const tab = current();
  const r = tab?.kind === "sql" ? tab.out?.tabs.find((x) => x.edit === edit) : undefined;
  if (!S.active || !r || tab?.kind !== "sql") return;
  const maxRows = Math.max(r.result.rows.length, rowLimit() ?? 0) || null;
  try {
    const ran = await call<Ran>("run_query", { name: sessionKey(), sql: r.sql, confirmed: false, unlock: null, maxRows });
    const next = ran.results[r.index];
    if (next?.columns.length) r.result = edit.result = next;
  } catch (e) {
    edit.message = [`Saved, but reloading failed: ${e}`, "error"];
  }
  if (tab.id === S.currentId) showResults(tab);
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
  if (!S.active) return;
  const conn = S.active;
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
  if (tab.id === S.currentId) (status(...tab.status), showResults(tab));
}

/** Exports result tabs; each one's statement runs again on the export's own connection. */
function exportResults(list: ResultTab[]) {
  const name = (r: ResultTab) => r.title.toLowerCase().replace(/\s+/g, "-");
  exportDialog(host.transferCtx(), { kind: "query", queries: list.map((r) => ({ sql: r.sql, name: name(r) })) });
}

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
  if (settings.paramFormat === "off" || !S.active) return statements;
  const format = settings.paramFormat;
  const found = statements.map((s) => findParams(s, S.active!.kind as Parameters<typeof findParams>[1], format));
  const names = [...new Set(found.flat().map((p) => p.name))];
  if (!names.length) return statements;
  const values = await askParams(names);
  return values && statements.map((s, i) => fillParams(s, found[i], values));
}

export const runCurrent = async () => {
  const filled = await withParams([host.editor.current()]);
  if (filled) execute(filled);
};

/** Runs every statement (in the selection, or the whole tab) in turn. */
export async function runAll() {
  const filled = await withParams(host.editor.all().map((s) => s.text));
  if (filled) execute(filled);
}

/** Find and replace in the shown result's cells, staged as edits to review and save (only an editable result). */
async function replaceInResult() {
  const tab = current();
  const res = tab?.kind === "sql" ? tab.out : undefined;
  const r = res?.tabs.find((t) => t.id === res.shown);
  const target = r && editable(r, true);
  if (!res || !target?.result) return;
  const got = await askFields("Replace in this result", [
    { key: "find", label: "Find", value: res.search ?? "" },
    { key: "replace", label: "Replace with", value: "" },
    { key: "matchCase", label: "Match case", value: false },
    { key: "wholeCell", label: "Whole cell only", value: false },
  ], "Replace All");
  if (!got) return;
  const cells = replaceCells(target.result.rows, String(got.find), String(got.replace), { matchCase: !!got.matchCase, wholeCell: !!got.wholeCell });
  res.search = ""; // show every row again, with the replaced values staged in them
  if (!cells.length || !grid.stageCells(target, cells)) return void (showResults(tab as SqlTab), status("Nothing to replace", ""));
  showResults(tab as SqlTab);
}

export function initRun(h: Host) {
  host = h;
  $("result-preview").onclick = () => {
    const t = resultTarget();
    if (t) grid.preview(t);
  };
  $("result-discard").onclick = () => {
    const t = resultTarget();
    if (t) (t.staged.clear(), host.renderMain());
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
  $("result-replace").onclick = () => void replaceInResult();

  $("result-search").addEventListener("input", (e) => {
    const tab = current();
    if (tab?.kind !== "sql" || !tab.out) return;
    tab.out.search = (e.target as HTMLInputElement).value;
    showResults(tab);
  });
  $("export-result").onclick = (e) => {
    const tab = current();
    if (tab?.kind !== "sql" || !S.active) return;
    const list = tab.out?.tabs ?? [];
    if (!list.length) return status("Run a query first; its result is exported by running it again.", "error");
    const on = list.find((r) => r.id === tab.out!.shown) ?? list.at(-1)!;
    if (list.length === 1) return exportResults(list);
    contextMenu(e, [[`Export ${on.title}…`, () => exportResults([on])], [`Export all ${list.length} results…`, () => exportResults(list)]]);
  };

  $("run").onclick = runCurrent;
  $("run-all").onclick = runAll;
  $("explain").onclick = async () => {
    const sql = host.editor.current();
    if (!sql.trim() || !S.active) return;
    const prefix = S.active.kind === "sqlite" ? "explain query plan " : "explain ";
    const filled = await withParams([prefix + sql]);
    if (filled) execute(filled);
  };
}
