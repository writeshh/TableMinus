// The structure view (columns, indexes, foreign keys, checks, triggers) with edits staged per tab and run only
// after their SQL is previewed, plus the sidebar's table operations (new, rename, clone, truncate, drop…).
// Everything here asks before it acts: the preview dialog is the confirmation, also for safe mode.

/// <reference types="vite/client" />
import { el } from "./dom";
import "./structure.css"; // vite injects it

export type ColumnInfo = {
  name: string; data_type: string; nullable: boolean; default: string | null; comment: string | null;
  auto_increment: boolean; primary: boolean;
};
type IndexInfo = { name: string; columns: string[]; unique: boolean; primary: boolean; method: string | null };
type ForeignKeyInfo = {
  name: string; columns: string[]; ref_table: string; ref_columns: string[]; on_update: string; on_delete: string;
};
type NamedDef = { name: string; definition: string };
type Structure = {
  columns: ColumnInfo[]; indexes: IndexInfo[]; foreign_keys: ForeignKeyInfo[]; checks: NamedDef[]; triggers: NamedDef[];
  comment: string | null;
};

type Staged = { added?: boolean; dropped?: boolean };
type ColumnDraft = ColumnInfo & Staged & { orig?: ColumnInfo };
type Draft = {
  table: string; create: boolean; base?: Structure; error?: string;
  name: string; comment: string;
  columns: ColumnDraft[]; indexes: (IndexInfo & Staged)[]; fks: (ForeignKeyInfo & Staged)[];
};

/** What the workspace (main.ts) lends this module. */
export type Host = {
  call<T>(cmd: string, args: Record<string, unknown>): Promise<T>;
  conn(): { name: string; kind: string } | null;
  tables(): string[];
  /** Reloads the sidebar's table list after DDL ran. */
  refreshTables(): Promise<void>;
  /** Opens a structure tab; `create` makes it a new, not yet existing table. */
  openStructure(table: string, create: boolean): void;
  /** Closes tabs of a table that was renamed or dropped. */
  forgetTable(table: string): void;
  /** A create or rename went through: the tab now shows `table`. */
  retarget(tabId: number, table: string): void;
  openSql(sql: string): void;
  note(text: string, kind: "" | "error" | "done"): void;
  /** The staged-change count changed: update the footer without redrawing the view (which would steal focus). */
  pendingChanged(): void;
};

let host: Host;
export const initStructure = (h: Host) => void (host = h);

const drafts = new Map<number, Draft>();
const RULES = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];
const TYPES: Record<string, string[]> = {
  postgres: ["integer", "bigint", "smallint", "serial", "bigserial", "text", "character varying(255)", "boolean", "numeric(10,2)",
    "real", "double precision", "date", "timestamp without time zone", "timestamp with time zone", "time", "uuid", "json", "jsonb", "bytea"],
  mysql: ["int", "bigint", "tinyint(1)", "smallint", "varchar(255)", "text", "longtext", "decimal(10,2)", "float", "double", "date",
    "datetime", "timestamp", "time", "json", "blob"],
  sqlite: ["INTEGER", "TEXT", "REAL", "NUMERIC", "BLOB"],
};
const kind = () => (host.conn()?.kind === "mariadb" ? "mysql" : host.conn()?.kind ?? "postgres");
const blank = (s: string | null | undefined) => (s ?? "").trim() || null;
const shortName = (table: string) => (kind() === "postgres" ? table.split(".").slice(1).join(".") || table : table);

// ---------- Staged changes ----------

const columnDef = (c: ColumnInfo) => ({
  name: c.name.trim(), data_type: c.data_type.trim(), nullable: c.nullable, default: blank(c.default), comment: blank(c.comment),
  auto_increment: c.auto_increment, primary: c.primary,
});
const sameColumn = (a: ColumnInfo, b: ColumnInfo) => JSON.stringify(columnDef(a)) === JSON.stringify(columnDef(b));

