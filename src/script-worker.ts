// Runs a custom script (scripts.ts) in a Web Worker: no page, no access to the app's backend, only the helpers below.
// `query` and `execute` ask the page to run SQL on the script's connection, where safe mode asks as it does for the
// editor.

type Result = { columns: string[]; rows: (string | null)[][]; affected: number; truncated: boolean };
type FromPage =
  | { type: "run"; code: string; connection: { name: string; kind: string; database: string | null } }
  | { type: "reply"; id: number; value?: unknown; error?: string };

const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
let nextId = 1;

function ask(sql: unknown): Promise<Result[]> {
  const id = nextId++;
  postMessage({ type: "query", id, sql: String(sql) });
  return new Promise((resolve, reject) => pending.set(id, { resolve: resolve as (v: unknown) => void, reject }));
}

function show(v: unknown) {
  if (typeof v === "string") return v;
  try {
    return JSON.stringify(v, null, 2) ?? String(v);
  } catch {
    return String(v);
  }
}

const helpers = {
  /** The rows of the statement's (last) result set, as objects keyed by column. */
  async query(sql: string) {
    const results = await ask(sql);
    const last = [...results].reverse().find((r) => r.columns.length);
    if (!last) return [];
    return last.rows.map((row) => Object.fromEntries(last.columns.map((c, i) => [c, row[i]])));
  },
  /** Every result of the SQL, as the editor gets them: columns, rows as arrays, rows affected. */
  execute: (sql: string) => ask(sql),
  print: (...values: unknown[]) => postMessage({ type: "print", text: values.map(show).join(" ") }),
  /** Shows an array of objects (or of arrays) as a table. */
  table(rows: unknown) {
    postMessage({ type: "table", rows: JSON.parse(JSON.stringify(rows ?? [])) });
  },
};
Object.assign(globalThis, helpers);
console.log = helpers.print;
console.info = helpers.print;
console.warn = console.error = (...values: unknown[]) => postMessage({ type: "print", text: values.map(show).join(" "), error: true });

onmessage = async (e: MessageEvent<FromPage>) => {
  const m = e.data;
  if (m.type === "reply") {
    const p = pending.get(m.id);
    pending.delete(m.id);
    if (m.error !== undefined) p?.reject(new Error(m.error));
    else p?.resolve(m.value);
    return;
  }
  Object.assign(globalThis, { connection: m.connection });
  try {
    const AsyncFunction = Object.getPrototypeOf(async () => {}).constructor as new (body: string) => () => Promise<unknown>;
    const value = await new AsyncFunction(m.code)();
    if (value !== undefined) helpers.print(value);
    postMessage({ type: "done" });
  } catch (err) {
    postMessage({ type: "error", text: err instanceof Error ? (err.stack ?? err.message) : show(err) });
  }
};
