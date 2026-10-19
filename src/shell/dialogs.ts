// The small pieces of UI everything asks through: a context menu at the pointer, a popover over a footer button, the
// confirmation window and the inline confirmation under the run bar.

import { $, el } from "../dom";

export function confirmDialog(text: string, okLabel: string): Promise<boolean> {
  const d = $<HTMLDialogElement>("confirm");
  $("confirm-text").textContent = text;
  $("confirm-ok").textContent = okLabel;
  d.showModal();
  $("confirm-cancel").focus(); // the safe choice is the default
  return new Promise((resolve) => {
    $("confirm-ok").onclick = () => (d.close(), resolve(true));
    $("confirm-cancel").onclick = () => d.close();
    d.onclose = () => resolve(false); // Esc, or after ok (already resolved)
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
