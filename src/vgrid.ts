// A virtualized table body: only the rows near the viewport are in the DOM, between two spacer rows that keep the
// scroll height, so a page of thousands of rows scrolls like one of fifty. Cells don't wrap, so every row has the
// same height, measured from a drawn row. Columns get a minimum width from the widest value of all rows, so they
// don't jump while other rows scroll in.

const OVERSCAN = 30; // rows drawn beyond each edge of the viewport
const STEP = 20; // the window moves in steps, so small scrolls don't redraw
const MAX_WIDTH = 420; // matches the cells' max-width in styles.css

type Grid = {
  body: HTMLTableSectionElement;
  count: () => number;
  /** Builds row `i`; the grid sets its `data-ri` and zebra class. */
  row: (i: number) => HTMLTableRowElement;
  /** The longest text of each column, row numbers first, for the natural widths. */
  widest: () => string[];
  /** Cells per row, row number included: the spacers span them all. */
  cols: number;
  from: number;
  to: number;
  sized: boolean;
};

const grids = new WeakMap<HTMLTableSectionElement, Grid>();
let rowHeight = 24; // shared: every grid uses the same font
let nextId = 1;

/** Makes `table`'s body virtual. Nothing is drawn until [`drawGrids`] runs with the table in the document. */
export function virtualize(table: HTMLTableElement, count: () => number, row: (i: number) => HTMLTableRowElement, widest: () => string[]) {
  const body = table.tBodies[0] ?? table.createTBody();
  body.classList.add("virtual");
  table.classList.add(`vg${nextId++}`);
  const cols = table.tHead?.rows[0]?.cells.length ?? 1;
  grids.set(body, { body, count, row, widest, cols, from: 0, to: 0, sized: false });
  // Full height until the first draw, so a redraw in place (after an edit) keeps the scroll position.
  body.replaceChildren(spacer(cols, count() * rowHeight), spacer(cols, 0));
}

/** How many rows a virtual body has, drawn or not. */
export const rowCount = (body: HTMLTableSectionElement) => grids.get(body)?.count() ?? 0;

/** The drawn row `i` of a virtual body, if it is in the window. */
export function rowAt(body: HTMLTableSectionElement, i: number): HTMLTableRowElement | undefined {
  const g = grids.get(body);
  return g && i >= g.from && i < g.to ? body.rows[i - g.from + 1] : undefined; // +1: the top spacer
}

/** The data index of a drawn row, or null for a spacer or a row of another table. */
export const indexOf = (tr: HTMLTableRowElement | null | undefined) => (tr?.dataset.ri === undefined ? null : Number(tr.dataset.ri));

/** Draws the window of every virtual grid in `scroller`; `force` redraws even when the window didn't move. */
export function drawGrids(scroller: HTMLElement, force = false) {
  for (const body of scroller.querySelectorAll<HTMLTableSectionElement>("tbody.virtual")) {
    const g = grids.get(body);
    if (g) draw(g, scroller, force);
  }
}

function spacer(cols: number, height: number) {
  const tr = document.createElement("tr");
  tr.className = "spacer";
  const td = tr.insertCell();
  td.colSpan = cols;
  td.style.height = `${height}px`;
  return tr;
}

function draw(g: Grid, scroller: HTMLElement, force: boolean) {
  if (!g.body.isConnected) return;
  const n = g.count();
  const view = scroller.getBoundingClientRect();
  const top = view.top - g.body.getBoundingClientRect().top; // how far the viewport's top is into the rows
  const from = Math.min(n, Math.max(0, Math.floor((top / rowHeight - OVERSCAN) / STEP) * STEP));
  const to = Math.min(n, Math.max(from, Math.ceil(((top + view.height) / rowHeight + OVERSCAN) / STEP) * STEP));
  if (!force && g.sized && from === g.from && to === g.to) return;
  // An open cell editor is about to leave the DOM: keep what was typed. Its blur redraws everything.
  const editing = g.body.querySelector<HTMLElement>(".cell-input");
  if (editing && editing === document.activeElement) {
    editing.blur();
    if (!g.body.isConnected) return;
  }
  const rows: HTMLTableRowElement[] = [];
  for (let i = from; i < to; i++) {
    const tr = g.row(i);
    tr.dataset.ri = String(i);
    if (i % 2) tr.classList.add("alt");
    rows.push(tr);
  }
  g.from = from;
  g.to = to;
  g.body.replaceChildren(spacer(g.cols, from * rowHeight), ...rows, spacer(g.cols, (n - to) * rowHeight));
  if (!g.sized) size(g);
  const h = rows[0]?.getBoundingClientRect().height;
  if (h && Math.abs(h - rowHeight) > 0.5) {
    rowHeight = h;
    draw(g, scroller, true);
  }
}

/** Minimum column widths from the widest text in each column, as a style only this table matches. Its
 *  specificity is below the column layout's (`#out tr > …`), so a width the user set still wins. */
function size(g: Grid) {
  const td = g.body.querySelector<HTMLElement>("tr:not(.spacer) > td:not(.rownum)");
  const table = g.body.parentElement as HTMLTableElement | null;
  const ctx = document.createElement("canvas").getContext("2d");
  if (!td || !table || !ctx) return;
  g.sized = true;
  const cs = getComputedStyle(td);
  ctx.font = cs.font;
  const pad = parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight) + parseFloat(cs.borderRightWidth) + 2;
  const id = [...table.classList].find((c) => /^vg\d+$/.test(c));
  const rules = g.widest().map((text, i) => {
    const w = Math.min(MAX_WIDTH, Math.ceil(ctx.measureText(text).width + pad));
    return `.${id} > thead > tr > :nth-child(${i + 1}) { min-width: ${w}px; }`;
  });
  const style = document.createElement("style");
  style.textContent = rules.join("\n");
  table.tHead?.rows[0]?.cells[0]?.append(style);
}

/** Scrolls so row `i` of `body` is in view, draws it, and returns it. */
export function reveal(scroller: HTMLElement, body: HTMLTableSectionElement, i: number) {
  const g = grids.get(body);
  if (!g) return undefined;
  if (!rowAt(body, i)) {
    const top = body.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
    scroller.scrollTop = top + i * rowHeight - scroller.clientHeight / 2;
    draw(g, scroller, false);
  }
  return rowAt(body, i);
}

/** The longest value (by length) of each column, row numbers first. A plain scan: cheap next to drawing. */
export function widestValues(rows: (string | null)[][], columns: number, lastRowNumber: number) {
  const out = [String(lastRowNumber), ...Array<string>(columns).fill("NULL")];
  for (const row of rows) {
    for (let c = 0; c < columns; c++) {
      const v = row[c];
      if (v !== null && v.length > out[c + 1].length) out[c + 1] = v.length > 200 ? v.slice(0, 200) : v;
    }
  }
  return out;
}
