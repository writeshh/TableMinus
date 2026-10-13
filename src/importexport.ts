// Export and import dialogs, and the panel that follows running jobs. The backend does the work on a connection
// of its own (export.rs, import.rs, jobs.rs); this module gathers the options and shows progress.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { el } from "./dom";
import { fileName, matchColumns } from "./transfer";

/** A browse request, as the table tab sends it. */
export type BrowseRequest = Record<string, unknown> & { table: string };
export type ExportSource =
  | { kind: "tables"; tables: string[] }
  | { kind: "rows"; request: BrowseRequest; pageRows: number }
  | { kind: "query"; sql: string };

/** What the dialogs need from the workspace. */
export type Ctx = {
  conn: string;
  database: string;
  tables: string[];
  columns: (table: string) => Promise<string[]>;
  confirm: (text: string, okLabel: string) => Promise<boolean>;
  /** After an import: refresh the table list and any open tab of `table`. */
  imported: (table: string) => void;
};

type Progress = { item: number; items: number; done: number; total: number | null; rows: number };
type Done = { id: number; rows: number; error: string | null };
type Preview = { fields: string[]; rows: (string | null)[][] };

const fmt = (n: number) => n.toLocaleString();
const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;
const option = (value: string, text: string, selected = false) => el("option", { value, textContent: text, selected });
const check = (text: string, checked: boolean) => {
  const input = el("input", { type: "checkbox", checked });
  return { input, label: el("label", { className: "check" }, input, text) };
};
const labelled = (text: string, control: HTMLElement) => el("label", {}, el("span", { textContent: text }), control);

/** A modal built for one use, removed when closed. */
function modal(title: string, className = "") {
  const d = el("dialog", { className: `transfer ${className}` }, el("h3", { textContent: title }));
  d.onclose = () => d.remove();
  document.body.append(d);
  return d;
}

// ---------- Jobs ----------

type Shown = { bar: HTMLProgressElement; text: HTMLElement; button: HTMLButtonElement; after?: (d: Done) => void };
const shown = new Map<number, Shown>();
const early = new Map<number, Done>(); // job-done can beat the start command's reply for tiny jobs
let listening: Promise<unknown> | null = null;

function listenOnce() {
  listening ??= Promise.all([
    listen<[number, Progress]>("job-progress", ({ payload: [id, p] }) => {
      const s = shown.get(id);
      if (!s) return;
      if (p.total) (s.bar.max = p.total), (s.bar.value = p.done);
      const of = p.items > 1 ? `file ${p.item + 1} of ${p.items} · ` : "";
      s.text.textContent = `${of}${fmt(p.rows)} rows${p.total ? ` · ${Math.floor((100 * p.done) / p.total)}%` : ""}`;
    }),
    listen<Done>("job-done", ({ payload }) => finish(payload)),
  ]);
  return listening;
}

function finish(d: Done) {
  const s = shown.get(d.id);
  if (!s) return void early.set(d.id, d);
  shown.delete(d.id);
  s.bar.max = 1;
  s.bar.value = d.error ? 0 : 1;
  s.text.textContent = d.error ?? `Done · ${fmt(d.rows)} rows`;
  s.text.className = d.error ? "error-text" : "";
  s.button.textContent = "Close";
  s.button.disabled = false;
  s.button.onclick = () => s.button.closest(".job")?.remove();
  s.after?.(d);
}

function jobsPanel() {
  return document.querySelector<HTMLElement>(".jobs") ?? document.body.appendChild(el("div", { className: "jobs" }));
}

/** Starts a job, asking first when safe mode wants confirmation, and shows it in the jobs panel. */
async function startJob(ctx: Ctx, label: string, cmd: string, args: Record<string, unknown>, after?: (d: Done) => void) {
  await listenOnce();
  const bar = el("progress", { max: 1, value: 0 });
  const text = el("span", { textContent: "Starting…" });
  const button = el("button", { type: "button", textContent: "Cancel" });
  jobsPanel().append(el("div", { className: "job" }, el("strong", { textContent: label }), bar, el("div", { className: "job-foot" }, text, button)));
  const show: Shown = { bar, text, button, after };
  bar.removeAttribute("value"); // indeterminate until a total arrives
  try {
    let id: number;
    try {
      id = await invoke<number>(cmd, { ...args, name: ctx.conn, confirmed: false });
    } catch (e) {
      if (e !== "CONFIRM" || !(await ctx.confirm(`${label} may change data on "${ctx.conn}". Go ahead?`, "Go ahead"))) throw e === "CONFIRM" ? "Cancelled" : e;
      id = await invoke<number>(cmd, { ...args, name: ctx.conn, confirmed: true });
    }
    button.onclick = () => ((button.disabled = true), invoke("cancel_job", { id }));
    shown.set(id, show);
    const done = early.get(id);
    if (done) early.delete(id), finish(done);
  } catch (e) {
    shown.set(-1, show);
    finish({ id: -1, rows: 0, error: String(e) });
  }
}

