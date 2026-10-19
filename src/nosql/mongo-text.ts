// MongoDB text the view shows (mongo.ts), kept apart from the DOM so it can be tested: documents (relaxed Extended
// JSON from nosql/mongo.rs) as grid columns and cells, values written back in mongosh syntax, and the shell text a
// collection's page is read with.

/** Extended JSON's wrappers, in the mongosh spelling they show as. */
function wrapped(v: Record<string, unknown>): string | null {
  const keys = Object.keys(v);
  if (keys.length === 1) {
    const [k] = keys;
    const x = v[k] as Record<string, unknown> | string;
    if (k === "$oid") return `ObjectId("${x}")`;
    if (k === "$date") return `ISODate("${typeof x === "object" ? new Date(Number(x.$numberLong)).toISOString() : x}")`;
    if (k === "$numberLong") return `NumberLong("${x}")`;
    if (k === "$numberDecimal") return `NumberDecimal("${x}")`;
    if (k === "$numberInt") return `NumberInt(${x})`;
    if (k === "$numberDouble") return String(x);
    if (k === "$uuid") return `UUID("${x}")`;
    if (k === "$minKey") return "MinKey()";
    if (k === "$maxKey") return "MaxKey()";
    if (k === "$timestamp" && typeof x === "object") return `Timestamp({ t: ${x.t}, i: ${x.i} })`;
    if (k === "$regularExpression" && typeof x === "object") return `/${x.pattern}/${x.options}`;
    if (k === "$binary" && typeof x === "object") {
      const sub = parseInt(String(x.subType), 16);
      if (sub === 4 && typeof x.base64 === "string") return `UUID("${uuidOf(x.base64)}")`;
      return `BinData(${sub}, "${x.base64}")`;
    }
  }
  return null;
}

function uuidOf(base64: string) {
  const hex = [...atob(base64)].map((c) => c.charCodeAt(0).toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

const plainKey = (k: string) => (/^[A-Za-z_$][\w$]*$/.test(k) ? k : JSON.stringify(k));

/** A value in mongosh syntax; `indent` lays objects and arrays out over lines. */
export function shellText(v: unknown, indent = "", step = ""): string {
  if (v === null) return "null";
  if (typeof v !== "object") return typeof v === "string" ? JSON.stringify(v) : String(v);
  const inner = indent + step;
  const sep = step ? `,\n${inner}` : ", ";
  if (Array.isArray(v)) {
    if (!v.length) return "[]";
    const items = v.map((x) => shellText(x, inner, step));
    return step ? `[\n${inner}${items.join(sep)}\n${indent}]` : `[${items.join(sep)}]`;
  }
  const w = wrapped(v as Record<string, unknown>);
  if (w !== null) return w;
  const entries = Object.entries(v as Record<string, unknown>).map(([k, x]) => `${plainKey(k)}: ${shellText(x, inner, step)}`);
  if (!entries.length) return "{}";
  return step ? `{\n${inner}${entries.join(sep)}\n${indent}}` : `{ ${entries.join(sep)} }`;
}

/** A document laid out for editing. */
export const documentText = (doc: unknown) => shellText(doc, "", "  ");

/** A cell: text as it is, numbers and booleans as written, anything else in mongosh syntax; null stays NULL. A
 *  missing field is `undefined`, shown empty. */
export function cell(v: unknown): string | null {
  if (v === null) return null;
  if (v === undefined) return "";
  if (typeof v === "string") return v;
  if (typeof v !== "object") return String(v);
  return shellText(v);
}

/** The grid's columns: every top-level field of the documents, in the order they first appear, `_id` first. */
export function columnsOf(docs: unknown[]): string[] {
  const seen = new Set<string>(["_id"]);
  for (const d of docs) if (d && typeof d === "object") for (const k of Object.keys(d)) seen.add(k);
  if (!docs.some((d) => d && typeof d === "object" && "_id" in d)) seen.delete("_id");
  return [...seen];
}

/** The shell expression for a value read from the server: its Extended JSON, which mongosh reads as the same value. */
export const expression = (v: unknown) => JSON.stringify(v);

/** What a cell typed into becomes, as a shell expression: text stays text where the field held text (or a missing
 *  or null field is given plain words); numbers, true/false/null, quoted strings, documents, arrays and calls such as
 *  ObjectId("…") are read as written. */
export function typedExpression(original: unknown, typed: string): string {
  const t = typed.trim();
  if (typeof original === "string") return JSON.stringify(typed);
  const literal = /^(-?\d+(\.\d+)?([eE][+-]?\d+)?|true|false|null)$/.test(t) || /^[[{"'/]/.test(t) || /^[A-Za-z]\w*\(/.test(t);
  return literal ? t : JSON.stringify(typed);
}

/** The script that reads one page of a collection: `filter` and `sort` as typed (blank for none). */
export function pageScript(collection: string, filter: string, sort: string, skip: number, limit: number) {
  const f = filter.trim() || "{}";
  const s = sort.trim();
  return `db.getCollection(${JSON.stringify(collection)}).find(${f})${s ? `.sort(${s})` : ""}.skip(${skip}).limit(${limit})`;
}

/** The script that counts the documents a filter matches. */
export const countScript = (collection: string, filter: string) =>
  `db.getCollection(${JSON.stringify(collection)}).countDocuments(${filter.trim() || "{}"})`;
