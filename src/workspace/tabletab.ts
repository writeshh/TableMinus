// Table tabs: browsing a table's rows a page at a time, with filters, sorting, foreign keys to follow, and the
// column and paging menus; the structure view is structure.ts. main.ts keeps the tab strip.

import { $, el } from "../dom";
import { lastPage, nextSort, pageLabel, totalLabel } from "./table-text";
import {
  call, current, loadColumns, note, okToDiscard, pendingCount, S, sessionKey, status, type Tab, type TableTab,
} from "./state";
import { activeFilters, blankFilter, renderFilters, type FilterRow } from "./filters";
import { noteOpened } from "./sidebar";
import { Staged } from "../grid/staging";
import { MAX_PAGE, clamp, setSetting, settings } from "../shell/settings";
import { arrange, layoutCss, loadLayout, moveColumn, saveLayout } from "./columns";
import { dropDraft, showSql, showStructure } from "../database/structure";
import * as grid from "../grid/grid-edit";
import { menuClose, openPopover, type MenuItem } from "../shell/dialogs";
import { paintSelection, render, selectedRows, type ForeignKey } from "../grid/gridview";
import { redraw } from "./workspaces";

/** What table tabs need of main.ts. */
type Host = {
  renderMain(): void;
  select(id: number): void;
  /** Adds and selects a tab; an empty tab showing is replaced by it. */
  addTab(tab: Tab): void;
  /** The sidebar's items again; Reload Current Tab on a SQL tab. */
  reloadItems(): Promise<void>;
};
let host: Host;

/** Shows `table`'s tab, opening one if needed: `fresh` opens another even when one is open, and a new tab starts
 *  with `filters` when given. */
export function openTable(table: string, view: TableTab["view"] = "data", create = false, opts: { filters?: FilterRow[]; fresh?: boolean } = {}) {
  const { filters, fresh = false } = opts;
  if (!create) noteOpened(table); // the sidebar's Recent section
  const existing = fresh ? undefined : (S.tabs.find((t) => t.kind === "table" && t.table === table) as TableTab | undefined);
  if (existing) {
    if (existing.view !== view && !pendingCount(existing)) existing.view = view;
    return host.select(existing.id);
  }
  const tab: TableTab = {
    id: S.nextId++, kind: "table", title: table, table, view, offset: 0, staged: new Staged(), create,
    sort: [], filters: filters ?? [], combine: "and", filtersShown: !!filters || settings.filterBar, layout: loadLayout(S.active!.name, table),
  };
  host.addTab(tab);
  if (view === "data") loadTable(tab);
}

/** Reload Current Tab: a table's rows (and count) or structure again; on a SQL tab, the sidebar's items. */
export async function reloadTab() {
  const tab = current();
  if (tab?.kind !== "table") return host.reloadItems();
  if (!(await okToDiscard(tab))) return;
  S.columnsCache.delete(tab.table);
  foreignKeys.delete(fkKey(tab.table));
  tab.total = undefined;
  if (tab.view === "data") loadTable(tab);
  else (dropDraft(tab.id), host.renderMain());
}

/** A table tab below the tab strip: the footer, then its structure, its error or its page of rows. */
export function renderTableTab(tab: TableTab) {
  $("result-strip").hidden = true;
  for (const b of $("view-seg").querySelectorAll<HTMLElement>("button")) b.setAttribute("aria-pressed", String(b.dataset.view === tab.view));
  const from = tab.offset;
  const page = { offset: from, shown: tab.result?.rows.length ?? 0, total: tab.total, estimated: tab.estimated };
  $("page-info").textContent =
    tab.view === "structure" ? (tab.create ? "New table" : "")
    : tab.loading ? "Loading…"
    : pageLabel(page);
  $<HTMLButtonElement>("page-info").disabled = tab.view !== "data";
  $<HTMLButtonElement>("page-prev").disabled = tab.view !== "data" || tab.offset === 0;
  $<HTMLButtonElement>("page-next").disabled = tab.view !== "data" || lastPage(page, settings.pageSize);
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
}

export function showPending(tab: Tab) {
  const pending = pendingCount(tab);
  $("edit-info").textContent = pending ? `${pending} unsaved change${pending === 1 ? "" : "s"}` : "";
  $("discard-edits").hidden = $("save-edits").hidden = !pending;
  $("preview-edits").hidden = !pending || tab.kind !== "table" || tab.view !== "data";
  $("save-edits").textContent = tab.kind === "table" && tab.view === "structure" ? "Preview SQL…" : "Save";
}

export async function loadTable(tab: TableTab) {
  const name = sessionKey(); // the tab's workspace, even if another takes the screen meanwhile
  tab.loading = true;
  tab.error = undefined;
  redraw(tab);
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
  redraw(tab); // the user may have moved to another tab, or pane, meanwhile
}

let filtersDrawnFor = 0; // the tab whose filter bar is in the DOM; redrawing on every render would steal focus

/** The filter bar draws again on the next render (another tab or pane, or new filters). */
export const redrawFilters = () => void (filtersDrawnFor = 0);

