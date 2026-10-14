// Safe mode in the UI: the five levels, asking the way a level wants when the backend stops a request
// (safety.rs decides), and the toolbar lock that switches the active connection's level.

import { invoke } from "@tauri-apps/api/core";
import { $, el } from "./dom";

export type Safety = "silent" | "alert1" | "alert2" | "safe1" | "safe2";
export const LEVELS: [Safety, string, string][] = [
  ["silent", "Silent", "Run everything"],
  ["alert1", "Alert 1", "Ask before every query"],
  ["alert2", "Alert 2", "Ask before anything but SELECT, EXPLAIN and SHOW"],
  ["safe1", "Safe 1", "Ask for the database password before every query"],
  ["safe2", "Safe 2", "Ask for the database password before anything but reads"],
];
const label = (s: Safety) => LEVELS.find(([v]) => v === s)?.[1] ?? s;

/** Fills a select with the levels. */
export function fillLevels(select: HTMLSelectElement) {
  select.replaceChildren(...LEVELS.map(([value, name, what]) => el("option", { value, textContent: `${name}: ${what}` })));
}

// ---------- Security preferences (this machine) ----------

export type Security = { touchId: boolean; autoLockMinutes: number };
export const security: Security = (() => {
  const defaults: Security = { touchId: true, autoLockMinutes: 0 };
  try {
    return { ...defaults, ...JSON.parse(localStorage.getItem("security") ?? "{}") };
  } catch {
    return defaults;
  }
})();
export function saveSecurity() {
  try {
    localStorage.setItem("security", JSON.stringify(security));
  } catch {} // unavailable storage: the choice holds for this session
}

let touchIdHere = false;
/** Resolves once Touch ID's availability is known. */
export const touchIdChecked = invoke<boolean>("touch_id_available").then((ok) => void (touchIdHere = ok), () => {});
export const touchIdAvailable = () => touchIdHere;
export const useTouchId = () => touchIdHere && security.touchId;

// ---------- Approval: what to resend with after safe mode stopped a request ----------

export type Unlock = { password: string } | "touch_id";
export type Approval = { confirmed: boolean; unlock: Unlock | null };
export const NOT_APPROVED: Approval = { confirmed: false, unlock: null };
/** What `guarded` throws when the user says no. */
export const CANCELLED = "Cancelled";
export const isSafetyStop = (e: unknown) => e === "CONFIRM" || e === "PASSWORD" || e === "WRONG PASSWORD";

/** After safe mode stopped a request with `e`: `confirm` for the Alert levels, the password (or Touch ID) for the
 * Safe ones. Resolves to what to resend with, or null when the user cancels; any other error is rethrown. */
export async function approval(e: unknown, conn: string, confirm: () => Promise<boolean>): Promise<Approval | null> {
  if (e === "CONFIRM") return (await confirm()) ? { confirmed: true, unlock: null } : null;
  if (e === "PASSWORD" || e === "WRONG PASSWORD") {
    const unlock = await askPassword(conn, e === "WRONG PASSWORD");
    return unlock && { confirmed: true, unlock };
  }
  throw e;
}

/** Sends, and sends again after asking as long as safe mode stops it. Throws CANCELLED when the user says no. */
export async function guarded<T>(conn: string, send: (a: Approval) => Promise<T>, confirm = () => Promise.resolve(true)): Promise<T> {
  let a = NOT_APPROVED;
  for (;;) {
    try {
      return await send(a);
    } catch (e) {
      const next = await approval(e, conn, confirm);
      if (!next) throw CANCELLED;
      a = next;
    }
  }
}

const pwInput = el("input", { type: "password", autocomplete: "off", placeholder: "Database password" });
const pwText = el("p");
const pwError = el("p", { className: "status error" });
const pwCancel = el("button", { type: "button", textContent: "Cancel" });
const pwTouch = el("button", { type: "button", textContent: "Use Touch ID" });
const pwForm = el("form", { method: "dialog", className: "safe-form" }, pwText, pwInput, pwError,
  el("footer", {}, pwCancel, el("span", { className: "spacer" }), pwTouch, el("button", { className: "primary", textContent: "Run" })));
const pwDialog = el("dialog", { className: "small" }, el("h3", { textContent: "Safe mode" }), pwForm);
document.body.append(pwDialog);

