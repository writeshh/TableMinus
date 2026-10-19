// A custom script's table(rows) output as cells (scripts.ts), kept apart from the DOM so it can be tested.

/** Rows a table in the output shows; the script still has them all. */
export const SHOWN_ROWS = 1_000;

/** A table for `rows`: an array of objects (keys as columns) or of arrays. */
export function tableOf(rows: unknown): { columns: string[]; cells: string[][]; more: number } {
  const list = Array.isArray(rows) ? rows : [rows];
  const shown = list.slice(0, SHOWN_ROWS);
  const text = (v: unknown) => (v === null || v === undefined ? "NULL" : typeof v === "object" ? JSON.stringify(v) : String(v));
  if (shown.every(Array.isArray)) {
    const width = Math.max(0, ...shown.map((r) => (r as unknown[]).length));
    return { columns: Array.from({ length: width }, (_, i) => String(i + 1)), cells: shown.map((r) => Array.from({ length: width }, (_, i) => text((r as unknown[])[i]))), more: list.length - shown.length };
  }
  const columns: string[] = [];
  for (const r of shown) for (const k of Object.keys(typeof r === "object" && r ? r : { value: r })) if (!columns.includes(k)) columns.push(k);
  const cells = shown.map((r) => columns.map((c) => text(typeof r === "object" && r ? (r as Record<string, unknown>)[c] : c === "value" ? r : undefined)));
  return { columns, cells, more: list.length - shown.length };
}
