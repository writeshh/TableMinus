// MongoDB workspaces (mongo/mod.rs): in place of tables and SQL, the sidebar lists collections and the main area
// shows the chosen collection's documents a page at a time (a filter and sort in mongosh syntax, cells edited in
// place, whole documents in an editor) over a shell that runs mongosh-style scripts. Every change and every script
// that may write goes through safe mode, like SQL.

import { EditorView, basicSetup } from "codemirror";
import { EditorState, Prec } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { $, el } from "./dom";
import { modal } from "./backup";
import { editorLook } from "./editor";
import { CANCELLED, guarded } from "./safety";
import { cell, columnsOf, countScript, documentText, expression, pageScript, shellText, typedExpression } from "./mongo-text";

type MenuItem = [string, () => void] | "-";
type Doc = Record<string, unknown>;
type Outcome =
  | { kind: "docs"; title: string; docs: Doc[]; more: boolean }
  | { kind: "value"; title: string; value: unknown }
  | { kind: "done"; title: string; text: string };
type Edit = Record<string, unknown> & { op: string };

export type MongoCtx = {
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  /** The collections the workspace's connect found (its `tables`). */
  collections: () => string[];
  /** Asks the backend for the collections again (`mongo_connect` on the open session). */
  reload: () => Promise<void>;
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  confirm: (text: string, okLabel: string) => Promise<boolean>;
  menu: (e: MouseEvent, items: MenuItem[]) => void;
  /** Documents a shell result shows at most (Settings › Editor's row limit). */
  limit: () => number;
};
let ctx: MongoCtx;

const PAGE = 100;

/** Each MongoDB workspace's chosen collection, its page and the shell. */
type State = {
  collection: string | null; filter: string; sort: string; skip: number;
  docs: Doc[]; more: boolean; total: number | null; loading: boolean; problem: string;
  script: string; outcomes: Outcome[]; shown: number; running: boolean; shellProblem: string;
  /** The row picked in the grid. */
  row: number | null;
};
const states = new Map<string, State>();
const stateOf = (name: string): State => {
  let s = states.get(name);
  if (!s) {
    s = { collection: null, filter: "", sort: "", skip: 0, docs: [], more: false, total: null, loading: false, problem: "",
      script: "db.getCollectionNames()", outcomes: [], shown: 0, running: false, shellProblem: "", row: null };
    states.set(name, s);
  }
  return s;
};