/** The typed change list the backend turns into DDL (see `ddl::Change`). */
function changes(d: Draft): Record<string, unknown>[] {
  const out: Record<string, unknown>[] = [];
  for (const c of d.columns) {
    if (c.orig && c.dropped) out.push({ op: "drop_column", name: c.orig.name });
    else if (!c.orig && !c.dropped) out.push({ op: "add_column", column: columnDef(c) });
    else if (c.orig && !sameColumn(c, c.orig)) out.push({ op: "modify_column", from: c.orig.name, column: columnDef(c) });
  }
  for (const i of d.indexes) {
    if (i.added && !i.dropped) out.push({ op: "add_index", name: i.name.trim(), columns: i.columns, unique: i.unique });
    else if (i.dropped && !i.added) out.push({ op: "drop_index", name: i.name });
  }
  for (const f of d.fks) {
    const { name, columns, ref_table, ref_columns, on_update, on_delete } = f;
    if (f.added && !f.dropped) out.push({ op: "add_foreign_key", name: name.trim(), columns, ref_table, ref_columns, on_update, on_delete });
    else if (f.dropped && !f.added) out.push({ op: "drop_foreign_key", name });
  }
  if (!d.create && d.name.trim() !== shortName(d.table)) out.push({ op: "rename", to: d.name.trim() });
  if (blank(d.comment) !== blank(d.base?.comment)) out.push({ op: "comment", comment: blank(d.comment) });
  return out;
}

export const structurePending = (tabId: number) => {
  const d = drafts.get(tabId);
  return d && (d.base || d.create) ? changes(d).length : 0;
};
export const dropDraft = (tabId: number) => void drafts.delete(tabId);

function fromBase(table: string, s: Structure): Draft {
  return {
    table, create: false, base: s, name: shortName(table), comment: s.comment ?? "",
    columns: s.columns.map((c) => ({ ...c, orig: c })),
    indexes: s.indexes.map((i) => ({ ...i })),
    fks: s.foreign_keys.map((f) => ({ ...f })),
  };
}

const newColumn = (): ColumnDraft => ({
  name: "", data_type: TYPES[kind()][0], nullable: true, default: null, comment: null, auto_increment: false, primary: false, added: true,
});

// ---------- The view ----------

/** Draws tab `tabId`'s structure into `out`, loading it the first time. */
export async function showStructure(out: HTMLElement, tabId: number, table: string, create: boolean) {
  let d = drafts.get(tabId);
  if (!d) {
    if (create) {
      d = { table, create, name: shortName(table), comment: "", columns: [{ ...newColumn(), name: "id", primary: true, nullable: false }], indexes: [], fks: [] };
      if (kind() !== "sqlite") d.columns[0].auto_increment = true;
      d.columns[0].data_type = { postgres: "bigint", mysql: "bigint", sqlite: "INTEGER" }[kind()]!;
      drafts.set(tabId, d);
      host.pendingChanged(); // the footer was drawn before this draft existed
    } else {
      out.replaceChildren(el("p", { className: "hint empty", textContent: "Loading structure…" }));
      try {
        d = fromBase(table, await host.call<Structure>("table_structure", { name: host.conn()!.name, table }));
        drafts.set(tabId, d);
      } catch (e) {
        out.replaceChildren(el("p", { className: "hint empty error-text", textContent: String(e) }));
        return;
      }
      host.pendingChanged();
    }
  }
  draw(out, tabId, d);
}

