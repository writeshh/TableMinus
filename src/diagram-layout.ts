// The ER diagram's geometry and file format: box sizes, the automatic layout, and reading and writing diagram
// files. No DOM here, so it's tested on its own (diagram-layout.test.ts); diagram.ts draws.

export type Column = { name: string; data_type: string; primary: boolean; nullable: boolean };
export type Table = { name: string; view: boolean; columns: Column[] };
export type Link = { name: string; table: string; columns: string[]; ref_table: string; ref_columns: string[] };
export type Diagram = { tables: Table[]; links: Link[] };
export type Point = { x: number; y: number };

export const HEAD = 28;
export const ROW = 20;
/** Columns past this show as one "… n more" row, so a 300-column table doesn't fill the canvas. */
export const MAX_COLUMNS = 40;
/** Width of one character of the diagram's monospace font at 12px, for sizing boxes without measuring text. */
const CHAR = 7.3;
const GAP_X = 90;
const GAP_Y = 36;

/** How many column rows a table's box draws (the "more" row included). */
export const shownRows = (t: Table) => (t.columns.length > MAX_COLUMNS ? MAX_COLUMNS + 1 : Math.max(t.columns.length, 1));

/** A box's size: the widest of its title and "key name  type" rows. */
export function boxSize(t: Table): { w: number; h: number } {
  const longest = Math.max(
    t.name.length + 4,
    ...t.columns.slice(0, MAX_COLUMNS).map((c) => 3 + c.name.length + 2 + c.data_type.length),
  );
  return { w: Math.round(Math.min(Math.max(longest * CHAR + 24, 160), 520)), h: HEAD + shownRows(t) * ROW + 6 };
}

/** The y of a column's row inside its box (its middle), or of the header when the column isn't drawn. */
export function columnY(t: Table, column: string): number {
  const i = t.columns.findIndex((c) => c.name === column);
  return i < 0 || i >= MAX_COLUMNS ? HEAD / 2 : HEAD + i * ROW + ROW / 2 + 3;
}

/**
 * Lays tables out in layers: tables that reference nothing on the left, each referencing table one column to the
 * right of the furthest table it references; tables with no keys at all go in a grid underneath. Within a layer the
 * order follows the neighbours' (a few barycentre sweeps), so lines cross less. Tall layers wrap into more columns.
 */