// ---------- DOM ----------

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
#workspace:not(.mongo) :is(#mongo-side, #mongo-main) { display: none; }
body.mongo-ws #dbpicker :is(#db-new, #db-rename, #db-drop) { display: none; }
#workspace.mongo :is(#side-items, .side-tabs, #side-search, #side-history) { display: none !important; }
#workspace.mongo :is(#panes, #filters, #table-foot) { display: none !important; }
body:has(#workspace.mongo) :is(#new-sql, #tools-button, #assistant-button, .tool-icons) { display: none !important; }
#mongo-side { display: flex; flex-direction: column; gap: 6px; min-height: 0; flex: 1; }
#mongo-side .mongo-find { display: flex; gap: 4px; }
#mongo-side .mongo-find input { flex: 1; min-width: 0; }
#mongo-colls { list-style: none; margin: 0; padding: 0; overflow: auto; flex: 1; min-height: 0; }
#mongo-colls li { display: flex; gap: 6px; align-items: center; padding: 3px 8px; border-radius: 999px; cursor: default; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font: 12px/1.4 var(--mono); }
#mongo-colls li:hover { background: var(--hover); }
#mongo-colls li.active { background: color-mix(in srgb, var(--accent) 16%, transparent); }
#mongo-side .mongo-foot { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; }
#mongo-main { display: flex; flex-direction: column; min-height: 0; flex: 1; }
#mongo-docs { flex: 1; min-height: 0; display: flex; flex-direction: column; }
#mongo-docs .bar { display: flex; gap: 6px; align-items: center; padding: 6px 10px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
#mongo-docs .bar h3 { margin: 0 6px 0 0; font: 600 13px/1.3 var(--mono); }
#mongo-docs .bar input { font: 12px/1.4 var(--mono); min-width: 140px; flex: 1; }
#mongo-docs .bar .sort { flex: 0 1 180px; }
#mongo-docs .bar .meta { color: var(--muted); font-size: 12px; white-space: nowrap; }
.mongo-grid { flex: 1; min-height: 0; overflow: auto; }
.mongo-grid table { border-collapse: collapse; font: var(--grid-size, 12px) var(--grid-font, var(--mono)); }
.mongo-grid :is(th, td) { padding: 3px 10px; border-bottom: 1px solid var(--grid-line, var(--line)); border-right: 1px solid var(--grid-line, var(--line)); text-align: left; white-space: nowrap; max-width: 320px; overflow: hidden; text-overflow: ellipsis; }
.mongo-grid th { position: sticky; top: 0; background: var(--side); font-weight: 600; z-index: 1; }
.mongo-grid td.null { color: var(--grid-null, var(--muted)); }
.mongo-grid td.missing { background: color-mix(in srgb, var(--muted) 6%, transparent); }
.mongo-grid td.num { text-align: right; font-variant-numeric: tabular-nums; }
.mongo-grid tr.picked td { background: color-mix(in srgb, var(--accent) 16%, transparent); }
.mongo-grid .hint { padding: 12px 16px; }
#mongo-shell { flex: none; height: 300px; display: flex; flex-direction: column; border-top: 1px solid var(--line); background: var(--side); }
#mongo-shell .editor { flex: 0 0 110px; min-height: 0; overflow: auto; border-bottom: 1px solid var(--line); }
#mongo-shell .editor .cm-editor { height: 100%; }
#mongo-shell .run { display: flex; gap: 6px; align-items: center; padding: 4px 8px; border-bottom: 1px solid var(--line); }
#mongo-shell .tabs { display: flex; gap: 4px; flex-wrap: wrap; }
#mongo-shell .tabs button[aria-pressed="true"] { background: color-mix(in srgb, var(--accent) 16%, transparent); }
#mongo-shell .out { flex: 1; min-height: 0; overflow: auto; }
#mongo-shell pre { margin: 0; padding: 8px 12px; font: 12px/1.45 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
.mongo-doc textarea { width: 100%; box-sizing: border-box; min-height: 320px; font: 12px/1.45 var(--mono); padding: 8px; resize: vertical; }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];

const button = (text: string, onclick: () => void, className = "ghost", title = "") =>
  Object.assign(el("button", { type: "button", textContent: text, className, title }), { onclick });

const find = el("input", { type: "search", placeholder: "Collections", title: "Show the collections whose names contain this" });
const collList = el("ul", { id: "mongo-colls" });
const sideFoot = el("div", { className: "mongo-foot" });
const side = el("div", { id: "mongo-side" }, el("div", { className: "mongo-find" }, find, button("+", () => void newCollection(), "ghost", "New collection")), collList, sideFoot);

const filterInput = el("input", { placeholder: "Filter, e.g. { age: { $gt: 30 } }", spellcheck: false, title: "A query document in mongosh syntax; Return applies it" });
const sortInput = el("input", { className: "sort", placeholder: "Sort, e.g. { _id: -1 }", spellcheck: false });
const meta = el("span", { className: "meta" });
const prev = button("‹", () => void page(-1), "ghost", "Previous page");
const next = button("›", () => void page(1), "ghost", "Next page");
const title = el("h3");
const bar = el("div", { className: "bar" }, title, filterInput, sortInput, button("Apply", () => void apply(), "", "Read the documents again with this filter and sort"),
  prev, meta, next, button("Add…", () => void addDocument(), "ghost", "Insert a document"), button("↻", () => void loadPage(), "ghost", "Reload"));
const grid = el("div", { className: "mongo-grid" });
const docsView = el("section", { id: "mongo-docs" }, bar, grid);