function draw(out: HTMLElement, tabId: number, d: Draft) {
  const redraw = () => (draw(out, tabId, d), host.pendingChanged());
  const touched = () => host.pendingChanged();
  const lite = kind() === "sqlite";
  const view = el("div", { className: "structure" });
  if (d.error) view.append(el("p", { className: "error-text structure-error", textContent: d.error }));

  // Table options
  const name = input(d.name, (v) => ((d.name = v), touched()), "Table name");
  name.classList.toggle("changed", !d.create && d.name.trim() !== shortName(d.table));
  name.addEventListener("input", () => name.classList.toggle("changed", !d.create && d.name.trim() !== shortName(d.table)));
  const opts = el("div", { className: "table-options" }, el("label", {}, "Name ", name));
  name.disabled = d.create; // the new name was chosen when the tab opened
  if (!lite) opts.append(el("label", {}, "Comment ", input(d.comment, (v) => ((d.comment = v), touched()), "Table comment")));
  view.append(opts);

  // Columns
  const types = el("datalist", { id: `types-${tabId}` }, ...TYPES[kind()].map((t) => el("option", { value: t })));
  const colRows = d.columns.map((c) => {
    const tr = el("tr");
    const mark = () => tr.classList.toggle("changed", !!c.orig && !c.dropped && !sameColumn(c, c.orig));
    const set = <K extends keyof ColumnInfo>(k: K) => (v: ColumnInfo[K]) => ((c[k] = v), mark(), touched());
    const type = input(c.data_type, set("data_type"), "Type");
    type.setAttribute("list", types.id);
    const comment = input(c.comment ?? "", set("comment"), "Comment");
    comment.disabled = lite;
    const primary = check(c.primary, set("primary"), "Primary key");
    primary.disabled = !d.create;
    tr.append(
      td(input(c.name, set("name"), "Column name")), td(type), td(check(c.nullable, set("nullable"), "Nullable")),
      td(input(c.default ?? "", set("default"), "Default (SQL)")), td(check(c.auto_increment, set("auto_increment"), "Auto-increment")),
      td(primary), td(comment), td(dropButton(c, redraw)),
    );
    tr.classList.toggle("added", !!c.added);
    tr.classList.toggle("dropped", !!c.dropped);
    for (const i of tr.querySelectorAll("input")) i.disabled ||= !!c.dropped;
    mark();
    return tr;
  });
  view.append(section(`Columns`, ["Name", "Type", "Null", "Default", "Auto inc.", "Key", "Comment", ""], colRows,
    addButton("Column", () => (d.columns.push(newColumn()), redraw()))), types);

  // Indexes
  const idxRows = d.indexes.map((i) => {
    const flags = [i.primary ? "PRIMARY" : i.unique ? "UNIQUE" : "", i.method ?? ""].filter(Boolean).join(" · ");
    const tr = i.added
      ? el("tr", {}, td(input(i.name, (v) => ((i.name = v), touched()), "Index name")),
          td(input(i.columns.join(", "), (v) => ((i.columns = list(v)), touched()), "Columns, comma separated")),
          td(el("label", { className: "inline" }, check(i.unique, (v) => ((i.unique = v), touched()), "Unique"), " unique")),
          td(dropButton(i, redraw)))
      : el("tr", {}, td(i.name), td(i.columns.join(", ")), td(flags), td(dropButton(i, redraw)));
    tr.classList.toggle("added", !!i.added);
    tr.classList.toggle("dropped", !!i.dropped);
    return tr;
  });
  view.append(section("Indexes", ["Name", "Columns", "Kind", ""], idxRows,
    addButton("Index", () => (d.indexes.push({ name: `${d.name.trim()}_idx${d.indexes.length + 1}`, columns: [], unique: false, primary: false, method: null, added: true }), redraw()))));

  // Foreign keys
  const fkRows = d.fks.map((f) => {
    const rule = (v: string, on: (v: string) => void) => {
      const s = el("select", {}, ...RULES.map((r) => el("option", { value: r, textContent: r, selected: r === v })));
      s.onchange = () => (on(s.value), touched());
      return s;
    };
    const tr = f.added
      ? el("tr", {}, td(input(f.name, (v) => ((f.name = v), touched()), "Constraint name")),
          td(input(f.columns.join(", "), (v) => ((f.columns = list(v)), touched()), "Columns")),
          td(tableSelect(f.ref_table, (v) => ((f.ref_table = v), touched()))),
          td(input(f.ref_columns.join(", "), (v) => ((f.ref_columns = list(v)), touched()), "Referenced columns")),
          td(rule(f.on_update, (v) => (f.on_update = v))), td(rule(f.on_delete, (v) => (f.on_delete = v))), td(dropButton(f, redraw)))
      : el("tr", {}, td(f.name), td(f.columns.join(", ")), td(f.ref_table), td(f.ref_columns.join(", ")), td(f.on_update), td(f.on_delete), td(dropButton(f, redraw)));
    tr.classList.toggle("added", !!f.added);
    tr.classList.toggle("dropped", !!f.dropped);
    return tr;
  });
  const addFk = lite && !d.create
    ? el("span", { className: "hint", textContent: "SQLite can only add foreign keys when the table is created." })
    : addButton("Foreign key", () => (d.fks.push({ name: `${d.name.trim()}_fk${d.fks.length + 1}`, columns: [], ref_table: host.tables()[0] ?? "", ref_columns: ["id"], on_update: "NO ACTION", on_delete: "NO ACTION", added: true }), redraw()));
  view.append(section("Foreign keys", ["Name", "Columns", "References", "Columns", "On update", "On delete", ""], fkRows, addFk));

  // Read-only for now
  const defs = (rows: NamedDef[]) => rows.map((r) => el("tr", {}, td(r.name), td(el("code", { textContent: r.definition }))));
  if (d.base?.checks.length) view.append(section("Checks", ["Name", "Definition"], defs(d.base.checks)));
  if (d.base?.triggers.length) view.append(section("Triggers", ["Name", "Definition"], defs(d.base.triggers)));

  out.replaceChildren(view);
}

