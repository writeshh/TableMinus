// Editing a table's rows in the grid: staged cell edits with type-aware editors, added, duplicated and deleted
// rows, paste, undo and redo, and saving (or previewing) everything as one transaction through save_rows.

import { CANCELLED, guarded, type Approval } from "./safety";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { connName, el } from "./dom";
import {
  Staged, boolValues, changesFor, columnKind, isBlobPlaceholder, nativeValue, parseTsv, pasteCells, same, sameJson,
  type Kind, type Value,
} from "./staging";
import { hexDialog, sqlDialog, valueDialog } from "./cell-editors";
import { indexOf, rowAt } from "./vgrid";
import "./grid-edit.css"; // vite injects it

type Column = { name: string; data_type: string; nullable: boolean; default: string | null; primary: boolean };
type StatusKind = "" | "busy" | "error" | "done";
type MenuItem = [string, () => void] | "-";

/** What this module needs of a table tab (main.ts TableTab has it all). */
export type EditTab = {
  id: number;
  table: string;
  result?: { columns: string[]; rows: (string | null)[][] };
  staged: Staged;
  /** Row identities (ctid / rowid) of the page's rows, when the table has them. */
  ids?: string[];
  message?: [string, StatusKind];
};

export type GridHost = {
  call<T>(cmd: string, args: Record<string, unknown>): Promise<T>;
  conn(): { name: string; kind: string } | null;
  /** The table's columns, once loaded. */
  columns(table: string): Column[] | undefined;
  /** Redraws the current tab. */
  render(): void;
  reload(tab: EditTab): Promise<void>;
  confirm(text: string, okLabel: string): Promise<boolean>;
  /** Writes a value into a grid cell the way the grid shows it. */
  fill(td: HTMLTableCellElement, v: string | null): void;
};

let host: GridHost;
export const initGridEdit = (h: GridHost) => void (host = h);

/** The backend's text for a key matching several rows (Error::SeveralRows). */
const SEVERAL_ROWS = "a key matched more than one row; nothing was saved";

const pageRows = (tab: EditTab) => tab.result?.rows.length ?? 0;
const engine = () => host.conn()?.kind ?? "";
const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** Primary-key column names and their positions in the result, or null when rows can't be identified. */
export function keyOf(tab: EditTab) {
  const names = (host.columns(tab.table) ?? []).filter((c) => c.primary).map((c) => c.name);
  const idx = names.map((n) => tab.result?.columns.indexOf(n) ?? -1);
  return names.length && idx.every((i) => i >= 0) ? { names, idx } : null;
}

const columnOf = (tab: EditTab, ci: number) => host.columns(tab.table)?.find((c) => c.name === tab.result?.columns[ci]);

// Enum values per loaded page; fetched once per page load, so a changed enum shows after a reload.
const enums = new WeakMap<object, Promise<Map<string, string[]>>>();
const enumsNow = new WeakMap<object, Map<string, string[]>>();

function loadEnums(tab: EditTab) {
  const r = tab.result;
  const conn = host.conn();
  if (!r || !conn || enums.has(r)) return enums.get(r!) ?? Promise.resolve(new Map<string, string[]>());
  const p = host
    .call<[string, string[]][]>("enum_values", { name: conn.name, table: tab.table })
    .then((list) => new Map(list))
    .catch(() => new Map<string, string[]>()); // no dropdown is fine: the text editor still works
  enums.set(r, p);
  p.then((m) => {
    enumsNow.set(r, m);
    if (m.size) host.render(); // the row detail can show dropdowns now
  });
  return p;
}

/** The editor a column gets; "enum" once its values are known. */
export function kindOf(tab: EditTab, ci: number): Kind | "enum" {
  const name = tab.result?.columns[ci] ?? "";
  if (tab.result && enumsNow.get(tab.result)?.has(name)) return "enum";
  return columnKind(columnOf(tab, ci)?.data_type ?? "", engine());
}

/** The choices of an enum or boolean column. */
export const choicesOf = (tab: EditTab, ci: number) =>
  kindOf(tab, ci) === "bool" ? boolValues(engine()) : (enumsNow.get(tab.result!)?.get(tab.result!.columns[ci]) ?? []);