const editorHost = el("div", { className: "editor" });
const runButton = button("Run", () => void runShell(), "primary", "Run the script (⌘↩)");
const outTabs = el("div", { className: "tabs" });
const shellNote = el("span", { className: "status" });
const out = el("div", { className: "out" });
const shell = el("section", { id: "mongo-shell" }, editorHost, el("div", { className: "run" }, runButton, outTabs, el("span", { className: "spacer" }), shellNote), out);
const main = el("section", { id: "mongo-main" }, docsView, shell);

let view: EditorView | null = null;
/** The shell editor, made the first time a MongoDB workspace shows (its language loads then). */
async function shellEditor() {
  if (view) return view;
  const { javascript } = await import("@codemirror/lang-javascript");
  view ??= new EditorView({
    parent: editorHost,
    state: EditorState.create({
      doc: stateOf(session()).script,
      extensions: [
        Prec.highest(keymap.of([{ key: "Mod-Enter", run: () => (void runShell(), true) }])),
        basicSetup, keymap.of([indentWithTab]), javascript(), editorLook,
        EditorView.updateListener.of((u) => u.docChanged && (stateOf(session()).script = u.state.doc.toString())),
      ],
    }),
  });
  return view;
}

const session = () => ctx.conn()?.name ?? "";
const isMongo = () => ctx.conn()?.kind === "mongodb";

/** Shows the MongoDB view when the workspace on screen is a MongoDB one; `fresh` reads the collections again. */
export function syncMongo(fresh = false) {
  const on = isMongo();
  $("workspace").classList.toggle("mongo", on);
  document.body.classList.toggle("mongo-ws", on); // the database window is outside the workspace
  if (!on) return;
  const s = stateOf(session());
  if (fresh && s.collection && !ctx.collections().includes(s.collection)) Object.assign(s, { collection: null, docs: [], total: null });
  void shellEditor().then((v) => {
    if (v.state.doc.toString() !== s.script) v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: s.script } });
  });
  renderCollections();
  renderDocs();
  renderShell();
}

// ---------- Collections ----------

function renderCollections() {
  const s = stateOf(session());
  const q = find.value.trim().toLowerCase();
  const all = ctx.collections();
  const shown = all.filter((c) => c.toLowerCase().includes(q));
  collList.replaceChildren(...shown.map((c) => {
    const li = el("li", { textContent: c, title: c });
    li.classList.toggle("active", c === s.collection);
    li.onclick = () => void openCollection(c);
    li.oncontextmenu = (e) => ctx.menu(e, [
      ["Open", () => void openCollection(c)],
      ["Copy name", () => void navigator.clipboard.writeText(c).catch(() => {})],
      ["Query in shell", () => setScript(`db.getCollection(${JSON.stringify(c)}).find({})`)],
      "-",
      ["Rename…", () => void renameCollection(c)],
      ["Drop…", () => void dropCollection(c)],
    ]);
    return li;
  }));
  sideFoot.replaceChildren(el("span", { textContent: `${all.length.toLocaleString()} collection${all.length === 1 ? "" : "s"}` }));
}
find.oninput = renderCollections;

async function openCollection(c: string) {
  const s = stateOf(session());
  if (s.collection !== c) Object.assign(s, { collection: c, filter: "", sort: "", skip: 0, total: null, row: null });
  renderCollections();
  await loadPage();
}

/** Runs `script` through safe mode (asking first when it may write); null when it wasn't run. */
async function run(script: string, maxDocs: number, what: string): Promise<Outcome[] | null> {
  const name = session();
  try {
    return await guarded(name, (a) => ctx.call<Outcome[]>("mongo_run", { name, script, maxDocs, confirmed: a.confirmed, unlock: a.unlock }),
      () => ctx.confirm(what, "Run"));
  } catch (e) {
    if (e === CANCELLED) return null;
    throw e;
  }
}

