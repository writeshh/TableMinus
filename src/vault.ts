// The master password of the secrets vault (secrets.rs): on a Linux desktop without a keychain, saved passwords live
// in a file encrypted with it. At startup an existing vault is unlocked before anything else; the first save of a
// password, before there is a vault, asks for a new master password instead.

import { codeOf, invoke } from "./ipc";
import { el } from "./dom";

type Status = { needed: boolean; exists: boolean; unlocked: boolean };

const title = el("h3");
const text = el("p");
const master = el("input", { type: "password", autocomplete: "off", placeholder: "Master password" });
const again = el("input", { type: "password", autocomplete: "off", placeholder: "The same again" });
const message = el("p", { className: "status error" });
const cancel = el("button", { type: "button", textContent: "Cancel" });
const forgot = el("button", { type: "button", className: "ghost", textContent: "Forgot master password?" });
const ok = el("button", { className: "primary" });
const form = el("form", { method: "dialog", className: "safe-form" }, text, master, again, message,
  el("footer", {}, cancel, forgot, el("span", { className: "spacer" }), ok));
const dialog = el("dialog", { className: "small" }, title, form);
document.body.append(dialog);

/** Asks for the master password, creating the vault when there is none. Resolves to whether it is open now;
 *  `required` (at startup) offers no Cancel, as nothing works with the vault shut. */
function ask(exists: boolean, required: boolean): Promise<boolean> {
  title.textContent = exists ? "Unlock saved passwords" : "Set a master password";
  text.textContent = exists
    ? "This computer has no keychain, so TableMinus keeps saved passwords in a file encrypted with your master password."
    : "This computer has no keychain, so TableMinus keeps saved passwords in a file encrypted with a master password. Choose one of at least 8 characters; it can't be recovered.";
  ok.textContent = exists ? "Unlock" : "Create";
  again.hidden = exists;
  forgot.hidden = !exists;
  cancel.hidden = required;
  master.value = again.value = message.textContent = "";
  delete forgot.dataset.armed;
  forgot.textContent = "Forgot master password?";
  dialog.showModal();
  master.focus();
  return new Promise((resolve) => {
    let done = false;
    const finish = (open: boolean) => ((done = open), dialog.close());
    dialog.oncancel = (e) => required && e.preventDefault();
    dialog.onclose = () => ((master.value = again.value = ""), resolve(done));
    cancel.onclick = () => finish(false);
    forgot.onclick = async () => {
      if (!forgot.dataset.armed) {
        forgot.dataset.armed = "1";
        forgot.textContent = "Start over without the saved passwords";
        return void (message.textContent = "Every saved password is lost; the old file stays beside the new one, renamed, in case you remember. Click again to go ahead.");
      }
      try {
        await invoke("vault_reset");
        finish(true); // nothing to unlock now; the next save makes a new vault
      } catch (e) {
        message.textContent = String(e);
      }
    };
    form.onsubmit = async (e) => {
      e.preventDefault();
      if (!exists && master.value !== again.value) return void (message.textContent = "The two passwords differ.");
      ok.disabled = true;
      message.textContent = "";
      try {
        await invoke("vault_unlock", { master: master.value });
        finish(true);
      } catch (err) {
        message.textContent = String(err);
        master.select();
      } finally {
        ok.disabled = false;
      }
    };
  });
}

/** At startup: unlocks an existing vault before the passcode is checked (its hash is in there). Outside the app,
 *  or with a keychain, there is nothing to do. */
export async function unlockAtStartup() {
  const s = await invoke<Status>("vault_status").catch(() => null);
  if (s?.needed && s.exists && !s.unlocked) await ask(true, true);
}

/** Sends something that saves a secret; when the vault is locked (or not made yet) asks for the master password,
 *  then sends it again. */
export async function withVault<T>(send: () => Promise<T>): Promise<T> {
  // Twice at most: "Forgot master password?" leaves no vault, and the next ask makes a new one.
  for (let asked = 0; ; asked++) {
    try {
      return await send();
    } catch (e) {
      if (codeOf(e) !== "vault_locked" || asked === 2) throw e;
      const s = await invoke<Status>("vault_status");
      if (!(await ask(s.exists, false))) throw "Not saved: the saved passwords are locked";
    }
  }
}