/** The cell's value as staged, or as loaded; undefined for an added row's column left to its default. */
export function valueAt(tab: EditTab, ri: number, ci: number): Value | undefined {
  const n = pageRows(tab);
  if (ri >= n) return tab.staged.added[ri - n]?.get(ci);
  const row = tab.staged.edits.get(ri);
  return row?.has(ci) ? row.get(ci)! : tab.result!.rows[ri][ci];
}

/** Why a cell can't be typed into, or null when it can. Binary cells change only by loading a file or NULL. */
export function readOnly(tab: EditTab, ri: number, ci: number): string | null {
  if (!keyOf(tab)) return "no primary key";
  if (tab.staged.deleted.has(ri)) return "the row is deleted";
  if (kindOf(tab, ci) === "binary" || (ri < pageRows(tab) && isBlobPlaceholder(engine(), tab.result!.rows[ri][ci]))) return "binary";
  return null;
}

/** Stages one cell's new value as an undo step (unchanged values are skipped) and redraws. */
export function stage(tab: EditTab, ri: number, ci: number, v: Value) {
  const n = pageRows(tab);
  const original = ri < n ? tab.result!.rows[ri][ci] : null;
  const now = valueAt(tab, ri, ci);
  // Re-indenting JSON isn't a change.
  if (typeof v === "string" && typeof original === "string" && kindOf(tab, ci) === "json" && sameJson(v, original)) v = original;
  if (now !== undefined && same(v, now)) return;
  tab.staged.change(() => tab.staged.set(ri, ci, v, n, original));
  tab.message = undefined;
  host.render();
}

function cellAt(ri: number, ci: number) {
  const body = document.querySelector<HTMLTableSectionElement>("#out tbody");
  return body ? rowAt(body, ri)?.cells[ci + 1] : undefined; // +1: the row number
}

function show(td: HTMLTableCellElement, v: Value) {
  if (v === null || typeof v === "string") return host.fill(td, v);
  td.textContent = `file: ${v.file.split(/[\\/]/).pop()}`;
  td.className = "file";
  td.title = v.file;
}

/** Draws row `ri`'s staged state into its row as the grid builds it: changed values, a deleted row, or (past the
 *  page's rows) an added row, which is drawn whole here. */
export function paintRow(tab: EditTab, tr: HTMLTableRowElement, ri: number) {
  const n = pageRows(tab);
  const s = tab.staged;
  if (ri >= n) {
    const row = s.added[ri - n];
    tr.className = "added";
    tr.append(el("td", { className: "rownum", textContent: "+" }));
    tab.result?.columns.forEach((_, ci) => {
      const td = tr.insertCell();
      if (row?.has(ci)) (show(td, row.get(ci)!), td.classList.add("changed"));
      else (td.textContent = "DEFAULT"), (td.className = "default");
    });
    return;
  }
  if (s.deleted.has(ri)) tr.classList.add("deleted");
  for (const [ci, v] of s.edits.get(ri) ?? []) {
    const td = tr.cells[ci + 1];
    if (td) (show(td, v), td.classList.add("changed"));
  }
}

/** Loads the enum values and wires double-click editing on the rendered page. */
export function decorate(tab: EditTab) {
  const body = document.querySelector<HTMLTableSectionElement>("#out tbody");
  if (!body || !tab.result) return;
  loadEnums(tab);
  if (!keyOf(tab)) return;
  body.ondblclick = (e) => {
    const td = (e.target as HTMLElement).closest("td");
    const ri = indexOf(td?.parentElement as HTMLTableRowElement | null);
    if (td && ri !== null && td.cellIndex > 0 && !td.querySelector("input, select")) editCell(tab, ri, td.cellIndex - 1);
  };
}

const NULL_OPTION = "\u0000null"; // a select value no real enum value can have

/** Edits a cell in place: a dropdown for enums and booleans, a native picker for dates and times, text otherwise.
 *  JSON and multi-line text open Quick Look instead (a one-line input would drop line breaks); binary cells
 *  open the hex view. */