async function loadPage() {
  const name = session();
  const s = stateOf(name);
  if (!s.collection) return renderDocs();
  s.loading = true;
  s.problem = "";
  renderDocs();
  try {
    const [found] = (await run(pageScript(s.collection, s.filter, s.sort, s.skip, PAGE), PAGE, "")) ?? [];
    if (found?.kind === "docs") Object.assign(s, { docs: found.docs, more: found.more });
    if (s.total === null) {
      const [counted] = (await run(countScript(s.collection, s.filter), 1, "")) ?? [];
      s.total = counted?.kind === "value" && typeof counted.value === "number" ? counted.value : null;
    }
  } catch (e) {
    Object.assign(s, { problem: String(e), docs: [], more: false });
  } finally {
    s.loading = false;
    if (session() === name) renderDocs();
  }
}

function apply() {
  const s = stateOf(session());
  Object.assign(s, { filter: filterInput.value, sort: sortInput.value, skip: 0, total: null, row: null });
  return loadPage();
}
for (const input of [filterInput, sortInput]) input.onkeydown = (e) => e.key === "Enter" && (e.preventDefault(), void apply());

function page(step: number) {
  const s = stateOf(session());
  s.skip = Math.max(0, s.skip + step * PAGE);
  s.row = null;
  return loadPage();
}

function renderDocs() {
  const s = stateOf(session());
  title.textContent = s.collection ?? "";
  bar.hidden = !s.collection;
  filterInput.value = s.filter;
  sortInput.value = s.sort;
  const shown = s.docs.length;
  const total = s.total === null ? "?" : s.total.toLocaleString();
  meta.textContent = s.loading ? "Loading…" : `${shown ? (s.skip + 1).toLocaleString() : 0}–${(s.skip + shown).toLocaleString()} of ${total}`;
  prev.disabled = s.loading || s.skip === 0;
  next.disabled = s.loading || !s.more;
  if (!s.collection) return grid.replaceChildren(el("p", { className: "hint", textContent: "Choose a collection on the left, or run a script below." }));
  if (s.problem) return grid.replaceChildren(el("p", { className: "hint status error", textContent: s.problem }));
  if (!shown && !s.loading) return grid.replaceChildren(el("p", { className: "hint", textContent: s.filter ? "No documents match the filter." : "This collection is empty." }));
  grid.replaceChildren(docTable(s.docs, true));
}

/** Documents as a table; `editable` ones (a collection's page) take edits, the shell's only show. */
function docTable(docs: Doc[], editable: boolean) {
  const columns = columnsOf(docs);
  const s = stateOf(session());
  const head = el("tr", {}, ...columns.map((c) => el("th", { textContent: c, title: c })));
  const rows = docs.map((d, ri) => {
    const tr = el("tr", {}, ...columns.map((c) => {
      const v = d[c];
      const text = cell(v);
      const td = el("td", { textContent: text ?? "NULL", title: text && text.length > 40 ? text : "" });
      td.className = v === null ? "null" : v === undefined ? "missing" : typeof v === "number" ? "num" : "";
      if (editable) {
        td.ondblclick = () => void editCell(d, c);
        td.oncontextmenu = (e) => ctx.menu(e, cellMenu(d, c));
      }
      return td;
    }));
    if (editable) {
      tr.classList.toggle("picked", s.row === ri);
      tr.onclick = () => ((s.row = ri), grid.querySelectorAll("tr.picked").forEach((r) => r.classList.remove("picked")), tr.classList.add("picked"));
    }
    return tr;
  });
  return el("table", {}, el("thead", {}, head), el("tbody", {}, ...rows));
}

