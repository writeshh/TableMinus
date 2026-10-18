// Sidebar items other than tables: the function and procedure editor (Ref: FunctionViewController), the sequence
// editor (Ref: SequenceController), New view / materialized view / function / procedure, dropping an item, and Copy
// Script As. Like the table operations in structure.ts, every change is shown as SQL first and runs through
// `run_ddl`, so the preview is the confirmation and safe mode still applies.
import { el } from "./dom";
import { miniEditor } from "./editor";
import { modal as windowModal } from "./backup";
import { CANCELLED } from "./safety";
import { ask, preview, runDdl, structureHost as host } from "./structure";

export type ItemKind = "schema" | "view" | "matview" | "function" | "procedure" | "sequence" | "type";
export type Script = "create" | "drop" | "select" | "select_top" | "insert" | "update" | "delete";
type MenuItem = [string, () => void] | "-";
type Sequence = {
  last_value: string | null; increment: string; min: string; max: string; start: string; cache: string; cycle: boolean;
  comment: string | null;
};

const conn = () => host().conn();
const kind = () => (conn()?.kind === "mariadb" ? "mysql" : conn()?.kind ?? "postgres");
const call = <T>(cmd: string, args: Record<string, unknown>) => host().call<T>(cmd, { name: conn()!.name, ...args });
const failed = (e: unknown) => e !== CANCELLED && host().note(String(e), "error");

/** Copy Script As: the script for a table or view (`item` kind null) or another item, onto the clipboard. */
export async function copyScript(itemKind: ItemKind | null, item: string, script: Script) {
  try {
    const sql = await call<string>("item_script", { kind: itemKind, item, script });
    await navigator.clipboard.writeText(sql);
    host().note(`Copied ${script.replace("_", " ").toUpperCase()} script`, "done");
  } catch (e) {
    failed(e);
  }
}

/** The Copy Script As choices for an item, as a second menu. */
export function scriptMenu(itemKind: ItemKind | null, item: string): MenuItem[] {
  const all: [Script, string][] = [["create", "CREATE"], ["drop", "DROP"], ["select", "SELECT"], ["insert", "INSERT"], ["update", "UPDATE"], ["delete", "DELETE"]];
  const table = itemKind === null || itemKind === "view";
  return all.filter(([s]) => table || s === "create" || s === "drop").map(([s, label]) => [label, () => copyScript(table ? null : itemKind, item, s)]);
}

/** Previews dropping an item (cascade on Postgres when asked) and runs it. */
export async function dropItem(itemKind: ItemKind, item: string) {
  const pg = kind() === "postgres";
  const v = pg ? await ask(`Drop ${item}`, [{ key: "cascade", label: "Cascade: also drop what depends on it", value: false }], "Drop…") : {};
  if (!v) return;
  try {
    const statements = await call<string[]>("drop_item_sql", { kind: itemKind, item, cascade: !!v.cascade });
    if (!(await preview(`Drop ${item}`, statements, "Drop", true))) return;
    await runDdl(conn()!.name, statements);
    host().note(`Dropped ${item}`, "done");
    await host().refreshTables();
  } catch (e) {
    failed(e);
  }
}

/** A name for a new item, in the schema shown on Postgres. Null when cancelled. */
async function askName(what: string, schema: string | null) {
  const v = await ask(`New ${what}`, [{ key: "name", label: "Name", value: "" }], "Next…");
  const name = String(v?.name ?? "");
  if (!name) return null;
  return kind() === "postgres" && !name.includes(".") ? `${schema ?? "public"}.${name}` : name;
}

/** A window with a SQL editor and Save…; `save` previews and runs the text, resolving true when it ran. */
function definitionWindow(title: string, text: string, save: (text: string, setStatus: (t: string, k: "" | "error") => void) => Promise<boolean>,
  extra?: HTMLElement) {
  const box = el("div", { className: "definition-editor tall" });
  const { d, footer, setStatus } = windowModal(title, "structure-dialog wide definition-window", ...(extra ? [extra] : []), box);
  const cancel = el("button", { type: "button", textContent: "Close" });
  const ok = el("button", { type: "button", className: "primary", textContent: "Save…" });
  footer.append(el("span", { className: "spacer" }), cancel, ok);
  let doc = text;
  const editor = miniEditor(box, text, conn()?.kind ?? "", (t) => (doc = t));
  cancel.onclick = () => d.close();
  ok.onclick = async () => {
    setStatus("", "");
    try {
      if (await save(doc, setStatus)) d.close();
    } catch (e) {
      if (e !== CANCELLED) setStatus(String(e), "error"); // the edited text stays in the window
    }
  };
  d.showModal();
  editor.focus();
  return { d, editor, setText: (t: string) => editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: (doc = t) } }) };
}

/** The function editor: a function's or procedure's CREATE statement, saved as written. On Postgres each overload
 *  of the name has its own; on MySQL the routine is dropped and created again, and put back if that fails. */
