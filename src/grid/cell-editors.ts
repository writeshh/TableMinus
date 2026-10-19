// Dialogs for one cell: Quick Look (the whole value, editable, with JSON tools for JSON columns), the hex view
// for binary cells, and the SQL preview of staged changes. Built on the fly, so index.html needs nothing.

import { el } from "../dom";
import { hexDump } from "./staging";

/** Shows `d` as a modal with `buttons` in its footer; resolves with the label pressed, or null on Esc. */
function modal(d: HTMLDialogElement, buttons: [string, string][]): Promise<string | null> {
  const foot = el("footer", {}, el("span", { className: "spacer" }));
  d.append(foot);
  return new Promise((resolve) => {
    let picked: string | null = null;
    for (const [label, cls] of buttons) {
      const b = el("button", { type: "button", textContent: label, className: cls });
      b.onclick = () => ((picked = label), d.close());
      foot.append(b);
    }
    d.onclose = () => (d.remove(), resolve(picked));
    document.body.append(d);
    d.showModal();
  });
}

const dialog = (title: string, ...body: Node[]) =>
  el("dialog", { className: "cell-dialog" }, el("h3", { textContent: title }), ...body);

type ValueOptions = { title: string; value: string | null; json: boolean; readOnly: boolean; nullable: boolean };

/** A step of a JSONPath: `.name` where that reads plainly, `["name"]` otherwise. */
const pathKey = (k: string) => (/^[A-Za-z_$][\w$]*$/.test(k) ? `.${k}` : `[${JSON.stringify(k)}]`);

/** A read-only tree of a parsed JSON value. Objects and arrays fold, and draw their children only when first opened,
 *  so a large document stays quick; clicking a value hands its path to `pick`. */
function jsonTree(value: unknown, pick: (path: string) => void): HTMLElement {
  const node = (key: string | null, v: unknown, path: string, open: boolean): HTMLElement => {
    const label = key === null ? [] : [el("span", { className: "jt-key", textContent: key }), ": "];
    if (v === null || typeof v !== "object") {
      const kind = v === null ? "null" : typeof v;
      const leaf = el("div", { className: "jt-leaf", title: `${path}: click to copy the path` }, ...label,
        el("span", { className: `jt-${kind}`, textContent: JSON.stringify(v) }));
      leaf.onclick = () => pick(path);
      return leaf;
    }
    const list = Array.isArray(v);
    const entries: [string, unknown][] = list ? v.map((x, i) => [String(i), x]) : Object.entries(v);
    const d = el("details", { className: "jt-branch" },
      el("summary", { title: path }, ...label, el("span", { className: "jt-meta", textContent: list ? `[${entries.length}]` : `{${entries.length}}` })));
    let drawn = false;
    const draw = () => {
      if (drawn) return;
      drawn = true;
      d.append(...entries.map(([k, x]) => node(k, x, list ? `${path}[${k}]` : path + pathKey(k), false)));
    };
    d.ontoggle = () => d.open && draw();
    if (open) (d.open = true), draw();
    return d;
  };
  return node(null, value, "$", true);
}

/** Quick Look: the whole value in a window. Resolves with the new value (null for NULL), or undefined when nothing
 *  changed or it was cancelled. JSON values open pretty-printed, with Pretty, Minify and a validity check. */