function cellMenu(d: Doc, field: string): MenuItem[] {
  const v = d[field];
  const has = "_id" in d;
  return [
    ...(has && field !== "_id" ? [["Edit value…", () => void editCell(d, field)] as MenuItem] : []),
    ...(has ? [["Edit document…", () => void editDocument(d)] as MenuItem] : []),
    "-",
    ["Copy value", () => void navigator.clipboard.writeText(cell(v) ?? "null").catch(() => {})],
    ["Copy document as JSON", () => void navigator.clipboard.writeText(JSON.stringify(d, null, 2)).catch(() => {})],
    ...(v !== undefined ? [[`Filter where ${field} is this`, () => ((filterInput.value = `{ ${JSON.stringify(field)}: ${shellText(v)} }`), void apply())] as MenuItem] : []),
    "-",
    ...(has && field !== "_id" && v !== undefined ? [[`Remove field ${field}…`, () => void change({ op: "unset", id: expression(d._id), field }, `Remove ${field} from this document?`)] as MenuItem] : []),
    ...(has ? [["Delete document…", () => void deleteDocument(d)] as MenuItem] : []),
  ];
}

/** Makes a change in the open collection through safe mode, then reads the page again. */
async function change(edit: Edit, what: string) {
  const name = session();
  const s = stateOf(name);
  if (!s.collection) return;
  try {
    const said = await guarded(name, (a) => ctx.call<string>("mongo_edit", { name, edit: { ...edit, collection: s.collection }, confirmed: a.confirmed, unlock: a.unlock }),
      () => ctx.confirm(what, "Change"));
    shellNote.textContent = said;
    shellNote.className = "status done";
  } catch (e) {
    if (e === CANCELLED) return;
    shellNote.textContent = String(e);
    shellNote.className = "status error";
    return;
  }
  s.total = null;
  await loadPage();
}

/** A dialog with one text field; resolves to its value, or null when cancelled. */
function ask(heading: string, label: string, value: string, ok: string, big = false): Promise<string | null> {
  const input = big ? el("textarea", { value, spellcheck: false }) : el("input", { value, spellcheck: false });
  const { d, footer } = modal(heading, big ? "wide mongo-doc" : "small redis-ask", el("label", { className: "stack" }, label, input));
  return new Promise((resolve) => {
    let result: string | null = null;
    const go = el("button", { className: "primary", textContent: ok });
    go.onclick = () => ((result = input.value), d.close());
    const cancel = el("button", { textContent: "Cancel" });
    cancel.onclick = () => d.close();
    footer.append(el("span", { className: "spacer" }), cancel, go);
    d.addEventListener("close", () => resolve(result));
    d.showModal();
    input.focus();
  });
}

async function editCell(d: Doc, field: string) {
  if (field === "_id" || !("_id" in d)) return editDocument(d);
  const v = d[field];
  const typed = await ask(`Edit ${field}`, typeof v === "string" ? "Text" : "Value, in mongosh syntax (\"text\", 42, true, ObjectId(\"…\"), { … })", typeof v === "string" ? v : v === undefined ? "" : shellText(v), "Save");
  if (typed === null) return;
  await change({ op: "set", id: expression(d._id), field, value: typedExpression(v, typed) }, `Set ${field} of this document?`);
}

async function editDocument(d: Doc) {
  const text = await ask("Edit document", "The whole document, in mongosh syntax; its _id stays", documentText(d), "Save", true);
  if (text === null) return;
  await change({ op: "replace", id: expression(d._id), document: text }, "Replace this document?");
}

async function addDocument() {
  const text = await ask("Add document", "A document, in mongosh syntax; without an _id one is made", "{\n  \n}", "Insert", true);
  if (text === null) return;
  await change({ op: "insert", document: text }, "Insert this document?");
}

async function deleteDocument(d: Doc) {
  if (!(await ctx.confirm(`Delete the document with _id ${cell(d._id)}? This can't be undone.`, "Delete"))) return;
  await change({ op: "delete", ids: [expression(d._id)] }, "Delete this document?");
}

async function newCollection() {
  const name = await ask("New collection", "Name", "", "Create");
  if (!name?.trim()) return;
  await shellAction(`db.createCollection(${JSON.stringify(name.trim())})`, `Create the collection ${name.trim()}?`);
  await openCollection(name.trim());
}

