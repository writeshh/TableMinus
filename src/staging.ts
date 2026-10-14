// The grid's staged changes (edited cells, added rows, deleted rows) with undo and redo, and the pure helpers
// around them: pasting tab-separated text, column kinds for the cell editors, the hex dump and the save payload.
// No DOM here, so it's tested on its own (staging.test.ts).

/** A cell's staged value: text, NULL, or the bytes of a file (binary columns). */
export type Value = string | null | { file: string };
type Snapshot = { edits: [number, [number, Value][]][]; added: [number, Value][][]; deleted: number[] };

/** Rows are numbered as on the page: 0…n-1 are the page's rows, n… the added rows in order. */
export class Staged {
  edits = new Map<number, Map<number, Value>>();
  added: Map<number, Value>[] = [];
  deleted = new Set<number>();
  private past: Snapshot[] = [];
  private future: Snapshot[] = [];

  /** Runs `fn` as one undo step. */
  change(fn: () => void) {
    this.past.push(this.snapshot());
    this.future = [];
    fn();
  }

  undo() {
    const s = this.past.pop();
    if (s) (this.future.push(this.snapshot()), this.restore(s));
    return !!s;
  }

  redo() {
    const s = this.future.pop();
    if (s) (this.past.push(this.snapshot()), this.restore(s));
    return !!s;
  }

  clear() {
    this.restore({ edits: [], added: [], deleted: [] });
    this.past = [];
    this.future = [];
  }

  /** Changes waiting to be saved: edited cells of rows that stay, added rows and deleted rows. */
  get count() {
    let n = this.added.length + this.deleted.size;
    for (const [ri, cells] of this.edits) if (!this.deleted.has(ri)) n += cells.size;
    return n;
  }

  /** Sets a cell of row `ri` on a page of `n` rows. Back to `original` unstages it. Call inside `change`. */
  set(ri: number, ci: number, v: Value, n: number, original: Value) {
    if (ri >= n) return void this.added[ri - n]?.set(ci, v);
    const row = this.edits.get(ri) ?? new Map<number, Value>();
    if (same(v, original)) row.delete(ci);
    else row.set(ci, v);
    if (row.size) this.edits.set(ri, row);
    else this.edits.delete(ri);
  }

  /** Deletes rows (added ones disappear); when every page row given is deleted already, restores them instead. */
  remove(rows: number[], n: number) {
    const page = rows.filter((ri) => ri < n);
    const restore = page.length > 0 && page.every((ri) => this.deleted.has(ri));
    for (const ri of page) restore ? this.deleted.delete(ri) : this.deleted.add(ri);
    const gone = new Set(rows.filter((ri) => ri >= n).map((ri) => ri - n));
    this.added = this.added.filter((_, i) => !gone.has(i));
  }

  private snapshot(): Snapshot {
    return {
      edits: [...this.edits].map(([ri, cells]) => [ri, [...cells]]),
      added: this.added.map((row) => [...row]),
      deleted: [...this.deleted],
    };
  }

  private restore(s: Snapshot) {
    this.edits = new Map(s.edits.map(([ri, cells]) => [ri, new Map(cells)]));
    this.added = s.added.map((row) => new Map(row));
    this.deleted = new Set(s.deleted);
  }
}

export const same = (a: Value, b: Value) =>
  a === b || (typeof a === "object" && typeof b === "object" && a?.file === b?.file);

/** Clipboard text as cells: the inverse of copy.ts `plain`. Tabs split cells, line breaks split rows, a field in
 *  double quotes may hold either (with "" for a quote), and an unquoted NULL is NULL. */
export function parseTsv(text: string): (string | null)[][] {
  text = text.replace(/\r?\n$/, ""); // the line break most apps add after the last row
  const rows: (string | null)[][] = [];
  let row: (string | null)[] = [];
  let i = 0;
  for (;;) {
    let cell: string | null;
    if (text[i] === '"') {
      let v = "";
      i++;
      for (;;) {
        const q = text.indexOf('"', i);
        if (q < 0) return [[text]]; // an unclosed quote: not our format, so take the text as one value
        v += text.slice(i, q);
        i = q + 1;
        if (text[i] !== '"') break;
        v += '"';
        i++;
      }
      cell = v;
    } else {
      const end = text.slice(i).search(/\t|\r?\n/);
      const raw = end < 0 ? text.slice(i) : text.slice(i, i + end);
      i += raw.length;
      cell = raw === "NULL" ? null : raw;
    }
    row.push(cell);
    if (i >= text.length) break;
    if (text[i] === "\t") i++;
    else if (text[i] === "\r" || text[i] === "\n") {
      i += text.startsWith("\r\n", i) ? 2 : 1;
      rows.push(row);
      row = [];
    } else return [[text]]; // text after a closing quote: not our format
  }
  rows.push(row);
  return rows;
}

