// The console log (⇧⌘C): every statement the app sends, as the backend records it (console.rs). Read while the
// panel is open, newest at the bottom.

import { invoke } from "./ipc";
import { $, el } from "./dom";
import { addCommands } from "./commands";
import { divider } from "./splitters";
import { highlightSql } from "./editor";

type Entry = { id: number; at: number; sql: string };

const KEEP = 1000; // as many as the backend keeps
const time = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });

const list = el("ol", { className: "console-list" });
const empty = el("p", { className: "hint empty", textContent: "Statements the app sends appear here." });
const clear = el("button", { type: "button", className: "ghost", textContent: "Clear" });
const close = el("button", { type: "button", className: "ghost", textContent: "×", title: "Close the console" });
const panel = el("section", { className: "console", hidden: true },
  el("header", {}, el("strong", { textContent: "Console" }), el("span", { className: "spacer" }), clear, close),
  el("div", { className: "console-body" }, list, empty));
$("workspace").querySelector("main")!.append(panel);
divider({
  host: panel, className: "console-edge", variable: "--console-h", pref: "layout.console", axis: "y", sign: -1, min: 80,
  max: () => panel.parentElement!.getBoundingClientRect().height - 160, size: () => panel.getBoundingClientRect().height,
  label: "Console height",
});
const button = el("button", { type: "button", className: "ghost", textContent: "Console", title: "Console log: every statement sent" });
$("new-sql").before(button);
button.onclick = () => toggle();

let last = 0;
let timer = 0;

async function poll() {
  try {
    const entries = await invoke<Entry[]>("console_log", { after: last });
    const body = panel.querySelector<HTMLElement>(".console-body")!;
    const atBottom = body.scrollTop + body.clientHeight >= body.scrollHeight - 4;
    for (const e of entries) {
      const sql = highlightSql(e.sql, "");
      sql.className = "console-sql";
      list.append(el("li", {}, el("span", { className: "console-when", textContent: time.format(e.at) }), sql));
      last = e.id;
    }
    while (list.childElementCount > KEEP) list.firstElementChild!.remove();
    empty.hidden = list.childElementCount > 0;
    if (atBottom && entries.length) body.scrollTop = body.scrollHeight;
  } catch {} // outside the app there's no backend; the panel just stays empty
}

function toggle(open = panel.hidden) {
  panel.hidden = !open;
  button.setAttribute("aria-pressed", String(open));
  clearInterval(timer);
  if (open) (poll(), (timer = window.setInterval(poll, 1000)));
}

clear.onclick = async () => {
  await invoke("console_clear").catch(() => {});
  list.replaceChildren();
  empty.hidden = false;
};
close.onclick = () => toggle(false);

addCommands([
  { id: "console", label: "Console Log", menu: "View", keys: ["Mod+Shift+KeyC"], when: () => !$("workspace").hidden, run: () => toggle() },
]);