const td = (...kids: (Node | string)[]) => el("td", {}, ...kids);
const list = (v: string) => v.split(",").map((s) => s.trim()).filter(Boolean);

function input(value: string, on: (v: string) => void, label: string) {
  const i = el("input", { value, spellcheck: false, placeholder: label });
  i.setAttribute("aria-label", label);
  i.oninput = () => on(i.value);
  return i;
}

function check(value: boolean, on: (v: boolean) => void, label: string) {
  const i = el("input", { type: "checkbox", checked: value, title: label });
  i.setAttribute("aria-label", label);
  i.onchange = () => on(i.checked);
  return i;
}

function tableSelect(value: string, on: (v: string) => void) {
  const s = el("select", {}, ...host.tables().map((t) => el("option", { value: t, textContent: t, selected: t === value })));
  s.onchange = () => on(s.value);
  return s;
}

/** Drops an existing item (undoable until saved) or removes one added since. */
function dropButton(item: Staged, redraw: () => void) {
  const b = el("button", { className: "ghost", textContent: item.dropped ? "↺" : "×", title: item.dropped ? "Keep" : "Drop" });
  b.onclick = () => ((item.dropped = !item.dropped), redraw());
  return b;
}

function addButton(what: string, on: () => void) {
  const b = el("button", { className: "ghost add", textContent: `+ ${what}` });
  b.onclick = on;
  return b;
}

function section(title: string, heads: string[], rows: HTMLTableRowElement[], ...extra: Node[]) {
  const table = el("table", {}, el("thead", {}, el("tr", {}, ...heads.map((h) => el("th", { textContent: h })))), el("tbody", {}, ...rows));
  if (!rows.length) table.tBodies[0].append(el("tr", {}, el("td", { className: "hint", colSpan: heads.length, textContent: "None" })));
  return el("section", {}, el("h4", { textContent: title }), table, ...extra);
}

// ---------- Saving ----------

/** Previews the staged changes as SQL and runs them when confirmed. */
export async function saveStructure(tabId: number, out: HTMLElement) {
  const d = drafts.get(tabId);
  const conn = host.conn();
  if (!d || !conn) return;
  const list = changes(d);
  if (!list.length) return host.note("No changes to save", "");
  d.error = undefined;
  const table = d.table;
  try {
    const statements = await host.call<string[]>("structure_sql", { name: conn.name, table, create: d.create, changes: list });
    if (!(await preview(d.create ? `Create ${table}` : `Change ${table}`, statements, d.create ? "Create" : "Apply", false))) return;
    await host.call("run_ddl", { name: conn.name, statements, confirmed: true });
  } catch (e) {
    d.error = String(e); // staged edits stay, so nothing typed is lost
    draw(out, tabId, d);
    return;
  }
  const renamed = !d.create && d.name.trim() !== shortName(d.table);
  const now = renamed ? (kind() === "postgres" && d.table.includes(".") ? `${d.table.split(".")[0]}.${d.name.trim()}` : d.name.trim()) : d.table;
  drafts.delete(tabId);
  await host.refreshTables();
  host.retarget(tabId, now);
  host.note(`${d.create ? "Created" : "Changed"} ${now}`, "done");
}

// ---------- Dialogs ----------

