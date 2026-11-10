// The small pieces of UI everything asks through: a context menu at the pointer, a popover over a footer button, the
// confirmation window and the inline confirmation under the run bar.

import { $, el, icon } from "../dom";

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

/** A window built for one use and removed when it closes: its centred title, the body, a status line for its
 *  progress and errors, then the footer, where the caller puts other actions, a spacer, Cancel and the main action. */
export function windowDialog(title: string, className: string, ...body: Node[]) {
  const status = el("p", { className: "status" });
  const footer = el("footer");
  const d = el("dialog", { className }, el("h3", { textContent: title }), ...body, status, footer);
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  const setStatus = (text: string, kind: "" | "busy" | "error" | "done" = "") => {
    status.textContent = text;
    status.className = `status ${kind}`;
  };
  return { d, footer, status, setStatus };
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
/** A command, a command with a tick (`checked`), a submenu (its items), or a rule. */
export type MenuItem = [string, () => void] | [string, () => void, { checked: boolean }] | [string, MenuItem[]] | "-";

/** A context menu at the pointer; any click outside or Esc closes it. Items with `checked` show a drawn tick in a
 *  column the whole menu shares; a submenu opens beside its item on hover, click or →, and ← or Esc goes back. */
export function contextMenu(e: MouseEvent, items: MenuItem[]) {
  e.preventDefault();
  menuClose?.();
  // Inside the topmost modal window when there is one, or the window would keep it out of reach.
  const host = [...document.querySelectorAll("dialog:modal")].pop() ?? document.body;
  const open: HTMLElement[] = []; // the root menu, then each open submenu

  const place = (menu: HTMLElement, x: number, y: number, flipFrom?: number) => {
    const r = menu.getBoundingClientRect();
    const left = x + r.width > innerWidth - 8 && flipFrom !== undefined ? flipFrom - r.width : Math.min(x, innerWidth - r.width - 8);
    menu.style.left = `${Math.max(8, left)}px`;
    menu.style.top = `${Math.min(y, innerHeight - r.height - 8)}px`;
  };
  const closeFrom = (depth: number) => { for (const m of open.splice(depth)) m.remove(); };

  function render(list: MenuItem[], depth: number): HTMLElement {
    const menu = el("div", { className: "menu" });
    menu.setAttribute("role", "menu");
    const ticks = list.some((i) => i !== "-" && i.length === 3);
    for (const item of list) {
      if (item === "-") { menu.append(el("hr")); continue; }
      const b = el("button", { type: "button" });
      const label = el("span", { className: "menu-label", textContent: item[0] });
      if (Array.isArray(item[1])) {
        const sub = item[1];
        b.setAttribute("role", "menuitem");
        b.setAttribute("aria-haspopup", "menu");
        if (ticks) b.append(el("span", { className: "tick" }));
        b.append(label, el("span", { className: "sub-arrow" }, icon("chevron")));
        const show = () => {
          if (open[depth + 1]?.dataset.of === item[0]) return;
          closeFrom(depth + 1);
          const child = render(sub, depth + 1);
          child.dataset.of = item[0];
          const r = b.getBoundingClientRect();
          place(child, r.right - 4, r.top - 5, r.left + 4);
          return child;
        };
        b.onpointerenter = () => void show();
        b.onclick = () => show()?.querySelector("button")?.focus();
        b.onkeydown = (ev) => ev.key === "ArrowRight" && (ev.preventDefault(), show()?.querySelector("button")?.focus());
      } else {
        const [, run, opts] = item;
        b.setAttribute("role", opts ? "menuitemcheckbox" : "menuitem");
        if (opts) b.setAttribute("aria-checked", String(opts.checked));
        if (ticks) b.append(el("span", { className: "tick" }, ...(opts?.checked ? [icon("check")] : [])));
        b.append(label);
        b.onpointerenter = () => closeFrom(depth + 1);
        b.onclick = () => (close(), run());
      }
      menu.append(b);
    }
    menu.onkeydown = (ev) => {
      const buttons = [...menu.querySelectorAll<HTMLButtonElement>(":scope > button")];
      const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        buttons[(at + (ev.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length]?.focus();
      } else if (ev.key === "ArrowLeft" && depth > 0) {
        ev.preventDefault();
        closeFrom(depth);
        open[depth - 1]?.querySelector<HTMLElement>(`[aria-haspopup]`)?.focus();
      }
    };
    host.append(menu);
    open[depth] = menu;
    return menu;
  }

  const root = render(items, 0);
  place(root, e.clientX, e.clientY);
  root.querySelector("button")?.focus();
  const onDown = (ev: Event) => !open.some((m) => m.contains(ev.target as Node)) && close();
  // Esc closes only the menu, not a window it sits in.
  const onKey = (ev: KeyboardEvent) => ev.key === "Escape" && (ev.preventDefault(), ev.stopPropagation(), close());
  function close() {
    closeFrom(0);
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