export async function editCell(tab: EditTab, ri: number, ci: number) {
  const why = readOnly(tab, ri, ci);
  if (why === "binary") return showBytes(tab, ri, ci);
  if (why) return;
  await loadEnums(tab);
  const v = valueAt(tab, ri, ci);
  if (v !== null && typeof v === "object") return; // a file to load: undo or load another to change it
  const kind = kindOf(tab, ci);
  if (kind === "json" || /[\r\n]/.test(v ?? "")) return quickLook(tab, ri, ci);
  const td = cellAt(ri, ci);
  if (!td || td.querySelector("input, select")) return;
  const value = v ?? null;
  const nullable = columnOf(tab, ci)?.nullable ?? true;
  let control: HTMLInputElement | HTMLSelectElement;
  let read: () => string | null;
  const native = kind === "date" || kind === "datetime" || kind === "time" ? nativeValue(kind, value) : null;
  if (kind === "enum" || kind === "bool") {
    const choices = choicesOf(tab, ci);
    const options = [...(nullable || value === null ? [NULL_OPTION] : []), ...choices];
    if (value !== null && !choices.includes(value)) options.push(value);
    const select = el("select", { className: "cell-input" }, ...options.map((o) => el("option", { value: o, textContent: o === NULL_OPTION ? "NULL" : o })));
    select.value = value ?? NULL_OPTION;
    read = () => (select.value === NULL_OPTION ? null : select.value);
    control = select;
  } else if (native !== null) {
    const type = kind === "datetime" ? "datetime-local" : kind;
    const input = el("input", { className: "cell-input", type, value: native, step: /\.\d/.test(native) ? "0.001" : "1" });
    read = () => (input.value ? input.value.replace("T", " ") : null); // a cleared picker means NULL
    control = input;
  } else {
    const input = el("input", { className: "cell-input", value: value ?? "", placeholder: v === undefined ? "DEFAULT" : value === null ? "NULL" : "" });
    read = () => input.value;
    control = input;
  }
  control.title = "Enter to keep · Esc to cancel · Tab for next cell · ⌘⌫ for NULL";
  td.replaceChildren(control);
  td.classList.add("editing");
  control.focus();
  if (control instanceof HTMLInputElement && control.type === "text") control.select();
  let touched = false;
  let done = false;
  // undefined = cancel; a value is staged (or unstaged when it equals the original)
  const finish = (next: string | null | undefined, then?: () => void) => {
    if (done) return;
    done = true;
    if (next !== undefined && (touched || next === null)) stage(tab, ri, ci, next);
    host.render();
    then?.();
  };
  control.oninput = () => (touched = true);
  if (control instanceof HTMLSelectElement) control.onchange = () => ((touched = true), finish(read()));
  control.onblur = () => finish(read());
  control.onkeydown = (e) => {
    if (e.key === "Enter") (e.preventDefault(), finish(read()));
    else if (e.key === "Escape") (e.preventDefault(), e.stopPropagation(), finish(undefined));
    else if (e.key === "Backspace" && (e.metaKey || e.ctrlKey)) (e.preventDefault(), finish(null));
    else if (e.key === "Tab") {
      e.preventDefault();
      const next = ci + (e.shiftKey ? -1 : 1);
      finish(read(), () => next >= 0 && next < tab.result!.columns.length && editCell(tab, ri, next));
    }
  };
}

/** The whole value in a window; JSON columns get the JSON tools. */
export async function quickLook(tab: EditTab, ri: number, ci: number) {
  const v = valueAt(tab, ri, ci);
  if (v !== null && typeof v === "object") return;
  const why = readOnly(tab, ri, ci);
  if (why === "binary") return showBytes(tab, ri, ci);
  const column = tab.result!.columns[ci];
  const next = await valueDialog({
    title: `${tab.table}.${column}`,
    value: v ?? null,
    json: kindOf(tab, ci) === "json",
    readOnly: !!why,
    nullable: columnOf(tab, ci)?.nullable ?? true,
  });
  if (next !== undefined) stage(tab, ri, ci, next);
}