async function renameCollection(c: string) {
  const to = await ask(`Rename ${c}`, "New name", c, "Rename");
  if (!to?.trim() || to.trim() === c) return;
  if (await shellAction(`db.getCollection(${JSON.stringify(c)}).renameCollection(${JSON.stringify(to.trim())})`, `Rename ${c} to ${to.trim()}?`)) {
    const s = stateOf(session());
    if (s.collection === c) s.collection = to.trim();
    renderDocs();
  }
}

async function dropCollection(c: string) {
  if (!(await ctx.confirm(`Drop the collection ${c} and all its documents? This can't be undone.`, "Drop"))) return;
  if (await shellAction(`db.getCollection(${JSON.stringify(c)}).drop()`, `Drop ${c}?`)) {
    const s = stateOf(session());
    if (s.collection === c) Object.assign(s, { collection: null, docs: [], total: null });
    renderDocs();
  }
}

/** Runs a script the view wrote (create, rename, drop a collection), then lists the collections again. */
async function shellAction(script: string, what: string): Promise<boolean> {
  try {
    const r = await run(script, 1, what);
    if (!r) return false;
    await ctx.reload();
    renderCollections();
    return true;
  } catch (e) {
    shellNote.textContent = String(e);
    shellNote.className = "status error";
    return false;
  }
}

// ---------- Shell ----------

function setScript(text: string) {
  const s = stateOf(session());
  s.script = text;
  view?.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
  view?.focus();
}

async function runShell() {
  const name = session();
  const s = stateOf(name);
  if (s.running || !s.script.trim()) return;
  s.running = true;
  s.shellProblem = "";
  renderShell();
  try {
    const r = await run(s.script, ctx.limit(), `Run this script? It may change data. ${s.script.length > 300 ? s.script.slice(0, 300) + "…" : s.script}`);
    if (r) Object.assign(s, { outcomes: r, shown: r.length - 1 });
    else s.shellProblem = "Not run.";
  } catch (e) {
    s.shellProblem = String(e);
  } finally {
    s.running = false;
  }
  if (session() !== name) return;
  renderShell();
  // The script may have changed the collections or the documents on screen.
  await ctx.reload();
  renderCollections();
  if (s.collection) await loadPage();
}

function renderShell() {
  const s = stateOf(session());
  runButton.disabled = s.running;
  shellNote.textContent = s.running ? "Running…" : s.shellProblem;
  shellNote.className = `status ${s.shellProblem && !s.running ? "error" : s.running ? "busy" : ""}`;
  outTabs.replaceChildren(...(s.outcomes.length > 1 ? s.outcomes.map((o, i) => {
    const b = button(o.title, () => ((s.shown = i), renderShell()));
    b.setAttribute("aria-pressed", String(i === s.shown));
    return b;
  }) : []));
  const o = s.outcomes[s.shown];
  if (!o) return out.replaceChildren(el("pre", { className: "hint", textContent: "Scripts use mongosh syntax: db.orders.find({ total: { $gt: 5 } }).sort({ _id: -1 }), show collections, use otherdb. ⌘↩ runs." }));
  if (o.kind === "done") return out.replaceChildren(el("pre", { textContent: o.text }));
  if (o.kind === "value") return out.replaceChildren(el("pre", { textContent: typeof o.value === "string" ? o.value : documentText(o.value) }));
  const note = `${o.docs.length.toLocaleString()} document${o.docs.length === 1 ? "" : "s"}${o.more ? `, the first ${o.docs.length.toLocaleString()} (raise the row limit, or add .skip() and .limit())` : ""}`;
  out.replaceChildren(el("pre", { className: "hint", textContent: note }), ...(o.docs.length ? [el("div", { className: "mongo-grid" }, docTable(o.docs, false))] : []));
}

export function initMongo(c: MongoCtx) {
  ctx = c;
  $("side-items").after(side);
  $("panes").after(main);
}

/** Forgets a closed workspace's page and shell. */
export const forgetMongo = (name: string) => void states.delete(name);
