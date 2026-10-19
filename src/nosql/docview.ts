// The workspace view of the document stores (MongoDB, DynamoDB): in place of tables and SQL, the sidebar lists the
// engine's collections and the main area shows the chosen one's documents a page at a time (with a filter, and a
// sort where the engine has one), cells edited in place, whole documents in an editor, over a console for the
// engine's own query language. What differs by engine (its commands, how values read and are typed back, how a
// document is found again) is its `Engine` (mongo.ts, dynamo.ts). Every change, and every script that may write,
// goes through safe mode, like SQL.

import { EditorView, basicSetup } from "codemirror";
import { EditorState, Prec, type Extension } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { $, el } from "../dom";
import { modal } from "../database/backup";
import { editorLook } from "../editor/editor";
import { CANCELLED, guarded, type Approval } from "../shell/safety";

type MenuItem = [string, () => void] | "-";
export type Doc = Record<string, unknown>;
/** What one statement or operation gave, for its own result tab. */
export type Outcome =
  | { kind: "docs"; title: string; docs: Doc[]; more: boolean }
  | { kind: "value"; title: string; value: unknown }
  | { kind: "done"; title: string; text: string };
/** A page of documents; `next` reads the one after it (null on the last). */
export type Page = { docs: Doc[]; next: string | null };
/** A change to make, in the engine's own terms (its backend's edit command takes it). */
export type Edit = Record<string, unknown> & { op: string };

export type Host = {
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  /** The collections the workspace's connect found (its `tables`). */
  collections: () => string[];
  /** Lists the collections again (the engine's connect on the open session). */
  reload: () => Promise<void>;
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  confirm: (text: string, okLabel: string) => Promise<boolean>;
  menu: (e: MouseEvent, items: MenuItem[]) => void;
  /** Documents a console result shows at most (Settings › Editor's row limit). */
  limit: () => number;
};

/** What makes one engine's view. `call` sends a command with the session's name added. */
export type Engine = {
  kind: string;
  /** The class on #workspace while it shows (mongo, dynamo). */
  css: string;
  words: { collection: string; doc: string };
  filterHint: string;
  /** Null when the engine has no sort. */
  sortHint: string | null;
  console: { language: () => Promise<Extension>; start: string; hint: string; confirm: (script: string) => string };
  /** A page of documents: the first, or the one `token` names. */
  page: (call: Call, collection: string, q: { filter: string; sort: string; token: string | null; size: number }) => Promise<Page>;
  /** How many documents the filter matches, as shown ("2", "~1,200"); null when unknown. */
  count: (call: Call, collection: string, filter: string) => Promise<string | null>;
  /** The grid's columns for these documents. */
  columns: (docs: Doc[], collection: string) => string[];
  /** A cell: null for NULL, "" for a missing field. */
  cell: (v: unknown) => string | null;
  /** A value as it's typed back; and what a typed text becomes, given the value it replaces. */
  valueText: (v: unknown) => string;
  typed: (original: unknown, text: string) => string;
  docText: (d: Doc) => string;
  newDocText: string;
  /** The filter that finds documents with this value in `field`. */
  filterFor: (field: string, v: unknown) => string;
  /** How a document is found again: the fields that make its identity, which can't change. */
  identity: (d: Doc, collection: string) => Edit | null;
  /** Fields that can't change (`_id`, a table's key). */
  fixed: (field: string, collection: string) => boolean;
  /** The edit command, and the edits it takes. */
  editCommand: string;
  edits: {
    set: (id: Edit, field: string, value: string) => Edit;
    remove: (id: Edit, field: string) => Edit;
    replace: (id: Edit, text: string) => Edit;
    insert: (collection: string, text: string) => Edit;
    delete: (ids: Edit[]) => Edit;
  };
  /** Runs console text; may write, so it goes through safe mode. */
  run: (call: Call, script: string, limit: number, a: Approval) => Promise<Outcome[]>;
  /** Called when a collection opens, before its first page (DynamoDB reads the table's key). */
  opened?: (call: Call, collection: string) => Promise<void>;
  /** Extra items in a collection's menu, and the New button's action; console text runs through `script`. */
  collectionMenu?: (c: string, script: Script, ask: Ask) => MenuItem[];
  newCollection?: (ask: Ask, script: Script) => Promise<string | null>;
};
/** Runs console text the view wrote; with `askFirst`, asks `what` before running whatever safe mode says. */
type Script = (text: string, what: string, askFirst?: boolean) => Promise<boolean>;
export type Call = <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
type Ask = (heading: string, label: string, value: string, ok: string, big?: boolean) => Promise<string | null>;

