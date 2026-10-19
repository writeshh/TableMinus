// The result grids: drawing a result's rows (only those near the viewport, vgrid.ts), header sorting, resizing and
// dragging, and selecting cells and copying them. Table tabs and SQL results both draw through here.

import { $, el } from "./dom";
import { current, note, S } from "./state";
import { syncRowDetail } from "./rowdetail";
import { contextMenu, type MenuItem } from "./dialogs";
import type { QueryResult } from "./results";
import * as vgrid from "./vgrid";
import * as grid from "./grid-edit";
import { MIN_WIDTH } from "./columns";
import { csv, inserts, markdown, plain, type Dialect } from "./copy";
import { sqlKind } from "./statements";
import { isMac } from "./commands";
import { pluginMenu } from "./plugins";

export type Sort = { column: string; desc: boolean };

/** The row detail follows the selection's row on a table tab's data. */
export function syncDetail() {
  const tab = current();
  const table = tab?.kind === "table" && tab.view === "data" && !tab.error ? tab : null;
  const sel = selection;
  syncRowDetail(table, sel && sel.r === table?.result ? sel.focus[0] : null);
}

/** The rows the selection covers, top to bottom. */
export function selectedRows() {
  if (!selection) return [];
  const { r0, r1 } = selectedBlock(selection);
  return Array.from({ length: r1 - r0 + 1 }, (_, i) => r0 + i);
}

export function fillCell(td: HTMLTableCellElement, v: string | null) {
  td.textContent = v ?? "NULL";
  td.className = v === null ? "null" : v === "" ? "empty-val" : numeric.test(v) ? "num" : "";
  td.title = v && v.length > 40 ? v : ""; // full value on hover when the cell is truncated
}

export type ForeignKey = { name: string; columns: string[]; ref_table: string; ref_columns: string[] };

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
export function render(r: QueryResult, offset: number, sorting?: RenderOptions) {
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

/** The table each result was last drawn in, for painting the selection when several are on screen. */
export const drawn = new WeakMap<QueryResult, HTMLTableElement>();

/** Result grid with row numbers starting after `offset`. With options, headers sort the rows when clicked. */
export function gridTable(r: QueryResult, offset: number, sorting?: RenderOptions): HTMLElement {
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

/** A block of cells, from the anchor (first click) to the focus (Shift-click or drag end), inclusive. */
type Selection = { r: QueryResult; anchor: [number, number]; focus: [number, number]; table?: string };
export let selection: Selection | null = null;
export const setSelection = (s: Selection | null) => void (selection = s);

export function selectedBlock(sel: Selection) {
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

export function paintSelection() {
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

export async function copyText(text: string, what: string) {
  try {
    await navigator.clipboard.writeText(text);
    note(`Copied ${what}`, "done");
  } catch (e) {
    note(`Couldn't copy: ${e}`, "error");
  }
}

export function copySelection(format: "plain" | "csv" | "markdown" | "insert") {
  const cells = selectedCells(format === "insert");
  if (!cells || !S.active) return;
  const n = cells.rows.length;
  const rows = `${n} row${n === 1 ? "" : "s"}`;
  if (format === "plain") return copyText(plain(cells), n === 1 && cells.columns.length === 1 ? "cell" : rows);
  if (format === "csv") return copyText(csv(cells), `${rows} as CSV`);
  if (format === "markdown") return copyText(markdown(cells), `${rows} as Markdown`);
  copyText(inserts(cells, selection!.table ?? "table_name", sqlKind(S.active.kind) as Dialect), `${rows} as INSERT statements`);
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
      ...pluginMenu("rows", () => {
        const cells = selectedCells(true);
        const rows = (cells?.rows ?? []).map((row) => Object.fromEntries(cells!.columns.map((c, i) => [c, row[i]])));
        return { table: options?.table ?? null, columns: cells?.columns ?? [], rows };
      }),
      ...(options?.menu?.(ri, ci) ?? []),
    ]);
  };
}

export function initGridView() {
  $("out").addEventListener("scroll", queueDraw, { passive: true });
  new ResizeObserver(queueDraw).observe($("out"));
}
