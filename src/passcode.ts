// The app passcode: the lock screen at startup and after time away, and its rows in Settings › Security
// (set, change, remove, auto-lock, Touch ID). The backend (passcode.rs) keeps only an argon2 hash, in the keychain.
// ponytail: a dialog opened after locking (an MCP approval) shows above the lock; hold those back while locked if it matters.

import { invoke } from "@tauri-apps/api/core";
import { $, el } from "./dom";
import { modal } from "./backup";
import { saveSecurity, security, touchIdAvailable, touchIdChecked, useTouchId } from "./safety";

const isMac = navigator.userAgent.includes("Mac");
let passcodeSet = false;
let locked = false;
let lastActive = Date.now();

// ---------- Lock screen: a full-window modal, so it sits above any dialog already open ----------

const input = el("input", { type: "password", autocomplete: "off", placeholder: "Passcode" });
const message = el("p", { className: "status" });
const touch = el("button", { type: "button", textContent: "Use Touch ID" });
const forgot = el("button", { type: "button", className: "ghost", textContent: "Forgot passcode?" });
const form = el("form", { className: "lock-form" },
  el("div", { className: "mark", textContent: "db" }),
  el("h2", { textContent: "Table Minus is locked" }),
  input, el("button", { className: "primary", textContent: "Unlock" }), touch, message, forgot);
const screen = el("dialog", { id: "lock" }, form);
screen.addEventListener("cancel", (e) => e.preventDefault()); // Esc doesn't unlock
document.body.append(screen);
screen.showModal(); // blank until we know whether a passcode is set, so nothing shows through at startup
form.hidden = true;

function lock() {
  locked = true;
  form.hidden = false;
  input.value = "";
  message.textContent = "";
  message.className = "status";
  delete forgot.dataset.armed;
  forgot.textContent = "Forgot passcode?";
  touch.hidden = !useTouchId();
  if (screen.open) screen.close(); // reopen on top of whatever is open now
  screen.showModal();
  input.focus();
}

function unlocked() {
  locked = false;
  input.value = "";
  lastActive = Date.now();
  screen.close();
}

const say = (text: string, kind = "") => ((message.textContent = text), (message.className = `status ${kind}`));

form.onsubmit = async (e) => {
  e.preventDefault();
  say("Checking…", "busy");
  try {
    await invoke("unlock", { passcode: input.value });
    unlocked();
  } catch (err) {
    say(String(err), "error");
    input.select();
  }
};
touch.onclick = async () => {
  try {
    await invoke("unlock_with_touch_id");
    unlocked();
  } catch (err) {
    say(String(err), "error");
  }
};
// First click explains, the second removes the passcode.
forgot.onclick = async () => {
  if (forgot.dataset.armed !== "1") {
    forgot.dataset.armed = "1";
    forgot.textContent = "Remove the passcode";
    return say(isMac
      ? "Your Mac's Touch ID or login password removes the passcode."
      : "Removing the passcode also removes every saved connection's passwords from the keychain; you'll enter them again.");
  }
  delete forgot.dataset.armed;
  forgot.textContent = "Forgot passcode?";
  try {
    await invoke("forgot_passcode");
    passcodeSet = false;
    unlocked();
    renderRows();
  } catch (err) {
    say(String(err), "error");
  }
};

invoke<boolean>("passcode_set").then(
  (set) => ((passcodeSet = set), set ? lock() : screen.close(), renderRows()),
  () => screen.close(), // outside the app (a plain browser preview) there's no backend
);

// ---------- Auto-lock after N minutes away ----------

for (const type of ["pointerdown", "pointermove", "keydown", "wheel"]) addEventListener(type, () => (lastActive = Date.now()), { passive: true, capture: true });
function checkAway() {
  const limit = security.autoLockMinutes * 60_000;
  if (passcodeSet && limit > 0 && !locked && Date.now() - lastActive > limit) lock();
}
setInterval(checkAway, 15_000);
document.addEventListener("visibilitychange", checkAway);

// ---------- Settings › Security ----------

