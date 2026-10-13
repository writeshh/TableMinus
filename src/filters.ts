// The filter bar for table tabs: rows of column · operator · value, combined with AND or OR. The values go to
// the backend's `browse` command, which binds them; nothing here builds SQL.
import { el } from "./dom";

export type Op =
  | "eq" | "ne" | "lt" | "le" | "gt" | "ge"
  | "contains" | "not_contains" | "starts_with" | "ends_with"
  | "in" | "not_in" | "between" | "not_between"
  | "is_null" | "not_null" | "is_empty" | "not_empty" | "raw";

export type FilterRow = { on: boolean; column: string | null; op: Op; value: string; value2: string };
export type FilterState = { filters: FilterRow[]; combine: "and" | "or" };

const OPS: [Op, string][] = [
  ["eq", "="], ["ne", "≠"], ["lt", "<"], ["le", "≤"], ["gt", ">"], ["ge", "≥"],
  ["contains", "contains"], ["not_contains", "doesn't contain"], ["starts_with", "starts with"], ["ends_with", "ends with"],
  ["in", "in (a, b, …)"], ["not_in", "not in (a, b, …)"], ["between", "between"], ["not_between", "not between"],
  ["is_null", "is NULL"], ["not_null", "is not NULL"], ["is_empty", "is empty"], ["not_empty", "is not empty"],
  ["raw", "raw SQL"],
];
const NO_VALUE = new Set<Op>(["is_null", "not_null", "is_empty", "not_empty"]);
const TWO_VALUES = new Set<Op>(["between", "not_between"]);

export const blankFilter = (): FilterRow => ({ on: true, column: null, op: "contains", value: "", value2: "" });

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
export function renderFilters(host: HTMLElement, state: FilterState, columns: string[], apply: () => void) {
  if (!state.filters.length) state.filters.push(blankFilter());
  const redraw = () => renderFilters(host, state, columns, apply);

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
    for (const input of [value, value2]) input.onkeydown = (e) => e.key === "Enter" && (e.preventDefault(), apply());

    const remove = el("button", { type: "button", className: "ghost rm", textContent: "−", title: "Remove this filter" });
    remove.onclick = () => {
      state.filters.splice(i, 1);
      redraw();
      apply();
    };
    const add = el("button", { type: "button", className: "ghost add", textContent: "+", title: "Add a filter below" });
    add.onclick = () => {
      state.filters.splice(i + 1, 0, { ...blankFilter(), column: f.column });
      redraw();
      host.querySelectorAll<HTMLInputElement>(".filter-row input[type=text]")[(i + 1) * 2]?.focus();
    };
    const row = el("div", { className: TWO_VALUES.has(f.op) ? "filter-row two" : "filter-row" });
    row.append(on, column, op, value, value2, remove, add);
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
    state.filters = [blankFilter()];
    redraw();
    apply();
  };
  const run = el("button", { type: "button", className: "primary", textContent: "Apply" });
  run.onclick = apply;

  host.replaceChildren(...rows, el("div", { className: "filter-foot" }, match, el("span", { className: "spacer" }), clear, run));
}