const PAGE = 100;

type State = {
  collection: string | null; filter: string; sort: string;
  /** Tokens of the pages read so far; the last is the page on screen (null: the first). */
  tokens: (string | null)[];
  next: string | null;
  docs: Doc[]; total: string | null; loading: boolean; problem: string;
  script: string; outcomes: Outcome[]; shown: number; running: boolean; note: [string, string];
  row: number | null;
};

/** Sets up one engine's view; `sync` shows it when the workspace on screen is of that engine. */
export function docView(engine: Engine, host: Host) {
  DOC_CSS.add(engine.css);
  const states = new Map<string, State>();
  const session = () => host.conn()?.name ?? "";
  const on = () => host.conn()?.kind === engine.kind;
  const call: Call = (cmd, args) => host.call(cmd, { name: session(), ...args });
  const stateOf = (name: string): State => {
    let s = states.get(name);
    if (!s) {
      s = { collection: null, filter: "", sort: "", tokens: [null], next: null, docs: [], total: null, loading: false, problem: "",
        script: engine.console.start, outcomes: [], shown: 0, running: false, note: ["", ""], row: null };
      states.set(name, s);
    }
    return s;
  };
  const id = (part: string) => `${engine.css}-${part}`;

  // ---------- DOM ----------

  const button = (text: string, onclick: () => void, className = "ghost", title = "") =>
    Object.assign(el("button", { type: "button", textContent: text, className, title }), { onclick });
  const find = el("input", { type: "search", placeholder: `${cap(engine.words.collection)}s`, title: `Show the ${engine.words.collection}s whose names contain this` });
  const list = el("ul", { className: "doc-colls" });
  const sideFoot = el("div", { className: "doc-foot" });
  const side = el("div", { id: id("side"), className: "doc-side" },
    el("div", { className: "doc-find" }, find, ...(engine.newCollection ? [button("+", () => void newCollection(), "ghost", `New ${engine.words.collection}`)] : [])), list, sideFoot);
  const filterInput = el("input", { placeholder: engine.filterHint, spellcheck: false, title: "Return applies it" });
  const sortInput = el("input", { className: "sort", placeholder: engine.sortHint ?? "", spellcheck: false });
  const meta = el("span", { className: "meta" });
  const prev = button("‹", () => void step(-1), "ghost", "Previous page");
  const next = button("›", () => void step(1), "ghost", "Next page");
  const title = el("h3");
  const bar = el("div", { className: "bar" }, title, filterInput, ...(engine.sortHint ? [sortInput] : []),
    button("Apply", () => void apply(), "", "Read the documents again with this filter"), prev, meta, next,
    button("Add…", () => void addDocument(), "ghost", `Insert ${article(engine.words.doc)}`), button("↻", () => void loadPage(), "ghost", "Reload"));
  const grid = el("div", { className: "doc-grid" });
  const docsView = el("section", { className: "doc-docs" }, bar, grid);
  const editorHost = el("div", { className: "editor" });
  const runButton = button("Run", () => void runConsole(), "primary", "Run (⌘↩)");
  const outTabs = el("div", { className: "tabs" });
  const note = el("span", { className: "status" });
  const out = el("div", { className: "out" });
  const consolePane = el("section", { className: "doc-console" }, editorHost, el("div", { className: "run" }, runButton, outTabs, el("span", { className: "spacer" }), note), out);
  const main = el("section", { id: id("main"), className: "doc-main" }, docsView, consolePane);

  let view: EditorView | null = null;
  async function consoleEditor() {
    if (view) return view;
    const language = await engine.console.language();
    view ??= new EditorView({
      parent: editorHost,
      state: EditorState.create({
        doc: stateOf(session()).script,
        extensions: [
          Prec.highest(keymap.of([{ key: "Mod-Enter", run: () => (void runConsole(), true) }])),
          basicSetup, keymap.of([indentWithTab]), language, editorLook,
          EditorView.updateListener.of((u) => u.docChanged && (stateOf(session()).script = u.state.doc.toString())),
        ],
      }),
    });
    return view;
  }

  function say(text: string, kind: "" | "error" | "done" | "busy" = "") {
    stateOf(session()).note = [text, kind];
    note.textContent = text;
    note.className = `status ${kind}`;
  }

  // ---------- Collections ----------

  function renderCollections() {
    const s = stateOf(session());
    const q = find.value.trim().toLowerCase();
    const all = host.collections();
    list.replaceChildren(...all.filter((c) => c.toLowerCase().includes(q)).map((c) => {
      const li = el("li", { textContent: c, title: c });
      li.classList.toggle("active", c === s.collection);
      li.onclick = () => void openCollection(c);
      li.oncontextmenu = (e) => host.menu(e, [
        ["Open", () => void openCollection(c)],
        ["Copy name", () => void navigator.clipboard.writeText(c).catch(() => {})],
        ...(engine.collectionMenu?.(c, script, ask) ?? []),
      ]);
      return li;
    }));
    sideFoot.replaceChildren(el("span", { textContent: `${all.length.toLocaleString()} ${engine.words.collection}${all.length === 1 ? "" : "s"}` }));
  }
  find.oninput = renderCollections;

  async function openCollection(c: string) {
    const s = stateOf(session());
    if (s.collection !== c) Object.assign(s, { collection: c, filter: "", sort: "", tokens: [null], total: null, row: null });
    renderCollections();
    try {
      await engine.opened?.(call, c);
    } catch (e) {
      s.problem = String(e);
      return renderDocs();
    }
    await loadPage();
  }

  async function loadPage() {
    const name = session();
    const s = stateOf(name);
    if (!s.collection) return renderDocs();
    s.loading = true;
    s.problem = "";
    renderDocs();
    try {
      const page = await engine.page(call, s.collection, { filter: s.filter, sort: s.sort, token: s.tokens.at(-1) ?? null, size: PAGE });
      Object.assign(s, { docs: page.docs, next: page.next });
      if (s.total === null) s.total = await engine.count(call, s.collection, s.filter);
    } catch (e) {
      Object.assign(s, { problem: String(e), docs: [], next: null });
    } finally {
      s.loading = false;
      if (session() === name) renderDocs();
    }
  }

  function apply() {
    const s = stateOf(session());
    Object.assign(s, { filter: filterInput.value, sort: sortInput.value, tokens: [null], total: null, row: null });
    return loadPage();
  }
  // void: a handler that returns false cancels the key, so nothing could be typed.
  for (const input of [filterInput, sortInput]) input.onkeydown = (e) => void (e.key === "Enter" && (e.preventDefault(), apply()));

  function step(by: number) {
    const s = stateOf(session());
    if (by > 0 && s.next) s.tokens.push(s.next);
    if (by < 0 && s.tokens.length > 1) s.tokens.pop();
    s.row = null;
    return loadPage();
  }

  function renderDocs() {
    const s = stateOf(session());
    title.textContent = s.collection ?? "";
    bar.hidden = !s.collection;
    filterInput.value = s.filter;
    sortInput.value = s.sort;
    const pageNo = s.tokens.length;
    const shown = s.docs.length;
    meta.textContent = s.loading ? "Loading…" : `Page ${pageNo} · ${shown.toLocaleString()} shown${s.total !== null ? ` of ${s.total}` : ""}`;
    prev.disabled = s.loading || pageNo === 1;
    next.disabled = s.loading || !s.next;
    if (!s.collection) return grid.replaceChildren(el("p", { className: "hint", textContent: `Choose ${article(engine.words.collection)} on the left, or run something below.` }));
    if (s.problem) return grid.replaceChildren(el("p", { className: "hint status error", textContent: s.problem }));
    if (!shown && !s.loading) return grid.replaceChildren(el("p", { className: "hint", textContent: s.filter ? `No ${engine.words.doc}s match the filter.` : `This ${engine.words.collection} is empty.` }));
    grid.replaceChildren(docTable(s.docs, s.collection));
  }

  /** Documents as a table; those of a `collection` (its page) take edits, a console result's only show. */
  function docTable(docs: Doc[], collection: string | null) {
    const columns = engine.columns(docs, collection ?? "");
    const s = stateOf(session());
    const head = el("tr", {}, ...columns.map((c) => el("th", { textContent: c, title: c })));
    const rows = docs.map((d, ri) => {
      const tr = el("tr", {}, ...columns.map((c) => {
        const v = d[c];
        const text = engine.cell(v);
        const td = el("td", { textContent: text ?? "NULL", title: text && text.length > 40 ? text : "" });
        td.className = v === null || text === null ? "null" : v === undefined ? "missing" : /^-?\d+(\.\d+)?$/.test(text) ? "num" : "";
        if (collection) {
          td.ondblclick = () => void editCell(d, c);
          td.oncontextmenu = (e) => host.menu(e, cellMenu(d, c));
        }
        return td;
      }));
      if (collection) {
        tr.classList.toggle("picked", s.row === ri);
        tr.onclick = () => ((s.row = ri), grid.querySelectorAll("tr.picked").forEach((r) => r.classList.remove("picked")), tr.classList.add("picked"));
      }
      return tr;
    });
    return el("table", {}, el("thead", {}, head), el("tbody", {}, ...rows));
  }

  function cellMenu(d: Doc, field: string): MenuItem[] {
    const s = stateOf(session());
    const c = s.collection ?? "";
    const v = d[field];
    const found = engine.identity(d, c);
    const fixed = engine.fixed(field, c);
    const doc = cap(engine.words.doc);
    return [
      ...(found && !fixed ? [["Edit value…", () => void editCell(d, field)] as MenuItem] : []),
      ...(found ? [[`Edit ${engine.words.doc}…`, () => void editDocument(d)] as MenuItem] : []),
      "-",
      ["Copy value", () => void navigator.clipboard.writeText(engine.cell(v) ?? "null").catch(() => {})],
      [`Copy ${engine.words.doc} as JSON`, () => void navigator.clipboard.writeText(JSON.stringify(d, null, 2)).catch(() => {})],
      ...(v !== undefined ? [[`Filter where ${field} is this`, () => ((filterInput.value = engine.filterFor(field, v)), void apply())] as MenuItem] : []),
      "-",
      ...(found && !fixed && v !== undefined ? [[`Remove ${field}…`, () => void change(engine.edits.remove(found, field), `Remove ${field} from this ${engine.words.doc}?`)] as MenuItem] : []),
      ...(found ? [[`Delete ${doc.toLowerCase()}…`, () => void deleteDocument(d)] as MenuItem] : []),
    ];
  }

  /** Makes a change through safe mode, then reads the page again. */
  async function change(edit: Edit, what: string) {
    const name = session();
    const s = stateOf(name);
    if (!s.collection) return;
    try {
      say(await guarded(name, (a) => host.call<string>(engine.editCommand, { name, edit, confirmed: a.confirmed, unlock: a.unlock }), () => host.confirm(what, "Change")), "done");
    } catch (e) {
      if (e !== CANCELLED) say(String(e), "error");
      return;
    }
    s.total = null;
    await loadPage();
  }

  /** A dialog with one text field; resolves to its value, or null when cancelled. */
  const ask: Ask = (heading, label, value, ok, big = false) => {
    const input = big ? el("textarea", { value, spellcheck: false }) : el("input", { value, spellcheck: false });
    const { d, footer } = modal(heading, big ? "wide doc-edit" : "small", el("label", { className: "stack" }, label, input));
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
  };

  async function editCell(d: Doc, field: string) {
    const c = stateOf(session()).collection ?? "";
    const found = engine.identity(d, c);
    if (!found || engine.fixed(field, c)) return found && editDocument(d);
    const v = d[field];
    const typed = await ask(`Edit ${field}`, "Value", v === undefined ? "" : engine.valueText(v), "Save");
    if (typed === null) return;
    await change(engine.edits.set(found, field, engine.typed(v, typed)), `Set ${field} of this ${engine.words.doc}?`);
  }

  async function editDocument(d: Doc) {
    const found = engine.identity(d, stateOf(session()).collection ?? "");
    if (!found) return;
    const text = await ask(`Edit ${engine.words.doc}`, `The whole ${engine.words.doc}; its identity stays`, engine.docText(d), "Save", true);
    if (text !== null) await change(engine.edits.replace(found, text), `Replace this ${engine.words.doc}?`);
  }

  async function addDocument() {
    const c = stateOf(session()).collection;
    if (!c) return;
    const text = await ask(`Add ${article(engine.words.doc)}`, cap(engine.words.doc), engine.newDocText, "Insert", true);
    if (text !== null) await change(engine.edits.insert(c, text), `Insert this ${engine.words.doc}?`);
  }

  async function deleteDocument(d: Doc) {
    const found = engine.identity(d, stateOf(session()).collection ?? "");
    if (!found || !(await host.confirm(`Delete this ${engine.words.doc}? This can't be undone.`, "Delete"))) return;
    await change(engine.edits.delete([found]), `Delete this ${engine.words.doc}?`);
  }

  /** Runs console text the view wrote (create, rename, drop a collection), then lists the collections again. */
  async function script(text: string, what: string, askFirst = false): Promise<boolean> {
    const name = session();
    if (askFirst && !(await host.confirm(what, "Run"))) return false;
    try {
      await guarded(name, (a) => engine.run(call, text, 1, a), () => host.confirm(what, "Run"));
      await host.reload();
      renderCollections();
      const s = stateOf(name);
      if (s.collection && !host.collections().includes(s.collection)) Object.assign(s, { collection: null, docs: [], total: null });
      renderDocs();
      return true;
    } catch (e) {
      if (e !== CANCELLED) say(String(e), "error");
      return false;
    }
  }

  async function newCollection() {
    const made = await engine.newCollection?.(ask, script);
    if (made) await openCollection(made);
  }

  // ---------- Console ----------

  async function runConsole() {
    const name = session();
    const s = stateOf(name);
    if (s.running || !s.script.trim()) return;
    s.running = true;
    say("Running…", "busy");
    renderConsole();
    try {
      const r = await guarded(name, (a) => engine.run(call, s.script, host.limit(), a), () => host.confirm(engine.console.confirm(s.script), "Run"));
      Object.assign(s, { outcomes: r, shown: r.length - 1 });
      say("");
    } catch (e) {
      say(e === CANCELLED ? "Not run." : String(e), e === CANCELLED ? "" : "error");
    } finally {
      s.running = false;
    }
    if (session() !== name) return;
    renderConsole();
    await host.reload(); // the console may have changed the collections, or the documents on screen
    renderCollections();
    if (s.collection) await loadPage();
  }

  function renderConsole() {
    const s = stateOf(session());
    runButton.disabled = s.running;
    [note.textContent, note.className] = [s.note[0], `status ${s.note[1]}`];
    outTabs.replaceChildren(...(s.outcomes.length > 1 ? s.outcomes.map((o, i) => {
      const b = button(o.title, () => ((s.shown = i), renderConsole()));
      b.setAttribute("aria-pressed", String(i === s.shown));
      return b;
    }) : []));
    const o = s.outcomes[s.shown];
    if (!o) return out.replaceChildren(el("pre", { className: "hint", textContent: engine.console.hint }));
    if (o.kind === "done") return out.replaceChildren(el("pre", { textContent: o.text }));
    if (o.kind === "value") return out.replaceChildren(el("pre", { textContent: typeof o.value === "string" ? o.value : JSON.stringify(o.value, null, 2) }));
    const n = o.docs.length;
    const words = `${n.toLocaleString()} ${engine.words.doc}${n === 1 ? "" : "s"}${o.more ? `, the first ${n.toLocaleString()}` : ""}`;
    out.replaceChildren(el("pre", { className: "hint", textContent: words }), ...(n ? [el("div", { className: "doc-grid" }, docTable(o.docs, null))] : []));
  }

  $("side-items").after(side);
  $("panes").after(main);

  return {
    /** Shows the view when the workspace on screen is of this engine; `fresh` reads the collections again. */
    sync(fresh = false) {
      const shown = on();
      const ws = $("workspace").classList;
      ws.toggle(engine.css, shown);
      // Shared by every document view: on while any of them shows. The database window is outside the workspace.
      const any = [...DOC_CSS].some((c) => ws.contains(c));
      ws.toggle("doc-ws", any);
      document.body.classList.toggle("doc-ws-body", any);
      side.style.display = main.style.display = shown ? "" : "none"; // the other document views' stay out
      if (!shown) return;
      const s = stateOf(session());
      if (fresh && s.collection && !host.collections().includes(s.collection)) Object.assign(s, { collection: null, docs: [], total: null });
      void consoleEditor().then((v) => {
        if (v.state.doc.toString() !== s.script) v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: s.script } });
      });
      renderCollections();
      renderDocs();
      renderConsole();
    },
    /** Forgets a closed workspace's page and console. */
    forget: (name: string) => void states.delete(name),
  };
}