const rows = el("div", { className: "passcode-rows" });
$("safe-default").closest(".row")!.parentElement!.append(rows);

function row(label: string, ...kids: Node[]) {
  return el("div", { className: "row" }, el("span", { className: "k", textContent: label }), el("span", { className: "row-items" }, ...kids));
}

function renderRows() {
  const buttons = passcodeSet
    ? [button("Change passcode…", () => passcodeDialog(true)), button("Remove…", removeDialog)]
    : [button("Set passcode…", () => passcodeDialog(false))];
  const away = el("select", {}, ...[0, 1, 5, 15, 30, 60].map((m) => el("option", { value: String(m), textContent: m ? `After ${m} minute${m === 1 ? "" : "s"} away` : "Only at startup" })));
  away.value = String(security.autoLockMinutes);
  away.disabled = !passcodeSet;
  away.onchange = () => ((security.autoLockMinutes = Number(away.value)), saveSecurity());
  const touchBox = el("input", { type: "checkbox", checked: security.touchId });
  touchBox.onchange = () => ((security.touchId = touchBox.checked), saveSecurity());
  rows.replaceChildren(
    row("Passcode", ...buttons),
    row("Lock", away),
    ...(touchIdAvailable() ? [row("Touch ID", el("label", { className: "check" }, touchBox, " Unlock and approve safe mode with Touch ID"))] : []),
    el("p", { className: "hint note", textContent: "The passcode locks Table Minus at startup and after time away. Only a hash of it is kept, in the keychain." }),
  );
}

function button(text: string, onclick: () => void) {
  return el("button", { type: "button", textContent: text, onclick });
}

const secret = (placeholder: string) => el("input", { type: "password", autocomplete: "off", placeholder });

function passcodeDialog(change: boolean) {
  const current = secret("Current passcode");
  const next = secret("New passcode (4 characters or more)");
  const again = secret("New passcode again");
  const { d, footer, setStatus } = modal(change ? "Change passcode" : "Set passcode", "small passcode-dialog", ...(change ? [current] : []), next, again);
  const save = el("button", { className: "primary", textContent: "Save" });
  save.onclick = async () => {
    if (next.value !== again.value) return setStatus("The new passcodes don't match.", "error");
    try {
      await invoke("set_passcode", { current: change ? current.value : null, passcode: next.value });
      passcodeSet = true;
      d.close();
      renderRows();
    } catch (e) {
      setStatus(String(e), "error");
    }
  };
  footer.append(button("Cancel", () => d.close()), el("span", { className: "spacer" }), save);
  d.showModal();
  (change ? current : next).focus();
}

function removeDialog() {
  const current = secret("Current passcode");
  const { d, footer, setStatus } = modal("Remove passcode", "small passcode-dialog", current);
  const remove = el("button", { className: "danger", textContent: "Remove" });
  remove.onclick = async () => {
    try {
      await invoke("remove_passcode", { current: current.value });
      passcodeSet = false;
      d.close();
      renderRows();
    } catch (e) {
      setStatus(String(e), "error");
    }
  };
  footer.append(button("Cancel", () => d.close()), el("span", { className: "spacer" }), remove);
  d.showModal();
  current.focus();
}

renderRows();
touchIdChecked.then(renderRows);

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
#lock { width: 100vw; height: 100vh; max-width: none; max-height: none; margin: 0; border: 0; border-radius: 0; padding: 0; background: var(--bg); box-shadow: none; animation: none; }
#lock::backdrop { background: var(--bg); }
.lock-form { height: 100%; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; }
.lock-form h2 { font-size: 17px; text-transform: none; letter-spacing: 0; color: var(--fg); margin: 8px 0; }
.lock-form input, .lock-form button:not(.ghost) { width: 240px; }
.row-items { display: flex; gap: 8px; align-items: center; }
.passcode-rows { display: grid; gap: 14px; }
.passcode-dialog[open] { display: grid; gap: 10px; }
.passcode-dialog h3, .passcode-dialog footer { margin: 0; }
`);
document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
