// Cassandra in the document view (docview.ts; nosql/cassandra.rs): a keyspace's tables, their rows a page at a time as
// JSON (`SELECT JSON`) with a CQL condition as the filter, values typed as JSON (Cassandra's `fromJson()` reads them),
// and a console for CQL. A row is found again by its primary key, which the table's schema gives when it opens.

import { docView, type Engine, type Host, type Outcome } from "./docview";
import { typedJson } from "./elastic";

type Page = { rows: Record<string, unknown>[]; next: string | null };
type Described = { keys: [string, string][] };

/** Each table's primary key, by session and table. */
const described = new Map<string, Described>();
let session = () => "";
const keysOf = (table: string) => (described.get(`${session()}\u0000${table}`)?.keys ?? []).map(([k]) => k);
const quote = (name: string) => `"${name.replaceAll('"', '""')}"`;

/** A value as CQL writes it, for a filter. */
function cqlLiteral(v: unknown): string {
  if (typeof v === "string") return `'${v.replaceAll("'", "''")}'`;
  if (v === null || v === undefined) return "null";
  return typeof v === "object" ? `'${JSON.stringify(v).replaceAll("'", "''")}'` : String(v);
}

const cassandra: Engine = {
  kind: "cassandra",
  css: "cassandra",
  words: { collection: "table", doc: "row" },
  filterHint: "Filter: a CQL condition, e.g. owner = 'ada' AND id > 3 (tables are read through with ALLOW FILTERING)",
  sortHint: null,
  console: {
    language: async () => {
      const { sql, StandardSQL } = await import("@codemirror/lang-sql");
      return sql({ dialect: StandardSQL });
    },
    start: "SELECT * FROM ",
    hint: "CQL: SELECT * FROM pets WHERE owner = 'ada'; INSERT, UPDATE, DELETE, CREATE TABLE, USE keyspace. Statements end in ;. ⌘↩ runs.",
    confirm: (text) => `Run this? It may change data. ${text.length > 300 ? text.slice(0, 300) + "…" : text}`,
  },
  async page(call, table, q) {
    const page = await call<Page>("cassandra_page", { table, filter: q.filter, size: q.size, next: q.token });
    return { docs: page.rows, next: page.next };
  },
  count: async () => null, // Cassandra counts by reading every row; SELECT count(*) in the console does that
  columns(docs, table) {
    const seen = new Set<string>(keysOf(table));
    for (const d of docs) for (const k of Object.keys(d)) seen.add(k);
    return [...seen];
  },
  cell: (v) => (v === null ? null : v === undefined ? "" : typeof v === "string" ? v : JSON.stringify(v)),
  valueText: (v) => (typeof v === "string" ? v : JSON.stringify(v)),
  typed: typedJson,
  docText: (d) => JSON.stringify(d, null, 2),
  newDocText: "{\n  \n}",
  filterFor: (field, v) => `${quote(field)} = ${cqlLiteral(v)}`,
  identity(d, table) {
    const keys = keysOf(table);
    if (!keys.length || keys.some((k) => d[k] === undefined || d[k] === null)) return null;
    return { op: "", table, key: Object.fromEntries(keys.map((k) => [k, d[k]])) };
  },
  fixed: (field, table) => keysOf(table).includes(field),
  editCommand: "cassandra_edit",
  edits: {
    set: ({ table, key }, field, value) => ({ op: "set", table, key, field, value }),
    remove: ({ table, key }, field) => ({ op: "remove", table, key, field }),
    replace: ({ table, key }, row) => ({ op: "replace", table, key, row }),
    insert: (table, row) => ({ op: "insert", table, row }),
    delete: (ids) => ({ op: "delete", table: ids[0]?.table, keys: ids.map((i) => i.key) }),
  },
  run: (call, text, limit, a) => call<Outcome[]>("cassandra_run", { text, limit, confirmed: a.confirmed, unlock: a.unlock }),
  async opened(call, table) {
    described.set(`${session()}\u0000${table}`, await call<Described>("cassandra_describe", { table }));
  },
  collectionMenu: (table, script) => [
    "-",
    ["Truncate…", () => void script(`TRUNCATE ${quote(table)};`, `Delete every row of ${table}? This can't be undone.`, true)],
    ["Drop…", () => void script(`DROP TABLE ${quote(table)};`, `Drop the table ${table} and all its rows? This can't be undone.`, true)],
  ],
};

let view: ReturnType<typeof docView> | null = null;
export function initCassandra(host: Host) {
  session = () => host.conn()?.name ?? "";
  view = docView(cassandra, host);
}
export const syncCassandra = (fresh = false) => view?.sync(fresh);
export const forgetCassandra = (name: string) => view?.forget(name);
