// Elasticsearch in the document view (docview.ts; elastic/mod.rs): indices, their documents a page at a time with a
// Lucene query as the filter and `field:asc` sorts, values as JSON, and a console for Elasticsearch SQL or requests
// written as Kibana's Dev Tools does. A document is found again by its `_id`.

import { docView, type Engine, type Host, type Outcome } from "./docview";

type Page = { docs: Record<string, unknown>[]; total: string | null; more: boolean };

/** The last page's count, by session and index, for the count the bar shows. */
const totals = new Map<string, string | null>();
let session = () => "";
const at = (index: string) => `${session()}\u0000${index}`;

/** What a typed cell becomes, as JSON: text stays text where the field held text, and plain words are text. */
export function typedJson(original: unknown, typed: string): string {
  if (typeof original === "string") return JSON.stringify(typed);
  try {
    JSON.parse(typed);
    return typed.trim();
  } catch {
    return JSON.stringify(typed);
  }
}

const quote = (v: unknown) => (typeof v === "string" ? `"${v.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"` : JSON.stringify(v));

const elastic: Engine = {
  kind: "elasticsearch",
  css: "elastic",
  words: { collection: "index", doc: "document" },
  filterHint: "Filter: a Lucene query, e.g. status:active AND age:>30",
  sortHint: "Sort, e.g. age:desc, name",
  console: {
    language: async () => {
      const { sql, StandardSQL } = await import("@codemirror/lang-sql");
      return sql({ dialect: StandardSQL });
    },
    start: "SELECT * FROM ",
    hint: "Elasticsearch SQL (SELECT * FROM orders WHERE total > 5), or a request as in Kibana: GET /orders/_search on the first line, its JSON body after. ⌘↩ runs.",
    confirm: (text) => `Run this? It may change data. ${text.length > 300 ? text.slice(0, 300) + "…" : text}`,
  },
  async page(call, index, q) {
    const from = Number(q.token ?? 0);
    const page = await call<Page>("elastic_page", { index, filter: q.filter, sort: q.sort, from, size: q.size });
    totals.set(at(index), page.total);
    return { docs: page.docs, next: page.more ? String(from + q.size) : null };
  },
  count: async (_call, index) => totals.get(at(index)) ?? null,
  columns(docs) {
    const seen = new Set<string>(["_id"]);
    for (const d of docs) for (const k of Object.keys(d)) seen.add(k);
    return [...seen];
  },
  cell: (v) => (v === null ? null : v === undefined ? "" : typeof v === "string" ? v : JSON.stringify(v)),
  valueText: (v) => (typeof v === "string" ? v : JSON.stringify(v)),
  typed: typedJson,
  docText: (d) => JSON.stringify(Object.fromEntries(Object.entries(d).filter(([k]) => k !== "_id")), null, 2),
  newDocText: "{\n  \n}",
  filterFor: (field, v) => `${field}:${quote(v)}`,
  identity: (d, index) => (typeof d._id === "string" ? { op: "", index, id: d._id } : null),
  fixed: (field) => field.startsWith("_"),
  editCommand: "elastic_edit",
  edits: {
    set: ({ index, id }, field, value) => ({ op: "set", index, id, field, value }),
    remove: ({ index, id }, field) => ({ op: "remove", index, id, field }),
    replace: ({ index, id }, document) => ({ op: "replace", index, id, document }),
    insert: (index, document) => ({ op: "insert", index, document }),
    delete: (ids) => ({ op: "delete", index: ids[0]?.index, ids: ids.map((i) => i.id) }),
  },
  run: (call, text, limit, a) => call<Outcome[]>("elastic_run", { text, limit, confirmed: a.confirmed, unlock: a.unlock }),
  collectionMenu: (index, script) => [
    "-",
    ["Delete index…", () => void script(`DELETE /${encodeURIComponent(index)}`, `Delete the index ${index} and all its documents? This can't be undone.`, true)],
  ],
  async newCollection(ask, script) {
    const name = (await ask("New index", "Name (lower case)", "", "Create"))?.trim();
    if (!name || !(await script(`PUT /${encodeURIComponent(name)}`, `Create the index ${name}?`))) return null;
    return name;
  },
};

let view: ReturnType<typeof docView> | null = null;
export function initElastic(host: Host) {
  session = () => host.conn()?.name ?? "";
  view = docView(elastic, host);
}
export const syncElastic = (fresh = false) => view?.sync(fresh);
export const forgetElastic = (name: string) => view?.forget(name);
