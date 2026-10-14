// Open Anything (⌘P): one search box over tables, views and other items, SQL tabs, workspaces and app commands.
// Arrow keys move, Enter opens, Esc closes.
import { el } from "./dom";
import { rank } from "./fuzzy";

export type Item = {
  label: string;
  /** What kind of thing it is, shown on the right: "table", "view", "SQL tab", "command"… */
  kind: string;
  /** Extra text, e.g. a command's shortcut. */
  detail?: string;
  run: () => unknown;
};

const SHOWN = 200; // a database with thousands of tables still draws quickly

/** Opens the palette over `items`, built now so it reflects the workspace as it is. */
export function openAnything(items: Item[]) {
  const input = el("input", { type: "search", placeholder: "Open a table, view, SQL tab or command…", autocomplete: "off" });
  const list = el("ul", { className: "anything-list" });
  list.setAttribute("role", "listbox");
  const d = el("dialog", { className: "anything" }, input, list);
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  let pick = 0;
  let shown: { item: Item; at: number[] }[] = [];

  const draw = () => {
    shown = rank(input.value, items, (i) => i.label).slice(0, SHOWN);
    pick = Math.min(pick, Math.max(0, shown.length - 1));
    list.replaceChildren(
      ...shown.map(({ item, at }, i) => {
        const name = el("span", { className: "name" });
        // The matched characters in bold; textContent only, names come from databases.
        let last = 0;
        for (const a of at) name.append(item.label.slice(last, a), el("b", { textContent: item.label[a] })), (last = a + 1);
        name.append(item.label.slice(last));
        const li = el("li", {}, name, el("span", { className: "detail", textContent: item.detail ?? "" }),
          el("span", { className: "kind", textContent: item.kind }));
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(i === pick));
        li.onpointermove = () => pick !== i && ((pick = i), mark());
        li.onclick = () => choose(i);
        return li;
      }),
    );
    if (!shown.length) list.append(el("li", { className: "hint", textContent: "Nothing matches." }));
    mark();
  };
  const mark = () => {
    list.querySelectorAll("li[role=option]").forEach((li, i) => li.setAttribute("aria-selected", String(i === pick)));
    list.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  };
  const choose = (i: number) => {
    const item = shown[i]?.item;
    if (!item) return;
    d.close(); // first, so a command that opens a dialog of its own isn't blocked by this one
    d.remove();
    item.run();
  };

  input.oninput = () => ((pick = 0), draw());
  input.onkeydown = (e) => {
    if (e.key === "ArrowDown") pick = Math.min(shown.length - 1, pick + 1);
    else if (e.key === "ArrowUp") pick = Math.max(0, pick - 1);
    else if (e.key === "Enter") return e.preventDefault(), choose(pick);
    else return;
    e.preventDefault();
    mark();
  };
  draw();
  d.showModal();
  input.focus();
}
