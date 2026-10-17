// Custom scripts (Tools › Run Custom Script…): JavaScript that queries the current connection. The script runs in a
// Web Worker (script-worker.ts), which can't reach the page or the app's backend; its `query` and `execute` come
// back here and run through `run_query`, so safe mode asks as it does for the editor. Stop ends the worker at once.

import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { invoke } from "./ipc";
import { EditorView, basicSetup } from "codemirror";
import { EditorState, Prec } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { indentWithTab } from "@codemirror/commands";
import { el } from "./dom";
import { modal } from "./backup";
import { editorLook } from "./editor";
import { readPref, writePref } from "./prefs";
import { CANCELLED, guarded } from "./safety";
import { tableOf } from "./script-table";

type Result = { columns: string[]; rows: (string | null)[][]; affected: number; truncated: boolean };
type FromWorker =
  | { type: "query"; id: number; sql: string }
  | { type: "print"; text: string; error?: boolean }
  | { type: "table"; rows: unknown }
  | { type: "done" }
  | { type: "error"; text: string };

export type ScriptCtx = {
  /** The on-screen connection: `name` is the session key commands take. */
  conn: () => { name: string; kind: string } | null;
  database: () => string | null;
  call: <T>(cmd: string, args: Record<string, unknown>) => Promise<T>;
  /** Safe mode's question for the Alert levels. */
  confirm: (sql: string) => Promise<boolean>;
};

const PREF = "script.text";
/** Rows one query hands the script. */
const MAX_ROWS = 100_000;
/** Output lines kept; a script printing in a loop would otherwise grow the page without end. */
const MAX_LINES = 5_000;
const FILES = [{ name: "JavaScript", extensions: ["js"] }];

const EXAMPLE = `// query(sql) gives the rows as objects; execute(sql) every result as the editor gets it.
// print(...values) and table(rows) show output. \`connection\` says where this runs.
const rows = await query("select 1 as one, 2 as two");
table(rows);
print(\`\${rows.length} row(s) on \${connection.name}\`);
`;

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