// ---------- Export ----------

type ExportPrefs = {
  format: "csv" | "json" | "sql"; delimiter: string; crlf: boolean; header: boolean; quoteAll: boolean; nullEmpty: boolean;
  structure: boolean; drop: boolean; content: boolean; gzip: boolean; pattern: string;
};
const EXPORT_DEFAULTS: ExportPrefs = {
  format: "csv", delimiter: ",", crlf: false, header: true, quoteAll: false, nullEmpty: true,
  structure: true, drop: true, content: true, gzip: false, pattern: "{table}",
};
const PREFS_KEY = "export-options";

function loadPrefs(): ExportPrefs {
  try {
    return { ...EXPORT_DEFAULTS, ...JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}") };
  } catch {
    return { ...EXPORT_DEFAULTS };
  }
}

/** The export dialog: what, format and its options, file names; then a folder to write into. */
export function exportDialog(ctx: Ctx, source: ExportSource) {
  const p = loadPrefs();
  const what =
    source.kind === "query" ? "the query's result"
    : source.kind === "rows" ? source.request.table
    : source.tables.length === 1 ? source.tables[0] : `${source.tables.length} tables`;
  const d = modal(`Export ${what}`);

  // Which tables, or which rows.
  const picks = new Map<string, HTMLInputElement>();
  if (source.kind === "tables") {
    const list = el("div", { className: "pick-list" });
    for (const t of ctx.tables) {
      const c = check(t, source.tables.includes(t));
      picks.set(t, c.input);
      list.append(c.label);
    }
    d.append(list);
  }
  const rows = el("select", {}, option("all", "Every row that matches the filters"), option("page", `Only the current page${source.kind === "rows" ? ` (${fmt(source.pageRows)} rows)` : ""}`));
  if (source.kind === "rows") d.append(labelled("Rows", rows));

  const format = el("select", {}, option("csv", "CSV", p.format === "csv"), option("json", "JSON", p.format === "json"), option("sql", "SQL", p.format === "sql"));
  const delimiter = el("select", {}, ...[[",", "Comma"], [";", "Semicolon"], ["\t", "Tab"], ["|", "Pipe"]].map(([v, t]) => option(v, t, v === p.delimiter)));
  const lineBreak = el("select", {}, option("lf", "LF (macOS, Linux)", !p.crlf), option("crlf", "CRLF (Windows)", p.crlf));
  const header = check("Field names in the first row", p.header);
  const quoteAll = check("Quote every field (not only where needed)", p.quoteAll);
  const nullEmpty = check("NULL as an empty field (otherwise the text NULL)", p.nullEmpty);
  const csvBox = el("div", { className: "opts" }, labelled("Delimiter", delimiter), labelled("Line break", lineBreak), header.label, quoteAll.label, nullEmpty.label);

  const structure = check("Structure (CREATE TABLE)", p.structure);
  const drop = check("DROP TABLE IF EXISTS first", p.drop);
  const content = check("Content (INSERT statements)", p.content);
  const insertInto = el("input", { value: "table_name", spellcheck: false });
  const sqlBox = el("div", { className: "opts" }, ...(source.kind === "query" ? [labelled("Insert into", insertInto)] : [structure.label, drop.label]), content.label);

  const gzip = check("Compress with gzip (.gz)", p.gzip);
  const pattern = el("input", { value: p.pattern, spellcheck: false });
  const status = el("p", { className: "status" });
  const showOpts = () => ((csvBox.hidden = format.value !== "csv"), (sqlBox.hidden = format.value !== "sql"));
  format.onchange = showOpts;
  showOpts();

  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const go = el("button", { type: "button", className: "primary", textContent: "Export…" });
  cancel.onclick = () => d.close();
  d.append(
    labelled("Format", format), csvBox, sqlBox, gzip.label,
    labelled("File name", pattern),
    el("p", { className: "hint", textContent: "{connection} {database} {table} {date} {time} are filled in; the extension is added." }),
    status,
    el("footer", {}, cancel, el("span", { className: "spacer" }), go),
  );

  go.onclick = async () => {
    const tables = source.kind === "tables" ? [...picks].filter(([, c]) => c.checked).map(([t]) => t) : [];
    if (source.kind === "tables" && !tables.length) return void (status.textContent = "Choose at least one table.");
    const prefs: ExportPrefs = {
      format: format.value as ExportPrefs["format"], delimiter: delimiter.value, crlf: lineBreak.value === "crlf",
      header: header.input.checked, quoteAll: quoteAll.input.checked, nullEmpty: nullEmpty.input.checked,
      structure: structure.input.checked, drop: drop.input.checked, content: content.input.checked,
      gzip: gzip.input.checked, pattern: pattern.value,
    };
    try {
      localStorage.setItem(PREFS_KEY, JSON.stringify(prefs));
    } catch {} // only a convenience
    const dir = await openDialog({ directory: true, title: "Export into folder" });
    if (typeof dir !== "string") return;
    const ext = prefs.format + (prefs.gzip ? ".gz" : "");
    const now = new Date();
    const path = (table: string) => `${dir}/${fileName(prefs.pattern, { connection: ctx.conn, database: ctx.database, table, now }, ext)}`;
    const items =
      source.kind === "tables" ? tables.map((t) => ({ path: path(t), source: { table: t } }))
      : source.kind === "rows" ? [{ path: path(source.request.table), source: { rows: { request: source.request, page: rows.value === "page" } } }]
      : [{ path: path("query"), source: { query: { sql: source.sql, table: insertInto.value.trim() || "table_name" } } }];
    const fmtOpts =
      prefs.format === "csv" ? { csv: { delimiter: prefs.delimiter, quote_all: prefs.quoteAll, crlf: prefs.crlf, header: prefs.header, null_empty: prefs.nullEmpty } }
      : prefs.format === "sql" ? { sql: { structure: prefs.structure, drop: prefs.drop, content: prefs.content } }
      : "json";
    d.close();
    const label = source.kind !== "tables" ? what : tables.length === 1 ? tables[0] : `${tables.length} tables`;
    startJob(ctx, `Export ${label}`, "start_export", { export: { items, format: fmtOpts, gzip: prefs.gzip } });
  };
  d.showModal();
}

