// Per-table column layout (hidden columns and widths), remembered on this machine and applied as one
// generated style sheet, so hiding or resizing never re-renders the grid.

export type Layout = { hidden: string[]; widths: Record<string, number> };

export const MIN_WIDTH = 40;
const storageKey = (connection: string, table: string) => `columns:${connection}:${table}`;

export function loadLayout(connection: string, table: string): Layout {
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey(connection, table)) ?? "{}");
    return { hidden: Array.isArray(saved.hidden) ? saved.hidden : [], widths: saved.widths ?? {} };
  } catch {
    return { hidden: [], widths: {} };
  }
}

export function saveLayout(connection: string, table: string, layout: Layout) {
  const empty = !layout.hidden.length && !Object.keys(layout.widths).length;
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