/** The classes of the document views, so one view showing doesn't hide another's shared layout. */
const DOC_CSS = new Set<string>();

const cap = (s: string) => s[0].toUpperCase() + s.slice(1);
const article = (noun: string) => `${/^[aeiou]/i.test(noun) ? "an" : "a"} ${noun}`;

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
#workspace:not(.doc-ws) :is(.doc-side, .doc-main) { display: none; }
body.doc-ws-body #dbpicker :is(#db-new, #db-rename, #db-drop) { display: none; }
#workspace.doc-ws :is(#side-items, .side-tabs, #side-search, #side-history) { display: none !important; }
#workspace.doc-ws :is(#panes, #filters, #table-foot) { display: none !important; }
body:has(#workspace.doc-ws) :is(#new-sql, #tools-button, #assistant-button, .tool-icons) { display: none !important; }
.doc-side { display: flex; flex-direction: column; gap: 6px; min-height: 0; flex: 1; }
.doc-find { display: flex; gap: 4px; }
.doc-find input { flex: 1; min-width: 0; }
.doc-colls { list-style: none; margin: 0; padding: 0; overflow: auto; flex: 1; min-height: 0; }
.doc-colls li { display: flex; gap: 6px; align-items: center; padding: 3px 8px; border-radius: 999px; cursor: default; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font: 12px/1.4 var(--mono); }
.doc-colls li:hover { background: var(--hover); }
.doc-colls li.active { background: color-mix(in srgb, var(--accent) 16%, transparent); }
.doc-foot { display: flex; align-items: center; gap: 6px; color: var(--muted); font-size: 12px; }
.doc-main { display: flex; flex-direction: column; min-height: 0; flex: 1; }
.doc-docs { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.doc-docs .bar { display: flex; gap: 6px; align-items: center; padding: 6px 10px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
.doc-docs .bar h3 { margin: 0 6px 0 0; font: 600 13px/1.3 var(--mono); }
.doc-docs .bar input { font: 12px/1.4 var(--mono); min-width: 140px; flex: 1; }
.doc-docs .bar .sort { flex: 0 1 180px; }
.doc-docs .bar .meta { color: var(--muted); font-size: 12px; white-space: nowrap; }
.doc-grid { flex: 1; min-height: 0; overflow: auto; }
.doc-grid table { border-collapse: collapse; font: var(--grid-size, 12px) var(--grid-font, var(--mono)); }
.doc-grid :is(th, td) { padding: 3px 10px; border-bottom: 1px solid var(--grid-line, var(--line)); border-right: 1px solid var(--grid-line, var(--line)); text-align: left; white-space: nowrap; max-width: 320px; overflow: hidden; text-overflow: ellipsis; }
.doc-grid th { position: sticky; top: 0; background: var(--side); font-weight: 600; z-index: 1; }
.doc-grid td.null { color: var(--grid-null, var(--muted)); }
.doc-grid td.missing { background: color-mix(in srgb, var(--muted) 6%, transparent); }
.doc-grid td.num { text-align: right; font-variant-numeric: tabular-nums; }
.doc-grid tr.picked td { background: color-mix(in srgb, var(--accent) 16%, transparent); }
.doc-grid .hint { padding: 12px 16px; }
.doc-console { flex: none; height: 300px; display: flex; flex-direction: column; border-top: 1px solid var(--line); background: var(--side); }
.doc-console .editor { flex: 0 0 110px; min-height: 0; overflow: auto; border-bottom: 1px solid var(--line); }
.doc-console .editor .cm-editor { height: 100%; }
.doc-console .run { display: flex; gap: 6px; align-items: center; padding: 4px 8px; border-bottom: 1px solid var(--line); }
.doc-console .tabs { display: flex; gap: 4px; flex-wrap: wrap; }
.doc-console .tabs button[aria-pressed="true"] { background: color-mix(in srgb, var(--accent) 16%, transparent); }
.doc-console .out { flex: 1; min-height: 0; overflow: auto; }
.doc-console pre { margin: 0; padding: 8px 12px; font: 12px/1.45 var(--mono); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
.doc-edit textarea { width: 100%; box-sizing: border-box; min-height: 320px; font: 12px/1.45 var(--mono); padding: 8px; resize: vertical; }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