// ---------- Import ----------

const EXTENSIONS = { csv: ["csv", "tsv", "txt"], json: ["json"], sql: ["sql"] };

/** Asks for a file, then shows the import wizard for it. `table` preselects the target. */
export async function importFile(ctx: Ctx, kind: "csv" | "json" | "sql", table: string | null) {
  const path = await openDialog({ title: `Import from ${kind.toUpperCase()}`, filters: [{ name: kind.toUpperCase(), extensions: EXTENSIONS[kind] }] });
  if (typeof path !== "string") return;
  if (kind === "sql") return sqlWizard(ctx, path);
  importWizard(ctx, kind, path, table);
}

/** Shows the start of a .sql file, then runs it statement by statement. */
async function sqlWizard(ctx: Ctx, path: string) {
  const d = modal(`Run ${baseName(path)}`);
  const pre = el("pre", { className: "sql-preview", textContent: "Loading…" });
  const run = el("button", { type: "button", className: "primary", textContent: "Run" });
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  cancel.onclick = () => d.close();
  d.append(
    el("p", { className: "hint", textContent: `Every statement runs on "${ctx.conn}", one after another, stopping at the first error.` }),
    pre, el("footer", {}, cancel, el("span", { className: "spacer" }), run),
  );
  run.onclick = () => {
    d.close();
    startJob(ctx, `Run ${baseName(path)}`, "start_sql_import", { path }, (r) => !r.error && ctx.imported(""));
  };
  d.showModal();
  try {
    const text = await invoke<string>("read_text_file", { path });
    pre.textContent = text.length > 4000 ? text.slice(0, 4000) + "\n…" : text;
  } catch (e) {
    pre.textContent = `No preview: ${e}`;
  }
}

const NEW_TABLE = "\u0000new";

