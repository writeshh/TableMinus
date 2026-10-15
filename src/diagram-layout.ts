// The ER diagram's geometry and file format: box sizes, the automatic layout, the lines' routes around boxes, and
// reading and writing diagram files (positions, colours and notes). No DOM here, so it's tested on its own (diagram-layout.test.ts); diagram.ts draws.

export type Column = { name: string; data_type: string; primary: boolean; nullable: boolean };
/** `external`: a table in another schema that a foreign key here points at, drawn as a stub of the columns referenced. */
export type Table = { name: string; view: boolean; columns: Column[]; external?: boolean };
export type Link = { name: string; table: string; columns: string[]; ref_table: string; ref_columns: string[] };
export type Diagram = { tables: Table[]; links: Link[] };
export type Point = { x: number; y: number };
export type Rect = { x: number; y: number; w: number; h: number };

/** Header colours a table can take: the brand's four primaries and two more. */
export const TABLE_COLOURS = ["blue", "pink", "yellow", "orange", "green", "purple"] as const;
export type TableColour = (typeof TABLE_COLOURS)[number];
/** A table's look beyond its place: a header colour and a note under its box. */
export type Decoration = { color?: TableColour; note?: string };
/** Longer than a note on a table needs to be; a file with a longer one is refused. */
export const MAX_NOTE = 500;

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

// ---------- Lines ----------

/** How far a line runs straight out of a box before it turns, and how far it keeps from other boxes. */
const STUB = 16;
const CLEAR = 8;

/** Whether the axis-aligned segment from `a` to `b` passes through `box` (or within CLEAR of it). */
function crosses(box: Rect, a: Point, b: Point) {
  const [x0, x1, y0, y1] = [Math.min(a.x, b.x), Math.max(a.x, b.x), Math.min(a.y, b.y), Math.max(a.y, b.y)];
  return x1 >= box.x - CLEAR && x0 <= box.x + box.w + CLEAR && y1 >= box.y - CLEAR && y0 <= box.y + box.h + CLEAR;
}

/**
 * An orthogonal route from `from` (on a box's side, leaving in direction `out`: 1 right, -1 left) to `to` (entered
 * from direction `into`). It runs out a stub, along to a vertical channel, down or up it, and in: the channel is the
 * one nearest the middle whose three runs pass no box of `boxes` (the two ends' own boxes left out by the caller);
 * with none clear it takes the middle. A line back into the same side goes round outside both ends.
 */
export function route(from: Point, out: 1 | -1, to: Point, into: 1 | -1, boxes: Rect[]): Point[] {
  const s = { x: from.x + out * STUB, y: from.y };
  const e = { x: to.x + into * STUB, y: to.y };
  const across = (out === 1 && into === -1 && s.x <= e.x) || (out === -1 && into === 1 && e.x <= s.x);
  let candidates: number[];
  if (across) {
    const [lo, hi] = [Math.min(s.x, e.x), Math.max(s.x, e.x)];
    const mid = (lo + hi) / 2;
    candidates = [mid];
    for (let d = CLEAR; d <= (hi - lo) / 2; d += CLEAR) candidates.push(mid - d, mid + d);
  } else {
    const edge = out === 1 ? Math.max(s.x, e.x) : Math.min(s.x, e.x);
    candidates = Array.from({ length: 40 }, (_, k) => edge + out * k * CLEAR);
  }
  // Only boxes near the route can block it.
  const [left, right] = [Math.min(s.x, e.x, ...candidates), Math.max(s.x, e.x, ...candidates)];
  const [top, bottom] = [Math.min(s.y, e.y), Math.max(s.y, e.y)];
  const near = boxes.filter((b) => b.x <= right + CLEAR && b.x + b.w >= left - CLEAR && b.y <= bottom + CLEAR && b.y + b.h >= top - CLEAR);
  const clear = (x: number) => {
    const [p, q] = [{ x, y: s.y }, { x, y: e.y }];
    return !near.some((b) => crosses(b, s, p) || crosses(b, p, q) || crosses(b, q, e));
  };
  const x = candidates.find(clear) ?? candidates[0];
  return [from, s, { x, y: s.y }, { x, y: e.y }, e, to];
}

/** An SVG path along `points`, its corners rounded by up to `radius` (less where a run is short). Repeated points
 *  and straight-through ones are dropped first. */
export function roundedPath(points: Point[], radius = 6): string {
  const pts = points.filter((p, i) => i === 0 || p.x !== points[i - 1].x || p.y !== points[i - 1].y)
    .filter((p, i, all) => {
      if (i === 0 || i === all.length - 1) return true;
      const [a, b] = [all[i - 1], all[i + 1]];
      return !((a.x === p.x && p.x === b.x) || (a.y === p.y && p.y === b.y));
    });
  const r1 = (n: number) => Math.round(n * 10) / 10;
  let d = `M${r1(pts[0].x)} ${r1(pts[0].y)}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const [a, p, b] = [pts[i - 1], pts[i], pts[i + 1]];
    const r = Math.min(radius, Math.hypot(p.x - a.x, p.y - a.y) / 2, Math.hypot(b.x - p.x, b.y - p.y) / 2);
    const toward = (q: Point, k: number) => ({ x: p.x + Math.sign(q.x - p.x) * k, y: p.y + Math.sign(q.y - p.y) * k });
    const [before, after] = [toward(a, r), toward(b, r)];
    d += ` L${r1(before.x)} ${r1(before.y)} Q${r1(p.x)} ${r1(p.y)} ${r1(after.x)} ${r1(after.y)}`;
  }
  const last = pts[pts.length - 1];
  return `${d} L${r1(last.x)} ${r1(last.y)}`;
}

// ---------- Diagram files ----------

/** What a diagram file holds: which tables, where, and how each looks. The columns and keys are read from the
 *  database on opening. */
export type FileTable = { name: string; x: number; y: number } & Decoration;
export type DiagramFile = { version: 1; schema: string | null; tables: FileTable[] };
/** More tables than any real diagram has; a file claiming more is refused rather than drawn. */
export const MAX_FILE_TABLES = 10_000;

export function toFile(schema: string | null, positions: Map<string, Point>, looks = new Map<string, Decoration>()): string {
  const tables = [...positions].map(([name, p]) => ({ name, x: Math.round(p.x), y: Math.round(p.y), ...looks.get(name) }));
  const file: DiagramFile = { version: 1, schema, tables };
  return JSON.stringify({ app: "TableMinus diagram", ...file }, null, 2) + "\n";
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
    const { color, note } = t as Decoration;
    if (color !== undefined && !(TABLE_COLOURS as readonly unknown[]).includes(color)) throw new Error(`The diagram file gives ${e.name} an unknown colour.`);
    if (note !== undefined && (typeof note !== "string" || note.length > MAX_NOTE)) throw new Error(`The diagram file's note on ${e.name} isn't text of up to ${MAX_NOTE} characters.`);
  }
  return {
    version: 1, schema: f.schema ?? null,
    tables: f.tables.map(({ name, x, y, color, note }) => ({ name, x, y, ...(color ? { color } : {}), ...(note ? { note } : {}) })),
  };
}