export async function valueDialog(o: ValueOptions): Promise<string | null | undefined> {
  let text = o.value ?? "";
  if (o.json) {
    try {
      text = JSON.stringify(JSON.parse(text), null, 2);
    } catch {} // invalid JSON stays as it is, and the check below says why
  }
  const area = el("textarea", { className: "cell-text", value: text, readOnly: o.readOnly, spellcheck: false, placeholder: o.value === null ? "NULL" : "" });
  const wrap = el("input", { type: "checkbox", checked: true });
  wrap.onchange = () => (area.wrap = wrap.checked ? "soft" : "off");
  const check = el("span", { className: "json-check" });
  const tools = el("div", { className: "cell-tools" }, el("label", { className: "check" }, wrap, "Wrap lines"));
  const d = dialog(o.title, tools, area);
  let touched = false;
  const validate = () => {
    if (!o.json) return true;
    try {
      JSON.parse(area.value);
      check.textContent = "Valid JSON";
      check.className = "json-check ok";
      return true;
    } catch (e) {
      check.textContent = e instanceof Error ? e.message : String(e);
      check.className = "json-check error";
      return false;
    }
  };
  if (o.json) {
    const reformat = (indent?: number) => () => {
      if (!validate()) return;
      area.value = JSON.stringify(JSON.parse(area.value), null, indent);
      touched = true;
    };
    const pretty = el("button", { type: "button", textContent: "Pretty" });
    const minify = el("button", { type: "button", textContent: "Minify" });
    pretty.onclick = reformat(2);
    minify.onclick = reformat();
    // Text or Tree: the tree reads what the text holds now, so edits stay in the text.
    const tree = el("div", { className: "json-tree", hidden: true });
    area.after(tree);
    const textView = el("button", { type: "button", textContent: "Text" });
    const treeView = el("button", { type: "button", textContent: "Tree" });
    const view = (asTree: boolean) => {
      if (asTree && !validate()) return;
      if (asTree) tree.replaceChildren(jsonTree(JSON.parse(area.value), (path) => {
        navigator.clipboard.writeText(path).catch(() => {}); // best effort; the line below still shows the path
        check.textContent = `Copied ${path}`;
        check.className = "json-check ok";
      }));
      area.hidden = asTree;
      tree.hidden = !asTree;
      pretty.disabled = minify.disabled = asTree;
      textView.setAttribute("aria-pressed", String(!asTree));
      treeView.setAttribute("aria-pressed", String(asTree));
    };
    textView.onclick = () => view(false);
    treeView.onclick = () => view(true);
    tools.prepend(el("span", { className: "seg" }, textView, treeView), pretty, minify, check);
    view(false);
    validate();
  }
  const enableSave = () => {
    const save = d.querySelector<HTMLButtonElement>("footer button.primary");
    if (save) save.disabled = !validate();
  };
  area.oninput = () => ((touched = true), enableSave());
  const buttons: [string, string][] = o.readOnly
    ? [["Close", ""]]
    : [["Cancel", ""], ...(o.nullable ? [["Set NULL", ""] as [string, string]] : []), ["Save", "primary"]];
  const done = modal(d, buttons);
  enableSave();
  area.focus();
  const picked = await done;
  if (picked === "Set NULL") return null;
  return picked === "Save" && touched ? area.value : undefined;
}

/** A read-only hex dump of a binary value; `hex` may be only the start of it. */
export async function hexDialog(title: string, hex: string, size: number) {
  const shown = hex.length / 2;
  const note = shown < size ? `The first ${shown.toLocaleString()} of ${size.toLocaleString()} bytes.` : `${size.toLocaleString()} bytes.`;
  const pre = el("pre", { className: "hex-view", textContent: hexDump(hex) || "(empty)" });
  await modal(dialog(title, el("p", { className: "hint", textContent: note }), pre), [["Close", ""]]);
}

const sqlText = (statements: string[]) => statements.map((s) => `${s};`).join("\n");

/** Shows the statements a save would run; true when the user saves from here. */
export async function sqlDialog(title: string, statements: string[], where: string): Promise<boolean> {
  const pre = el("pre", { className: "sql-preview", textContent: sqlText(statements) });
  const note = el("p", { className: "hint", textContent: `Runs in one transaction on ${where}. Values are sent as bound parameters.` });
  const picked = await modal(dialog(title, pre, note), [["Cancel", ""], ["Copy", ""], ["Save", "primary"]]);
  if (picked === "Copy") {
    await navigator.clipboard.writeText(sqlText(statements)).catch(() => {});
    return sqlDialog(title, statements, where);
  }
  return picked === "Save";
}