/** The CSV or JSON wizard: file options, a preview, where each field goes, and what to do with duplicates. */
function importWizard(ctx: Ctx, kind: "csv" | "json", path: string, table: string | null) {
  const d = modal(`Import ${baseName(path)}`, "wide");
  const header = check("First row has field names", true);
  const delimiter = el("select", {}, ...[[",", "Comma"], [";", "Semicolon"], ["\t", "Tab"], ["|", "Pipe"]].map(([v, t]) => option(v, t, v === (path.endsWith(".tsv") ? "\t" : ","))));
  const quote = el("select", {}, option('"', 'Double quote "'), option("'", "Single quote '"));
  const emptyNull = check("Empty fields as NULL", true);
  const csvOpts = el("div", { className: "row-opts" }, header.label, labelled("Delimiter", delimiter), labelled("Quote", quote), emptyNull.label);
  csvOpts.hidden = kind !== "csv";

  const target = el("select", {}, ...ctx.tables.map((t) => option(t, t, t === table)), option(NEW_TABLE, "New table…", !table));
  const newName = el("input", { placeholder: "New table name", spellcheck: false, value: baseName(path).replace(/\.\w+$/, "") });
  const match = el("select", {}, option("name-ci", "By name, ignoring case"), option("name", "By name"), option("order", "By order"));
  const conflict = el("select", {}, option("stop", "Stop and undo the whole import"), option("skip", "Keep the existing row"), option("update", "Update the existing row"));
  const mapping = el("div", { className: "mapping" });
  const grid = el("div", { className: "preview-grid" });
  const status = el("p", { className: "status" });
  const go = el("button", { type: "button", className: "primary", textContent: "Import", disabled: true });
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  cancel.onclick = () => d.close();
  d.append(
    csvOpts,
    el("div", { className: "row-opts" }, labelled("Into", target), newName, labelled("Match", match)),
    labelled("Duplicates", conflict),
    el("div", { className: "wizard" }, mapping, grid),
    status,
    el("footer", {}, cancel, el("span", { className: "spacer" }), go),
  );

  let preview: Preview = { fields: [], rows: [] };
  let pickers: (HTMLSelectElement | HTMLInputElement)[] = [];
  const format = () => (kind === "json" ? "json" : { csv: { delimiter: delimiter.value, quote: quote.value, header: header.input.checked, empty_is_null: emptyNull.input.checked } });

  async function load() {
    status.textContent = "Reading…";
    status.className = "status busy";
    try {
      preview = await invoke<Preview>("preview_import", { path, format: format() });
      const n = preview.rows.length;
      status.textContent = n === 100 ? "The first 100 rows" : `${fmt(n)} row${n === 1 ? "" : "s"}`;
      status.className = "status";
    } catch (e) {
      preview = { fields: [], rows: [] };
      status.textContent = String(e);
      status.className = "status error";
    }
    grid.replaceChildren(
      el("table", {},
        el("thead", {}, el("tr", {}, ...preview.fields.map((f) => el("th", { textContent: f })))),
        el("tbody", {}, ...preview.rows.map((r) => el("tr", {}, ...preview.fields.map((_, i) => {
          const v = r[i] ?? null;
          return el("td", { textContent: v ?? "NULL", className: v === null ? "null" : "" });
        })))),
      ),
    );
    await remap();
  }

  /** One row per field: where it goes. A new table takes field names as column names. */
  async function remap() {
    const creating = target.value === NEW_TABLE;
    newName.hidden = !creating;
    match.disabled = creating;
    conflict.disabled = creating;
    let columns: string[] = [];
    if (!creating) {
      try {
        columns = await ctx.columns(target.value);
      } catch (e) {
        status.textContent = String(e);
        status.className = "status error";
      }
    }
    const matched = creating ? preview.fields : matchColumns(preview.fields, columns, match.value !== "order", match.value === "name-ci");
    pickers = preview.fields.map((_, i) => {
      if (creating) return el("input", { value: preview.fields[i], spellcheck: false, placeholder: "skip" });
      return el("select", {}, option("", "— skip —"), ...columns.map((c) => option(c, c, c === matched[i])));
    });
    mapping.replaceChildren(
      el("div", { className: "map-head" }, el("span", { textContent: "Field" }), el("span", { textContent: creating ? "New column" : "Column" })),
      ...preview.fields.map((f, i) => el("div", { className: "map-row" }, el("span", { className: "label", textContent: f, title: f }), pickers[i])),
    );
    go.disabled = !preview.fields.length;
  }

  for (const c of [header.input, delimiter, quote, emptyNull.input]) c.onchange = load;
  target.onchange = match.onchange = remap;

  go.onclick = () => {
    const creating = target.value === NEW_TABLE;
    const table = creating ? newName.value.trim() : target.value;
    if (!table) return void (status.textContent = "Name the new table.");
    const columns = pickers.map((p) => p.value.trim() || null);
    if (!columns.some((c) => c)) return void (status.textContent = "Match at least one field to a column.");
    d.close();
    const imp = { path, format: format(), table, columns, create: creating, conflict: creating ? "stop" : conflict.value };
    startJob(ctx, `Import ${baseName(path)} into ${table}`, "start_import", { import: imp }, (r) => !r.error && ctx.imported(table));
  };

  d.showModal();
  load();
}