export async function openScript(ctx: ScriptCtx) {
  const c = ctx.conn();
  if (!c) return;
  const { javascript } = await import("@codemirror/lang-javascript"); // only once a script window opens
  const host = el("div", { className: "script-editor" });
  const output = el("div", { className: "script-output" });
  output.setAttribute("aria-live", "polite");
  const where = el("span", { className: "hint inline" });
  const { d, footer, setStatus } = modal("Custom script", "wide script-dialog", where, host, output);
  where.textContent = `Runs on ${c.name.split("\u001f")[0]}${ctx.database() ? ` · ${ctx.database()}` : ""}. Queries go through safe mode like the editor's.`;

  let path: string | null = null;
  const view = new EditorView({
    parent: host,
    state: EditorState.create({
      doc: readPref<string>(PREF) ?? EXAMPLE,
      extensions: [
        // Ahead of basicSetup's own Mod-Enter (insert a blank line).
        Prec.highest(keymap.of([{ key: "Mod-Enter", run: () => (void run(), true) }])),
        basicSetup, keymap.of([indentWithTab]), javascript(), editorLook,
        EditorView.updateListener.of((u) => u.docChanged && writePref(PREF, u.state.doc.toString())),
      ],
    }),
  });

  let worker: Worker | null = null;
  let lines = 0;
  const say = (node: HTMLElement) => {
    if (lines++ === MAX_LINES) output.append(el("p", { className: "status error", textContent: `Output stops after ${MAX_LINES.toLocaleString()} lines.` }));
    if (lines > MAX_LINES) return;
    const atBottom = output.scrollTop + output.clientHeight >= output.scrollHeight - 4;
    output.append(node);
    if (atBottom) output.scrollTop = output.scrollHeight;
  };
  const showTable = (rows: unknown) => {
    const t = tableOf(rows);
    const table = el("table", {}, el("tr", {}, ...t.columns.map((name) => el("th", { textContent: name }))));
    for (const r of t.cells) table.append(el("tr", {}, ...r.map((v) => el("td", { textContent: v, title: v }))));
    say(el("div", { className: "script-table" }, table, ...(t.more ? [el("p", { className: "hint", textContent: `…and ${t.more.toLocaleString()} more rows` })] : [])));
  };

  function stop(note: string, kind: "" | "error" | "done" = "") {
    worker?.terminate();
    worker = null;
    runButton.disabled = false;
    stopButton.disabled = true;
    setStatus(note, kind);
  }

  async function answer(w: Worker, id: number, sql: string) {
    const conn = ctx.conn();
    try {
      if (!conn || conn.name !== c!.name) throw "the workspace changed; run the script again";
      const ran = await guarded(conn.name, (a) => ctx.call<{ results: Result[] }>("run_query", {
        name: conn.name, sql, confirmed: a.confirmed, unlock: a.unlock, maxRows: MAX_ROWS,
      }), () => ctx.confirm(sql));
      w.postMessage({ type: "reply", id, value: ran.results });
    } catch (e) {
      w.postMessage({ type: "reply", id, error: e === CANCELLED ? "Cancelled in safe mode" : String(e) });
    }
  }

  async function run() {
    if (worker) return;
    output.replaceChildren();
    lines = 0;
    const started = performance.now();
    const w = new Worker(new URL("./script-worker.ts", import.meta.url), { type: "module" });
    worker = w;
    runButton.disabled = true;
    stopButton.disabled = false;
    setStatus("Running…", "busy");
    w.onmessage = (e: MessageEvent<FromWorker>) => {
      if (worker !== w) return; // stopped meanwhile
      const m = e.data;
      if (m.type === "query") void answer(w, m.id, m.sql);
      else if (m.type === "print") say(el("pre", { className: m.error ? "error" : "", textContent: m.text }));
      else if (m.type === "table") showTable(m.rows);
      else if (m.type === "done") stop(`Done in ${((performance.now() - started) / 1000).toFixed(1)} s`, "done");
      else {
        say(el("pre", { className: "error", textContent: m.text }));
        stop("The script failed", "error");
      }
    };
    w.onerror = (e) => (say(el("pre", { className: "error", textContent: e.message })), stop("The script failed", "error"));
    w.postMessage({ type: "run", code: view.state.doc.toString(), connection: { name: c!.name.split("\u001f")[0], kind: c!.kind, database: ctx.database() } });
  }

  async function openFile() {
    const picked = await openDialog({ filters: FILES });
    if (typeof picked !== "string") return;
    try {
      const text = await invoke<string>("read_text_file", { path: picked });
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
      path = picked;
      setStatus(`Opened ${fileName(picked)}`);
    } catch (e) {
      setStatus(String(e), "error");
    }
  }

  async function saveFile() {
    const target = path ?? (await saveDialog({ filters: FILES, defaultPath: "script.js" }));
    if (!target) return;
    try {
      await invoke("write_text_file", { path: target, text: view.state.doc.toString() });
      path = target;
      setStatus(`Saved ${fileName(target)}`, "done");
    } catch (e) {
      setStatus(String(e), "error");
    }
  }

  const runButton = el("button", { className: "primary", textContent: "Run", title: "Run the script (⌘↩)" });
  const stopButton = el("button", { textContent: "Stop", disabled: true });
  runButton.onclick = () => void run();
  stopButton.onclick = () => stop("Stopped");
  const clearButton = el("button", { className: "ghost", textContent: "Clear output" });
  clearButton.onclick = () => ((lines = 0), output.replaceChildren());
  const openButton = el("button", { className: "ghost", textContent: "Open…" });
  openButton.onclick = () => void openFile();
  const saveButton = el("button", { className: "ghost", textContent: "Save…" });
  saveButton.onclick = () => void saveFile();
  const close = el("button", { textContent: "Close" });
  close.onclick = () => d.close();
  footer.append(openButton, saveButton, clearButton, el("span", { className: "spacer" }), stopButton, runButton, close);
  d.addEventListener("close", () => {
    worker?.terminate();
    view.destroy();
  });
  d.showModal();
  view.focus();
}
