import { invoke } from "@tauri-apps/api/core";

type Saved = { name: string; kind: string; url: string };
type QueryResult = { columns: string[]; rows: (string | null)[][]; affected: number };

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const sqlBox = $<HTMLTextAreaElement>("sql");
const placeholders: Record<string, string> = {
  postgres: "postgres://user@localhost:5432/db",
  mysql: "mysql://user@localhost:3306/db",
  sqlite: "/path/to/file.db",
};
let active: Saved | null = null;

// Everything below builds DOM with textContent, never innerHTML: cell values come from the database.
function item(text: string, onClick: () => void, onDelete?: () => void) {
  const li = document.createElement("li");
  li.textContent = text;
  li.onclick = onClick;
  if (onDelete) {
    const x = document.createElement("button");
    x.textContent = "×";
    x.title = "Delete";
    x.onclick = (e) => (e.stopPropagation(), onDelete());
    li.append(x);
  }
  return li;
}

function status(text: string, error = false) {
  const s = $("status");
  s.textContent = text;
  s.className = error ? "error" : "";
}

async function refresh() {
  const conns = await invoke<Saved[]>("list_connections");
  $("conns").replaceChildren(
    ...conns.map((c) =>
      item(`${c.name} · ${c.kind}`, () => connect(c), async () => {
        if (!confirm(`Delete connection "${c.name}"?`)) return;
        await invoke("delete_connection", { name: c.name });
        if (active?.name === c.name) (active = null), $("tables").replaceChildren();
        refresh();
      }),
    ),
  );
}

async function connect(c: Saved) {
  status(`Connecting to ${c.name}…`);
  try {
    const tables = await invoke<string[]>("connect", { name: c.name });
    active = c;
    $("tables").replaceChildren(...tables.map((t) => item(t, () => run(`select * from ${quote(t)} limit 200`))));
    status(`Connected to ${c.name}: ${tables.length} tables`);
  } catch (e) {
    status(String(e), true);
  }
}

function quote(table: string) {
  const q = active!.kind === "mysql" ? "`" : '"';
  const parts = active!.kind === "postgres" ? table.split(".") : [table]; // postgres names are schema.table
  return parts.map((p) => q + p.split(q).join(q + q) + q).join(".");
}

async function run(sql: string, confirmed = false) {
  if (!active) return status("Pick a connection first", true);
  if (!sql.trim()) return;
  status("Running…");
  const t0 = performance.now();
  try {
    const r = await invoke<QueryResult>("run_query", { name: active.name, sql, confirmed });
    render(r);
    const ms = Math.round(performance.now() - t0);
    status(r.columns.length ? `${r.rows.length} rows · ${ms} ms` : `${r.affected} rows affected · ${ms} ms`);
  } catch (e) {
    if (e === "CONFIRM") {
      if (confirm(`Safe mode: this may change data on "${active.name}". Run it?`)) return run(sql, true);
      return status("Cancelled");
    }
    status(String(e), true);
  }
}

function render(r: QueryResult) {
  if (!r.columns.length) return $("out").replaceChildren();
  const table = document.createElement("table");
  const head = table.createTHead().insertRow();
  for (const c of r.columns) head.append(Object.assign(document.createElement("th"), { textContent: c }));
  const body = table.createTBody();
  // ponytail: plain table; switch to a virtualized grid when result sets get large.
  for (const row of r.rows) {
    const tr = body.insertRow();
    for (const v of row) {
      const td = tr.insertCell();
      td.textContent = v ?? "NULL";
      if (v === null) td.className = "null";
    }
  }
  $("out").replaceChildren(table);
}

$("form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target as HTMLFormElement);
  const conn = { name: String(f.get("name")), kind: String(f.get("kind")), url: String(f.get("url")) };
  try {
    await invoke("save_connection", { conn, password: String(f.get("password")) || null });
    (e.target as HTMLFormElement).reset();
    refresh();
  } catch (err) {
    status(String(err), true);
  }
});

$("form").querySelector("select")!.addEventListener("change", (e) => {
  const url = $<HTMLFormElement>("form").elements.namedItem("url") as HTMLInputElement;
  url.placeholder = placeholders[(e.target as HTMLSelectElement).value];
});

const runEditor = () =>
  run(sqlBox.value.slice(sqlBox.selectionStart, sqlBox.selectionEnd) || sqlBox.value);
$("run").onclick = runEditor;
sqlBox.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) (e.preventDefault(), runEditor());
});

refresh();
