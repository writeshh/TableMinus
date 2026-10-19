// The SQL viewer: a window for reading and editing a .sql file without a connection (File › New SQL Viewer, or a
// .sql file opened from Finder). It has no database to run against, so it only opens, edits and saves text.

import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import type { EditorView } from "codemirror";
import { invoke } from "../ipc";
import { el } from "../dom";
import { miniEditor } from "./editor";

const SQL_FILES = [{ name: "SQL", extensions: ["sql"] }, { name: "All files", extensions: ["*"] }];
const DIALECTS: [string, string][] = [["standard", "Standard SQL"], ["postgres", "PostgreSQL"], ["mysql", "MySQL"], ["mariadb", "MariaDB"], ["sqlite", "SQLite"]];
const fileName = (path: string) => path.split(/[\\/]/).pop() || path;

type Confirm = (text: string, okLabel: string) => Promise<boolean>;

let dialog: HTMLDialogElement | undefined;
let view: EditorView | undefined;
let path: string | undefined;
let dirty = false;
let ask: Confirm = async () => true;
let title: HTMLElement;
let message: HTMLElement;
let body: HTMLElement;
let dialectPick: HTMLSelectElement;

function setStatus(text: string, error = false) {
  message.textContent = text;
  message.className = `status ${error ? "error" : "done"}`;
}

function showTitle() {
  title.textContent = `${path ? fileName(path) : "Untitled"}${dirty ? " ●" : ""}`;
  title.title = path ?? "";
}

/** Puts `text` in a fresh editor (a new undo history, the dialect picked). */
function load(text: string) {
  view?.destroy();
  body.replaceChildren();
  dirty = false;
  view = miniEditor(body, text, dialectPick.value, () => ((dirty = true), showTitle()));
  showTitle();
}

/** Whether it's fine to drop what's in the editor: nothing unsaved, or the user says so. */
async function mayDiscard() {
  return !dirty || ask("This file has unsaved changes.", "Discard");
}

async function openFile(chosen?: string) {
  const file = chosen ?? (await openFileDialog({ multiple: false, directory: false, filters: SQL_FILES }));
  if (typeof file !== "string" || !(await mayDiscard())) return;
  try {
    const text = await invoke<string>("read_text_file", { path: file });
    path = file;
    load(text);
    setStatus("");
  } catch (e) {
    setStatus(String(e), true);
  }
}

async function save(pick: boolean) {
  if (!view) return;
  let target = path;
  if (pick || !target) {
    const chosen = await saveFileDialog({ defaultPath: target ?? "query.sql", filters: SQL_FILES });
    if (!chosen) return;
    target = chosen;
  }
  try {
    await invoke("write_text_file", { path: target, text: view.state.doc.toString() });
    path = target;
    dirty = false;
    showTitle();
    setStatus(`Saved ${fileName(target)}`);
  } catch (e) {
    setStatus(String(e), true);
  }
}

function build(): HTMLDialogElement {
  title = el("strong", { className: "viewer-title" });
  message = el("span", { className: "status" });
  body = el("div", { className: "viewer-body" });
  dialectPick = el("select", { title: "Highlighting" }, ...DIALECTS.map(([value, label]) => el("option", { value, textContent: label })));
  const button = (text: string, run: () => void, extra = "") => {
    const b = el("button", { type: "button", textContent: text, className: extra });
    b.onclick = run;
    return b;
  };
  const close = async () => (await mayDiscard()) && d.close();
  const d = el("dialog", { className: "viewer" },
    el("header", {}, title, el("span", { className: "spacer" }), dialectPick,
      button("Open…", () => openFile()), button("Save", () => save(false), "primary"), button("Save As…", () => save(true)), button("Close", close)),
    body, el("footer", {}, message));
  dialectPick.onchange = () => {
    if (!view) return;
    const unsaved = dirty;
    load(view.state.doc.toString()); // a new editor takes the dialect; the text stays (the undo history doesn't)
    dirty = unsaved;
    showTitle();
  };
  // Esc asks first when there are unsaved changes, like Close.
  d.oncancel = (e) => (e.preventDefault(), void close());
  d.onclose = () => (view?.destroy(), (view = undefined), (path = undefined), (dirty = false));
  document.body.append(d);
  return d;
}

/** Opens the viewer on `file`, or empty. `confirm` asks before unsaved text is dropped. */
export async function openViewer(confirm: Confirm, file?: string) {
  ask = confirm;
  dialog ??= build();
  if (!dialog.open) {
    path = undefined;
    setStatus("");
    load("");
    dialog.showModal();
  }
  if (file) await openFile(file);
  else view?.focus();
}