/** A modal built on the fly, so this module needs nothing in index.html. Resolves with the button pressed. */
function modal(title: string, body: Node[], buttons: [string, string][], wide = false): Promise<string | null> {
  const d = el("dialog", { className: wide ? "structure-dialog wide" : "structure-dialog" });
  const foot = el("footer", {}, el("span", { className: "spacer" }));
  d.append(el("h3", { textContent: title }), ...body, foot);
  return new Promise((resolve) => {
    let picked: string | null = null;
    for (const [label, cls] of buttons) {
      const b = el("button", { type: "button", textContent: label, className: cls });
      b.onclick = () => ((picked = label), d.close());
      foot.append(b);
    }
    d.onclose = () => (d.remove(), resolve(picked));
    document.body.append(d);
    d.showModal();
    (foot.querySelector("button") as HTMLElement).focus(); // Cancel (or Close) first: Enter never runs by accident
    (d.querySelector("input:not([type=checkbox])") as HTMLInputElement | null)?.select();
  });
}

const sqlText = (statements: string[]) => statements.map((s) => `${s};`).join("\n\n");

/** Shows the statements about to run; true when the user runs them. */
async function preview(title: string, statements: string[], okLabel: string, danger: boolean) {
  const conn = host.conn();
  const note = kind() === "mysql" && statements.length > 1
    ? "MySQL commits each statement as it runs; if one fails, the ones before it stay applied."
    : `Runs in one transaction on ${conn?.name}.`;
  const pre = el("pre", { className: "sql-preview", textContent: sqlText(statements) });
  const picked = await modal(title, [pre, el("p", { className: "hint", textContent: note })],
    [["Cancel", ""], ["Copy", ""], [okLabel, danger ? "danger" : "primary"]], true);
  if (picked === "Copy") {
    await navigator.clipboard.writeText(sqlText(statements)).catch(() => {});
    return preview(title, statements, okLabel, danger);
  }
  return picked === okLabel;
}

type Field = { key: string; label: string; value: string | boolean };

/** Asks for an operation's options; null when cancelled. */
async function ask(title: string, fields: Field[], okLabel: string) {
  const inputs = fields.map((f) => {
    const i = typeof f.value === "boolean" ? el("input", { type: "checkbox", checked: f.value }) : el("input", { value: f.value, spellcheck: false });
    return [f, i] as const;
  });
  const body = inputs.map(([f, i]) => (i.type === "checkbox" ? el("label", { className: "check" }, i, ` ${f.label}`) : el("label", { className: "field" }, f.label, i)));
  const form = el("form", { method: "dialog" }, ...body);
  form.onsubmit = (e) => (e.preventDefault(), (form.closest("dialog")!.querySelector("footer button:last-child") as HTMLElement).click());
  const picked = await modal(title, [form], [["Cancel", ""], [okLabel, "primary"]]);
  if (picked !== okLabel) return null;
  return Object.fromEntries(inputs.map(([f, i]) => [f.key, i.type === "checkbox" ? i.checked : i.value.trim()]));
}

// ---------- Table operations (sidebar menu) ----------

type MenuItem = [string, () => void] | "-";

/** Generates an operation's SQL, shows it, and runs it when confirmed. */
async function runOp(title: string, op: Record<string, unknown>, okLabel: string, danger: boolean, after?: () => void) {
  const conn = host.conn();
  if (!conn) return;
  try {
    const statements = await host.call<string[]>("table_op_sql", { name: conn.name, op });
    if (!(await preview(title, statements, okLabel, danger))) return;
    await host.call("run_ddl", { name: conn.name, statements, confirmed: true });
    host.note(`${title}: done`, "done");
    after?.();
    await host.refreshTables();
  } catch (e) {
    host.note(String(e), "error");
  }
}

async function viewDdl(table: string) {
  try {
    const ddl = await host.call<string>("table_ddl", { name: host.conn()!.name, table });
    const picked = await modal(`DDL of ${table}`, [el("pre", { className: "sql-preview", textContent: ddl })],
      [["Close", ""], ["Copy", ""], ["Open in SQL tab", "primary"]], true);
    if (picked === "Copy") await navigator.clipboard.writeText(ddl).catch(() => {});
    if (picked === "Open in SQL tab") host.openSql(ddl);
  } catch (e) {
    host.note(String(e), "error");
  }
}