/** The hex view of a binary cell: Postgres sends bytea as hex already; other engines are asked for the bytes. */
export async function showBytes(tab: EditTab, ri: number, ci: number) {
  const conn = host.conn();
  const column = tab.result!.columns[ci];
  const v = valueAt(tab, ri, ci);
  const title = `${tab.table}.${column}`;
  if (v !== null && typeof v === "object") return void (await valueDialog({ title, value: `Will be loaded from ${v.file} on save.`, json: false, readOnly: true, nullable: false }));
  if (v === null || v === undefined) return void (await hexDialog(title, "", 0));
  if (v.startsWith("\\x")) return void (await hexDialog(title, v.slice(2), (v.length - 2) / 2));
  const key = keyOf(tab);
  if (!conn || ri >= pageRows(tab) || (!key && !tab.ids)) return;
  const original = tab.result!.rows;
  const row = {
    key: key ? key.names.map((n, i): [string, string] => [n, original[ri][key.idx[i]] ?? ""]) : [],
    ...(key ? {} : { id: tab.ids![ri] }),
  };
  try {
    const bytes = await host.call<{ hex: string; size: number }>("cell_bytes", { name: conn.name, table: tab.table, column, row });
    await hexDialog(title, bytes.hex, bytes.size);
  } catch (e) {
    tab.message = [String(e), "error"];
    host.render();
  }
}

/** Loads a cell's value from a file: its bytes for binary columns (read when saving), its text otherwise. */
export async function loadFromFile(tab: EditTab, ri: number, ci: number) {
  const path = await openFileDialog({ multiple: false, directory: false });
  if (typeof path !== "string") return;
  if (kindOf(tab, ci) === "binary" || readOnly(tab, ri, ci) === "binary") return stage(tab, ri, ci, { file: path });
  try {
    stage(tab, ri, ci, await host.call<string>("read_text_file", { path }));
  } catch (e) {
    tab.message = [String(e), "error"];
    host.render();
  }
}

export function addRow(tab: EditTab) {
  if (!keyOf(tab)) return;
  tab.staged.change(() => tab.staged.added.push(new Map()));
  host.render();
  const out = document.getElementById("out");
  if (out) out.scrollTop = out.scrollHeight; // the new row is last; scrolling draws it
}

/** Adds copies of `rows` as new rows. Keys with a default (serials, auto-increment) and binary values that only
 *  show as placeholders are left out, so the copies get fresh ones. */
export function duplicateRows(tab: EditTab, rows: number[]) {
  const columns = host.columns(tab.table) ?? [];
  const leaveOut = (ci: number) => {
    const c = columnOf(tab, ci);
    const auto = c?.primary && (c.default !== null || (engine() === "sqlite" && /^integer$/i.test(c.data_type) && columns.filter((x) => x.primary).length === 1));
    return auto || (kindOf(tab, ci) === "binary" && engine() !== "postgres");
  };
  const copies = rows.filter((ri) => !tab.staged.deleted.has(ri)).map((ri) => {
    const copy = new Map<number, Value>();
    tab.result!.columns.forEach((_, ci) => {
      const v = valueAt(tab, ri, ci);
      if (v !== undefined && !leaveOut(ci) && !isBlobPlaceholder(engine(), v)) copy.set(ci, v);
    });
    return copy;
  });
  if (!copies.length) return;
  tab.staged.change(() => tab.staged.added.push(...copies));
  host.render();
}

/** Deletes rows (or restores them when they are all deleted already); added rows just go away. */
export function deleteRows(tab: EditTab, rows: number[]) {
  if (!keyOf(tab) || !rows.length) return;
  tab.staged.change(() => tab.staged.remove(rows, pageRows(tab)));
  host.render();
}

/** Pastes tab-separated text into the selection: one value fills it, a block starts at its top-left cell. Cells
 *  that can't be typed into are skipped. */
export function paste(tab: EditTab, block: { r0: number; c0: number; r1: number; c1: number }, text: string) {
  if (!keyOf(tab) || !tab.result) return;
  const n = pageRows(tab);
  const cells = pasteCells(parseTsv(text), block, n + tab.staged.added.length, tab.result.columns.length)
    .filter(([ri, ci]) => !readOnly(tab, ri, ci));
  if (!cells.length) return;
  tab.staged.change(() => {
    for (const [ri, ci, v] of cells) tab.staged.set(ri, ci, v, n, ri < n ? tab.result!.rows[ri][ci] : null);
  });
  tab.message = [`Pasted ${plural(cells.length, "cell")}`, "done"];
  host.render();
}

