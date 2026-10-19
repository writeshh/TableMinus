// DynamoDB text the view shows (dynamo.ts), kept apart from the DOM so it can be tested: items in DynamoDB's typed
// JSON (`{"S": "Ada"}`, `{"N": "3"}`, `{"M": {…}}`) as grid cells and as PartiQL literals, and what a typed cell
// becomes. dynamo/literal.rs reads the same literals back.

type Attr = Record<string, unknown>;

const quote = (s: string) => `'${s.replaceAll("'", "''")}'`;

/** A typed value as a PartiQL literal: `'text'`, `3`, `{'a': 1}`, `[…]`, `<<'x'>>`. */
export function literal(v: unknown): string {
  if (!v || typeof v !== "object") return "null";
  const a = v as Attr;
  if ("S" in a) return quote(String(a.S));
  if ("N" in a) return String(a.N);
  if ("BOOL" in a) return String(a.BOOL);
  if ("NULL" in a) return "null";
  if ("B" in a) return `'<${binaryBytes(String(a.B))} bytes>'`;
  if ("L" in a && Array.isArray(a.L)) return `[${a.L.map(literal).join(", ")}]`;
  if ("M" in a && a.M && typeof a.M === "object") {
    return `{${Object.entries(a.M as Attr).map(([k, x]) => `${quote(k)}: ${literal(x)}`).join(", ")}}`;
  }
  if ("SS" in a && Array.isArray(a.SS)) return `<<${a.SS.map((s) => quote(String(s))).join(", ")}>>`;
  if ("NS" in a && Array.isArray(a.NS)) return `<<${a.NS.join(", ")}>>`;
  if ("BS" in a && Array.isArray(a.BS)) return `<<${a.BS.length} binary values>>`;
  return JSON.stringify(v);
}

const binaryBytes = (base64: string) => Math.floor((base64.replace(/=+$/, "").length * 3) / 4);

/** A cell: text as it is, numbers as written, anything else as a literal; null stays NULL, a missing attribute empty. */
export function cell(v: unknown): string | null {
  if (v === undefined) return "";
  const a = v as Attr;
  if (a && typeof a === "object") {
    if ("NULL" in a) return null;
    if ("S" in a) return String(a.S);
    if ("N" in a) return String(a.N);
  }
  return literal(v);
}

/** The grid's columns: the key's attributes first, then every other attribute in the order it first appears. */
export function columnsOf(items: Attr[], keys: string[]): string[] {
  const seen = new Set<string>(keys);
  for (const item of items) for (const k of Object.keys(item)) seen.add(k);
  return [...seen];
}

/** What a typed cell becomes, as a literal: text stays text where the attribute held text (or a missing or null one
 *  is given plain words); numbers, true/false/null, quoted text, maps, lists and sets are read as written. */
export function typedLiteral(original: unknown, typed: string): string {
  const t = typed.trim();
  const a = original as Attr | undefined;
  if (a && typeof a === "object" && "S" in a) return quote(typed);
  const literalish = /^(-?\d+(\.\d+)?([eE][+-]?\d+)?|true|false|null)$/i.test(t) || /^(['"{[]|<<)/.test(t);
  return literalish ? t : quote(typed);
}

/** An item laid out for editing, an attribute a line. */
export function itemText(item: Attr): string {
  const entries = Object.entries(item).map(([k, v]) => `  ${quote(k)}: ${literal(v)}`);
  return entries.length ? `{\n${entries.join(",\n")}\n}` : "{}";
}

/** The filter that finds items with this value in `field`. */
export const filterFor = (field: string, v: unknown) => `"${field.replaceAll('"', '""')}" = ${literal(v)}`;
