// The small pieces of UI everything asks through: a context menu at the pointer, a popover over a footer button, the
// confirmation window and the inline confirmation under the run bar.

import { $, el } from "../dom";

/** Actions that destroy something or can't be undone; their button is danger, any other action's is primary. */
const DESTROYS = /^(Delete|Drop|Remove|Discard|Clear|Kill|Restore|Truncate|Overwrite)\b/;

export function confirmDialog(text: string, okLabel: string, tone: "danger" | "primary" = DESTROYS.test(okLabel) ? "danger" : "primary"): Promise<boolean> {
  const d = $<HTMLDialogElement>("confirm");
  $("confirm-text").textContent = text;
  $("confirm-ok").textContent = okLabel;
  $("confirm-ok").className = tone;
  d.showModal();
  $("confirm-cancel").focus(); // the safe choice is the default
  return new Promise((resolve) => {
    $("confirm-ok").onclick = () => (d.close(), resolve(true));
    $("confirm-cancel").onclick = () => d.close();
    d.onclose = () => resolve(false); // Esc, or after ok (already resolved)
  });
}

export type Prompt = {
  title: string;
  /** A sentence under the title. */
  text?: string;
  /** The field's label, over it. */
  label?: string;
  ok?: string;
  value?: string;
  placeholder?: string;
  /** A password: never left in the page after it closes. */
  secret?: boolean;
  /** A multi-line value (a whole document), in a wide window. */
  big?: boolean;
  /** An error from the last try, shown under the field. */
  error?: string;
};

/** The one-field window: a title, a sentence, a labelled field and its error line, then Cancel and the action.
 *  Resolves to the value as typed, or null when cancelled. */
export function prompt(o: Prompt): Promise<string | null> {
  const input = o.big ? el("textarea", { value: o.value ?? "", spellcheck: false }) : el("input", { type: o.secret ? "password" : "text", value: o.value ?? "", placeholder: o.placeholder ?? "", autocomplete: "off", spellcheck: false });
  const field = o.label ? el("label", { className: "stack" }, o.label, input) : input;
  const cancel = el("button", { type: "button", textContent: "Cancel" });
  const form = el("form", { method: "dialog", className: "prompt-form" },
    ...(o.text ? [el("p", { textContent: o.text })] : []), field, el("p", { className: "status error", textContent: o.error ?? "" }),
    el("footer", {}, el("span", { className: "spacer" }), cancel, el("button", { type: "submit", className: "primary", textContent: o.ok ?? "OK" })));
  const d = el("dialog", { className: o.big ? "wide" : "small" }, el("h3", { textContent: o.title }), form);
  // Inside the topmost modal window when one is open, as contextMenu does.
  ([...document.querySelectorAll("dialog:modal")].pop() ?? document.body).append(d);
  return new Promise((resolve) => {
    let result: string | null = null;
    form.onsubmit = (e) => (e.preventDefault(), (result = input.value), d.close());
    cancel.onclick = () => d.close();
    d.onclose = () => ((input.value = ""), d.remove(), resolve(result));
    d.showModal();
    if (input instanceof HTMLInputElement) input.select();
    else input.focus();
  });
}

export let menuClose: (() => void) | null = null;
export type MenuItem = [string, () => void] | "-";

/** A small context menu at the pointer; any click or Esc closes it. */
export function contextMenu(e: MouseEvent, items: MenuItem[]) {
  e.preventDefault();
  menuClose?.();
  const menu = el("div", { className: "menu" });
  menu.setAttribute("role", "menu");
  for (const item of items) {
    if (item === "-") menu.append(el("hr"));
    else {
      const b = el("button", { type: "button", textContent: item[0] });
      b.setAttribute("role", "menuitem");
      b.onclick = () => (close(), item[1]());
      menu.append(b);
    }
  }
  // Inside the topmost modal window when there is one, or the window would keep it out of reach.
  ([...document.querySelectorAll("dialog:modal")].pop() ?? document.body).append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(e.clientX, innerWidth - r.width - 8)}px`;
  menu.style.top = `${Math.min(e.clientY, innerHeight - r.height - 8)}px`;
  menu.querySelector("button")?.focus();
  const onDown = (ev: Event) => !menu.contains(ev.target as Node) && close();
  // Esc closes only the menu, not a window it sits in.
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && (ev.preventDefault(), ev.stopPropagation(), close());
  function close() {
    menu.remove();
    removeEventListener("pointerdown", onDown, true);
    removeEventListener("keydown", onKey, true);
    menuClose = null;
  }
  addEventListener("pointerdown", onDown, true);
  addEventListener("keydown", onKey, true);
  menuClose = close;
}

/** Inline confirmation under the run bar. Cancel is focused, so Enter or Esc never confirms by accident. */
export function ask(text: string, detail: string, okLabel: string): Promise<boolean> {
  const bar = $("ask");
  $("ask-text").textContent = text;
  $("ask-detail").textContent = detail.length > 120 ? detail.slice(0, 120) + "…" : detail;
  $("ask-ok").textContent = okLabel;
  bar.hidden = false;
  $("ask-cancel").focus();
  return new Promise((resolve) => {
    const done = (ok: boolean) => {
      bar.hidden = true;
      document.removeEventListener("keydown", onKey);
      resolve(ok);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && done(false);
    document.addEventListener("keydown", onKey);
    $("ask-ok").onclick = () => done(true);
    $("ask-cancel").onclick = () => done(false);
  });
}

/** Shows `menu` above `anchor`, a footer button; a click outside or Esc closes it. */
export function openPopover(anchor: HTMLElement, menu: HTMLElement) {
  menuClose?.();
  document.body.append(menu);
  const r = anchor.getBoundingClientRect();
  const { width, height } = menu.getBoundingClientRect();
  Object.assign(menu.style, { left: `${Math.min(innerWidth - width - 8, Math.max(8, r.left))}px`, top: `${Math.max(8, r.top - height - 6)}px` });
  const onDown = (ev: Event) => !menu.contains(ev.target as Node) && ev.target !== anchor && close();
  // Esc closes only the menu, not a window it sits in.
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && (ev.preventDefault(), ev.stopPropagation(), close());
  function close() {
    menu.remove();
    removeEventListener("pointerdown", onDown, true);
    removeEventListener("keydown", onKey, true);
    menuClose = null;
  }
  addEventListener("pointerdown", onDown, true);
  addEventListener("keydown", onKey, true);
  menuClose = close;
}