export function showFilters(tab: Tab) {
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
  const columns = S.columnsCache.get(tab.table)?.map((c) => c.name);
  if (filtersDrawnFor === tab.id || !columns) return;
  filtersDrawnFor = tab.id;
  renderFilters(bar, tab, columns, () => applyFilters(tab), {
    primary: (S.columnsCache.get(tab.table) ?? []).filter((c) => c.primary).map((c) => c.name),
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

/** The rows a table tab shows, without paging: its table, filters and sort. */
export const browseRequest = (tab: TableTab) => ({ table: tab.table, filters: activeFilters(tab), combine: tab.combine, sort: tab.sort });

async function applyFilters(tab: TableTab) {
  if (!(await okToDiscard(tab))) return;
  tab.offset = 0;
  tab.total = undefined; // a different filter matches a different number of rows
  loadTable(tab);
}

export function toggleFilters(open?: boolean) {
  const tab = current();
  if (tab?.kind !== "table" || tab.view !== "data") return;
  tab.filtersShown = open ?? !tab.filtersShown;
  filtersDrawnFor = 0;
  host.renderMain();
  if (tab.filtersShown) $("filters").querySelector<HTMLInputElement>("input[type=text]:not([hidden])")?.focus();
}

/** Header click: ascending, then descending, then off. With Shift, the column joins the existing sort. */
async function sortBy(tab: TableTab, column: string, add: boolean) {
  if (!(await okToDiscard(tab))) return;
  tab.sort = nextSort(tab.sort, column, add);
  tab.offset = 0;
  loadTable(tab);
}

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
    if (s.foreign_keys.length && tab?.kind === "table" && tab.table === table && fkKey(table) === key) host.renderMain(); // mark the headers
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
  const open = S.tabs.find((t): t is TableTab => t.kind === "table" && t.table === fk.ref_table);
  if (!open) return openTable(fk.ref_table, "data", false, { filters });
  if (!(await okToDiscard(open))) return;
  Object.assign(open, { view: "data", filters, combine: "and", filtersShown: true });
  filtersDrawnFor = 0;
  host.select(open.id);
  applyFilters(open);
}

/** A Search in Database hit: the table, filtered to rows holding `text`. */
export function openFound(table: string, mode: FilterRow["op"], text: string) {
  openTable(table);
  const tab = current();
  if (tab?.kind !== "table" || tab.table !== table) return;
  tab.filters = [{ ...blankFilter(), op: mode, value: text }];
  tab.filtersShown = true;
  filtersDrawnFor = 0;
  applyFilters(tab);
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

export function applyLayout(tab: Tab) {
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
  if (done) saveLayout(S.active!.name, tab.table, tab.layout);
}

/** Moves a table tab's column and remembers the order for the table. Staged edits refer to columns by position,
 *  so the order holds still while there are some. */
function reorderColumn(tab: TableTab, from: number, to: number) {
  if (!tab.result) return;
  if (tab.staged.count) return note("Save or discard the changes before moving columns", "error");
  tab.layout.order = moveColumn(tab.result.columns, from, to);
  saveLayout(S.active!.name, tab.table, tab.layout);
  tab.result = arrange(tab.result, tab.layout.order);
  host.renderMain();
}

/** The Columns button: a checklist of the table's columns. Unchecking hides a column. */
function showColumnsMenu() {
  const tab = current();
  if (tab?.kind !== "table" || !tab.result) return;
  const t = tab;
  const menu = el("div", { className: "popover columns-menu" });
  const save = () => (saveLayout(S.active!.name, t.table, t.layout), applyLayout(t));
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
    const own = S.columnsCache.get(t.table)?.map((c) => c.name);
    if (!own) return;
    if (t.staged.count) return note("Save or discard the changes before moving columns", "error");
    t.layout.order = [];
    save();
    t.result = arrange(t.result!, own);
    menuClose?.();
    host.renderMain();
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
    total.textContent = totalLabel(t.total, t.estimated);
    count.hidden = t.total !== undefined && !t.estimated;
  };
  showTotal();
  count.onclick = async () => {
    count.disabled = true;
    total.textContent = "Counting…";
    try {
      const page = await call<{ total: number | null }>("browse", { name: sessionKey(), request: { ...browseRequest(t), limit: 0, offset: 0, count: true } });
      if (page.total !== null) (t.total = page.total), (t.estimated = false);
      if (t.id === S.currentId) host.renderMain();
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
const turnPage = async (delta: number) => {
  const tab = current();
  if (tab?.kind !== "table" || !(await okToDiscard(tab))) return;
  tab.offset = Math.max(0, tab.offset + delta * settings.pageSize);
  loadTable(tab);
};

export function initTableTabs(h: Host) {
  host = h;
  for (const b of $("view-seg").querySelectorAll<HTMLElement>("button"))
    b.onclick = async () => {
      const tab = current();
      if (tab?.kind !== "table" || tab.view === b.dataset.view || !(await okToDiscard(tab))) return;
      tab.view = b.dataset.view as TableTab["view"];
      if (tab.view === "structure") host.renderMain(); // the structure view loads its own
      else loadTable(tab);
    };
  $("discard-edits").onclick = () => {
    const tab = current();
    if (tab?.kind === "table") (tab.staged.clear(), dropDraft(tab.id), host.renderMain());
  };
  $("toggle-filters").onclick = () => toggleFilters();
  $("add-row").onclick = () => current()?.kind === "table" && grid.addRow(current() as TableTab);
  $("preview-edits").onclick = () => current()?.kind === "table" && grid.preview(current() as TableTab);
  $("columns-button").onclick = () => (menuClose ? menuClose() : showColumnsMenu());
  $("page-prev").onclick = () => turnPage(-1);
  $("page-info").onclick = showPagingMenu;
  $("page-next").onclick = () => turnPage(1);
}
