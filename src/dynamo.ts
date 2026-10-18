// DynamoDB in the document view (docview.ts; dynamo/mod.rs): tables, their items a page at a time with a PartiQL
// condition as the filter, values as PartiQL literals, and a console for PartiQL statements. An item is found again
// by its key, which DescribeTable gives when the table opens.

import { docView, type Engine, type Host } from "./docview";
import { cell, columnsOf, filterFor, itemText, literal, typedLiteral } from "./dynamo-text";

type Page = { items: Record<string, unknown>[]; next: string | null; done: boolean };
type Described = { keys: [string, string][]; items: number | null; indexes: string[] };

/** Each table's key attributes, and DynamoDB's own item count, by session and table. */
const described = new Map<string, Described>();
let session = () => "";
const tableInfo = (table: string) => described.get(`${session()}\u0000${table}`);

const dynamo: Engine = {
  kind: "dynamodb",
  css: "dynamo",
  words: { collection: "table", doc: "item" },
  filterHint: "Filter: a PartiQL condition, e.g. \"age\" > 30 AND begins_with(\"name\", 'A')",
  sortHint: null,
  console: {
    language: async () => {
      const { sql, StandardSQL } = await import("@codemirror/lang-sql");
      return sql({ dialect: StandardSQL });
    },
    start: "SELECT * FROM ",
    hint: "PartiQL, one statement at a time: SELECT * FROM \"orders\" WHERE \"pk\" = 'a', INSERT INTO \"orders\" VALUE {'pk': 'b'}, UPDATE, DELETE. ⌘↩ runs.",
    confirm: (statement) => `Run this statement? It may change data. ${statement.length > 300 ? statement.slice(0, 300) + "…" : statement}`,
  },
  async page(call, table, q) {
    const page = await call<Page>("dynamo_page", { table, filter: q.filter, limit: q.size, next: q.token });
    return { docs: page.items, next: page.next };
  },
  async count(_call, table, filter) {
    const n = tableInfo(table)?.items;
    // DynamoDB counts a table about every six hours, and can't count what a filter matches without reading it all.
    return filter.trim() || n == null ? null : `~${n.toLocaleString()}`;
  },
  columns: (docs, table) => columnsOf(docs, (tableInfo(table)?.keys ?? []).map(([k]) => k)),
  cell,
  valueText: (v) => {
    const a = v as Record<string, unknown>;
    return a && typeof a === "object" && "S" in a ? String(a.S) : literal(v);
  },
  typed: typedLiteral,
  docText: (d) => itemText(d),
  newDocText: "{\n  \n}",
  filterFor,
  identity(d, table) {
    const keys = tableInfo(table)?.keys ?? [];
    if (!keys.length || keys.some(([k]) => !(k in d))) return null;
    return { op: "", table, key: Object.fromEntries(keys.map(([k]) => [k, d[k]])) };
  },
  fixed: (field, table) => !!tableInfo(table)?.keys.some(([k]) => k === field),
  editCommand: "dynamo_edit",
  edits: {
    set: ({ table, key }, field, value) => ({ op: "set", table, key, field, value }),
    remove: ({ table, key }, field) => ({ op: "remove", table, key, field }),
    replace: ({ table, key }, item) => ({ op: "replace", table, key, item }),
    insert: (table, item) => ({ op: "insert", table, key_names: (tableInfo(table)?.keys ?? []).map(([k]) => k), item }),
    delete: (ids) => ({ op: "delete", table: ids[0]?.table, keys: ids.map((i) => i.key) }),
  },
  async run(call, statement, _limit, a) {
    const page = await call<Page>("dynamo_run", { statementText: statement, next: null, confirmed: a.confirmed, unlock: a.unlock });
    if (page.done) return [{ kind: "done", title: "statement", text: "Done." }];
    return [{ kind: "docs", title: "statement", docs: page.items, more: !!page.next }];
  },
  async opened(call, table) {
    described.set(`${session()}\u0000${table}`, await call<Described>("dynamo_describe", { table }));
  },
};

let view: ReturnType<typeof docView> | null = null;
export function initDynamo(host: Host) {
  session = () => host.conn()?.name ?? "";
  view = docView(dynamo, host);
}
export const syncDynamo = (fresh = false) => view?.sync(fresh);
export const forgetDynamo = (name: string) => view?.forget(name);
