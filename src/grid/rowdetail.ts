// The right sidebar's row detail (Space): every field of the selected row as a form, with search. Changes are
// staged exactly like grid edits (grid-edit.ts), so they count, undo and save with them.

import { $, el } from "../dom";
import { sameJson } from "./staging";
import { choicesOf, kindOf, readOnly, stage, valueAt, type EditTab } from "./grid-edit";

let open = false;
/** What the panel shows now, so a redraw for the same row only refreshes values and keeps focus. */
let shown: { tab: EditTab; ri: number; result: object; kinds: string; fields: Map<number, HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement> } | null = null;
let last: { tab: EditTab | null; ri: number | null } = { tab: null, ri: null };

const NULL_OPTION = "\u0000null";
const panel = () => $("row-detail");

/** Switches the sidebar on or off (it shows when a table row is selected), with the toolbar button's pressed look. */
export function toggleRowDetail() {
  open = !open;
  document.getElementById("row-detail-button")?.setAttribute("aria-pressed", String(open));
  shown = null;
  syncRowDetail(last.tab, last.ri);
}

/** The text a field shows: JSON pretty-printed. */
function shownText(tab: EditTab, ci: number, v: string | null) {
  if (v === null || kindOf(tab, ci) !== "json") return v ?? "";
  try {
    return JSON.stringify(JSON.parse(v), null, 2);
  } catch {
    return v;
  }
}

/** Shows row `ri` of `tab` (a table tab's data), or hides the panel when there is nothing to show. */
export function syncRowDetail(tab: EditTab | null, ri: number | null) {
  last = { tab, ri };
  const p = panel();
  p.hidden = !open || !tab?.result;
  if (p.hidden || !tab?.result) return void (shown = null);
  const n = tab.result.rows.length + tab.staged.added.length;
  if (ri === null || ri >= n) {
    shown = null;
    return p.replaceChildren(head("No row selected"), el("p", { className: "hint", textContent: "Select a row to see its fields." }));
  }
  // Kinds change once enum values arrive, and a field then becomes a dropdown.
  const kinds = tab.result.columns.map((_, ci) => kindOf(tab, ci)).join();
  if (shown?.tab === tab && shown.ri === ri && shown.result === tab.result && shown.kinds === kinds) return refresh();
  const search = el("input", { type: "search", placeholder: "Search fields" });
  const list = el("div", { className: "fields" });
  const fields = new Map<number, HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>();
  tab.result.columns.forEach((name, ci) => {
    const control = field(tab, ri, ci);
    fields.set(ci, control);
    const kind = kindOf(tab, ci);
    const label = el("label", { className: "field" }, el("span", { className: "name", textContent: name }), el("span", { className: "type", textContent: kind === "text" ? "" : kind }), control);
    label.dataset.name = name.toLowerCase();
    list.append(label);
  });
  search.oninput = () => {
    const q = search.value.toLowerCase();
    for (const l of list.children as HTMLCollectionOf<HTMLElement>) l.hidden = !l.dataset.name!.includes(q);
  };
  const title = ri < tab.result.rows.length ? `Row ${ri + 1}` : "New row";
  p.replaceChildren(head(title), search, list);
  shown = { tab, ri, result: tab.result, kinds, fields };
  refresh();
}

function head(title: string) {
  const close = el("button", { className: "ghost", textContent: "×", title: "Close (Space)" });
  close.onclick = toggleRowDetail;
  return el("header", {}, el("strong", { textContent: title }), el("span", { className: "spacer" }), close);
}

function field(tab: EditTab, ri: number, ci: number) {
  const kind = kindOf(tab, ci);
  let control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
  if (kind === "enum" || kind === "bool") {
    const v = valueAt(tab, ri, ci);
    const choices = choicesOf(tab, ci);
    const extra = typeof v === "string" && !choices.includes(v) ? [v] : [];
    control = el("select", {}, ...[NULL_OPTION, ...choices, ...extra].map((o) => el("option", { value: o, textContent: o === NULL_OPTION ? "NULL" : o })));
  } else if (kind === "json" || kind === "text") {
    control = el("textarea", { rows: kind === "json" ? 4 : 1, spellcheck: false });
  } else control = el("input", {});
  const commit = () => {
    const v = control instanceof HTMLSelectElement && control.value === NULL_OPTION ? null : control.value;
    const now = valueAt(tab, ri, ci);
    if (typeof v === "string" && typeof now === "string" && kind === "json" && sameJson(v, now)) return;
    if (v === "" && (now === null || now === undefined) && !(control instanceof HTMLSelectElement)) return; // left empty
    stage(tab, ri, ci, v);
  };
  control.onchange = commit;
  control.onkeydown = (e) => {
    const ke = e as KeyboardEvent;
    if (ke.key === "Backspace" && (ke.metaKey || ke.ctrlKey)) (ke.preventDefault(), stage(tab, ri, ci, null));
    else if (ke.key === "Enter" && !(control instanceof HTMLTextAreaElement && kind === "json") && !ke.shiftKey) (ke.preventDefault(), control.blur());
    else if (ke.key === "Escape") (ke.stopPropagation(), (shown = null), syncRowDetail(last.tab, last.ri)); // back to the staged value
  };
  return control;
}

/** Puts the current (staged or loaded) values into the fields, except the one being typed in. */
function refresh() {
  if (!shown) return;
  const { tab, ri, fields } = shown;
  for (const [ci, control] of fields) {
    const v = valueAt(tab, ri, ci);
    const why = readOnly(tab, ri, ci);
    const staged = ri >= tab.result!.rows.length ? v !== undefined : !!tab.staged.edits.get(ri)?.has(ci);
    control.classList.toggle("changed", staged);
    control.disabled = !!why && why !== "binary";
    if (control instanceof HTMLInputElement || control instanceof HTMLTextAreaElement) control.readOnly = why === "binary";
    if (document.activeElement === control) continue;
    if (control instanceof HTMLSelectElement) control.value = typeof v === "string" ? v : NULL_OPTION;
    else if (v !== null && typeof v === "object") control.value = `file: ${v.file}`;
    else {
      control.value = shownText(tab, ci, v ?? null);
      control.placeholder = v === undefined ? "DEFAULT" : v === null ? "NULL" : "";
    }
  }
}