export async function editRoutine(itemKind: "function" | "procedure", item: string, create = false) {
  let defs: string[];
  try {
    defs = create ? [await call<string>("new_routine", { kind: itemKind, item })] : await call<string[]>("routine_definitions", { kind: itemKind, item });
  } catch (e) {
    return failed(e);
  }
  const edited = [...defs];
  let at = 0;
  const pick = el("select", { title: "Overload" }, ...defs.map((d, i) => el("option", { value: String(i), textContent: d.split("\n")[0] })));
  const w = definitionWindow(create ? `New ${itemKind} ${item}` : `${itemKind === "function" ? "Function" : "Procedure"} ${item}`, defs[0],
    async (text) => {
      edited[at] = text;
      const statements = await call<string[]>("save_routine_sql", { kind: itemKind, item, definition: text, create });
      if (!(await preview(`${create ? "Create" : "Change"} ${item}`, statements, create ? "Create" : "Apply", false))) return false;
      await runDdl(conn()!.name, statements, kind() === "mysql" && !create ? [defs[at]] : undefined);
      host().note(`${create ? "Created" : "Changed"} ${item}`, "done");
      await host().refreshTables();
      return true;
    }, defs.length > 1 ? el("label", { className: "field" }, `${defs.length} overloads `, pick) : undefined);
  pick.onchange = () => {
    edited[at] = w.editor.state.doc.toString();
    at = Number(pick.value);
    w.setText(edited[at]);
  };
}

/** New view or materialized view: a name, then its SELECT. */
export async function newView(matview: boolean, schema: string | null) {
  const item = await askName(matview ? "materialized view" : "view", schema);
  if (!item) return;
  definitionWindow(`New ${matview ? "materialized view" : "view"} ${item}`, "SELECT\n  1 AS one", async (body) => {
    const statements = await call<string[]>("save_view_sql", { view: item, matview, body, create: true });
    if (!(await preview(`Create ${item}`, statements, "Create", false))) return false;
    await runDdl(conn()!.name, statements);
    host().note(`Created ${item}`, "done");
    await host().refreshTables();
    return true;
  });
}

/** New function or procedure: a name, then a starting definition to edit. */
export async function newRoutine(itemKind: "function" | "procedure", schema: string | null) {
  const item = await askName(itemKind, schema);
  if (item) editRoutine(itemKind, item, true);
}

/** The sequence editor (Postgres): current value, increment, bounds, cache, cycle and comment. */
export async function editSequence(item: string) {
  let cur: Sequence;
  try {
    cur = await call<Sequence>("sequence", { sequence: item });
  } catch (e) {
    return failed(e);
  }
  const fields: [keyof Sequence, string][] = [["last_value", "Current value"], ["increment", "Increment"], ["min", "Min"], ["max", "Max"], ["start", "Start"], ["cache", "Cache size"]];
  const inputs = fields.map(([k, label]) => {
    const i = el("input", { value: String(cur[k] ?? ""), spellcheck: false, inputMode: "numeric" });
    if (k === "last_value") i.placeholder = "Not used yet";
    return [k, el("label", { className: "field" }, label, i), i] as const;
  });
  const cycle = el("input", { type: "checkbox", checked: cur.cycle });
  const comment = el("input", { value: cur.comment ?? "", spellcheck: false });
  const form = el("form", { className: "sequence-form" }, ...inputs.map(([, l]) => l), el("label", { className: "check" }, cycle, " Cycle when the end is reached"),
    el("label", { className: "field" }, "Comment", comment));
  const { d, footer, setStatus } = windowModal(`Sequence ${item}`, "structure-dialog", form);
  const close = el("button", { type: "button", textContent: "Close" });
  const ok = el("button", { type: "button", className: "primary", textContent: "Save…" });
  footer.append(el("span", { className: "spacer" }), close, ok);
  close.onclick = () => d.close();
  form.onsubmit = (e) => (e.preventDefault(), ok.click());
  ok.onclick = async () => {
    const want: Sequence = { ...cur, cycle: cycle.checked, comment: comment.value.trim() || null };
    for (const [k, , i] of inputs) (want as Record<string, unknown>)[k] = k === "last_value" ? i.value.trim() || null : i.value.trim();
    try {
      const statements = await call<string[]>("save_sequence_sql", { sequence: item, want });
      if (!statements.length) return setStatus("No changes to save.", "");
      if (!(await preview(`Change ${item}`, statements, "Apply", false))) return;
      await runDdl(conn()!.name, statements);
      host().note(`Changed ${item}`, "done");
      d.close();
    } catch (e) {
      if (e !== CANCELLED) setStatus(String(e), "error");
    }
  };
  d.showModal();
}

/** Opens an item's editor, where it has one. */
export function openItem(itemKind: ItemKind, item: string) {
  if (itemKind === "function" || itemKind === "procedure") editRoutine(itemKind, item);
  else if (itemKind === "sequence" && kind() === "postgres") editSequence(item);
}

/** Whether an item opens in an editor (a click on its sidebar row). */
export const editable = (itemKind: ItemKind) =>
  itemKind === "function" || itemKind === "procedure" || (itemKind === "sequence" && kind() === "postgres");

/** The sidebar's "New" entries beyond tables and schemas, for the schema shown. */
export function newMenu(schema: string | null): MenuItem[] {
  const k = kind();
  return [
    ["New view…", () => newView(false, schema)],
    ...(k === "postgres" ? [["New materialized view…", () => newView(true, schema)] as MenuItem] : []),
    ...(k !== "sqlite" && k !== "duckdb" && k !== "mssql" ? [["New function…", () => newRoutine("function", schema)], ["New procedure…", () => newRoutine("procedure", schema)]] as MenuItem[] : []),
  ];
}
