// MongoDB in the document view (docview.ts; nosql/mongo.rs): collections, their documents a page at a time with a
// filter and sort in mongosh syntax, values in mongosh syntax (`ObjectId("…")`), and a shell for mongosh-style
// scripts. A document is found again by its `_id`.

import { docView, type Engine, type Host, type Outcome } from "./docview";
import { cell, columnsOf, countScript, documentText, expression, pageScript, shellText, typedExpression } from "./mongo-text";

/** One script, run through mongo_run with safe mode's answer. */
const run = (call: Parameters<Engine["run"]>[0], script: string, maxDocs: number, a: { confirmed: boolean; unlock: unknown }) =>
  call<Outcome[]>("mongo_run", { script, maxDocs, confirmed: a.confirmed, unlock: a.unlock });

const mongo: Engine = {
  kind: "mongodb",
  css: "mongo",
  words: { collection: "collection", doc: "document" },
  filterHint: "Filter, e.g. { age: { $gt: 30 } }",
  sortHint: "Sort, e.g. { _id: -1 }",
  console: {
    language: async () => (await import("@codemirror/lang-javascript")).javascript(),
    start: "db.getCollectionNames()",
    hint: "Scripts use mongosh syntax: db.orders.find({ total: { $gt: 5 } }).sort({ _id: -1 }), show collections, use otherdb. ⌘↩ runs.",
    confirm: (script) => `Run this script? It may change data. ${script.length > 300 ? script.slice(0, 300) + "…" : script}`,
  },
  async page(call, collection, q) {
    const skip = Number(q.token ?? 0);
    const [found] = await run(call, pageScript(collection, q.filter, q.sort, skip, q.size), q.size, { confirmed: false, unlock: null });
    const docs = found?.kind === "docs" ? found.docs : [];
    const more = found?.kind === "docs" && found.more;
    return { docs, next: more ? String(skip + q.size) : null };
  },
  async count(call, collection, filter) {
    const [counted] = await run(call, countScript(collection, filter), 1, { confirmed: false, unlock: null });
    return counted?.kind === "value" && typeof counted.value === "number" ? counted.value.toLocaleString() : null;
  },
  columns: (docs) => columnsOf(docs),
  cell,
  valueText: (v) => (typeof v === "string" ? v : shellText(v)),
  typed: typedExpression,
  docText: documentText,
  newDocText: "{\n  \n}",
  filterFor: (field, v) => `{ ${JSON.stringify(field)}: ${shellText(v)} }`,
  identity: (d, collection) => ("_id" in d ? { op: "", collection, id: expression(d._id) } : null),
  fixed: (field) => field === "_id",
  editCommand: "mongo_edit",
  edits: {
    set: ({ collection, id }, field, value) => ({ op: "set", collection, id, field, value }),
    remove: ({ collection, id }, field) => ({ op: "unset", collection, id, field }),
    replace: ({ collection, id }, document) => ({ op: "replace", collection, id, document }),
    insert: (collection, document) => ({ op: "insert", collection, document }),
    delete: (ids) => ({ op: "delete", collection: ids[0]?.collection, ids: ids.map((i) => i.id) }),
  },
  run,
  collectionMenu: (c, script, ask) => [
    "-",
    ["Rename…", () => void (async () => {
      const to = (await ask(`Rename ${c}`, "New name", c, "Rename"))?.trim();
      if (to && to !== c) await script(`db.getCollection(${JSON.stringify(c)}).renameCollection(${JSON.stringify(to)})`, `Rename ${c} to ${to}?`);
    })()],
    ["Drop…", () => void script(`db.getCollection(${JSON.stringify(c)}).drop()`, `Drop the collection ${c} and all its documents? This can't be undone.`, true)],
  ],
  async newCollection(ask, script) {
    const name = (await ask("New collection", "Name", "", "Create"))?.trim();
    if (!name || !(await script(`db.createCollection(${JSON.stringify(name)})`, `Create the collection ${name}?`))) return null;
    return name;
  },
};

let view: ReturnType<typeof docView> | null = null;
export const initMongo = (host: Host) => void (view = docView(mongo, host));
export const syncMongo = (fresh = false) => view?.sync(fresh);
export const forgetMongo = (name: string) => view?.forget(name);