export function layout(tables: Table[], links: Link[]): Map<string, Point> {
  const byName = new Map(tables.map((t) => [t.name, t]));
  const out = new Map<string, string[]>(); // table → tables it references
  const near = new Map<string, Set<string>>();
  for (const t of tables) (out.set(t.name, []), near.set(t.name, new Set()));
  for (const l of links) {
    if (l.table === l.ref_table || !byName.has(l.table) || !byName.has(l.ref_table)) continue;
    out.get(l.table)!.push(l.ref_table);
    near.get(l.table)!.add(l.ref_table);
    near.get(l.ref_table)!.add(l.table);
  }
  const rank = new Map<string, number>();
  const visiting = new Set<string>();
  const rankOf = (name: string): number => {
    const known = rank.get(name);
    if (known !== undefined) return known;
    if (visiting.has(name)) return 0; // a cycle: break it here
    visiting.add(name);
    const r = Math.max(-1, ...out.get(name)!.map(rankOf)) + 1;
    visiting.delete(name);
    rank.set(name, r);
    return r;
  };
  const linked = tables.filter((t) => near.get(t.name)!.size > 0).sort((a, b) => a.name.localeCompare(b.name));
  const loose = tables.filter((t) => near.get(t.name)!.size === 0).sort((a, b) => a.name.localeCompare(b.name));

  const ranked: string[][] = [];
  for (const t of linked) (ranked[rankOf(t.name)] ??= []).push(t.name);
  const layers = ranked.filter(Boolean); // a broken cycle can leave a rank empty
  const index = new Map<string, number>();
  const reindex = () => layers.forEach((l) => l.forEach((n, i) => index.set(n, i / Math.max(l.length, 1))));
  reindex();
  for (let sweep = 0; sweep < 4; sweep++) {
    for (const l of layers) {
      const centre = (n: string) => {
        const ns = [...near.get(n)!];
        return ns.length ? ns.reduce((s, m) => s + index.get(m)!, 0) / ns.length : index.get(n)!;
      };
      const keyed = new Map(l.map((n) => [n, centre(n)]));
      l.sort((a, b) => keyed.get(a)! - keyed.get(b)!);
    }
    reindex();
  }

  const area = tables.reduce((s, t) => {
    const { w, h } = boxSize(t);
    return s + (w + GAP_X) * (h + GAP_Y);
  }, 0);
  const maxHeight = Math.max(900, Math.sqrt(area) * 0.9);
  const pos = new Map<string, Point>();
  let x = 0;
  let bottom = 0;
  for (const l of layers) {
    let y = 0;
    let width = 0;
    for (const n of l) {
      const { w, h } = boxSize(byName.get(n)!);
      if (y > 0 && y + h > maxHeight) (x += width + GAP_X, (y = 0), (width = 0)); // wrap into another column
      pos.set(n, { x, y });
      y += h + GAP_Y;
      width = Math.max(width, w);
      bottom = Math.max(bottom, y);
    }
    x += width + GAP_X;
  }
  // The keyless tables: rows as wide as the layers above (or a square-ish grid when there are none).
  const rowWidth = Math.max(x - GAP_X, Math.sqrt(area) * 1.2);
  let [gx, gy, rowHeight] = [0, bottom ? bottom + GAP_Y * 2 : 0, 0];
  for (const t of loose) {
    const { w, h } = boxSize(t);
    if (gx > 0 && gx + w > rowWidth) (gx = 0, (gy += rowHeight + GAP_Y), (rowHeight = 0));
    pos.set(t.name, { x: gx, y: gy });
    gx += w + GAP_X / 2;
    rowHeight = Math.max(rowHeight, h);
  }
  return pos;
}

// ---------- Diagram files ----------

/** What a diagram file holds: which tables, where. The columns and keys are read from the database on opening. */
export type DiagramFile = { version: 1; schema: string | null; tables: { name: string; x: number; y: number }[] };
/** More tables than any real diagram has; a file claiming more is refused rather than drawn. */
export const MAX_FILE_TABLES = 10_000;

export function toFile(schema: string | null, positions: Map<string, Point>): string {
  const tables = [...positions].map(([name, p]) => ({ name, x: Math.round(p.x), y: Math.round(p.y) }));
  const file: DiagramFile = { version: 1, schema, tables };
  return JSON.stringify({ app: "Table Minus diagram", ...file }, null, 2) + "\n";
}

/** Reads a diagram file, or throws an Error saying what's wrong with it. Nothing is changed until it's whole. */
export function fromFile(text: string): DiagramFile {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    throw new Error("This isn't a diagram file (it isn't JSON).");
  }
  const f = raw as Partial<DiagramFile> | null;
  if (typeof f !== "object" || f === null || !("version" in f)) throw new Error("This isn't a diagram file (no version).");
  if (f.version !== 1) throw new Error(`This diagram file is version ${String(f.version)}; this app reads version 1.`);
  if (f.schema !== null && f.schema !== undefined && typeof f.schema !== "string") throw new Error("The diagram file's schema isn't text.");
  if (!Array.isArray(f.tables)) throw new Error("The diagram file has no table list.");
  if (f.tables.length > MAX_FILE_TABLES) throw new Error(`The diagram file lists over ${MAX_FILE_TABLES.toLocaleString()} tables.`);
  const seen = new Set<string>();
  for (const t of f.tables as unknown[]) {
    const e = t as { name?: unknown; x?: unknown; y?: unknown } | null;
    if (typeof e !== "object" || e === null || typeof e.name !== "string" || !Number.isFinite(e.x) || !Number.isFinite(e.y))
      throw new Error("The diagram file has a table without a name or position.");
    if (seen.has(e.name)) throw new Error(`The diagram file lists ${e.name} twice.`);
    seen.add(e.name);
  }
  return { version: 1, schema: f.schema ?? null, tables: f.tables.map(({ name, x, y }) => ({ name, x, y })) };
}