/** The password a Safe level wants; null when cancelled. */
export function askPassword(conn: string, wrong = false): Promise<Unlock | null> {
  pwText.textContent = `Enter the database password of “${conn}” to go ahead.`;
  pwError.textContent = wrong ? "That isn't the saved password." : "";
  pwInput.value = "";
  pwTouch.hidden = !useTouchId();
  pwDialog.showModal();
  pwInput.focus();
  return new Promise((resolve) => {
    let result: Unlock | null = null;
    pwForm.onsubmit = (e) => (e.preventDefault(), (result = { password: pwInput.value }), pwDialog.close());
    pwTouch.onclick = () => ((result = "touch_id"), pwDialog.close());
    pwCancel.onclick = () => pwDialog.close();
    pwDialog.onclose = () => ((pwInput.value = ""), resolve(result));
  });
}

// ---------- Toolbar lock: the active connection's level ----------

type Conn = { name: string; safety: Safety };
let conn: Conn | null = null;
const lockButton = el("button", { id: "safety-button", className: "ghost", title: "Safe mode level" });
$("tools-button").before(lockButton);

function lockIcon(open: boolean) {
  const ns = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(ns, "svg");
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("aria-hidden", "true");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", open ? "M6 11h12v10H6zM9 11V7a3 3 0 0 1 6 0" : "M6 11h12v10H6zM9 11V7a3 3 0 0 1 6 0v4");
  s.append(p);
  return s;
}

/** Shows `c`'s level in the toolbar; its `safety` is updated in place when the lock changes it. */
export function showSafety(c: Conn) {
  conn = c;
  lockButton.replaceChildren(lockIcon(c.safety === "silent"), ` ${label(c.safety)}`);
  lockButton.dataset.level = c.safety;
}

let menu: HTMLElement | null = null;
function closeMenu() {
  menu?.remove();
  menu = null;
  removeEventListener("pointerdown", outside, true);
}
const outside = (e: Event) => !menu?.contains(e.target as Node) && e.target !== lockButton && closeMenu();

lockButton.onclick = () => {
  if (menu) return closeMenu();
  if (!conn) return;
  const c = conn;
  menu = el("div", { className: "menu safety-menu" });
  for (const [value, name, what] of LEVELS) {
    const b = el("button", { title: what }, el("span", { className: "tick", textContent: value === c.safety ? "✓" : "" }), name, el("span", { className: "what", textContent: what }));
    b.setAttribute("role", "menuitemradio");
    b.setAttribute("aria-checked", String(value === c.safety));
    b.onclick = async () => {
      closeMenu();
      if (value === c.safety) return;
      try {
        await guarded(c.name, (a) => invoke("set_safety", { name: c.name, safety: value, unlock: a.unlock }));
        c.safety = value;
        if (conn === c) showSafety(c);
      } catch (e) {
        if (e !== CANCELLED) alertError(String(e));
      }
    };
    menu.append(b);
  }
  const r = lockButton.getBoundingClientRect();
  Object.assign(menu.style, { top: `${r.bottom + 4}px`, left: `${Math.max(8, r.right - 300)}px` });
  document.body.append(menu);
  addEventListener("pointerdown", outside, true);
  (menu.querySelector("[aria-checked=true]") as HTMLElement | null)?.focus();
};
addEventListener("keydown", (e) => e.key === "Escape" && menu && closeMenu());

function alertError(text: string) {
  const d = el("dialog", { className: "small" }, el("p", { className: "error-text", textContent: text }),
    el("form", { method: "dialog" }, el("footer", {}, el("span", { className: "spacer" }), el("button", { className: "primary", textContent: "OK" }))));
  document.body.append(d);
  d.addEventListener("close", () => d.remove());
  d.showModal();
}

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
#safety-button[data-level="alert1"], #safety-button[data-level="alert2"] { color: var(--warn); }
#safety-button[data-level="safe1"], #safety-button[data-level="safe2"] { color: var(--danger); }
.safety-menu { width: 300px; }
#safe-default, select[name="safety"] { min-width: 0; max-width: 100%; }
.safety-menu button { display: grid; grid-template-columns: 16px 1fr; column-gap: 4px; }
.safety-menu .what { grid-column: 2; font-size: 11px; color: var(--muted); }
.safe-form { display: grid; gap: 10px; }
.safe-form p { margin: 0; }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