async function newTable(schema?: string) {
  const v = await ask("New table", [{ key: "name", label: "Name", value: "" }], "Create…");
  if (!v?.name) return;
  const name = String(v.name);
  host.openStructure(kind() === "postgres" && !name.includes(".") ? `${schema ?? "public"}.${name}` : name, true);
}

function schemaItems(schema?: string): MenuItem[] {
  if (kind() !== "postgres") return [];
  const items: MenuItem[] = ["-", ["New schema…", async () => {
    const v = await ask("New schema", [{ key: "name", label: "Name", value: "" }], "Create…");
    if (v?.name) runOp(`Create schema ${v.name}`, { op: "create_schema", name: v.name }, "Create", false);
  }]];
  if (schema)
    items.push(
      [`Rename schema ${schema}…`, async () => {
        const v = await ask(`Rename schema ${schema}`, [{ key: "to", label: "New name", value: schema }], "Rename…");
        if (v?.to) runOp(`Rename schema ${schema}`, { op: "rename_schema", name: schema, to: v.to }, "Rename", false);
      }],
      [`Drop schema ${schema}…`, async () => {
        const v = await ask(`Drop schema ${schema}`, [{ key: "cascade", label: "Cascade: also drop everything in it", value: false }], "Drop…");
        if (v) runOp(`Drop schema ${schema}`, { op: "drop_schema", name: schema, cascade: v.cascade }, "Drop", true);
      }],
    );
  return items;
}

/** The right-click menu for a table (or view) in the sidebar. */
export function tableMenu(table: string): MenuItem[] {
  const k = kind();
  const schema = k === "postgres" ? table.split(".")[0] : undefined;
  const fkField: Field[] = k === "mysql" ? [{ key: "ignore_fk", label: "Ignore foreign key checks", value: false }] : [];
  return [
    ["Open structure", () => host.openStructure(table, false)],
    ["View DDL", () => viewDdl(table)],
    "-",
    ["New table…", () => newTable(schema)],
    ["Rename…", async () => {
      const v = await ask(`Rename ${table}`, [{ key: "to", label: "New name", value: shortName(table) }], "Rename…");
      if (v?.to) runOp(`Rename ${table}`, { op: "rename", table, to: v.to }, "Rename", false, () => host.forgetTable(table));
    }],
    ["Clone…", async () => {
      const v = await ask(`Clone ${table}`, [{ key: "to", label: "Name of the copy", value: `${shortName(table)}_copy` }, { key: "data", label: "Copy the rows too", value: true }], "Clone…");
      if (v?.to) runOp(`Clone ${table}`, { op: "clone", table, to: v.to, data: v.data }, "Clone", false);
    }],
    ["Reset auto-increment", () => runOp(`Reset auto-increment of ${table}`, { op: "reset_auto_increment", table }, "Reset", false)],
    "-",
    ["Truncate…", async () => {
      const fields: Field[] = k === "postgres"
        ? [{ key: "restart_identity", label: "Restart identity (auto-increment)", value: false }, { key: "cascade", label: "Cascade to tables that reference it", value: false }]
        : k === "sqlite" ? [{ key: "restart_identity", label: "Reset auto-increment", value: false }] : fkField;
      const v = fields.length ? await ask(`Truncate ${table}`, fields, "Truncate…") : {};
      if (v) runOp(`Truncate ${table}`, { op: "truncate", table, cascade: !!v.cascade, restart_identity: !!v.restart_identity, ignore_fk: !!v.ignore_fk }, "Delete all rows", true);
    }],
    ["Drop…", async () => {
      const fields: Field[] = k === "postgres" ? [{ key: "cascade", label: "Cascade: also drop what depends on it", value: false }] : fkField;
      const v = fields.length ? await ask(`Drop ${table}`, fields, "Drop…") : {};
      if (v) runOp(`Drop ${table}`, { op: "drop", table, cascade: !!v.cascade, ignore_fk: !!v.ignore_fk }, "Drop", true, () => host.forgetTable(table));
    }],
    ...schemaItems(schema),
  ];
}

/** The right-click menu for the sidebar's empty space. */
export const sidebarMenu = (): MenuItem[] => [["New table…", () => newTable()], ...schemaItems()];
