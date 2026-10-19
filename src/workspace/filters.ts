// The filter bar for table tabs: rows of column · operator · value, combined with AND or OR. The values go to
// the backend's `browse` command, which binds them; nothing here builds SQL (the SQL button asks the backend to show
// it). New rows follow the filter defaults (Settings › General).
import { el } from "../dom";

export type Op =
  | "eq" | "ne" | "lt" | "le" | "gt" | "ge"
  | "contains" | "not_contains" | "starts_with" | "ends_with"
  | "in" | "not_in" | "between" | "not_between"
  | "is_null" | "not_null" | "is_empty" | "not_empty" | "raw";

export type FilterRow = { on: boolean; column: string | null; op: Op; value: string; value2: string };
export type FilterState = { filters: FilterRow[]; combine: "and" | "or" };

export const OPS: [Op, string][] = [
  ["eq", "="], ["ne", "≠"], ["lt", "<"], ["le", "≤"], ["gt", ">"], ["ge", "≥"],
  ["contains", "contains"], ["not_contains", "doesn't contain"], ["starts_with", "starts with"], ["ends_with", "ends with"],
  ["in", "in (a, b, …)"], ["not_in", "not in (a, b, …)"], ["between", "between"], ["not_between", "not between"],
  ["is_null", "is NULL"], ["not_null", "is not NULL"], ["is_empty", "is empty"], ["not_empty", "is not empty"],
  ["raw", "raw SQL"],
];
const NO_VALUE = new Set<Op>(["is_null", "not_null", "is_empty", "not_empty"]);
const TWO_VALUES = new Set<Op>(["between", "not_between"]);

/** Which column a new filter starts on: any column, the first, or the primary key's first column. */
export type DefaultColumn = "any" | "first" | "primary";
export type FilterDefaults = { column: DefaultColumn; op: Op };
let defaults: FilterDefaults = { column: "any", op: "contains" };
export const setFilterDefaults = (d: FilterDefaults) => void (defaults = d);

export const blankFilter = (): FilterRow => ({ on: true, column: null, op: defaults.op, value: "", value2: "" });

/** A new row as the defaults say, now that the table's columns are known. */
function defaultFilter(columns: string[], primary: string[]): FilterRow {
  const column = defaults.column === "first" ? columns[0] : defaults.column === "primary" ? (primary[0] ?? columns[0]) : null;
  return { ...blankFilter(), column: column ?? null };
}

/** What the bar can do beyond filtering, provided by the workspace. */
export type FilterActions = { primary: string[]; showSql: () => void; exportRows: () => void };

/** What to send: rows switched on, with a value wherever the operator needs one. */
export function activeFilters(state: FilterState) {
  return state.filters
    .filter((f) => f.on && (NO_VALUE.has(f.op) || f.value.trim() !== ""))
    .map(({ column, op, value, value2 }) => ({ column: op === "raw" ? null : column, op, value, value2 }));
}

/**
 * Draws the bar into `host`. Edits change `state` in place; `apply` runs on Enter, on Apply, and when a row is
 * removed or switched, so the grid always shows what the bar says.
 */
export function renderFilters(host: HTMLElement, state: FilterState, columns: string[], apply: () => void, more: FilterActions) {
  if (!state.filters.length) state.filters.push(defaultFilter(columns, more.primary));
  const redraw = () => renderFilters(host, state, columns, apply, more);

  const rows = state.filters.map((f, i) => {
    const on = el("input", { type: "checkbox", checked: f.on, title: "Include this filter" });
    on.onchange = () => ((f.on = on.checked), apply());

    const column = el("select", { title: "Column" },
      el("option", { value: "", textContent: "Any column" }),
      ...columns.map((c) => el("option", { value: c, textContent: c })),
    );
    column.value = f.column ?? "";
    column.disabled = f.op === "raw";
    column.onchange = () => (f.column = column.value || null);

    const op = el("select", { title: "Operator" }, ...OPS.map(([v, label]) => el("option", { value: v, textContent: label })));
    op.value = f.op;
    op.onchange = () => ((f.op = op.value as Op), redraw());

    const value = el("input", {
      type: "text",
      className: "v1",
      value: f.value,
      placeholder: f.op === "raw" ? "SQL condition, e.g. created_at > now() - interval '1 day'" : f.op.endsWith("in") ? "a, b, c" : "Value",
      hidden: NO_VALUE.has(f.op),
    });
    value.oninput = () => (f.value = value.value);
    const value2 = el("input", { type: "text", className: "v2", value: f.value2, placeholder: "and", hidden: !TWO_VALUES.has(f.op) });
    value2.oninput = () => (f.value2 = value2.value);
    // `void`: an on-handler that returns false cancels the event, so every other key would type nothing.
    for (const input of [value, value2]) input.onkeydown = (e) => void (e.key === "Enter" && (e.preventDefault(), apply()));

    const remove = el("button", { type: "button", className: "ghost rm", textContent: "−", title: "Remove this filter" });
    remove.onclick = () => {
      state.filters.splice(i, 1);
      redraw();
      apply();
    };
    const add = el("button", { type: "button", className: "ghost add", textContent: "+", title: "Add a filter below" });
    const below = (row: FilterRow) => {
      state.filters.splice(i + 1, 0, row);
      redraw();
      host.querySelectorAll<HTMLInputElement>(".filter-row input[type=text]")[(i + 1) * 2]?.focus();
    };
    add.onclick = () => below({ ...blankFilter(), column: f.column });
    const clone = el("button", { type: "button", className: "ghost clone", textContent: "⧉", title: "Duplicate this filter" });
    clone.onclick = () => below({ ...f });
    const only = el("button", { type: "button", className: "ghost only", textContent: "▸", title: "Apply only this filter (switches the others off)" });
    only.onclick = () => {
      for (const g of state.filters) g.on = g === f;
      redraw();
      apply();
    };
    const row = el("div", { className: TWO_VALUES.has(f.op) ? "filter-row two" : "filter-row" });
    row.append(on, column, op, value, value2, only, clone, remove, add);
    return row;
  });

  const match = el("select", { title: "How the filters combine" },
    el("option", { value: "and", textContent: "Match all" }),
    el("option", { value: "or", textContent: "Match any" }),
  );
  match.value = state.combine;
  match.onchange = () => ((state.combine = match.value as FilterState["combine"]), apply());
  const clear = el("button", { type: "button", textContent: "Clear" });
  clear.onclick = () => {
    state.filters = [];
    redraw();
    apply();
  };
  const sql = el("button", { type: "button", textContent: "SQL", title: "Show the SQL these filters produce" });
  sql.onclick = more.showSql;
  const exp = el("button", { type: "button", textContent: "Export…", title: "Export the rows these filters match" });
  exp.onclick = more.exportRows;
  const run = el("button", { type: "button", className: "primary", textContent: "Apply" });
  run.onclick = apply;

  host.replaceChildren(...rows, el("div", { className: "filter-foot" }, match, el("span", { className: "spacer" }), sql, exp, clear, run));
}