/** Where pasted cells go: from the selection's top-left corner, clipped to the grid. One value fills the
 *  whole selection. Returns [row, column, value] triples. */
export function pasteCells(
  clip: (string | null)[][],
  block: { r0: number; c0: number; r1: number; c1: number },
  rows: number,
  cols: number,
): [number, number, string | null][] {
  const out: [number, number, string | null][] = [];
  if (clip.length === 1 && clip[0].length === 1) {
    for (let r = block.r0; r <= block.r1; r++) for (let c = block.c0; c <= block.c1; c++) out.push([r, c, clip[0][0]]);
    return out;
  }
  clip.forEach((line, dr) =>
    line.forEach((v, dc) => {
      const [r, c] = [block.r0 + dr, block.c0 + dc];
      if (r < rows && c < cols) out.push([r, c, v]);
    }),
  );
  return out;
}

export type Kind = "text" | "binary" | "json" | "date" | "datetime" | "time" | "bool";

/** Which editor a column gets, from its type as the catalog reports it (Postgres udt names, MySQL column types,
 *  SQLite declared types). Enums are found separately (enum_values). */
export function columnKind(dataType: string, engine: string): Kind {
  const t = dataType.toLowerCase();
  if (engine !== "postgres" && engine !== "sqlite" && t === "tinyint(1)") return "bool";
  const base = t.replace(/\(.*$/, "").trim();
  if (base === "bytea" || /blob|binary/.test(base)) return "binary";
  if (base === "json" || base === "jsonb") return "json";
  if (base === "bool" || base === "boolean") return "bool";
  if (base === "date") return "date";
  if (base === "timestamp" || base === "datetime") return "datetime";
  if (base === "time") return "time";
  return "text";
}

/** The values a boolean column offers, as the engine shows them. */
export const boolValues = (engine: string) => (engine === "postgres" ? ["true", "false"] : ["1", "0"]);

/** SQLite shows blobs as "<n bytes>"; such a cell must never be saved back as that text. */
export const isBlobPlaceholder = (engine: string, v: Value) => engine === "sqlite" && typeof v === "string" && /^<\d+ bytes>$/.test(v);

const NATIVE: Partial<Record<Kind, RegExp>> = {
  date: /^\d{4}-\d{2}-\d{2}$/,
  time: /^\d{2}:\d{2}(:\d{2}(\.\d{1,3})?)?$/,
  datetime: /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(:\d{2}(\.\d{1,3})?)?$/,
};

/** A native date/time input's value for `v`, or null when the native input can't show it (time zones, more than
 *  millisecond precision, 'infinity'…), so the plain text editor is used instead. */
export function nativeValue(kind: Kind, v: string | null): string | null {
  if (v === null) return "";
  return NATIVE[kind]?.test(v) ? v.replace(" ", "T") : null;
}

/** Whether two JSON texts hold the same value, so re-indenting alone isn't a change. */
export function sameJson(a: string, b: string) {
  try {
    return JSON.stringify(JSON.parse(a)) === JSON.stringify(JSON.parse(b));
  } catch {
    return a === b;
  }
}

/** A hex dump: offset, 16 bytes in hex, and the printable ASCII. */
export function hexDump(hex: string): string {
  const lines: string[] = [];
  for (let at = 0; at < hex.length; at += 32) {
    const bytes = hex.slice(at, at + 32).match(/../g) ?? [];
    const ascii = bytes.map((b) => {
      const n = parseInt(b, 16);
      return n >= 0x20 && n < 0x7f ? String.fromCharCode(n) : ".";
    });
    const hexPart = bytes.map((b, i) => (i === 8 ? " " : "") + b).join(" ");
    lines.push(`${(at / 2).toString(16).padStart(8, "0")}  ${hexPart.padEnd(48)}  ${ascii.join("")}`);
  }
  return lines.join("\n");
}

type Key = { names: string[]; idx: number[] };
type Pair = [string, Value];
export type Changes = {
  deletes: { key: [string, string][]; id?: string }[];
  updates: { key: [string, string][]; id?: string; set: Pair[] }[];
  inserts: Pair[][];
};

/** The save_rows payload. Rows are found by `key`, or by `ids` (row identities from the page) when given. */
export function changesFor(s: Staged, columns: string[], rows: (string | null)[][], key: Key, ids?: string[]): Changes {
  const find = (ri: number) => {
    const k: [string, string][] = key.names.map((n, i) => [n, rows[ri][key.idx[i]] ?? ""]);
    return ids ? { key: k, id: ids[ri] } : { key: k };
  };
  return {
    deletes: [...s.deleted].sort((a, b) => a - b).map(find),
    updates: [...s.edits]
      .filter(([ri]) => !s.deleted.has(ri))
      .map(([ri, cells]) => ({ ...find(ri), set: [...cells].map(([ci, v]): Pair => [columns[ci], v]) })),
    inserts: s.added.map((row) => [...row].map(([ci, v]): Pair => [columns[ci], v])),
  };
}
