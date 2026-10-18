// Updates (updater.rs): a quiet check at startup unless Settings › General turns it off, Help › Check for Updates…
// for one on demand, and a dialog with the new version's notes that installs it and starts it. Help › Release Notes
// shows the changelog bundled with this version.

import { invoke } from "./ipc";
import { el } from "./dom";
import { modal } from "./backup";
import { readPref, writePref } from "./prefs";
import changelog from "../CHANGELOG.md?raw";

type Available = { version: string; current: string; notes: string | null; date: string | null };

/** The version the user chose to skip; quiet checks don't offer it again. */
const SKIPPED = "update-skipped";
let checking = false;

/** Looks for a newer version. A quiet check says nothing unless it finds one the user hasn't skipped. */
export async function checkForUpdates(opts: { beta: boolean; quiet: boolean }) {
  if (checking) return;
  checking = true;
  try {
    const found = await invoke<Available | null>("update_check", { beta: opts.beta });
    if (!found) {
      if (!opts.quiet) notice("You're up to date", "This is the newest version of TableMinus.");
    } else if (!opts.quiet || readPref<string>(SKIPPED) !== found.version) {
      offer(found);
    }
  } catch (e) {
    if (!opts.quiet) notice("Couldn't check for updates", String(e));
  } finally {
    checking = false;
  }
}

function notice(title: string, text: string) {
  const { d, footer } = modal(title, "small", el("p", { textContent: text }));
  const ok = el("button", { className: "primary", textContent: "OK" });
  ok.onclick = () => d.close();
  footer.append(el("span", { className: "spacer" }), ok);
  d.showModal();
}

function offer(u: Available) {
  const notes = el("pre", { className: "release-notes", textContent: u.notes?.trim() || "No notes for this version." });
  const { d, footer, setStatus } = modal(`TableMinus ${u.version} is available`, "wide",
    el("p", { className: "hint", textContent: `You have ${u.current}.${u.date ? ` Released ${u.date.slice(0, 10)}.` : ""}` }), notes);
  const install = el("button", { className: "primary", textContent: "Install and Restart" });
  const later = el("button", { textContent: "Later" });
  const skip = el("button", { className: "ghost", textContent: "Skip This Version" });
  install.onclick = async () => {
    install.disabled = later.disabled = skip.disabled = true;
    setStatus("Downloading…", "busy");
    try {
      await invoke("update_install"); // restarts the app when it's done
    } catch (e) {
      setStatus(String(e), "error");
      install.disabled = later.disabled = skip.disabled = false;
    }
  };
  later.onclick = () => d.close();
  skip.onclick = () => (writePref(SKIPPED, u.version), d.close());
  footer.append(skip, el("span", { className: "spacer" }), later, install);
  d.showModal();
}

export function showReleaseNotes() {
  const { d, footer } = modal("Release Notes", "wide", el("pre", { className: "release-notes", textContent: changelog.trim() }));
  const ok = el("button", { className: "primary", textContent: "Close" });
  ok.onclick = () => d.close();
  footer.append(el("span", { className: "spacer" }), ok);
  d.showModal();
}