export function undo(tab: EditTab) {
  if (tab.staged.undo()) host.render();
}

export function redo(tab: EditTab) {
  if (tab.staged.redo()) host.render();
}

function changes(tab: EditTab, byIdentity: boolean) {
  return changesFor(tab.staged, tab.result!.columns, tab.result!.rows, keyOf(tab)!, byIdentity ? tab.ids : undefined);
}

/** Saves every staged change in one transaction. Safe mode asks first; a key that matches several rows can be
 *  saved again with row identities instead. */
export async function save(tab: EditTab, confirmed = false, byIdentity = false): Promise<void> {
  const conn = host.conn();
  const n = tab.staged.count;
  if (!conn || !n || !tab.result) return;
  try {
    // Safe mode's question, password or Touch ID, as the connection's level asks; `confirmed` skips the question
    // when the statements were just shown in the preview.
    const send = (a: Approval) =>
      host.call<number>("save_rows", {
        name: conn.name, table: tab.table, changes: changes(tab, byIdentity), confirmed: confirmed || a.confirmed, unlock: a.unlock,
      });
    const rows = await guarded(conn.name, send, () => host.confirm(`Save ${plural(n, "change")} to ${tab.table} on "${connName(conn.name)}"?`, "Save"));
    tab.staged.clear();
    tab.message = [`Saved ${plural(n, "change")} (${plural(rows, "row")})`, "done"];
    await host.reload(tab);
  } catch (e) {
    if (e === CANCELLED) return;
    if (e === SEVERAL_ROWS && tab.ids && !byIdentity) {
      const id = conn.kind === "postgres" ? "ctid" : "rowid";
      const again = await host.confirm(
        `A key matched more than one row, so nothing was saved. Save again, finding each row by its row identity (${id}) instead?`,
        "Save Again with Row Identity",
      );
      if (again) return save(tab, confirmed, true);
    }
    tab.message = [String(e), "error"]; // edits stay staged so nothing typed is lost
    host.render();
  }
}

/** Shows the statements a save would run, as the backend generates them; saving from there skips safe mode's
 *  question, since the statements were just shown. */
export async function preview(tab: EditTab) {
  const conn = host.conn();
  if (!conn || !tab.staged.count || !tab.result) return;
  try {
    const statements = await host.call<string[]>("rows_sql", { name: conn.name, table: tab.table, changes: changes(tab, false) });
    if (await sqlDialog(`Changes to ${tab.table}`, statements, `"${connName(conn.name)}"`)) await save(tab, true);
  } catch (e) {
    tab.message = [String(e), "error"];
    host.render();
  }
}

/** The grid's right-click items for editing: cell tools, then row operations on the selected rows. */
export function cellMenu(tab: EditTab, ri: number, ci: number, rows: number[]): MenuItem[] {
  if (!tab.result) return [];
  const kind = kindOf(tab, ci);
  const why = readOnly(tab, ri, ci);
  const items: MenuItem[] = ["-"];
  if (why === "binary") items.push(["View as hex…", () => showBytes(tab, ri, ci)]);
  else items.push([kind === "json" ? "Edit JSON…" : "Open in Quick Look…", () => quickLook(tab, ri, ci)]);
  if (!keyOf(tab)) return items;
  if (!tab.staged.deleted.has(ri)) {
    if (valueAt(tab, ri, ci) !== null && (columnOf(tab, ci)?.nullable ?? true)) items.push(["Set NULL", () => stage(tab, ri, ci, null)]);
    items.push(["Load value from file…", () => loadFromFile(tab, ri, ci)]);
  }
  const all = rows.length ? rows : [ri];
  const restore = all.every((r) => r >= pageRows(tab) || tab.staged.deleted.has(r)) && all.some((r) => r < pageRows(tab));
  const many = all.length > 1 ? `${all.length} rows` : "row";
  items.push(
    "-",
    ["Add row", () => addRow(tab)],
    [`Duplicate ${many}`, () => duplicateRows(tab, all)],
    [`${restore ? "Restore" : "Delete"} ${many}`, () => deleteRows(tab, all)],
  );
  return items;
}
