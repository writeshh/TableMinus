// Per-table column layout (hidden columns, widths and order), remembered on this machine. Hiding and resizing
// apply as one generated style sheet, so they never re-render the grid; the order rearranges the loaded rows.

/** `order`: column names in the order shown; empty for the table's own order. */
export type Layout = { hidden: string[]; widths: Record<string, number>; order: string[] };

export const MIN_WIDTH = 40;
const storageKey = (connection: string, table: string) => `columns:${connection}:${table}`;

export function loadLayout(connection: string, table: string): Layout {
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey(connection, table)) ?? "{}");
    const list = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
    return { hidden: list(saved.hidden), widths: saved.widths ?? {}, order: list(saved.order) };
  } catch {
    return { hidden: [], widths: {}, order: [] };
  }
}

export function saveLayout(connection: string, table: string, layout: Layout) {
  const empty = !layout.hidden.length && !Object.keys(layout.widths).length && !layout.order.length;
  try {
    if (empty) localStorage.removeItem(storageKey(connection, table));
    else localStorage.setItem(storageKey(connection, table), JSON.stringify(layout));
  } catch {} // unavailable storage: the layout still applies until the tab closes
}

/** CSS for `#out` that hides and sizes columns by position; the row number is the first cell of each row. */
export function layoutCss(columns: string[], layout: Layout): string {
  return columns
    .map((name, i) => {
      const cell = `#out tr > :nth-child(${i + 2})`;
      if (layout.hidden.includes(name)) return `${cell} { display: none; }`;
      const w = layout.widths[name];
      return w ? `${cell} { width: ${w}px; min-width: ${w}px; max-width: ${w}px; }` : "";
    })
    .join("\n");
}

/** `r` with its columns in `order`: the named ones first, as ordered, then the rest (columns added since) in their
 *  own order. The same object when nothing moves. */
export function arrange<R extends { columns: string[]; rows: (string | null)[][] }>(r: R, order: string[]): R {
  const known = order.filter((c) => r.columns.includes(c));
  const perm = [...known, ...r.columns.filter((c) => !known.includes(c))].map((c) => r.columns.indexOf(c));
  if (perm.every((p, i) => p === i)) return r;
  return { ...r, columns: perm.map((i) => r.columns[i]), rows: r.rows.map((row) => perm.map((i) => row[i])) };
}

/** `columns` with the one at `from` moved to `to`. */
export function moveColumn(columns: string[], from: number, to: number) {
  const next = [...columns];
  next.splice(to, 0, ...next.splice(from, 1));
  return next;
}
