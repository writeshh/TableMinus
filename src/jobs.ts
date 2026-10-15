// Background jobs on screen: one panel at the bottom right for every job's card (exports and imports from
// importexport.ts, backups and restores from backup.ts), and Notifications, this session's list of finished jobs.
// A card stays until dismissed; its job stays in Notifications (the header's Jobs button, or View › Notifications)
// after that, with Show in Finder for the files it wrote.

import { $, el, revealButton } from "./dom";
import { addCommands } from "./commands";

/** The one panel the cards stack in. */
export function jobsPanel() {
  return document.querySelector<HTMLElement>(".jobs") ?? document.body.appendChild(el("div", { className: "jobs" }));
}

type Note = { title: string; text: string; ok: boolean; at: number; paths: string[] };
const notes: Note[] = [];
const MAX_NOTES = 100; // a session's worth; the oldest go first
let unseen = 0;

const button = $("notes-button");
const badge = $("notes-badge");
const time = new Intl.DateTimeFormat(undefined, { timeStyle: "short" });

function showButton() {
  button.hidden = !notes.length;
  badge.hidden = !unseen;
  badge.textContent = String(unseen);
  button.title = unseen ? `${unseen} finished job${unseen === 1 ? "" : "s"} not yet seen` : "Finished jobs";
}

/** Records a finished job (also a failed or cancelled one) for Notifications. */
export function notify(title: string, text: string, ok: boolean, paths: string[] = []) {
  notes.unshift({ title, text, ok, at: Date.now(), paths });
  notes.length = Math.min(notes.length, MAX_NOTES);
  unseen++;
  showButton();
}

function openNotes() {
  unseen = 0;
  showButton();
  const list = el("ul", { className: "notes" });
  const draw = () => {
    list.replaceChildren(...notes.map((n) => el("li", { className: n.ok ? "" : "failed" },
      el("div", { className: "note-head" }, el("strong", { textContent: n.title }), el("span", { className: "spacer" }),
        el("span", { className: "note-time", textContent: time.format(n.at) })),
      el("div", { className: "note-text", textContent: n.text, title: n.text }),
      ...(n.ok && n.paths.length ? [revealButton(n.paths, "icon")] : []))));
    empty.hidden = notes.length > 0;
  };
  const empty = el("p", { className: "hint", textContent: "Exports, imports and backups appear here when they finish." });
  const clear = el("button", { type: "button", textContent: "Clear" });
  const close = el("button", { type: "button", className: "primary", textContent: "Close" });
  const d = el("dialog", { className: "notes-dialog" }, el("h3", { textContent: "Notifications" }), list, empty,
    el("footer", {}, clear, el("span", { className: "spacer" }), close));
  clear.onclick = () => ((notes.length = 0), draw(), showButton());
  close.onclick = () => d.close();
  d.addEventListener("close", () => d.remove());
  document.body.append(d);
  draw();
  d.showModal();
}

button.onclick = openNotes;
showButton();

addCommands([{ id: "notifications", label: "Notifications", menu: "View", run: openNotes }]);
